use super::*;
use crate::fs::{FileError, OpenedFile, Root};
use serde_json::json;
use std::fs::File;
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::{fs, io, thread, time};

const DATA_PATTERN: &str = "ARBITRARY DATA";

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

fn make_payload(size: usize) -> Vec<u8> {
    let pattern = DATA_PATTERN.as_bytes();
    pattern.iter().copied().cycle().take(size).collect()
}

fn get_test_data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests")
}

fn get_test_qcow() -> PathBuf {
    get_test_data_dir().join("test_disk_nbd.qcow2")
}

fn create_prerequisite_disk(path: &PathBuf) {
    let script = get_test_data_dir().join("build_test_qcow_disk.sh");
    let status = Command::new(&script)
        .arg(path.as_path())
        .arg(DATA_PATTERN)
        .status()
        .expect(format!("{:?} failed", script).as_str());
    if !status.success() {
        panic!("{script:?} failed");
    }
}

fn lock_tests_directory() -> io::Result<File> {
    let opened = File::open(get_test_data_dir())?;
    opened.lock()?;
    Ok(opened)
}

struct NBDServerProcess {
    process: Child,
    url: String,
}

impl NBDServerProcess {
    pub(super) fn get_url(&self) -> &str {
        &self.url
    }
}

impl Drop for NBDServerProcess {
    fn drop(&mut self) {
        self.process.kill().unwrap();
        self.process.wait().unwrap();
    }
}

fn run_nbd_server(listen_ip: &str) -> NBDServerProcess {
    let locked_tests_directory = lock_tests_directory().unwrap();
    let disk_path = get_test_qcow();
    if !disk_path.exists() {
        create_prerequisite_disk(&disk_path)
    }
    let export_name = "disk";
    let test_disk = get_test_qcow().to_string_lossy().to_string();
    let nbd_process = Command::new("qemu-nbd")
        .arg(format!("--bind={listen_ip}"))
        .arg("--port=0")
        .arg(format!("--export-name={export_name}"))
        .arg("--read-only")
        .arg(test_disk)
        .spawn()
        .unwrap();
    let listen_port = get_listen_tcp_port(nbd_process.id())
        .expect(format!("Could not get listener port for {nbd_process:?}").as_str());
    drop(locked_tests_directory);
    let nbd_url = String::from(format!("nbd://{listen_ip}:{listen_port}/{export_name}"));
    eprintln!("Started NBD server on {nbd_url}");
    NBDServerProcess {
        process: nbd_process,
        url: nbd_url,
    }
}

fn get_listen_tcp_port(pid: u32) -> io::Result<u16> {
    let inode = get_single_socket_inode(pid, time::Duration::new(5, 0))
        .expect(format!("Can't find an inode for PID {pid}").as_str());
    get_tcp_port(inode)
}

fn get_tcp_port(socket_inode: u64) -> io::Result<u16> {
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

fn get_single_socket_inode(pid: u32, timeout: time::Duration) -> io::Result<u64> {
    let deadline = time::Instant::now() + timeout;
    loop {
        let inodes = get_socket_inodes(pid)?;
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
                eprintln!("Found unexpected multiple socket inodes: {:?}", inodes);
                if time::Instant::now() > deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!("Found unexpected multiple socket inodes: {:?}", inodes),
                    ));
                }
                thread::sleep(time::Duration::from_millis(100));
            }
        }
    }
}

fn get_socket_inodes(pid: u32) -> io::Result<Vec<u64>> {
    let mut result = Vec::new();
    for fd_name in get_fd_symlink_names(pid)? {
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

fn get_fd_symlink_names(pid: u32) -> io::Result<Vec<String>> {
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

#[test]
fn test_add_nbd_disk() {
    let nbd_process = run_nbd_server("127.0.0.2");
    let start_time = time::Instant::now();
    let result = attach_nbd_disk(nbd_process.get_url());
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
    drop(nbd_process);
}

#[test]
fn test_add_non_existing_share_disk() {
    let nbd_process = run_nbd_server("127.0.0.2");
    let non_existing_share = "non_existing_share";
    let (url_prefix, _existing_share) = nbd_process.get_url().rsplit_once("/").unwrap();
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
    let nbd_process = run_nbd_server("127.0.0.2");
    let mut disk = attach_nbd_disk(nbd_process.get_url()).unwrap();
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
    let nbd_process = run_nbd_server("127.0.0.2");
    let mut disk = attach_nbd_disk(nbd_process.get_url()).unwrap();
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
    let nbd_process = run_nbd_server("127.0.0.2");
    let mut disk = attach_nbd_disk(nbd_process.get_url()).unwrap();
    let partitions = disk.list_partitions().unwrap();
    let root = partitions.get(1).unwrap();
    let boot = partitions.get(0).unwrap();
    assert!(root.mount_ro("/").is_ok());
    assert!(boot.mount_ro("/boot").is_ok());
    let chroot = RemoteChroot::new(disk, "/boot");
    let file = "aligned.file";
    let mut opened = chroot.open(file).unwrap();
    let expected_data = make_payload(opened.get_size().unwrap());
    let read_data = read_file(opened.as_mut());
    assert_eq!(read_data, expected_data);
}

#[test]
fn read_existing_nonaligned_file() {
    let nbd_process = run_nbd_server("127.0.0.2");
    let mut disk = attach_nbd_disk(nbd_process.get_url()).unwrap();
    let partitions = disk.list_partitions().unwrap();
    let root = partitions.get(1).unwrap();
    let boot = partitions.get(0).unwrap();
    assert!(root.mount_ro("/").is_ok());
    assert!(boot.mount_ro("/boot").is_ok());
    let chroot = RemoteChroot::new(disk, "/boot");
    let file = "nonaligned.file";
    let mut opened = chroot.open(file).unwrap();
    let expected_data = make_payload(opened.get_size().unwrap());
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
    let nbd_process = run_nbd_server("127.0.0.2");
    let config = json!({
        "url": nbd_process.get_url(),
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
