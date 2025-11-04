use std::any::type_name;
use std::fs::{File, create_dir};
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::{env, fs, io, thread, time};

const _DATA_PATTERN: &str = "ARBITRARY DATA";

pub(super) fn make_payload(size: usize) -> Vec<u8> {
    let pattern = _DATA_PATTERN.as_bytes();
    pattern.iter().copied().cycle().take(size).collect()
}

pub(super) fn get_test_data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests")
}

pub(super) fn get_test_qcow() -> PathBuf {
    get_test_data_dir().join("test_disk.qcow2")
}

pub(super) fn ensure_prerequisite_disk() {
    let lock = _lock_tests_directory().unwrap();
    if !get_test_qcow().exists() {
        _create_prerequisite_disk()
    }
    drop(lock);
}

fn _create_prerequisite_disk() {
    let script = get_test_data_dir().join("build_test_qcow_disk.sh");
    let status = Command::new(&script)
        .arg(get_test_qcow().as_path())
        .arg(_DATA_PATTERN)
        .status()
        .expect(format!("{:?} failed", script).as_str());
    if !status.success() {
        panic!("{script:?} failed");
    }
}

fn _lock_tests_directory() -> io::Result<File> {
    let opened = File::open(get_test_data_dir())?;
    opened.lock()?;
    Ok(opened)
}

pub(super) struct _NBDServerProcess {
    process: Child,
    url: String,
}

impl _NBDServerProcess {
    pub(super) fn get_url(&self) -> &str {
        &self.url
    }
}

impl Drop for _NBDServerProcess {
    fn drop(&mut self) {
        self.process.kill().unwrap();
        self.process.wait().unwrap();
    }
}

pub(super) fn run_nbd_server(listen_ip: &str) -> _NBDServerProcess {
    let locked_tests_directory = _lock_tests_directory().unwrap();
    if !get_test_qcow().exists() {
        _create_prerequisite_disk()
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
    let listen_port = _get_listen_tcp_port(nbd_process.id())
        .expect(format!("Could not get listener port for {nbd_process:?}").as_str());
    drop(locked_tests_directory);
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

fn get_fn_name<T>(_: T) -> &'static str {
    type_name::<T>()
}

pub(super) fn mk_tmp<T>(test_func: T) -> PathBuf {
    let test_dir_name = get_fn_name(test_func).replace("::", "_");
    let pid = std::process::id();
    let test_tmp_dir = env::temp_dir().join(format!("rtftp_{pid}_{test_dir_name}"));
    create_dir(&test_tmp_dir).unwrap();
    test_tmp_dir
}
