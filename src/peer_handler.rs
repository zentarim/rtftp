use crate::cursor::{ReadCursor, WriteCursor};
use crate::fs::{Config, FileError, OpenedFile, Root, VirtualRootError};
use crate::local_fs::LocalRoot;
use crate::messages::{OptionsAcknowledge, ReadRequest, TFTPError, UNDEFINED_ERROR};
use crate::nbd_disk::NBDConfig;
use crate::options::{AckTimeout, Blksize, TSize};
use serde_json::Value;
use std::collections::HashMap;
use std::fmt::{Debug, Display, Formatter};
use std::io::ErrorKind;
use std::net::{IpAddr, SocketAddr};
use std::ops::DerefMut;
use std::path::{Path, PathBuf};
use std::thread::Builder;
use std::time::Duration;
use std::{fmt, fs, thread, time};
use tokio::net::UdpSocket;
use tokio::runtime;
use tokio::sync::mpsc;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::task::{JoinHandle, LocalSet};
use tokio::time::timeout;

const ACK: u16 = 0x04;
const DATA: u16 = 0x03;

const ERROR: u16 = 0x05;

const FILE_NOT_FOUND: u16 = 0x01;

const ACCESS_VIOLATION: u16 = 0x02;

const MAX_SESSIONS_PER_IP: usize = 128;

struct DatagramStream {
    local_socket: UdpSocket,
    peer_address: SocketAddr,
    display: String,
}

impl DatagramStream {
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

    pub(super) fn remote_port(&self) -> u16 {
        self.peer_address.port()
    }

    pub async fn send(&self, buffer: &[u8]) -> std::io::Result<()> {
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

    pub async fn recv(&self, buffer: &mut [u8], min_size: usize) -> std::io::Result<usize> {
        loop {
            match self.local_socket.recv_from(buffer).await {
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
                Err(error) => return Err(error),
            }
        }
    }
}

impl Debug for DatagramStream {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "<{}>", self.display)
    }
}

impl Display for DatagramStream {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "<{}>", self.display)
    }
}

pub struct TFTPStream {
    udp_stream: DatagramStream,
    ack_timeout: AckTimeout,
    send_attempts: usize,
    recv_buffer: Vec<u8>,
}

impl TFTPStream {
    fn new(udp_stream: DatagramStream, ack_timeout: AckTimeout, send_attempts: usize) -> Self {
        Self {
            udp_stream,
            ack_timeout,
            send_attempts,
            recv_buffer: vec![0u8; u16::MAX as usize],
        }
    }

    async fn send_data(&mut self, block: &[u8], block_num: u16) -> Result<(), SendError> {
        for attempt in 0..self.send_attempts {
            if let Err(err) = self.udp_stream.send(block).await {
                return Err(SendError::Network(err.to_string()));
            }
            match self.read_acknowledge().await {
                Ok(block_ack) => {
                    if block_ack == block_num {
                        return Ok(());
                    }
                    eprintln!("{self}: Expected acknowledge {block_num}, received {block_ack}");
                }
                Err(SendError::Timeout) => {
                    eprintln!("{self}: Timeout waiting for {block_num}, attempt {attempt}");
                }
                Err(send_error) => return Err(send_error),
            }
        }
        Err(SendError::Timeout)
    }

    async fn fire_error(&mut self, buffer: &[u8]) {
        _ = self.udp_stream.send(buffer).await;
    }

    async fn read_acknowledge(&mut self) -> Result<u16, SendError> {
        let recv_future = self.udp_stream.recv(&mut self.recv_buffer, 4);
        if let Ok(read_result) = self.ack_timeout.timeout(recv_future).await {
            let _read_size = match read_result {
                Ok(size) => size,
                Err(err) => return Err(SendError::Network(err.to_string())),
            };
            let mut datagram = ReadCursor::new(&self.recv_buffer);
            match datagram.extract_ushort() {
                Ok(opcode) if opcode == ACK => Ok(datagram
                    .extract_ushort()
                    .map_err(|_| SendError::ACKParseError)?),
                Ok(opcode) if opcode == ERROR => {
                    let error_code = datagram
                        .extract_ushort()
                        .map_err(|_| SendError::ACKParseError)?;
                    let error_message = datagram
                        .extract_string()
                        .map_err(|_| SendError::ACKParseError)?;
                    Err(SendError::ClientError(error_code, error_message))
                }
                Ok(opcode) => {
                    eprintln!("{self}: Received unknown opcode 0x{opcode:02x}");
                    Err(SendError::ACKParseError)
                }
                Err(_) => Err(SendError::ACKParseError),
            }
        } else {
            Err(SendError::Timeout)
        }
    }
}

impl Display for TFTPStream {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "<TFTPStream: {} {}>", self.udp_stream, self.ack_timeout)
    }
}

impl Debug for TFTPStream {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "<TFTPStream: {} {}>", self.udp_stream, self.ack_timeout)
    }
}

#[derive(Debug)]
pub(super) enum SendError {
    Network(String),
    Timeout,
    ClientError(u16, String),
    ACKParseError,
}

pub(super) struct PeerHandler {
    sender_address: IpAddr,
    requests_channel: Sender<(u16, ReadRequest)>,
    thread_handle: thread::JoinHandle<()>,
}

impl Display for PeerHandler {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "<PeerHandler: {}>", self.sender_address)
    }
}

impl Debug for PeerHandler {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "<PeerHandler: {}>", self.sender_address)
    }
}

impl PeerHandler {
    pub(super) fn new(
        peer: IpAddr,
        local_address: IpAddr,
        tftp_root: PathBuf,
        idle_timeout: Duration,
    ) -> Self {
        let (tx, rx) = mpsc::channel::<(u16, ReadRequest)>(10);
        let handle = Builder::new()
            .name(format!("Handler {peer}"))
            .spawn(move || {
                let mut available_roots: Vec<Box<dyn Root>> =
                    vec![Box::new(LocalRoot::new(tftp_root.join(peer.to_string())))];
                available_roots.extend(get_available_remote_roots(&tftp_root, &peer.to_string()));
                eprintln!("{peer}: Available roots: {available_roots:?}");
                let runtime = runtime::Builder::new_current_thread()
                    .enable_time()
                    .enable_io()
                    .build()
                    .unwrap();
                let local_task_set = LocalSet::new();
                local_task_set.spawn_local(peer_requests_handler(
                    peer,
                    local_address,
                    available_roots,
                    rx,
                    idle_timeout,
                ));
                runtime.block_on(local_task_set);
                eprintln!("{peer}: Handler closed");
            })
            .unwrap();
        Self {
            sender_address: peer,
            requests_channel: tx,
            thread_handle: handle,
        }
    }

    pub(super) fn shutdown(self) {
        eprintln!("{self}: Shutdown requested");
        drop(self.requests_channel);
        self.thread_handle.join().expect("Can't join thread");
    }

    pub(super) fn feed(&mut self, sender_port: u16, request: ReadRequest) -> bool {
        self.requests_channel
            .blocking_send((sender_port, request))
            .is_ok()
    }

    pub(super) fn is_finished(&self) -> bool {
        self.thread_handle.is_finished()
    }
}

async fn peer_requests_handler(
    peer: IpAddr,
    local_address: IpAddr,
    mut available_roots: Vec<Box<dyn Root>>,
    mut rx_channel: Receiver<(u16, ReadRequest)>,
    idle_timeout: Duration,
) {
    let mut send_sessions: HashMap<u16, JoinHandle<()>> =
        HashMap::with_capacity(MAX_SESSIONS_PER_IP);
    let mut last_active = time::Instant::now();
    loop {
        match timeout(Duration::from_secs(1), rx_channel.recv()).await {
            Ok(Some((peer_port, request))) => {
                eprintln!("{peer}: sessions: {:?}", send_sessions.len());
                if send_sessions.contains_key(&peer_port) {
                    eprintln!("{peer}: Ignore repeated request from port {peer_port}");
                    continue;
                };
                let local_socket = UdpSocket::bind(SocketAddr::new(local_address, 0))
                    .await
                    .unwrap_or_else(|_| {
                        panic!("Can't bind to address {local_address} to random port")
                    });
                let udp_stream =
                    DatagramStream::new(local_socket, SocketAddr::new(peer, peer_port));
                if let Err(error) = handle_request(
                    request,
                    &mut send_sessions,
                    &mut available_roots,
                    udp_stream,
                )
                .await
                {
                    eprintln!(
                        "{peer}: Irrecoverable error occurred: {error}. A handler will be closed"
                    );
                    break;
                };
            }
            Ok(None) => {
                eprintln!("{peer}: Handler shutdown is requested");
                break;
            }
            Err(_elapsed) => {
                send_sessions.retain(|_peer_port, handle| !handle.is_finished());
                if send_sessions.is_empty() {
                    if time::Instant::now() - last_active > idle_timeout {
                        eprintln!("{peer}: Handler inactive, shutting down");
                        break;
                    }
                } else {
                    last_active = time::Instant::now();
                }
            }
        };
    }
    rx_channel.close();
    if !send_sessions.is_empty() {
        eprintln!("{peer}: Waiting sessions to finish ...");
    }
    for (_peer_port, handle) in send_sessions {
        _ = handle.await;
    }
}

async fn handle_request(
    read_request: ReadRequest,
    send_sessions: &mut HashMap<u16, JoinHandle<()>>,
    available_roots: &mut [Box<dyn Root>],
    datagram_stream: DatagramStream,
) -> Result<(), IrrecoverableError> {
    let mut send_buffer: Vec<u8> = vec![0; u16::MAX as usize];
    send_sessions.retain(|_peer_port, handle| !handle.is_finished());
    if send_sessions.len() >= send_sessions.capacity() {
        let error_message = "Maximum sessions per IP exceeded";
        let tftp_error = TFTPError::new(error_message, UNDEFINED_ERROR);
        if let Ok(to_send) = tftp_error.serialize(&mut send_buffer) {
            if let Err(error) = datagram_stream.send(&send_buffer[..to_send]).await {
                eprintln!("{datagram_stream}: Error sending {tftp_error}: {error}");
            }
        }
        return Err(IrrecoverableError(error_message.to_owned()));
    };
    let mut opened_file = match open_file(&read_request, available_roots) {
        Ok(file) => file,
        Err(tftp_error) => {
            eprintln!("{datagram_stream}: {read_request} denied: {tftp_error}");
            if let Ok(to_send) = tftp_error.serialize(&mut send_buffer) {
                if let Err(error) = datagram_stream.send(&send_buffer[..to_send]).await {
                    eprintln!("{datagram_stream}: Error sending {tftp_error}: {error}");
                }
            }
            return Ok(());
        }
    };
    eprintln!("{datagram_stream}: Opened {opened_file} ({read_request})");
    send_sessions.insert(
        datagram_stream.remote_port(),
        tokio::task::spawn_local(async {
            if let Some((mut tftp_stream, block_size)) = negotiate_options(
                datagram_stream,
                opened_file.as_mut(),
                &mut send_buffer,
                read_request.options,
            )
            .await
            {
                send_file(opened_file, &mut tftp_stream, block_size, &mut send_buffer).await;
            }
            drop(send_buffer);
        }),
    );
    Ok(())
}

fn get_available_remote_roots(tftp_root: &PathBuf, ip: &str) -> Vec<Box<dyn Root>> {
    let mut result: Vec<Box<dyn Root>> = Vec::new();
    eprintln!("Looking for TFTP root configs in {tftp_root:?} ...");
    for file_path in files_sorted(tftp_root) {
        if match_ip(&file_path, ip) {
            eprintln!("Found TFTP root config {file_path:?}");
            if let Some(json_struct) = read_json(&file_path) {
                eprintln!("Found JSON file {file_path:?}");
                if let Some(nbd_config) = NBDConfig::from_json(&json_struct) {
                    eprintln!("Found NBD TFTP root config {file_path:?}");
                    match nbd_config.connect() {
                        Ok(disk) => {
                            eprintln!("Connected config {file_path:?}");
                            result.push(disk);
                        }
                        Err(VirtualRootError::ConfigError(error)) => {
                            eprintln!("Invalid config {file_path:?}: {error}");
                        }
                        Err(VirtualRootError::SetupError(error)) => {
                            eprintln!(
                                "Failed to connect disk using config {file_path:?}: {error:?}"
                            );
                        }
                    }
                }
            }
        }
    }
    result
}

fn files_sorted<P: AsRef<Path>>(parent: P) -> Vec<PathBuf> {
    let mut files = fs::read_dir(parent)
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            if let Ok(entry) = entry {
                let path = entry.path();
                if path.is_file() {
                    return Some(path);
                };
            };
            None
        })
        .collect::<Vec<_>>();
    files.sort();
    files
}

fn match_ip(path: &Path, ip: &str) -> bool {
    if let Some(file_name) = path.file_name().and_then(|os| os.to_str()) {
        file_name.starts_with(ip)
    } else {
        false
    }
}

fn read_json(path: &Path) -> Option<Value> {
    if let Ok(content) = fs::read_to_string(path) {
        if let Ok(json_struct) = serde_json::from_str::<Value>(&content) {
            return Some(json_struct);
        }
    }
    None
}

async fn negotiate_options(
    udp_stream: DatagramStream,
    opened_file: &mut dyn OpenedFile,
    send_buffer: &mut [u8],
    options: HashMap<String, String>,
) -> Option<(TFTPStream, Blksize)> {
    let mut oack = OptionsAcknowledge::new();
    let ack_timeout = {
        if let Some(timeout) = AckTimeout::find_in(&options) {
            oack.push(timeout.as_key_pair());
            timeout
        } else {
            Default::default()
        }
    };
    let block_size = {
        if let Some(block_size) = Blksize::find_in(&options) {
            oack.push(block_size.as_key_pair());
            block_size
        } else {
            Default::default()
        }
    };
    let mut tftp_stream = TFTPStream::new(udp_stream, ack_timeout, 5);
    if TSize::is_requested(&options) {
        match TSize::obtain(opened_file) {
            Ok(tsize) => oack.push(tsize.as_key_pair()),
            Err(err) => {
                eprintln!("{tftp_stream}: Can't obtain TSize due to {err:?}")
            }
        }
    };
    if oack.has_options() {
        eprintln!("{tftp_stream}: {oack}");
        match oack.serialize(send_buffer) {
            Ok((oack_size, block_num)) => {
                match tftp_stream
                    .send_data(&send_buffer[..oack_size], block_num)
                    .await
                {
                    Ok(_) => {}
                    Err(SendError::ClientError(code, message)) => {
                        eprintln!("{tftp_stream}: Client responded with [{code}]: {message}");
                        return None;
                    }
                    Err(error) => {
                        eprintln!("{tftp_stream}: Error sending options: {error:?}");
                        return None;
                    }
                }
            }
            Err(buffer_error) => {
                eprintln!("{tftp_stream}: Error building options: {buffer_error}");
                let tftp_error = TFTPError::new("OACK build error", UNDEFINED_ERROR);
                if let Ok(error_length) = tftp_error.serialize(send_buffer) {
                    tftp_stream.fire_error(&send_buffer[..error_length]).await;
                } else {
                    eprintln!("{tftp_stream}: Error serializing {buffer_error}");
                }
                return None;
            }
        }
    };
    Some((tftp_stream, block_size))
}

fn open_file(
    read_request: &ReadRequest,
    roots: &mut [Box<dyn Root>],
) -> Result<Box<dyn OpenedFile>, TFTPError> {
    for remote_root in roots.iter_mut() {
        match read_request.open_in(remote_root.deref_mut()) {
            Ok(file) => return Ok(file),
            Err(FileError::FileNotFound) => continue,
            Err(FileError::AccessViolation) => {
                return Err(TFTPError::new("Access violation", ACCESS_VIOLATION));
            }
            Err(FileError::ReadError) => return Err(TFTPError::new("Read error", UNDEFINED_ERROR)),
            Err(_unknown_error) => return Err(TFTPError::new("Server Error", UNDEFINED_ERROR)),
        }
    }
    Err(TFTPError::new("File not found", FILE_NOT_FOUND))
}

async fn send_file(
    mut opened_file: Box<dyn OpenedFile>,
    tftp_stream: &mut TFTPStream,
    block_size: Blksize,
    send_buffer: &mut [u8],
) {
    let mut offset: usize = 0;
    let mut block_num: u16 = 1;
    loop {
        let header_size = place_block_header(send_buffer, block_num);
        let chunk_size =
            match block_size.read_chunk(opened_file.as_mut(), &mut send_buffer[header_size..]) {
                Ok(chunk_size) => chunk_size,
                Err(_err) => {
                    let tftp_error = TFTPError::new("Read error occurred", UNDEFINED_ERROR);
                    eprintln!("{tftp_stream}: {tftp_error}");
                    if let Ok(error_length) = tftp_error.serialize(send_buffer) {
                        tftp_stream.fire_error(&send_buffer[..error_length]).await;
                    } else {
                        eprintln!("{tftp_stream}: Error serializing {tftp_error}");
                    }
                    return;
                }
            };
        match tftp_stream
            .send_data(&send_buffer[..header_size + chunk_size], block_num)
            .await
        {
            Ok(_) => {}
            Err(SendError::Network(string)) => {
                eprintln!("{tftp_stream}: Network error while sending block {block_num}: {string}");
                return;
            }
            Err(SendError::Timeout) => {
                let tftp_error =
                    TFTPError::new(format!("Timed out block {block_num}"), UNDEFINED_ERROR);
                eprintln!("{tftp_stream}: {tftp_error}");
                if let Ok(error_length) = tftp_error.serialize(send_buffer) {
                    tftp_stream.fire_error(&send_buffer[..error_length]).await;
                } else {
                    eprintln!("{tftp_stream}: Error serializing {tftp_error}");
                }
                return;
            }
            Err(SendError::ClientError(code, message)) => {
                eprintln!("{tftp_stream}: Client error received: [{code}] {message}");
                return;
            }
            Err(send_error) => {
                eprintln!(
                    "{tftp_stream}: Unknown error while sending block {block_num}: {send_error:?}"
                );
                return;
            }
        }
        offset += chunk_size;
        if block_size.is_last(chunk_size) {
            eprintln!("{tftp_stream}: Sent {offset} bytes");
            return;
        }
        block_num = block_num.wrapping_add(1);
    }
}

fn place_block_header(buffer: &mut [u8], block_number: u16) -> usize {
    let mut datagram = WriteCursor::new(buffer);
    _ = datagram.put_ushort(DATA).unwrap();
    datagram.put_ushort(block_number).unwrap()
}

#[derive(Debug)]
struct IrrecoverableError(String);

impl Display for IrrecoverableError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "IrrecoverableFSError: {}", self.0)
    }
}
