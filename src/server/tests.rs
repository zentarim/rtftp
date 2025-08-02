use super::*;
use crate::cursor::{ReadCursor, WriteCursor};
use libc::{LOCK_EX, O_DIRECTORY, O_RDONLY, flock, open};
use serde_json::json;
use std::any::type_name;
use std::ffi::{CStr, CString};
use std::fs::{File, Permissions, create_dir, set_permissions};
use std::io::{BufRead, Write};
use std::os::fd::{FromRawFd, RawFd};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::thread::JoinHandle;
use std::{env, fmt, fs, io, thread, time};

const _DATA_PATTERN: &str = "ARBITRARY DATA";
const _BUFFER_SIZE: usize = 1536;
const _U16_SIZE: usize = size_of::<u16>();
const _RRQ: u16 = 0x01;
const _DATA: u16 = 0x03;
const _ACK: u16 = 0x04;
const _ERR: u16 = 0x05;
const _OACK: u16 = 0x06;

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
    let cwd_fd = open_dir_ro(_get_test_data_dir().to_str().unwrap())?;
    if unsafe { flock(cwd_fd, LOCK_EX) } != 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { File::from_raw_fd(cwd_fd) })
    }
}

fn open_dir_ro(path: &str) -> io::Result<RawFd> {
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
    let inode = get_single_socket_inode(pid, time::Duration::new(5, 0))
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
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    format!("Found unexpected multiple socket inodes: {:?}", inodes),
                ));
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
        let entry = entry?;
        match fs::read_link(entry.path()) {
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

fn mk_tmp<T>(test_func: T) -> PathBuf {
    let test_dir_name = get_fn_name(test_func).replace("::", "_");
    let pid = std::process::id();
    let test_tmp_dir = env::temp_dir().join(format!("rtftp_{pid}_{test_dir_name}"));
    create_dir(&test_tmp_dir).unwrap();
    test_tmp_dir
}

#[derive(Debug)]
struct _ThreadedTFTPServer {
    shutdown_flag: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    listen_socket: SocketAddr,
}

impl _ThreadedTFTPServer {
    fn new(root_dir: PathBuf, bind_ip: &str, idle_timeout: u64) -> Self {
        let server_socket = UdpSocket::bind((bind_ip, 0)).unwrap();
        let listen_socket = server_socket.local_addr().unwrap();
        let mut server = TFTPServer::new(server_socket, root_dir, idle_timeout);
        let shutdown_flag = Arc::new(AtomicBool::new(false));
        let shutdown_flag_clone = shutdown_flag.clone();
        let handle = thread::spawn(move || {
            if let Err(serve_result) = server.serve_until_shutdown(&*shutdown_flag_clone) {
                eprintln!("{server} shutdown error: {serve_result:?}");
            } else {
                eprintln!("{server} shutdown successfully");
            }
        });
        Self {
            shutdown_flag,
            handle: Some(handle),
            listen_socket,
        }
    }

    fn open_paired_client(&self, source_ip: &str) -> _TFTPClient {
        _TFTPClient::new(UdpSocket::bind((source_ip, 0)).unwrap(), self.listen_socket)
    }
}

impl Drop for _ThreadedTFTPServer {
    fn drop(&mut self) {
        self.shutdown_flag.store(true, atomic::Ordering::Relaxed);
        let handle = self.handle.take().unwrap();
        handle.join().unwrap();
    }
}

#[derive(Debug)]
struct _SendError<T> {
    message: String,
    sent: T,
}

#[derive(Debug)]
enum _Error<T: fmt::Debug> {
    IO(io::Error),
    Timeout(T),
    ClientError(u16, String),
    ParseError(String),
    UnexpectedData(Vec<u8>),
    UnexpectedPeer(IpAddr, Vec<u8>),
}

impl<T: fmt::Debug> Display for _Error<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            _Error::Timeout(msg) => write!(f, "Timeout: {msg:?}"),
            _Error::ClientError(code, msg) => write!(f, "ACK: [{code}] {msg}"),
            _Error::IO(error) => write!(f, "IO: {error:?} "),
            _Error::ParseError(error) => write!(f, "ParseError: {error}"),
            _Error::UnexpectedData(data) => write!(f, "UnexpectedData: {data:?}"),
            _Error::UnexpectedPeer(remote_ip, data) => {
                write!(f, "Unexpected peer {remote_ip}: {}", data.len())
            }
        }
    }
}

impl<T: fmt::Debug> std::error::Error for _Error<T> {}

impl<T: fmt::Debug> _Error<T> {
    #[allow(dead_code)]
    fn unpack(self) -> Option<T> {
        if let _Error::Timeout(sent) = self {
            Some(sent)
        } else {
            None
        }
    }

    #[allow(dead_code)]
    fn client_error(self) -> Option<(u16, String)> {
        if let _Error::ClientError(code, message) = self {
            Some((code, message))
        } else {
            None
        }
    }
}

#[derive(Debug)]
struct _TFTPClient {
    local_socket: UdpSocket,
    remote_addr: SocketAddr,
    read_buffer: [u8; _BUFFER_SIZE],
    write_buffer: [u8; _BUFFER_SIZE],
}

impl _TFTPClient {
    fn new(local_socket: UdpSocket, remote_addr: SocketAddr) -> Self {
        Self {
            local_socket,
            remote_addr,
            read_buffer: [0; _BUFFER_SIZE],
            write_buffer: [0; _BUFFER_SIZE],
        }
    }

    fn send_plain_read_request(mut self, file_name: &str) -> io::Result<_SentPlainReadRequest> {
        let (_write_cursor, buffer_size) = self.make_read_request(file_name);
        self.local_socket
            .send_to(&self.write_buffer[..buffer_size], &self.remote_addr)?;
        Ok(_SentPlainReadRequest {
            file_name: file_name.to_string(),
            local_socket: self.local_socket,
            remote_addr: self.remote_addr,
            read_buffer: self.read_buffer,
            write_buffer: self.write_buffer,
            sent_bytes: buffer_size,
        })
    }

    fn send_optioned_read_request(
        mut self,
        file_name: &str,
        options: &HashMap<String, String>,
    ) -> io::Result<_SentReadRequestWithOpts> {
        let (mut write_cursor, mut buffer_size) = self.make_read_request(file_name);
        for (option_name, option_value) in options {
            _ = write_cursor.put_string(option_name).unwrap();
            buffer_size = write_cursor.put_string(option_value).unwrap();
        }
        self.local_socket
            .send_to(&self.write_buffer[..buffer_size], &self.remote_addr)?;
        Ok(_SentReadRequestWithOpts {
            file_name: file_name.to_string(),
            options: options.clone(),
            local_socket: self.local_socket,
            remote_addr: self.remote_addr,
            read_buffer: self.read_buffer,
            write_buffer: self.write_buffer,
            sent_bytes: buffer_size,
        })
    }

    fn make_read_request(&mut self, file_name: &str) -> (WriteCursor, usize) {
        let mut write_cursor = WriteCursor::new(&mut self.write_buffer);
        _ = write_cursor.put_ushort(_RRQ).unwrap();
        _ = write_cursor.put_string(file_name).unwrap();
        let size = write_cursor.put_string("octet").unwrap();
        (write_cursor, size)
    }
}

struct _DatagramStream {
    local_socket: UdpSocket,
    peer_address: SocketAddr,
    display: String,
}

impl fmt::Debug for _DatagramStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.display)
    }
}

impl _DatagramStream {
    fn new(local_socket: UdpSocket, peer_address: SocketAddr) -> Self {
        let local_address = local_socket.local_addr().unwrap();
        let local_ip = local_address.ip().to_string();
        let local_port = local_address.port().to_string();
        let remote_ip = peer_address.ip().to_string();
        let remote_port = peer_address.port().to_string();
        let display = format!("{local_ip}:{local_port} <=> {remote_ip}:{remote_port}");
        Self {
            local_socket,
            peer_address,
            display,
        }
    }

    fn send(&self, buffer: &[u8]) -> io::Result<()> {
        match self.local_socket.send_to(buffer, self.peer_address) {
            Ok(sent) => {
                if sent != buffer.len() {
                    Err(ErrorKind::ConnectionReset.into())
                } else {
                    Ok(())
                }
            }
            Err(error) => Err(error),
        }
    }

    fn recv(&self, buffer: &mut [u8], read_timeout: usize, min_size: usize) -> io::Result<usize> {
        let mut now = time::Instant::now();
        let end_at = now + Duration::from_secs(read_timeout as u64);
        while now <= end_at {
            self.local_socket.set_read_timeout(Some(end_at - now))?;
            match self.local_socket.recv_from(buffer) {
                Ok((recv_size, remote_address)) => {
                    if remote_address != self.peer_address {
                        eprintln!(
                            "{self}: Ignore datagram {recv_size} long from alien {remote_address}"
                        );
                    } else if recv_size < min_size {
                        eprintln!("{self}: Ignore runt datagram {recv_size} long");
                    } else {
                        return Ok(recv_size);
                    }
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    return Err(ErrorKind::TimedOut.into());
                }
                Err(error) => return Err(error),
            }
            now = time::Instant::now();
        }
        Err(ErrorKind::TimedOut.into())
    }
}

impl Display for _DatagramStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.display)
    }
}

struct _SentReadRequestWithOpts {
    file_name: String,
    options: HashMap<String, String>,
    local_socket: UdpSocket,
    remote_addr: SocketAddr,
    read_buffer: [u8; _BUFFER_SIZE],
    write_buffer: [u8; _BUFFER_SIZE],
    sent_bytes: usize,
}

impl fmt::Debug for _SentReadRequestWithOpts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "_SentReadRequestWithOpts")
    }
}

impl _SentReadRequestWithOpts {
    fn read_oack(mut self, read_timeout: usize) -> Result<_OACK, _Error<Self>> {
        self.local_socket
            .set_read_timeout(Some(Duration::from_secs(read_timeout as u64)))
            .unwrap();
        match self.local_socket.recv_from(&mut self.read_buffer) {
            Ok((read_bytes, remote_address)) if remote_address.ip() == self.remote_addr.ip() => {
                let mut read_cursor = ReadCursor::new(&mut self.read_buffer[..read_bytes]);
                match read_cursor.extract_ushort() {
                    Ok(code) if code == _OACK => Ok(_OACK {
                        datagram_stream: _DatagramStream::new(self.local_socket, remote_address),
                        read_buffer: self.read_buffer,
                        write_buffer: self.write_buffer,
                        read_bytes,
                    }),
                    Ok(code) if code == _ERR => {
                        let error_code = read_cursor.extract_ushort().unwrap();
                        let message = read_cursor.extract_string().unwrap();
                        Err(_Error::ClientError(error_code, message))
                    }
                    Ok(_code) => Err(_Error::UnexpectedData(
                        self.read_buffer[..read_bytes].to_vec(),
                    )),
                    Err(parse_error) => Err(_Error::ParseError(format!("{parse_error:?}"))),
                }
            }
            Ok((read_bytes, remote_address)) => Err(_Error::UnexpectedPeer(
                remote_address.ip(),
                self.read_buffer[..read_bytes].to_vec(),
            )),
            Err(error) if error.kind() == ErrorKind::TimedOut => {
                Err(_Error::Timeout(_SentReadRequestWithOpts {
                    file_name: self.file_name,
                    options: self.options,
                    local_socket: self.local_socket,
                    remote_addr: self.remote_addr,
                    read_buffer: self.read_buffer,
                    write_buffer: self.write_buffer,
                    sent_bytes: self.sent_bytes,
                }))
            }
            Err(error) => Err(_Error::IO(error)),
        }
    }
}

struct _SentPlainReadRequest {
    file_name: String,
    local_socket: UdpSocket,
    remote_addr: SocketAddr,
    read_buffer: [u8; _BUFFER_SIZE],
    write_buffer: [u8; _BUFFER_SIZE],
    sent_bytes: usize,
}

impl fmt::Debug for _SentPlainReadRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:?} => {} {}",
            self.local_socket, self.remote_addr, self.file_name
        )
    }
}

impl _SentPlainReadRequest {
    fn read_next(mut self, read_timeout: usize) -> Result<_Block, _Error<Self>> {
        self.local_socket
            .set_read_timeout(Some(Duration::from_secs(read_timeout as u64)))
            .unwrap();
        match self.local_socket.recv_from(&mut self.read_buffer) {
            Ok((read_bytes, remote_address)) if remote_address.ip() == self.remote_addr.ip() => {
                let mut read_cursor = ReadCursor::new(&mut self.read_buffer[..read_bytes]);
                match read_cursor.extract_ushort() {
                    Ok(code) if code == _DATA => Ok(_Block {
                        datagram_stream: _DatagramStream::new(self.local_socket, remote_address),
                        read_buffer: self.read_buffer,
                        write_buffer: self.write_buffer,
                        read_bytes,
                    }),
                    Ok(code) if code == _ERR => {
                        let error_code = read_cursor.extract_ushort().unwrap();
                        let message = read_cursor.extract_string().unwrap();
                        Err(_Error::ClientError(error_code, message))
                    }
                    Ok(_code) => Err(_Error::UnexpectedData(
                        self.read_buffer[..read_bytes].to_vec(),
                    )),
                    Err(parse_error) => Err(_Error::ParseError(format!("{parse_error:?}"))),
                }
            }
            Ok((read_bytes, remote_address)) => Err(_Error::UnexpectedPeer(
                remote_address.ip(),
                self.read_buffer[..read_bytes].to_vec(),
            )),
            Err(error) if error.kind() == ErrorKind::TimedOut => {
                Err(_Error::Timeout(_SentPlainReadRequest {
                    file_name: self.file_name,
                    local_socket: self.local_socket,
                    remote_addr: self.remote_addr,
                    read_buffer: self.read_buffer,
                    write_buffer: self.write_buffer,
                    sent_bytes: self.sent_bytes,
                }))
            }
            Err(error) => Err(_Error::IO(error)),
        }
    }
}

struct _OACK {
    datagram_stream: _DatagramStream,
    read_buffer: [u8; _BUFFER_SIZE],
    write_buffer: [u8; _BUFFER_SIZE],
    read_bytes: usize,
}

impl fmt::Debug for _OACK {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "OACK")
    }
}

impl _OACK {
    fn fields(&self) -> HashMap<String, String> {
        let mut fields: HashMap<String, String> = HashMap::new();
        let mut cursor = ReadCursor::new(&self.read_buffer[2..self.read_bytes]);
        while let Ok(option) = cursor.extract_string() {
            fields.insert(option, cursor.extract_string().unwrap());
        }
        fields
    }
    fn acknowledge(mut self) -> Result<_SentACK, _Error<Self>> {
        let mut write_cursor = WriteCursor::new(&mut self.write_buffer);
        _ = write_cursor.put_ushort(_ACK).unwrap();
        let buffer_size = write_cursor.put_ushort(0u16).unwrap();
        self.datagram_stream
            .send(&self.write_buffer[..buffer_size])
            .or_else(|error| Err(_Error::IO(error)))?;
        Ok(_SentACK {
            datagram_stream: self.datagram_stream,
            read_buffer: self.read_buffer,
            write_buffer: self.write_buffer,
            buffer_size: buffer_size,
        })
    }
}

struct _Block {
    datagram_stream: _DatagramStream,
    read_buffer: [u8; _BUFFER_SIZE],
    write_buffer: [u8; _BUFFER_SIZE],
    read_bytes: usize,
}

impl fmt::Debug for _Block {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Block size [{}]", self.read_bytes - _U16_SIZE)
    }
}

impl _Block {
    fn data(&self) -> &[u8] {
        &self.read_buffer[_U16_SIZE * 2..self.read_bytes]
    }
    fn acknowledge(mut self) -> Result<_SentACK, _Error<Self>> {
        let mut write_cursor = WriteCursor::new(&mut self.write_buffer);
        _ = write_cursor.put_ushort(_ACK).unwrap();
        let block_num = u16::from_be_bytes([self.read_buffer[2], self.read_buffer[3]]);
        let buffer_size = write_cursor.put_ushort(block_num).unwrap();
        self.datagram_stream
            .send(&self.write_buffer[..buffer_size])
            .unwrap();
        Ok(_SentACK {
            datagram_stream: self.datagram_stream,
            read_buffer: self.read_buffer,
            write_buffer: self.write_buffer,
            buffer_size,
        })
    }

    fn send_error(mut self, code: u16, message: &str) -> Result<_SentError, _Error<Self>> {
        let mut write_cursor = WriteCursor::new(&mut self.write_buffer);
        _ = write_cursor.put_ushort(_ERR).unwrap();
        _ = write_cursor.put_ushort(code).unwrap();
        let buffer_size = write_cursor.put_string(message).unwrap();
        self.datagram_stream
            .send(&self.write_buffer[..buffer_size])
            .unwrap();
        Ok(_SentError {
            datagram_stream: self.datagram_stream,
            read_buffer: self.read_buffer,
            write_buffer: self.write_buffer,
            write_bytes: buffer_size,
        })
    }
}

struct _SentACK {
    datagram_stream: _DatagramStream,
    read_buffer: [u8; _BUFFER_SIZE],
    write_buffer: [u8; _BUFFER_SIZE],
    buffer_size: usize,
}

impl fmt::Debug for _SentACK {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "<SentACK {:?} ({})>",
            self.datagram_stream, self.buffer_size
        )
    }
}

impl _SentACK {
    fn read_next(mut self, read_timeout: usize) -> Result<_Block, _Error<Self>> {
        match self
            .datagram_stream
            .recv(&mut self.read_buffer, read_timeout, 4)
        {
            Ok(read_bytes) => {
                let mut read_cursor = ReadCursor::new(&mut self.read_buffer[..read_bytes]);
                match read_cursor.extract_ushort() {
                    Ok(code) if code == _DATA => Ok(_Block {
                        datagram_stream: self.datagram_stream,
                        read_buffer: self.read_buffer,
                        write_buffer: self.write_buffer,
                        read_bytes,
                    }),
                    Ok(code) if code == _ERR => {
                        let error_code = read_cursor.extract_ushort().unwrap();
                        let message = read_cursor.extract_string().unwrap();
                        Err(_Error::ClientError(error_code, message))
                    }
                    Ok(_code) => Err(_Error::UnexpectedData(
                        self.read_buffer[..read_bytes].to_vec(),
                    )),
                    Err(parse_error) => Err(_Error::ParseError(format!("{parse_error:?}"))),
                }
            }
            Err(error) if error.kind() == ErrorKind::TimedOut => Err(_Error::Timeout(self)),
            Err(error) => Err(_Error::IO(error)),
        }
    }
}

#[allow(dead_code)]
struct _SentError {
    datagram_stream: _DatagramStream,
    read_buffer: [u8; _BUFFER_SIZE],
    write_buffer: [u8; _BUFFER_SIZE],
    write_bytes: usize,
}

impl fmt::Debug for _SentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "<SentError {:?} ({})>",
            self.datagram_stream, self.write_bytes
        )
    }
}

impl _SentError {
    fn read_some(&mut self, read_timeout: usize) -> io::Result<&[u8]> {
        let recv_bytes = self
            .datagram_stream
            .recv(&mut self.read_buffer, read_timeout, 0)?;
        Ok(self.read_buffer[..recv_bytes].as_ref())
    }
}

fn _write_file(path: &PathBuf, data: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut file = File::create(path).unwrap();
    file.write_all(data).unwrap();
}

fn _download(client: _TFTPClient, file: &str) -> Result<Vec<u8>, _DownloadError> {
    let default_timeout: usize = 5;
    let default_block_size: usize = 512;
    let mut read_data: Vec<u8> = Vec::new();
    let sent_request = client
        .send_plain_read_request(file)
        .map_err(|error| _DownloadError::from(error))?;
    let mut block: Option<_Block> = Some(
        sent_request
            .read_next(default_timeout)
            .map_err(|error| _DownloadError::from(error))?,
    );
    while let Some(_block) = block.take() {
        let recv_block_len = _block.data().len();
        read_data.extend(_block.data());
        let sent_ack = _block
            .acknowledge()
            .map_err(|error| _DownloadError::from(error))?;
        if recv_block_len == default_block_size {
            block = Some(
                sent_ack
                    .read_next(default_timeout)
                    .map_err(|error| _DownloadError::from(error))?,
            );
        }
    }
    Ok(read_data)
}

#[derive(Debug)]
struct _DownloadError(String);

impl<T: fmt::Debug> From<_Error<T>> for _DownloadError {
    fn from(value: _Error<T>) -> Self {
        match value {
            _Error::Timeout(msg) => _DownloadError(format!("{:?}", msg)),
            error => _DownloadError(error.to_string()),
        }
    }
}

impl Display for _DownloadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.clone())
    }
}

impl From<io::Error> for _DownloadError {
    fn from(value: io::Error) -> Self {
        _DownloadError(value.to_string())
    }
}

#[test]
fn send_wrong_request_type() {
    let source_ip = "127.0.0.11";
    let server_dir = mk_tmp(send_wrong_request_type);
    let server = _ThreadedTFTPServer::new(server_dir, "127.0.0.10", 30);
    let wrong_code_packet = b"\xAAoctet\x00irrelevant\x00";
    let local_socket = UdpSocket::bind((source_ip, 0)).unwrap();
    local_socket
        .send_to(wrong_code_packet, server.listen_socket)
        .unwrap();
    let mut buffer = [0u8; _BUFFER_SIZE];
    let bytes_read = local_socket.recv(&mut buffer).unwrap();
    let error_message = CStr::from_bytes_with_nul(&buffer[4..bytes_read]).unwrap();
    assert!(
        error_message
            .to_str()
            .unwrap()
            .contains("Only RRQ is supported")
    );
}

#[test]
fn send_wrong_content_type() {
    let source_ip = "127.0.0.11";
    let server_dir = mk_tmp(send_wrong_content_type);
    let server = _ThreadedTFTPServer::new(server_dir, "127.0.0.10", 30);
    let wrong_code_packet = b"\x00\x01email\x00irrelevant\x00";
    let local_socket = UdpSocket::bind((source_ip, 0)).unwrap();
    local_socket
        .send_to(wrong_code_packet, server.listen_socket)
        .unwrap();
    let mut buffer = [0u8; _BUFFER_SIZE];
    let bytes_read = local_socket.recv(&mut buffer).unwrap();
    let error_message = CStr::from_bytes_with_nul(&buffer[4..bytes_read]).unwrap();
    assert!(
        error_message
            .to_str()
            .unwrap()
            .contains("Only octet mode is supported")
    );
}

#[test]
fn download_local_aligned_file() {
    let source_ip = "127.0.0.11";
    let server_dir = mk_tmp(download_local_aligned_file);
    let payload_size = 4096;
    let data = _make_payload(payload_size);
    let file_name = "file.txt";
    let file = server_dir.join(source_ip).join(file_name);
    _write_file(&file, &data);
    let server = _ThreadedTFTPServer::new(server_dir, "127.0.0.10", 30);
    let client = server.open_paired_client(source_ip);
    let read_result = _download(client, file_name);
    assert!(
        matches!(&read_result, Ok(recv_data) if data == *recv_data),
        "Unexpected error {read_result:?}"
    );
}

#[test]
fn download_local_non_aligned_file() {
    let source_ip = "127.0.0.11";
    let server_dir = mk_tmp(download_local_non_aligned_file);
    let payload_size = 4096 + 256;
    let data = _make_payload(payload_size);
    let file_name = "file.txt";
    let file = server_dir.join(source_ip).join(file_name);
    _write_file(&file, &data);
    let server = _ThreadedTFTPServer::new(server_dir, "127.0.0.10", 30);
    let client = server.open_paired_client(source_ip);
    let read_data = _download(client, file_name).unwrap();
    assert_eq!(read_data, data);
}

#[test]
fn download_file_with_root_prefix() {
    let source_ip = "127.0.0.11";
    let server_dir = mk_tmp(download_file_with_root_prefix);
    let payload_size = 512;
    let data = _make_payload(payload_size);
    let file_name = "file.txt";
    let file = server_dir.join(source_ip).join(file_name);
    _write_file(&file, &data);
    // Leading slash is expected to be stripped.
    let file_name_with_leading_slash = format!("/{file_name}");
    let server = _ThreadedTFTPServer::new(server_dir, "127.0.0.10", 30);
    let client = server.open_paired_client(source_ip);
    let read_data = _download(client, &file_name_with_leading_slash).unwrap();
    assert_eq!(read_data, data);
}

#[test]
fn attempt_download_nonexisting_file() {
    let arbitrary_source_ip = "127.0.0.11";
    let server_dir = mk_tmp(attempt_download_nonexisting_file);
    let nonexisting_file_name = "nonexisting.file";
    let server = _ThreadedTFTPServer::new(server_dir, "127.0.0.10", 30);
    let client = server.open_paired_client(arbitrary_source_ip);
    let sent_request = client
        .send_plain_read_request(nonexisting_file_name)
        .unwrap();
    let result = sent_request.read_next(5);
    assert!(
        matches!(&result, Err(_Error::ClientError(0x01, msg)) if msg == "File not found"),
        "Unexpected error {result:?}"
    );
}

#[test]
fn access_violation() {
    let server_dir = mk_tmp(access_violation);
    set_permissions(&server_dir, Permissions::from_mode(0o055)).unwrap();
    let arbitrary_source_ip = "127.0.0.11";
    let arbitrary_file_name = "arbitrary.file";
    let server = _ThreadedTFTPServer::new(server_dir, "127.0.0.10", 30);
    let client = server.open_paired_client(arbitrary_source_ip);
    let sent_request = client.send_plain_read_request(arbitrary_file_name).unwrap();
    let result = sent_request.read_next(5);
    assert!(
        matches!(&result, Err(_Error::ClientError(0x02, msg)) if msg == "Access violation"),
        "Unexpected result {result:?}"
    );
}

#[test]
fn early_terminate() {
    let source_ip = "127.0.0.11";
    let server_dir = mk_tmp(early_terminate);
    let payload_size = 4096;
    let data = _make_payload(payload_size);
    let file_name = "file.txt";
    let file = server_dir.join(source_ip).join(file_name);
    _write_file(&file, &data);
    let server = _ThreadedTFTPServer::new(server_dir, "127.0.0.10", 30);
    let client = server.open_paired_client(source_ip);
    let sent_request = client.send_plain_read_request(file_name).unwrap();
    let first_block = sent_request.read_next(5).unwrap();
    let mut sent_error = first_block.send_error(0x0, "Early termination").unwrap();
    let some = sent_error.read_some(5);
    assert!(
        matches!(&some, Err(error) if error.kind() == ErrorKind::TimedOut),
        "Unexpected result {some:?}"
    );
}

#[test]
fn change_block_size_local() {
    let source_ip = "127.0.0.11";
    let server_dir = mk_tmp(change_block_size_local);
    let payload_size = 4096;
    let data = _make_payload(payload_size);
    let file_name = "file.txt";
    let file = server_dir.join(source_ip).join(file_name);
    _write_file(&file, &data);
    let server = _ThreadedTFTPServer::new(server_dir, "127.0.0.10", 30);
    let client = server.open_paired_client(source_ip);
    let arbitrary_block_size: usize = 1001;
    let send_options = HashMap::from([("blksize".to_string(), arbitrary_block_size.to_string())]);
    let sent_request = client
        .send_optioned_read_request(file_name, &send_options)
        .unwrap();
    let oack = sent_request.read_oack(5).unwrap();
    let received_options = oack.fields();
    assert_eq!(received_options, send_options);
    let sent_ack = oack.acknowledge().unwrap();
    let first_block = sent_ack.read_next(5).unwrap();
    assert_eq!(first_block.data().len(), arbitrary_block_size);
    first_block.send_error(0x0, "Early termination").unwrap();
}

#[test]
fn request_file_size_local() {
    let source_ip = "127.0.0.11";
    let server_dir = mk_tmp(request_file_size_local);
    let payload_size: usize = 4096;
    let data = _make_payload(payload_size);
    let file_name = "file.txt";
    let file = server_dir.join(source_ip).join(file_name);
    _write_file(&file, &data);
    let server = _ThreadedTFTPServer::new(server_dir, "127.0.0.10", 30);
    let client = server.open_paired_client(source_ip);
    let send_options = HashMap::from([("tsize".to_string(), "0".to_string())]);
    let sent_request = client
        .send_optioned_read_request(file_name, &send_options)
        .unwrap();
    let oack = sent_request.read_oack(5).unwrap();
    let received_options = oack.fields();
    let raw_file_size = received_options.get("tsize").unwrap();
    assert_eq!(raw_file_size.parse::<usize>().unwrap(), payload_size);
    let sent_ack = oack.acknowledge().unwrap();
    let first_block = sent_ack.read_next(5).unwrap();
    first_block.send_error(0x0, "Early termination").unwrap();
}

#[test]
fn change_timeout() {
    let source_ip = "127.0.0.11";
    let server_dir = mk_tmp(change_timeout);
    let payload_size = 4096;
    let data = _make_payload(payload_size);
    let file_name = "file.txt";
    let file = server_dir.join(source_ip).join(file_name);
    _write_file(&file, &data);
    let server = _ThreadedTFTPServer::new(server_dir, "127.0.0.10", 30);
    let client = server.open_paired_client(source_ip);
    let minimal_timeout = 1;
    let send_options = HashMap::from([("timeout".to_string(), minimal_timeout.to_string())]);
    let sent_request = client
        .send_optioned_read_request(file_name, &send_options)
        .unwrap();
    let oack = sent_request.read_oack(5).unwrap();
    let received_options = oack.fields();
    let start = time::Instant::now();
    assert_eq!(received_options, send_options);
    let expected_oack = b"\x00\x06timeout\x001\x00";
    let mut retry_buffers = vec![
        (Vec::new(), 0u64),
        (Vec::new(), 0u64),
        (Vec::new(), 0u64),
        (Vec::new(), 0u64),
        (Vec::new(), 0u64),
    ];
    let local_read_timeout_sec = 2;
    oack.datagram_stream
        .local_socket
        .set_read_timeout(Some(Duration::from_secs(local_read_timeout_sec)))
        .unwrap();
    let mut buffer = [0u8; _BUFFER_SIZE];
    for (retry_message, timestamp) in &mut retry_buffers {
        if let Ok(read_bytes) = oack.datagram_stream.local_socket.recv(&mut buffer) {
            (*retry_message).extend_from_slice(&buffer[..read_bytes]);
            *timestamp = time::Instant::now().duration_since(start).as_secs();
            eprintln!(
                "{} {read_bytes}",
                time::Instant::now().duration_since(start).as_secs()
            );
        }
    }
    assert_eq!(
        retry_buffers[0].0, expected_oack,
        "1: Received: {:?}, Expected: {:?}",
        retry_buffers[0].0, expected_oack
    );
    assert_eq!(
        retry_buffers[0].1, 1,
        "1: Timestamp mismatch. Received: {}, Expected: {}",
        retry_buffers[0].1, 1
    );
    assert_eq!(
        retry_buffers[1].0, expected_oack,
        "2: Received: {:?}, Expected: {:?}",
        retry_buffers[1].0, expected_oack
    );
    assert_eq!(
        retry_buffers[1].1, 2,
        "2: Timestamp mismatch. Received: {}, Expected: {}",
        retry_buffers[1].1, 2
    );
    assert_eq!(
        retry_buffers[2].0, expected_oack,
        "3: Received: {:?}, Expected: {:?}",
        retry_buffers[2].0, expected_oack
    );
    assert_eq!(
        retry_buffers[2].1, 3,
        "3: Timestamp mismatch. Received: {}, Expected: {}",
        retry_buffers[2].1, 3
    );
    assert_eq!(
        retry_buffers[3].0, expected_oack,
        "4: Received: {:?}, Expected: {:?}",
        retry_buffers[3].0, expected_oack
    );
    assert_eq!(
        retry_buffers[3].1, 4,
        "4: Timestamp mismatch. Received: {}, Expected: {}",
        retry_buffers[3].1, 4
    );
    assert_eq!(
        retry_buffers[4].0, b"",
        "5: Received: {:?}, Expected: {:?}",
        retry_buffers[4].0, b""
    );
    assert_eq!(
        retry_buffers[4].1, 0,
        "5: Timestamp mismatch. Received: {}, Expected: {}",
        retry_buffers[4].1, 5
    );
}

#[test]
fn test_download_nbd_file_aligned() {
    let source_ip = "127.0.0.11";
    let server_dir = mk_tmp(test_download_nbd_file_aligned);
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
    });
    let nbd_share_config_file = server_dir.join(format!("{}.nbd", source_ip));
    _write_file(&nbd_share_config_file, config.to_string().as_bytes());
    let server = _ThreadedTFTPServer::new(server_dir, "127.0.0.10", 30);
    let client = server.open_paired_client(source_ip);
    let existing_file = "aligned.file";
    let read_data = _download(client, existing_file).unwrap();
    let data = _make_payload(51200);
    assert_eq!(read_data, data);
}

#[test]
fn test_download_nbd_file_nonaligned() {
    let source_ip = "127.0.0.11";
    let server_dir = mk_tmp(test_download_nbd_file_nonaligned);
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
    });
    let nbd_share_config_file = server_dir.join(format!("{}.nbd", source_ip));
    _write_file(&nbd_share_config_file, config.to_string().as_bytes());
    let server = _ThreadedTFTPServer::new(server_dir, "127.0.0.10", 30);
    let client = server.open_paired_client(source_ip);
    let existing_file = "nonaligned.file";
    let read_data = _download(client, existing_file).unwrap();
    let data = _make_payload(51205);
    assert_eq!(read_data, data);
}

#[test]
fn request_file_size_remote() {
    let source_ip = "127.0.0.11";
    let server_dir = mk_tmp(request_file_size_remote);
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
    });
    let nbd_share_config_file = server_dir.join(format!("{}.nbd", source_ip));
    _write_file(&nbd_share_config_file, config.to_string().as_bytes());
    let server = _ThreadedTFTPServer::new(server_dir, "127.0.0.10", 30);
    let client = server.open_paired_client(source_ip);
    let existing_file_name = "nonaligned.file";
    let existing_file_size: usize = 51205;
    let send_options = HashMap::from([("tsize".to_string(), "0".to_string())]);
    let sent_request = client
        .send_optioned_read_request(existing_file_name, &send_options)
        .unwrap();
    let oack = sent_request.read_oack(5).unwrap();
    let received_options = oack.fields();
    let raw_file_size = received_options.get("tsize").unwrap();
    assert_eq!(raw_file_size.parse::<usize>().unwrap(), existing_file_size);
    let sent_ack = oack.acknowledge().unwrap();
    let first_block = sent_ack.read_next(5).unwrap();
    first_block.send_error(0x0, "Early termination").unwrap();
}

#[test]
fn change_block_size_remote() {
    let source_ip = "127.0.0.11";
    let server_dir = mk_tmp(change_block_size_remote);
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
    });
    let nbd_share_config_file = server_dir.join(format!("{}.nbd", source_ip));
    _write_file(&nbd_share_config_file, config.to_string().as_bytes());
    let existing_file_name = "nonaligned.file";
    let server = _ThreadedTFTPServer::new(server_dir, "127.0.0.10", 30);
    let client = server.open_paired_client(source_ip);
    let arbitrary_block_size: usize = 1001;
    let send_options = HashMap::from([("blksize".to_string(), arbitrary_block_size.to_string())]);
    let sent_request = client
        .send_optioned_read_request(existing_file_name, &send_options)
        .unwrap();
    let oack = sent_request.read_oack(5).unwrap();
    let received_options = oack.fields();
    assert_eq!(received_options, send_options);
    let sent_ack = oack.acknowledge().unwrap();
    let first_block = sent_ack.read_next(5).unwrap();
    assert_eq!(first_block.data().len(), arbitrary_block_size);
    first_block.send_error(0x0, "Early termination").unwrap();
}

#[test]
fn test_local_file_takes_precedence() {
    let source_ip = "127.0.0.11";
    let server_dir = mk_tmp(test_local_file_takes_precedence);
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
    });
    let nbd_share_config_file = server_dir.join(format!("{}.nbd", source_ip));
    _write_file(&nbd_share_config_file, config.to_string().as_bytes());
    let size = 51200;
    let local_payload = b"local pattern"
        .iter()
        .copied()
        .cycle()
        .take(size)
        .collect::<Vec<_>>();
    let existing_file = "aligned.file";
    let local_file = server_dir.join(source_ip).join(existing_file);
    _write_file(&local_file, &local_payload);
    let server = _ThreadedTFTPServer::new(server_dir, "127.0.0.10", 30);
    let client = server.open_paired_client(source_ip);
    let read_data = _download(client, existing_file).unwrap();
    assert_eq!(read_data, local_payload);
}

#[test]
fn test_file_not_exists_in_both_local_remote() {
    let source_ip = "127.0.0.11";
    let server_dir = mk_tmp(test_file_not_exists_in_both_local_remote);
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
    });
    let nbd_share_config_file = server_dir.join(format!("{}.nbd", source_ip));
    _write_file(&nbd_share_config_file, config.to_string().as_bytes());
    let nonexisting_file = "nonexisted.file";
    let server = _ThreadedTFTPServer::new(server_dir, "127.0.0.10", 30);
    let client = server.open_paired_client(source_ip);
    let read_result = _download(client, nonexisting_file);
    assert!(
        matches!(&read_result, Err(message) if message.to_string().contains("File not found")),
        "Unexpected error {read_result:?}"
    );
}
