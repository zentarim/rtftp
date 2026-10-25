use crate::common::client::TFTPClient;
use std::any::type_name;
use std::fs::{File, create_dir};
use std::io::{BufRead, BufReader};
use std::net::{IpAddr, SocketAddr};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::{env, net, time};
use tokio::io::Interest;
use tokio::io::unix::AsyncFd;
use tokio::net::UdpSocket;
use tokio::task::JoinHandle;

pub(super) mod client;

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

fn get_free_port() -> u16 {
    let opened_socket = net::TcpListener::bind(("127.0.1.1", 0)).unwrap();
    opened_socket.local_addr().unwrap().port()
}

include!("nbd_server.rs");

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

pub(super) async fn start_rtftp(temp_dir: PathBuf) -> RunningServer {
    start_rtftp_with_idle_timeout(temp_dir, 30).await
}

pub(super) async fn start_rtftp_with_idle_timeout(
    temp_dir: PathBuf,
    idle_timeout: u64,
) -> RunningServer {
    let port = get_free_port();
    let ip = "127.0.0.10";
    let bin = env!("CARGO_BIN_EXE_rtftp");
    let mut process = Command::new(bin)
        .arg("--listen-ip")
        .arg(ip)
        .arg("--listen-port")
        .arg(port.to_string())
        .arg("--root-dir")
        .arg(temp_dir)
        .arg("--idle-timeout")
        .arg(idle_timeout.to_string())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stderr = process.stderr.take().unwrap();
    // The pipe holds 64KiB. This runtime drains it; blocking the test thread
    // would stall the current-thread runtime the tests already run on.
    set_nonblocking(stderr.as_raw_fd());
    let drain_fd = duplicate_fd(stderr.as_raw_fd());
    let stderr_output = Arc::new(Mutex::new(String::new()));
    let stderr_reader = spawn_stderr_reader(stderr, Arc::clone(&stderr_output));
    let listen_socket: SocketAddr = format!("{}:{}", ip, port).parse().unwrap();
    let server = RunningServer {
        process,
        listen_socket,
        stderr_output,
        stderr_reader,
        drain_fd,
    };
    let deadline = time::Instant::now() + time::Duration::from_secs(5);
    loop {
        if is_udp_port_open(listen_socket) && server.stderr_output().contains("Listening") {
            return server;
        }
        if time::Instant::now() >= deadline {
            panic!("rtftp did not listen\n{}", server.stderr_output());
        }
        tokio::time::sleep(time::Duration::from_millis(20)).await;
    }
}

pub(super) struct RunningServer {
    process: Child,
    pub(super) listen_socket: SocketAddr,
    stderr_output: Arc<Mutex<String>>,
    stderr_reader: JoinHandle<()>,
    drain_fd: OwnedFd,
}

impl RunningServer {
    pub(super) fn stderr_output(&self) -> String {
        self.stderr_output.lock().unwrap().clone()
    }

    pub(super) async fn open_paired_client(&self, source_ip: &str) -> TFTPClient {
        TFTPClient::new(
            UdpSocket::bind((source_ip, 0)).await.unwrap(),
            self.listen_socket,
        )
    }
}

impl Drop for RunningServer {
    fn drop(&mut self) {
        // Drop runs inside the current-thread runtime, so the reader task
        // cannot poll until this returns. Stop it and drain the duplicate
        // read end while the server exits.
        self.stderr_reader.abort();
        let pid = self.process.id();
        unsafe { libc::kill(pid as libc::pid_t, libc::SIGINT) };
        let drain_fd = self.drain_fd.as_raw_fd();
        let mut interest = libc::pollfd {
            fd: drain_fd,
            events: libc::POLLIN,
            revents: 0,
        };
        loop {
            interest.revents = 0;
            let polled = unsafe { libc::poll(&mut interest, 1, -1) };
            if polled < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                let _ = self.process.wait();
                break;
            }
            if (interest.revents & libc::POLLNVAL) != 0 {
                let _ = self.process.wait();
                break;
            }
            if drain_stderr(drain_fd, &self.stderr_output) {
                let _ = self.process.wait();
                break;
            }
            if let Ok(Some(_)) = self.process.try_wait() {
                let _ = drain_stderr(drain_fd, &self.stderr_output);
                break;
            }
        }
    }
}

fn set_nonblocking(fd: RawFd) {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        panic!("fcntl F_GETFL: {}", std::io::Error::last_os_error());
    }
    let status = unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
    if status < 0 {
        panic!("fcntl F_SETFL: {}", std::io::Error::last_os_error());
    }
}

fn duplicate_fd(fd: RawFd) -> OwnedFd {
    let duplicated = unsafe { libc::dup(fd) };
    if duplicated < 0 {
        panic!("dup: {}", std::io::Error::last_os_error());
    }
    // SAFETY: `dup` returned a new descriptor this function owns.
    unsafe { OwnedFd::from_raw_fd(duplicated) }
}

fn spawn_stderr_reader(
    stderr: std::process::ChildStderr,
    captured: Arc<Mutex<String>>,
) -> JoinHandle<()> {
    // SAFETY: `stderr` owns the pipe read end and is moved into `AsyncFd`,
    // which keeps that descriptor open until drop.
    let async_stderr = unsafe { AsyncFd::register_with_interest(stderr, Interest::READABLE) }
        .unwrap_or_else(|error| panic!("register rtftp stderr: {error}"));
    tokio::spawn(async move {
        let mut buffer = [0u8; 4096];
        loop {
            let mut guard = match async_stderr.readable().await {
                Ok(guard) => guard,
                Err(error) => panic!("rtftp stderr readiness: {error}"),
            };
            loop {
                match guard.try_io(|inner| read_fd(inner.as_raw_fd(), &mut buffer)) {
                    Ok(Ok(0)) => return,
                    Ok(Ok(count)) => append_stderr(&captured, &buffer[..count]),
                    Ok(Err(error)) => panic!("reading rtftp stderr: {error}"),
                    Err(_would_block) => break,
                }
            }
        }
    })
}

fn append_stderr(captured: &Mutex<String>, bytes: &[u8]) {
    let text = String::from_utf8_lossy(bytes);
    eprint!("{text}");
    captured.lock().unwrap().push_str(&text);
}

fn read_fd(fd: RawFd, buffer: &mut [u8]) -> std::io::Result<usize> {
    let count = unsafe { libc::read(fd, buffer.as_mut_ptr().cast::<libc::c_void>(), buffer.len()) };
    if count < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(usize::try_from(count).unwrap())
}

fn drain_stderr(fd: RawFd, captured: &Mutex<String>) -> bool {
    let mut buffer = [0u8; 4096];
    loop {
        match read_fd(fd, &mut buffer) {
            Ok(0) => return true,
            Ok(count) => append_stderr(captured, &buffer[..count]),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return false,
            Err(_) => return true,
        }
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
