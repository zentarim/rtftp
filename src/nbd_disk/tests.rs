use super::*;
use libc::{LOCK_EX, O_DIRECTORY, O_RDONLY, flock, open};
use serde_json::json;
use std::ffi::CString;
use std::fs::File;
use std::io::BufRead;
use std::os::fd::{FromRawFd, RawFd};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::{fs, io, thread, time};

const _DATA_PATTERN: &str = "ARBITRARY DATA";

fn _make_payload(size: usize) -> Vec<u8> {
    let pattern = _DATA_PATTERN.as_bytes();
    pattern.iter().copied().cycle().take(size).collect()
}
fn _get_test_data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests")
}

fn _get_test_qcow() -> PathBuf {
    _get_test_data_dir().join("test_disk.qcow2")
}

fn _ensure_prerequisite_disk() {
    if !_get_test_qcow().exists() {
        let script = _get_test_data_dir().join("build_test_disk.sh");
        let status = Command::new(&script)
            .arg(_get_test_qcow().as_path())
            .arg(_DATA_PATTERN)
            .status()
            .expect(format!("{:?} failed", script).as_str());

        if !status.success() {
            panic!("{script:?} failed");
        }
    }
}

fn _explicit_lock() -> io::Result<File> {
    let cwd_fd = _open_dir_ro(_get_test_data_dir().to_str().unwrap())?;
    if unsafe { flock(cwd_fd, LOCK_EX) } != 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { File::from_raw_fd(cwd_fd) })
    }
}

fn _open_dir_ro(path: &str) -> io::Result<RawFd> {
    let c_path = CString::new(path)?;
    let fd = unsafe { open(c_path.as_ptr(), O_RDONLY | O_DIRECTORY) as RawFd };
    if fd != 0 {
        Ok(fd)
    } else {
        Err(io::Error::last_os_error())
    }
}

struct _NBDServerProcess {
    process: Child,
    url: String,
}

impl Drop for _NBDServerProcess {
    fn drop(&mut self) {
        self.process.kill().unwrap();
        self.process.wait().unwrap();
    }
}

fn _run_nbd_server(listen_ip: &str) -> _NBDServerProcess {
    let lock_file = _explicit_lock().unwrap();
    _ensure_prerequisite_disk();
    let export_name = "disk";
    let test_disk = _get_test_qcow().to_string_lossy().to_string();
    let nbd_process = Command::new("qemu-nbd")
        .arg(format!("--bind={listen_ip}"))
        .arg("--port=0")
        .arg(format!("--export-name={export_name}"))
        .arg("--read-only")
        .arg(test_disk)
        .spawn()
        .unwrap();
    let listen_port = _get_listen_tcp_port(nbd_process.id())
        .expect(format!("Could not get listener port for {nbd_process:?}").as_str());
    drop(lock_file);
    let nbd_url = String::from(format!("nbd://{listen_ip}:{listen_port}/{export_name}"));
    eprintln!("Started NBD server on {nbd_url}");
    _NBDServerProcess {
        process: nbd_process,
        url: nbd_url,
    }
}

fn _get_listen_tcp_port(pid: u32) -> io::Result<u16> {
    let inode = _get_single_socket_inode(pid, time::Duration::new(5, 0))
        .expect(format!("Can't find an inode for PID {pid}").as_str());
    _get_tcp_port(inode)
}

fn _get_tcp_port(socket_inode: u64) -> io::Result<u16> {
    let path = Path::new("/proc/net/tcp");
    let file = fs::File::open(path)?;
    let reader = io::BufReader::new(file);
    for (index, line_res) in reader.lines().enumerate() {
        let line = line_res?;
        if index == 0 {
            continue;
        }
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 10 {
            continue;
        }
        let inode_field = fields[9];
        if inode_field.parse::<u64>().ok() != Some(socket_inode) {
            continue;
        }
        let port = match fields[1].split_once(':') {
            Some((_hex_ip, hex_port)) => u16::from_str_radix(hex_port, 16).unwrap(),
            None => continue,
        };
        return Ok(port);
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!("Can't find TCP socket for inode {socket_inode}"),
    ))
}

fn _get_single_socket_inode(pid: u32, timeout: time::Duration) -> io::Result<u64> {
    let deadline = time::Instant::now() + timeout;
    loop {
        let inodes = _get_socket_inodes(pid)?;
        match inodes.len() {
            0 => {
                if time::Instant::now() > deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!("Can't find a socket inode for pid {pid}"),
                    ));
                }
                thread::sleep(time::Duration::from_millis(100));
            }
            1 => return Ok(inodes[0]),
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    format!("Found unexpected multiple socket inodes: {:?}", inodes),
                ));
            }
        }
    }
}

fn _get_socket_inodes(pid: u32) -> io::Result<Vec<u64>> {
    let mut result = Vec::new();
    for fd_name in _get_fd_symlink_names(pid)? {
        if let Some(inode_str) = fd_name.strip_prefix("socket:[") {
            if let Some(inode_str) = inode_str.strip_suffix(']') {
                if let Ok(inode) = inode_str.parse::<u64>() {
                    result.push(inode);
                }
            }
        }
    }
    Ok(result)
}

fn _get_fd_symlink_names(pid: u32) -> io::Result<Vec<String>> {
    let fd_dir = PathBuf::from(format!("/proc/{pid}/fd"));
    let entries = match fs::read_dir(&fd_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::PermissionDenied => {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("PID {pid} does not exist or is not accessible"),
            ));
        }
        Err(err) => return Err(err),
    };
    let mut result = Vec::new();
    for entry in entries {
        match fs::read_link(entry?.path()) {
            Ok(target) => {
                if let Some(name) = target.to_str() {
                    result.push(name.to_string());
                }
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
            Err(err) => return Err(err),
        }
    }
    Ok(result)
}

fn read_file(opened: &mut dyn OpenedFile) -> Vec<u8> {
    let mut buffer = vec![];
    let mut chunk = vec![0u8; 512];
    loop {
        let read_size = opened.read_to(&mut chunk).unwrap();
        if read_size == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read_size]);
    }
    buffer
}

#[test]
fn test_add_nbd_disk() {
    let nbd_url = &_run_nbd_server("127.0.0.2").url;
    let start_time = time::Instant::now();
    let result = attach_nbd_disk(nbd_url);
    assert!(
        result.is_ok(),
        "Expected Ok, got Err: {:?}",
        result.unwrap_err()
    );
    let mut disk = result.unwrap();
    let partitions = disk.list_partitions().unwrap();
    eprintln!("{:?}", partitions);
    eprintln!("{}", disk);
    let end_time = time::Instant::now();
    eprintln!(
        "Spent: {:.3} s",
        end_time.duration_since(start_time).as_secs_f64()
    );
}

#[test]
fn test_add_non_existing_share_disk() {
    let nbd_url = &_run_nbd_server("127.0.0.2").url;
    let non_existing_share = "non_existing_share";
    let (url_prefix, _existing_share) = nbd_url.rsplit_once("/").unwrap();
    let non_exising_share = vec![url_prefix, non_existing_share].join("/");
    let result = attach_nbd_disk(non_exising_share);
    assert!(result.is_err(), "Unexpected success received");
    assert!(matches!(
        result.err().unwrap(),
        GuestFSError::ShareNotFound(_)
    ))
}

#[test]
fn test_add_invalid_url() {
    let non_existent_nbd_url = "nbd://127.1.1.1:1/invalid";
    let result = attach_nbd_disk(non_existent_nbd_url);
    assert!(result.is_err());
    assert!(matches!(
        result.err().unwrap(),
        GuestFSError::ConnectionRefused(_)
    ))
}

#[test]
fn open_existing_file() {
    let nbd_url = &_run_nbd_server("127.0.0.2").url;
    let mut disk = attach_nbd_disk(nbd_url).unwrap();
    let partitions = disk.list_partitions().unwrap();
    let boot = partitions.get(0).unwrap();
    let root = partitions.get(1).unwrap();
    assert!(root.mount_ro("/").is_ok());
    assert!(boot.mount_ro("/boot").is_ok());
    let file = "/boot/aligned.file";
    let opened = disk.open(file);
    assert!(opened.is_ok());
    assert_eq!(opened.unwrap().get_size(), Ok(4194304));
}

#[test]
fn open_non_existing_file() {
    let nbd_url = &_run_nbd_server("127.0.0.2").url;
    let mut disk = attach_nbd_disk(nbd_url).unwrap();
    let partitions = disk.list_partitions().unwrap();
    let root = partitions.get(1).unwrap();
    assert!(root.mount_ro("/").is_ok());
    let file = "/nonexisting/file";
    let opened = disk.open(file);
    assert!(opened.is_err());
    assert!(matches!(opened.err().unwrap(), FileError::FileNotFound))
}

#[test]
fn read_existing_aligned_file() {
    let nbd_url = &_run_nbd_server("127.0.0.2").url;
    let mut disk = attach_nbd_disk(nbd_url).unwrap();
    let partitions = disk.list_partitions().unwrap();
    let root = partitions.get(1).unwrap();
    let boot = partitions.get(0).unwrap();
    assert!(root.mount_ro("/").is_ok());
    assert!(boot.mount_ro("/boot").is_ok());
    let chroot = RemoteChroot::new(Box::new(disk), "/boot");
    let file = "aligned.file";
    let mut opened = chroot.open(file).unwrap();
    let expected_data = _make_payload(opened.get_size().unwrap());
    let read_data = read_file(opened.as_mut());
    assert_eq!(read_data, expected_data);
}

#[test]
fn read_existing_nonaligned_file() {
    let nbd_url = &_run_nbd_server("127.0.0.2").url;
    let mut disk = attach_nbd_disk(nbd_url).unwrap();
    let partitions = disk.list_partitions().unwrap();
    let root = partitions.get(1).unwrap();
    let boot = partitions.get(0).unwrap();
    assert!(root.mount_ro("/").is_ok());
    assert!(boot.mount_ro("/boot").is_ok());
    let chroot = RemoteChroot::new(Box::new(disk), "/boot");
    let file = "nonaligned.file";
    let mut opened = chroot.open(file).unwrap();
    let expected_data = _make_payload(opened.get_size().unwrap());
    let read_data = read_file(opened.as_mut());
    assert_eq!(read_data, expected_data);
}

#[test]
fn build_config() {
    let config = json!({
        "url": "nbd://127.0.0.1:1000/arbitrary",
        "mounts": [
            {
                "partition": 2,
                "mountpoint": "/",
            },
                {
                "partition": 1,
                "mountpoint": "/boot",
            }
        ],
        "tftp_root": "/boot",
    }
    );
    let nbd_config = NBDConfig::from_json(&config);
    assert!(nbd_config.is_some());
}

#[test]
fn connect_from_config() {
    let nbd_url = &_run_nbd_server("127.0.0.2").url;
    let config = json!({
        "url": nbd_url,
        "mounts": [
            {
                "partition": 2,
                "mountpoint": "/",
            },
                {
                "partition": 1,
                "mountpoint": "/boot",
            }
        ],
        "tftp_root": "/boot",
    }
    );
    let nbd_config = NBDConfig::from_json(&config).unwrap();
    let running_disk = nbd_config.connect();
    assert!(running_disk.is_ok());
}
