use crate::common::client::TFTPClient;
use std::any::type_name;
use std::fs::{File, create_dir};
use std::io::{BufRead, BufReader};
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::{env, fs, io, net, thread, time};
use tokio::net::UdpSocket;

pub(crate) mod client;

const _DATA_PATTERN: &str = "ARBITRARY DATA";

pub(super) fn get_free_port() -> u16 {
    let opened_socket = net::TcpListener::bind(("127.0.1.1", 0)).unwrap();
    opened_socket.local_addr().unwrap().port()
}

pub(super) fn make_payload(size: usize) -> Vec<u8> {
    let pattern = _DATA_PATTERN.as_bytes();
    pattern.iter().copied().cycle().take(size).collect()
}

fn get_test_data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests")
}

fn get_test_qcow() -> PathBuf {
    get_test_data_dir().join("test_disk.qcow2")
}

fn _ensure_prerequisite_disk() {
    if !get_test_qcow().exists() {
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

pub(crate) struct NBDServerProcess {
    process: Child,
    url: String,
}

impl NBDServerProcess {
    pub(crate) fn get_url(&self) -> &str {
        &self.url
    }
}

impl Drop for NBDServerProcess {
    fn drop(&mut self) {
        self.process.kill().unwrap();
        self.process.wait().unwrap();
    }
}

pub(crate) fn run_nbd_server(listen_ip: &str) -> NBDServerProcess {
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
    NBDServerProcess {
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

pub fn mk_tmp<T>(test_func: T) -> PathBuf {
    let test_dir_name = get_fn_name(test_func).replace("::", "_");
    let pid = std::process::id();
    let test_tmp_dir = env::temp_dir().join(format!("rtftp_{pid}_{test_dir_name}"));
    create_dir(&test_tmp_dir).unwrap();
    test_tmp_dir
}

pub(super) async fn start_rtftp(temp_dir: PathBuf) -> RunningServer {
    let port = get_free_port();
    let ip = "127.0.0.10";
    let bin = env!("CARGO_BIN_EXE_rtftp");
    let process = Command::new(bin)
        .arg("--listen-ip")
        .arg(ip)
        .arg("--listen-port")
        .arg(port.to_string())
        .arg("--root-dir")
        .arg(temp_dir)
        .arg("--idle-timeout")
        .arg("30")
        .spawn()
        .unwrap();
    let listen_socket: SocketAddr = format!("{}:{}", ip, port).parse().unwrap();
    while !is_udp_port_open(listen_socket) {
        tokio::time::sleep(time::Duration::from_millis(50)).await;
    }
    RunningServer {
        process,
        listen_socket,
    }
}

pub(super) struct RunningServer {
    process: Child,
    pub(super) listen_socket: SocketAddr,
}

impl RunningServer {
    pub(crate) async fn open_paired_client(&self, source_ip: &str) -> TFTPClient {
        TFTPClient::new(
            UdpSocket::bind((source_ip, 0)).await.unwrap(),
            self.listen_socket,
        )
    }
}

impl Drop for RunningServer {
    fn drop(&mut self) {
        let pid = self.process.id();
        unsafe { libc::kill(pid as libc::pid_t, libc::SIGINT) };
        self.process.wait().unwrap();
    }
}

fn is_udp_port_open(addr: SocketAddr) -> bool {
    let port = addr.port();
    let ip = match addr.ip() {
        IpAddr::V4(ipv4) => u32::from_ne_bytes(ipv4.octets()),
        _ => return false,
    };
    let Ok(file) = File::open("/proc/net/udp") else {
        return false;
    };
    for line in BufReader::new(file).lines().flatten().skip(1) {
        let parts: Vec<_> = line.split_whitespace().collect();
        if parts.len() < 2 {
            continue;
        }
        if let Some(local) = parts.get(1) {
            if let Some((ip_hex, port_hex)) = local.split_once(':') {
                if let (Ok(ip_val), Ok(port_val)) = (
                    u32::from_str_radix(ip_hex, 16),
                    u16::from_str_radix(port_hex, 16),
                ) {
                    if ip_val == ip && port_val == port {
                        return true;
                    }
                }
            }
        }
    }
    false
}
