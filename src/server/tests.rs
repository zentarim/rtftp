use super::*;
use crate::_tests::{make_payload, mk_tmp, run_nbd_server};
use crate::Watch;
use crate::cursor::{ReadCursor, WriteCursor};
use serde_json::json;
use std::ffi::CStr;
use std::fs::{File, Permissions, set_permissions};
use std::io::{ErrorKind, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::{fmt, fs, io, thread, time};
use tokio::runtime::Builder;
use tokio::sync::Notify;
use tokio::task::LocalSet;

const _DATA_PATTERN: &str = "ARBITRARY DATA";
const _BUFFER_SIZE: usize = 1536;
const _U16_SIZE: usize = size_of::<u16>();
const _RRQ: u16 = 0x01;
const _DATA: u16 = 0x03;
const _ACK: u16 = 0x04;
const _ERR: u16 = 0x05;
const _OACK: u16 = 0x06;

#[derive(Debug)]
struct _ThreadedTFTPServer {
    shutdown_notify: Arc<Notify>,
    handle: Option<JoinHandle<()>>,
    listen_socket: SocketAddr,
}

impl _ThreadedTFTPServer {
    async fn new(root_dir: PathBuf, bind_ip: &str, idle_timeout: u64) -> Self {
        let shutdown_notify = Arc::new(Notify::new());
        let shutdown_received = shutdown_notify.clone();
        let turn_duration = time::Duration::from_secs(1);
        let server_socket = UdpSocket::bind((bind_ip, 0)).await.unwrap();
        let listen_socket = server_socket.local_addr().unwrap();
        let handle = thread::spawn(move || {
            LocalSet::new().block_on(
                &Builder::new_current_thread().enable_all().build().unwrap(),
                async move {
                    let mut server = TFTPServer::new(server_socket, root_dir, idle_timeout);
                    tokio::select! {
                        _ = server.serve(turn_duration) => {},
                        _ = shutdown_received.notified() => eprintln!("Shutdown requested"),
                    }
                },
            )
        });
        Self {
            shutdown_notify,
            handle: Some(handle),
            listen_socket,
        }
    }

    async fn new_augmented(root_dir: PathBuf, bind_ip: &str, idle_timeout: u64) -> Self {
        let running_notify = Arc::new(Notify::new());
        let running_notify_clone = running_notify.clone();
        let shutdown_notify = Arc::new(Notify::new());
        let shutdown_received = shutdown_notify.clone();
        let turn_duration = time::Duration::from_secs(1);
        let server_socket = UdpSocket::bind((bind_ip, 0)).await.unwrap();
        let listen_socket = server_socket.local_addr().unwrap();
        let handle = thread::spawn(move || {
            LocalSet::new().block_on(
                &Builder::new_current_thread().enable_all().build().unwrap(),
                async move {
                    let observer = Watch::new()
                        .change()
                        .observe(&root_dir.to_string_lossy())
                        .unwrap();
                    let mut server = TFTPServer::new(server_socket, root_dir, idle_timeout);
                    running_notify_clone.notify_one();
                    tokio::select! {
                        _ = server.serve_augmented(turn_duration, &observer) => {},
                        _ = shutdown_received.notified() => eprintln!("Shutdown requested"),
                    }
                },
            )
        });
        running_notify.notified().await;
        Self {
            shutdown_notify,
            handle: Some(handle),
            listen_socket,
        }
    }

    async fn open_paired_client(&self, source_ip: &str) -> _TFTPClient {
        _TFTPClient::new(
            UdpSocket::bind((source_ip, 0)).await.unwrap(),
            self.listen_socket,
        )
    }
}

impl Drop for _ThreadedTFTPServer {
    fn drop(&mut self) {
        self.shutdown_notify.notify_one();
        self.handle.take().unwrap().join().unwrap();
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

    async fn send_plain_read_request(
        mut self,
        file_name: &str,
    ) -> io::Result<_SentPlainReadRequest> {
        let (_write_cursor, buffer_size) = self.make_read_request(file_name);
        self.local_socket
            .send_to(&self.write_buffer[..buffer_size], &self.remote_addr)
            .await?;
        Ok(_SentPlainReadRequest {
            file_name: file_name.to_string(),
            local_socket: self.local_socket,
            remote_addr: self.remote_addr,
            read_buffer: self.read_buffer,
            write_buffer: self.write_buffer,
            sent_bytes: buffer_size,
        })
    }

    async fn send_optioned_read_request(
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
            .send_to(&self.write_buffer[..buffer_size], &self.remote_addr)
            .await?;
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

    fn make_read_request(&mut self, file_name: &str) -> (WriteCursor<'_>, usize) {
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

    async fn send(&self, buffer: &[u8]) -> io::Result<()> {
        match self.local_socket.send_to(buffer, self.peer_address).await {
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

    async fn recv(
        &self,
        buffer: &mut [u8],
        read_timeout: usize,
        min_size: usize,
    ) -> io::Result<usize> {
        let mut now = time::Instant::now();
        let end_at = now + Duration::from_secs(read_timeout as u64);
        while now <= end_at {
            match tokio::time::timeout(end_at - now, self.local_socket.recv_from(buffer)).await {
                Ok(Ok((recv_size, remote_address))) => {
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
                Ok(Err(error)) => return Err(error),
                Err(_timeout_error) => return Err(ErrorKind::TimedOut.into()),
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
    async fn read_oack(mut self, read_timeout: usize) -> Result<_OACK, _Error<Self>> {
        let duration = time::Duration::from_secs(read_timeout as u64);
        let read_future = self.local_socket.recv_from(&mut self.read_buffer);
        match tokio::time::timeout(duration, read_future).await {
            Ok(Ok((read_bytes, remote_address)))
                if remote_address.ip() == self.remote_addr.ip() =>
            {
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
            Ok(Ok((read_bytes, remote_address))) => Err(_Error::UnexpectedPeer(
                remote_address.ip(),
                self.read_buffer[..read_bytes].to_vec(),
            )),
            Ok(Err(error)) => Err(_Error::IO(error)),
            Err(_timeout_error) => Err(_Error::Timeout(_SentReadRequestWithOpts {
                file_name: self.file_name,
                options: self.options,
                local_socket: self.local_socket,
                remote_addr: self.remote_addr,
                read_buffer: self.read_buffer,
                write_buffer: self.write_buffer,
                sent_bytes: self.sent_bytes,
            })),
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
    async fn read_next(mut self, read_timeout: usize) -> Result<_Block, _Error<Self>> {
        let duration = time::Duration::from_secs(read_timeout as u64);
        let read_future = self.local_socket.recv_from(&mut self.read_buffer);
        match tokio::time::timeout(duration, read_future).await {
            Ok(Ok((read_bytes, remote_address)))
                if remote_address.ip() == self.remote_addr.ip() =>
            {
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
            Ok(Ok((read_bytes, remote_address))) => Err(_Error::UnexpectedPeer(
                remote_address.ip(),
                self.read_buffer[..read_bytes].to_vec(),
            )),
            Ok(Err(error)) => Err(_Error::IO(error)),
            Err(_timeout_error) => Err(_Error::Timeout(_SentPlainReadRequest {
                file_name: self.file_name,
                local_socket: self.local_socket,
                remote_addr: self.remote_addr,
                read_buffer: self.read_buffer,
                write_buffer: self.write_buffer,
                sent_bytes: self.sent_bytes,
            })),
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
    async fn acknowledge(mut self) -> Result<_SentACK, _Error<Self>> {
        let mut write_cursor = WriteCursor::new(&mut self.write_buffer);
        _ = write_cursor.put_ushort(_ACK).unwrap();
        let buffer_size = write_cursor.put_ushort(0u16).unwrap();
        self.datagram_stream
            .send(&self.write_buffer[..buffer_size])
            .await
            .or_else(|error| Err(_Error::IO(error)))?;
        Ok(_SentACK {
            datagram_stream: self.datagram_stream,
            read_buffer: self.read_buffer,
            write_buffer: self.write_buffer,
            buffer_size,
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
    async fn acknowledge(mut self) -> Result<_SentACK, _Error<Self>> {
        let mut write_cursor = WriteCursor::new(&mut self.write_buffer);
        _ = write_cursor.put_ushort(_ACK).unwrap();
        let block_num = u16::from_be_bytes([self.read_buffer[2], self.read_buffer[3]]);
        let buffer_size = write_cursor.put_ushort(block_num).unwrap();
        self.datagram_stream
            .send(&self.write_buffer[..buffer_size])
            .await
            .unwrap();
        Ok(_SentACK {
            datagram_stream: self.datagram_stream,
            read_buffer: self.read_buffer,
            write_buffer: self.write_buffer,
            buffer_size,
        })
    }

    async fn send_error(mut self, code: u16, message: &str) -> Result<_SentError, _Error<Self>> {
        let mut write_cursor = WriteCursor::new(&mut self.write_buffer);
        _ = write_cursor.put_ushort(_ERR).unwrap();
        _ = write_cursor.put_ushort(code).unwrap();
        let buffer_size = write_cursor.put_string(message).unwrap();
        self.datagram_stream
            .send(&self.write_buffer[..buffer_size])
            .await
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
    async fn read_next(mut self, read_timeout: usize) -> Result<_Block, _Error<Self>> {
        let duration = time::Duration::from_secs(read_timeout as u64);
        let read_future = self
            .datagram_stream
            .recv(&mut self.read_buffer, read_timeout, 4);
        match tokio::time::timeout(duration, read_future).await {
            Ok(Ok(read_bytes)) => {
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
            Ok(Err(err)) => Err(_Error::IO(err)),
            Err(_timeout_error) => Err(_Error::Timeout(self)),
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
    async fn read_some(&mut self, read_timeout: usize) -> io::Result<&[u8]> {
        let recv_bytes = self
            .datagram_stream
            .recv(&mut self.read_buffer, read_timeout, 0)
            .await?;
        Ok(self.read_buffer[..recv_bytes].as_ref())
    }
}

fn _write_file(path: &PathBuf, data: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut file = File::create(path).unwrap();
    file.write_all(data).unwrap();
}

async fn _download(client: _TFTPClient, file: &str) -> Result<Vec<u8>, _DownloadError> {
    let default_timeout: usize = 5;
    let default_block_size: usize = 512;
    let mut read_data: Vec<u8> = Vec::new();
    let sent_request = client
        .send_plain_read_request(file)
        .await
        .map_err(|error| _DownloadError::from(error))?;
    let mut block: Option<_Block> = Some(
        sent_request
            .read_next(default_timeout)
            .await
            .map_err(|error| _DownloadError::from(error))?,
    );
    while let Some(_block) = block.take() {
        let recv_block_len = _block.data().len();
        read_data.extend(_block.data());
        let sent_ack = _block
            .acknowledge()
            .await
            .map_err(|error| _DownloadError::from(error))?;
        if recv_block_len == default_block_size {
            block = Some(
                sent_ack
                    .read_next(default_timeout)
                    .await
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

#[tokio::test(flavor = "current_thread")]
async fn send_wrong_request_type() {
    let source_ip = "127.0.0.11";
    let server_dir = mk_tmp(send_wrong_request_type);
    let server = _ThreadedTFTPServer::new(server_dir, "127.0.0.10", 30).await;
    let wrong_code_packet = b"\xAAoctet\x00irrelevant\x00";
    let local_socket = UdpSocket::bind((source_ip, 0)).await.unwrap();
    local_socket
        .send_to(wrong_code_packet, server.listen_socket)
        .await
        .unwrap();
    let mut buffer = [0u8; _BUFFER_SIZE];
    let bytes_read = local_socket.recv(&mut buffer).await.unwrap();
    let error_message = CStr::from_bytes_with_nul(&buffer[4..bytes_read]).unwrap();
    assert!(
        error_message
            .to_str()
            .unwrap()
            .contains("Only RRQ is supported")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn send_wrong_content_type() {
    let source_ip = "127.0.0.11";
    let server_dir = mk_tmp(send_wrong_content_type);
    let server = _ThreadedTFTPServer::new(server_dir, "127.0.0.10", 30).await;
    let wrong_code_packet = b"\x00\x01email\x00irrelevant\x00";
    let local_socket = UdpSocket::bind((source_ip, 0)).await.unwrap();
    local_socket
        .send_to(wrong_code_packet, server.listen_socket)
        .await
        .unwrap();
    let mut buffer = [0u8; _BUFFER_SIZE];
    let bytes_read = local_socket.recv(&mut buffer).await.unwrap();
    let error_message = CStr::from_bytes_with_nul(&buffer[4..bytes_read]).unwrap();
    assert!(
        error_message
            .to_str()
            .unwrap()
            .contains("Only octet mode is supported")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn download_local_aligned_file() {
    let source_ip = "127.0.0.11";
    let server_dir = mk_tmp(download_local_aligned_file);
    let payload_size = 4096;
    let data = make_payload(payload_size);
    let file_name = "file.txt";
    let file = server_dir.join(source_ip).join(file_name);
    _write_file(&file, &data);
    let server = _ThreadedTFTPServer::new(server_dir, "127.0.0.10", 30).await;
    let client = server.open_paired_client(source_ip).await;
    let read_result = _download(client, file_name).await;
    assert!(
        matches!(&read_result, Ok(recv_data) if data == *recv_data),
        "Unexpected error {read_result:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn download_local_non_aligned_file() {
    let source_ip = "127.0.0.11";
    let server_dir = mk_tmp(download_local_non_aligned_file);
    let payload_size = 4096 + 256;
    let data = make_payload(payload_size);
    let file_name = "file.txt";
    let file = server_dir.join(source_ip).join(file_name);
    _write_file(&file, &data);
    let server = _ThreadedTFTPServer::new(server_dir, "127.0.0.10", 30).await;
    let client = server.open_paired_client(source_ip).await;
    let read_data = _download(client, file_name).await.unwrap();
    assert_eq!(read_data, data);
}

#[tokio::test(flavor = "current_thread")]
async fn download_file_with_root_prefix() {
    let source_ip = "127.0.0.11";
    let server_dir = mk_tmp(download_file_with_root_prefix);
    let payload_size = 512;
    let data = make_payload(payload_size);
    let file_name = "file.txt";
    let file = server_dir.join(source_ip).join(file_name);
    _write_file(&file, &data);
    // Leading slash is expected to be stripped.
    let file_name_with_leading_slash = format!("/{file_name}");
    let server = _ThreadedTFTPServer::new(server_dir, "127.0.0.10", 30).await;
    let client = server.open_paired_client(source_ip).await;
    let read_data = _download(client, &file_name_with_leading_slash)
        .await
        .unwrap();
    assert_eq!(read_data, data);
}

#[tokio::test(flavor = "current_thread")]
async fn attempt_download_nonexisting_file() {
    let arbitrary_source_ip = "127.0.0.11";
    let server_dir = mk_tmp(attempt_download_nonexisting_file);
    let nonexisting_file_name = "nonexisting.file";
    let server = _ThreadedTFTPServer::new(server_dir, "127.0.0.10", 30).await;
    let client = server.open_paired_client(arbitrary_source_ip).await;
    let sent_request = client
        .send_plain_read_request(nonexisting_file_name)
        .await
        .unwrap();
    let result = sent_request.read_next(5).await;
    assert!(
        matches!(&result, Err(_Error::ClientError(0x01, msg)) if msg == "File not found"),
        "Unexpected error {result:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn access_violation() {
    let server_dir = mk_tmp(access_violation);
    set_permissions(&server_dir, Permissions::from_mode(0o055)).unwrap();
    let arbitrary_source_ip = "127.0.0.11";
    let arbitrary_file_name = "arbitrary.file";
    let server = _ThreadedTFTPServer::new(server_dir, "127.0.0.10", 30).await;
    let client = server.open_paired_client(arbitrary_source_ip).await;
    let sent_request = client
        .send_plain_read_request(arbitrary_file_name)
        .await
        .unwrap();
    let result = sent_request.read_next(5).await;
    assert!(
        matches!(&result, Err(_Error::ClientError(0x02, msg)) if msg == "Access violation"),
        "Unexpected result {result:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn early_terminate() {
    let source_ip = "127.0.0.11";
    let server_dir = mk_tmp(early_terminate);
    let payload_size = 4096;
    let data = make_payload(payload_size);
    let file_name = "file.txt";
    let file = server_dir.join(source_ip).join(file_name);
    _write_file(&file, &data);
    let server = _ThreadedTFTPServer::new(server_dir, "127.0.0.10", 30).await;
    let client = server.open_paired_client(source_ip).await;
    let sent_request = client.send_plain_read_request(file_name).await.unwrap();
    let first_block = sent_request.read_next(5).await.unwrap();
    let mut sent_error = first_block
        .send_error(0x0, "Early termination")
        .await
        .unwrap();
    let some = sent_error.read_some(5).await;
    assert!(
        matches!(&some, Err(error) if error.kind() == ErrorKind::TimedOut),
        "Unexpected result {some:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn change_block_size_local() {
    let source_ip = "127.0.0.11";
    let server_dir = mk_tmp(change_block_size_local);
    let payload_size = 4096;
    let data = make_payload(payload_size);
    let file_name = "file.txt";
    let file = server_dir.join(source_ip).join(file_name);
    _write_file(&file, &data);
    let server = _ThreadedTFTPServer::new(server_dir, "127.0.0.10", 30).await;
    let client = server.open_paired_client(source_ip).await;
    let arbitrary_block_size: usize = 1001;
    let send_options = HashMap::from([("blksize".to_string(), arbitrary_block_size.to_string())]);
    let sent_request = client
        .send_optioned_read_request(file_name, &send_options)
        .await
        .unwrap();
    let oack = sent_request.read_oack(5).await.unwrap();
    let received_options = oack.fields();
    assert_eq!(received_options, send_options);
    let sent_ack = oack.acknowledge().await.unwrap();
    let first_block = sent_ack.read_next(5).await.unwrap();
    assert_eq!(first_block.data().len(), arbitrary_block_size);
    first_block
        .send_error(0x0, "Early termination")
        .await
        .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn request_file_size_local() {
    let source_ip = "127.0.0.11";
    let server_dir = mk_tmp(request_file_size_local);
    let payload_size: usize = 4096;
    let data = make_payload(payload_size);
    let file_name = "file.txt";
    let file = server_dir.join(source_ip).join(file_name);
    _write_file(&file, &data);
    let server = _ThreadedTFTPServer::new(server_dir, "127.0.0.10", 30).await;
    let client = server.open_paired_client(source_ip).await;
    let send_options = HashMap::from([("tsize".to_string(), "0".to_string())]);
    let sent_request = client
        .send_optioned_read_request(file_name, &send_options)
        .await
        .unwrap();
    let oack = sent_request.read_oack(5).await.unwrap();
    let received_options = oack.fields();
    let raw_file_size = received_options.get("tsize").unwrap();
    assert_eq!(raw_file_size.parse::<usize>().unwrap(), payload_size);
    let sent_ack = oack.acknowledge().await.unwrap();
    let first_block = sent_ack.read_next(5).await.unwrap();
    first_block
        .send_error(0x0, "Early termination")
        .await
        .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn change_timeout() {
    let source_ip = "127.0.0.11";
    let server_dir = mk_tmp(change_timeout);
    let payload_size = 4096;
    let data = make_payload(payload_size);
    let file_name = "file.txt";
    let file = server_dir.join(source_ip).join(file_name);
    _write_file(&file, &data);
    let server = _ThreadedTFTPServer::new(server_dir, "127.0.0.10", 30).await;
    let client = server.open_paired_client(source_ip).await;
    let minimal_timeout = 1;
    let send_options = HashMap::from([("timeout".to_string(), minimal_timeout.to_string())]);
    let sent_request = client
        .send_optioned_read_request(file_name, &send_options)
        .await
        .unwrap();
    let oack = sent_request.read_oack(5).await.unwrap();
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
    let local_read_timeout = Duration::from_secs(2);
    let mut buffer = [0u8; _BUFFER_SIZE];
    for (retry_message, timestamp) in &mut retry_buffers {
        let read_future = oack.datagram_stream.local_socket.recv(&mut buffer);
        if let Ok(Ok(read_bytes)) = tokio::time::timeout(local_read_timeout, read_future).await {
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

#[tokio::test(flavor = "current_thread")]
async fn test_download_nbd_file_aligned() {
    let source_ip = "127.0.0.11";
    let server_dir = mk_tmp(test_download_nbd_file_aligned);
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
    });
    let nbd_share_config_file = server_dir.join(format!("{}.nbd", source_ip));
    _write_file(&nbd_share_config_file, config.to_string().as_bytes());
    let server = _ThreadedTFTPServer::new(server_dir, "127.0.0.10", 30).await;
    let client = server.open_paired_client(source_ip).await;
    let existing_file = "aligned.file";
    let read_data = _download(client, existing_file).await.unwrap();
    let data = make_payload(4194304);
    assert_eq!(read_data, data);
}

#[tokio::test(flavor = "current_thread")]
async fn test_download_nbd_file_nonaligned() {
    let source_ip = "127.0.0.11";
    let server_dir = mk_tmp(test_download_nbd_file_nonaligned);
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
    });
    let nbd_share_config_file = server_dir.join(format!("{}.nbd", source_ip));
    _write_file(&nbd_share_config_file, config.to_string().as_bytes());
    let server = _ThreadedTFTPServer::new(server_dir, "127.0.0.10", 30).await;
    let client = server.open_paired_client(source_ip).await;
    let existing_file = "nonaligned.file";
    let read_data = _download(client, existing_file).await.unwrap();
    let data = make_payload(4194319);
    assert_eq!(read_data, data);
}

#[tokio::test(flavor = "current_thread")]
async fn request_file_size_remote() {
    let source_ip = "127.0.0.11";
    let server_dir = mk_tmp(request_file_size_remote);
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
    });
    let nbd_share_config_file = server_dir.join(format!("{}.nbd", source_ip));
    _write_file(&nbd_share_config_file, config.to_string().as_bytes());
    let server = _ThreadedTFTPServer::new(server_dir, "127.0.0.10", 30).await;
    let client = server.open_paired_client(source_ip).await;
    let existing_file_name = "nonaligned.file";
    let existing_file_size: usize = 4194319;
    let send_options = HashMap::from([("tsize".to_string(), "0".to_string())]);
    let sent_request = client
        .send_optioned_read_request(existing_file_name, &send_options)
        .await
        .unwrap();
    let oack = sent_request.read_oack(5).await.unwrap();
    let received_options = oack.fields();
    let raw_file_size = received_options.get("tsize").unwrap();
    assert_eq!(raw_file_size.parse::<usize>().unwrap(), existing_file_size);
    let sent_ack = oack.acknowledge().await.unwrap();
    let first_block = sent_ack.read_next(5).await.unwrap();
    first_block
        .send_error(0x0, "Early termination")
        .await
        .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn change_block_size_remote() {
    let source_ip = "127.0.0.11";
    let server_dir = mk_tmp(change_block_size_remote);
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
    });
    let nbd_share_config_file = server_dir.join(format!("{}.nbd", source_ip));
    _write_file(&nbd_share_config_file, config.to_string().as_bytes());
    let existing_file_name = "nonaligned.file";
    let server = _ThreadedTFTPServer::new(server_dir, "127.0.0.10", 30).await;
    let client = server.open_paired_client(source_ip).await;
    let arbitrary_block_size: usize = 1001;
    let send_options = HashMap::from([("blksize".to_string(), arbitrary_block_size.to_string())]);
    let sent_request = client
        .send_optioned_read_request(existing_file_name, &send_options)
        .await
        .unwrap();
    let oack = sent_request.read_oack(5).await.unwrap();
    let received_options = oack.fields();
    assert_eq!(received_options, send_options);
    let sent_ack = oack.acknowledge().await.unwrap();
    let first_block = sent_ack.read_next(5).await.unwrap();
    assert_eq!(first_block.data().len(), arbitrary_block_size);
    first_block
        .send_error(0x0, "Early termination")
        .await
        .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn test_local_file_takes_precedence() {
    let source_ip = "127.0.0.11";
    let server_dir = mk_tmp(test_local_file_takes_precedence);
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
    });
    let nbd_share_config_file = server_dir.join(format!("{}.nbd", source_ip));
    _write_file(&nbd_share_config_file, config.to_string().as_bytes());
    let size = 4194304;
    let local_payload = b"local pattern"
        .iter()
        .copied()
        .cycle()
        .take(size)
        .collect::<Vec<_>>();
    let existing_file = "aligned.file";
    let local_file = server_dir.join(source_ip).join(existing_file);
    _write_file(&local_file, &local_payload);
    let server = _ThreadedTFTPServer::new(server_dir, "127.0.0.10", 30).await;
    let client = server.open_paired_client(source_ip).await;
    let read_data = _download(client, existing_file).await.unwrap();
    assert_eq!(read_data, local_payload);
}

#[tokio::test(flavor = "current_thread")]
async fn test_file_not_exists_in_both_local_remote() {
    let source_ip = "127.0.0.11";
    let server_dir = mk_tmp(test_file_not_exists_in_both_local_remote);
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
    });
    let nbd_share_config_file = server_dir.join(format!("{}.nbd", source_ip));
    _write_file(&nbd_share_config_file, config.to_string().as_bytes());
    let nonexisting_file = "nonexisted.file";
    let server = _ThreadedTFTPServer::new(server_dir, "127.0.0.10", 30).await;
    let client = server.open_paired_client(source_ip).await;
    let read_result = _download(client, nonexisting_file).await;
    assert!(
        matches!(&read_result, Err(message) if message.to_string().contains("File not found")),
        "Unexpected error {read_result:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn test_download_nbd_file_nonaligned_augmented() {
    let source_ip = "127.0.0.11";
    let server_dir = mk_tmp(test_download_nbd_file_nonaligned_augmented);
    let nbd_process = run_nbd_server("127.0.0.2");
    let server = _ThreadedTFTPServer::new_augmented(server_dir.clone(), "127.0.0.10", 30).await;
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
    });
    let nbd_share_config_file = server_dir.join(format!("{}.nbd", source_ip));
    _write_file(&nbd_share_config_file, config.to_string().as_bytes());
    let client = server.open_paired_client(source_ip).await;
    let existing_file = "nonaligned.file";
    let read_data = _download(client, existing_file).await.unwrap();
    let data = make_payload(4194319);
    assert_eq!(read_data, data);
}
