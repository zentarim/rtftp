use crate::common::client::TFTPClient;
use std::any::type_name;
use std::fs::{File, create_dir};
use std::io::{BufRead, BufReader};
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::process::{Child, Command};
use std::{env, net, time};
use tokio::net::UdpSocket;

pub(crate) mod client;

const DATA_PATTERN: &str = "ARBITRARY DATA";

pub(super) fn make_payload(size: usize) -> Vec<u8> {
    let pattern = DATA_PATTERN.as_bytes();
    pattern.iter().copied().cycle().take(size).collect()
}

fn ensure_prerequisite_disk() -> (PathBuf, File) {
    let test_data_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests");
    let test_disk = test_data_dir.join("test_disk.qcow2");
    let file = File::open(&test_data_dir).unwrap();
    file.lock().unwrap();
    if !test_disk.exists() {
        let script = test_data_dir.join("build_test_qcow_disk.sh");
        let status = Command::new(&script)
            .arg(&test_disk)
            .arg(DATA_PATTERN)
            .status()
            .expect(format!("{:?} failed", script).as_str());
        if !status.success() {
            panic!("{script:?} failed");
        }
    }
    (test_disk, file)
}

pub(super) fn get_free_port() -> u16 {
    let opened_socket = net::TcpListener::bind(("127.0.1.1", 0)).unwrap();
    opened_socket.local_addr().unwrap().port()
}

include!("nbd_server.rs");

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
