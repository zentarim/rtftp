use crate::cursor::ReadCursor;
use crate::datagram_stream::DatagramStream;
use crate::fs::{FileError, OpenedFile, Root};
use crate::local_fs::LocalRoot;
use crate::messages::{OptionsAcknowledge, ReadRequest, TFTPError, UNDEFINED_ERROR};
use crate::nbd_disk::NBDConfig;
use crate::options::{AckTimeout, Blksize, TSize, WindowSize};
use crate::remote_fs::{Config, VirtualRootError};
use serde_json::Value;
use std::collections::HashMap;
use std::fmt::{Debug, Display, Formatter};
use std::io;
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

#[cfg(test)]
mod tests;

const ACK: u16 = 0x04;
const DATA: u16 = 0x03;

const ERROR: u16 = 0x05;

const FILE_NOT_FOUND: u16 = 0x01;

const ACCESS_VIOLATION: u16 = 0x02;

const MAX_SESSIONS_PER_IP: usize = 128;

const SEND_ATTEMPTS: u16 = 5;

async fn fire_error(error: TFTPError, datagram_stream: &DatagramStream, buffer: &mut [u8]) {
    match error.serialize(buffer) {
        Ok(to_send) => {
            if let Err(send_error) = datagram_stream.send(&buffer[..to_send]).await {
                eprintln!("{datagram_stream}: Error sending {error}: {send_error}");
            } else {
                eprintln!("{datagram_stream}: Sent {error}");
            }
        }
        Err(buffer_error) => {
            eprintln!("{datagram_stream}: Error serializing {error}: {buffer_error}")
        }
    }
}

struct Window {
    block_size: u16,
    buffers: Vec<Vec<u8>>,
}

impl Window {
    fn new(block_size: u16, window_size: u16) -> Self {
        Self {
            block_size,
            buffers: (0..window_size)
                .map(|_| vec![0; block_size as usize + 2 * size_of::<u16>()])
                .collect(),
        }
    }

    fn size(&self) -> u16 {
        self.buffers.capacity() as u16
    }

    fn push_block(
        &mut self,
        opened_file: &mut dyn OpenedFile,
        index: u16,
    ) -> Result<(usize, bool), FileError> {
        let buffer = self.buffer(index);
        buffer[0] = 0;
        buffer[1] = DATA as u8;
        buffer[2] = (index >> 8) as u8;
        buffer[3] = index as u8;
        let read_bytes = opened_file.read_to(&mut buffer[4..])?;
        buffer.truncate(read_bytes + 4);
        Ok((read_bytes, read_bytes < self.block_size as usize))
    }
    fn buffer(&mut self, index: u16) -> &mut Vec<u8> {
        let window_length = self.buffers.len();
        let buffer = &mut self.buffers[index as usize % window_length];
        unsafe { buffer.set_len(buffer.capacity()) }
        buffer
    }

    async fn send(&mut self, index: u16, datagram_stream: &DatagramStream) -> std::io::Result<()> {
        let window_length = self.buffers.len();
        let buffer = &mut self.buffers[index as usize % window_length];
        datagram_stream.send(buffer).await
    }
}

async fn send_file(
    mut opened_file: Box<dyn OpenedFile>,
    datagram_stream: &DatagramStream,
    mut window: Window,
    ack_timeout: AckTimeout,
    buffer: &mut [u8],
) -> Result<(usize, usize), TFTPError> {
    let mut bytes_sent: usize = 0;
    let mut blocks_sent: usize = 0;
    let mut last_acknowledged_index: u16 = 0;
    let mut last_read_index: u16 = 0;
    let mut done = false;
    while !done {
        let unacknowledged_count = last_read_index.wrapping_sub(last_acknowledged_index);
        debug_assert!(unacknowledged_count <= window.size());
        let mut to_send = unacknowledged_count;
        while to_send < window.size() {
            last_read_index = last_read_index.wrapping_add(1);
            if let Ok((read_bytes, is_last)) =
                window.push_block(opened_file.as_mut(), last_read_index)
            {
                to_send += 1;
                bytes_sent += read_bytes;
                if is_last {
                    done = true;
                    break;
                }
            } else {
                return Err(TFTPError::new("Read file error occurred", UNDEFINED_ERROR));
            }
        }
        debug_assert!(to_send <= window.size());
        last_acknowledged_index = match send_reliably(
            &mut window,
            &ack_timeout,
            datagram_stream,
            buffer,
            last_acknowledged_index.wrapping_add(1),
            to_send,
        )
        .await
        {
            Ok(received_acknowledged) => received_acknowledged,
            Err(SendError::Timeout) => {
                return Err(TFTPError::new("Send timeout occurred", UNDEFINED_ERROR));
            }
            Err(SendError::ClientError(code, string)) => {
                eprintln!("{datagram_stream}: Early termination [{code}] {string}");
                blocks_sent += to_send as usize;
                return Ok((bytes_sent, blocks_sent));
            }
            Err(_) => {
                return Err(TFTPError::new("Unknown error occurred", UNDEFINED_ERROR));
            }
        };
    }
    Ok((bytes_sent, blocks_sent))
}

async fn read_acknowledge(
    datagram_stream: &DatagramStream,
    buffer: &mut [u8],
    ack_timeout: &AckTimeout,
) -> Result<u16, RecvError> {
    let recv_future = datagram_stream.recv(buffer, 4);
    if let Ok(read_result) = ack_timeout.timeout(recv_future).await {
        let _read_size = match read_result {
            Ok(size) => size,
            Err(err) => {
                eprintln!("{datagram_stream}: Read error: {:?}", err);
                return Err(RecvError::Network);
            }
        };
        let mut datagram = ReadCursor::new(buffer);
        match datagram.extract_ushort() {
            Ok(opcode) if opcode == ACK => {
                Ok(datagram.extract_ushort().map_err(|_| RecvError::ACKError)?)
            }
            Ok(opcode) if opcode == ERROR => {
                let error_code = datagram.extract_ushort().map_err(|_| RecvError::ACKError)?;
                let error_message = datagram.extract_string().map_err(|_| RecvError::ACKError)?;
                Err(RecvError::ClientError(error_code, error_message))
            }
            Ok(opcode) => {
                eprintln!("{datagram_stream}: Received unknown opcode 0x{opcode:02x}");
                Err(RecvError::ACKError)
            }
            Err(_) => Err(RecvError::ACKError),
        }
    } else {
        Err(RecvError::Timeout)
    }
}

#[derive(Debug)]
pub(super) enum SendError {
    Network,
    Timeout,
    ClientError(u16, String),
    ACKError,
}

#[derive(Debug)]
pub(super) enum RecvError {
    Network,
    Timeout,
    ClientError(u16, String),
    ACKError,
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
                available_roots.push(Box::new(LocalRoot::new(tftp_root.join("default"))));
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

    pub(super) async fn feed(&mut self, sender_port: u16, request: ReadRequest) -> bool {
        self.requests_channel
            .send((sender_port, request))
            .await
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
        fire_error(tftp_error, &datagram_stream, &mut send_buffer).await;
        return Err(IrrecoverableError(error_message.to_owned()));
    };
    let mut opened_file = match open_file(&read_request, available_roots) {
        Ok(file) => file,
        Err(tftp_error) => {
            eprintln!("{datagram_stream}: {read_request} denied: {tftp_error}");
            fire_error(tftp_error, &datagram_stream, &mut send_buffer).await;
            return Ok(());
        }
    };
    eprintln!("{datagram_stream}: Opened {opened_file} ({read_request})");
    send_sessions.insert(
        datagram_stream.remote_port(),
        tokio::task::spawn_local(async {
            if let Some((window, ack_timeout)) = negotiate_options(
                &datagram_stream,
                &mut opened_file,
                &mut send_buffer,
                read_request.options,
            )
            .await
            {
                match send_file(
                    opened_file,
                    &datagram_stream,
                    window,
                    ack_timeout,
                    &mut send_buffer,
                )
                .await
                {
                    Ok((sent_bytes, sent_blocks)) => eprintln!(
                        "{datagram_stream}: Sent {sent_bytes} bytes, {sent_blocks} blocks"
                    ),
                    Err(tftp_error) => {
                        fire_error(tftp_error, &datagram_stream, &mut send_buffer).await
                    }
                };
                drop(send_buffer);
                drop(datagram_stream);
            }
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
                            result.push(Box::new(disk));
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
    if let Ok(content) = fs::read_to_string(path)
        && let Ok(json_struct) = serde_json::from_str::<Value>(&content)
    {
        return Some(json_struct);
    }
    None
}

async fn send_reliably(
    window: &mut Window,
    ack_timeout: &AckTimeout,
    datagram_stream: &DatagramStream,
    buffer: &mut [u8],
    window_index: u16,
    count: u16,
) -> Result<u16, SendError> {
    for attempt in 1..=SEND_ATTEMPTS {
        for block_index in (0..count).map(|v| window_index.wrapping_add(v)) {
            if let Err(send_error) = window.send(block_index, datagram_stream).await {
                eprintln!(
                    "{datagram_stream}: Network error while sending block {block_index}: {send_error}"
                );
                return Err(SendError::Network);
            }
        }
        return match read_acknowledge(datagram_stream, buffer, ack_timeout).await {
            Ok(received_ack) if received_ack >= window_index => Ok(received_ack),
            Ok(unexpected_ack) => {
                let tftp_error = TFTPError::new("Received ACK from the past", UNDEFINED_ERROR);
                eprintln!(
                    "{datagram_stream}: Received ACK {unexpected_ack} while expected > {window_index}"
                );
                fire_error(tftp_error, datagram_stream, buffer).await;
                Err(SendError::ACKError)
            }
            Err(RecvError::Timeout) => {
                let window_end_index = window_index.wrapping_add(count);
                eprintln!(
                    "{datagram_stream}: Timeout waiting for {window_index} .. {window_end_index}, attempt {attempt}"
                );
                continue;
            }
            Err(RecvError::ClientError(error_code, error_message)) => {
                Err(SendError::ClientError(error_code, error_message))
            }
            Err(_) => Err(SendError::Network),
        };
    }
    Err(SendError::Timeout)
}

async fn send_oack_reliably(
    oack: &OptionsAcknowledge,
    datagram_stream: &DatagramStream,
    ack_timeout: &AckTimeout,
    buffer: &mut [u8],
) -> io::Result<()> {
    let oack_index = 0;
    let oack_size = match oack.serialize(buffer) {
        Ok(size) => size,
        Err(buffer_error) => {
            let tftp_error = TFTPError::new("OACK build error", UNDEFINED_ERROR);
            fire_error(tftp_error, datagram_stream, buffer).await;
            return Err(io::Error::other(format!(
                "Error building options: {buffer_error}"
            )));
        }
    };
    for attempt in 1..=SEND_ATTEMPTS {
        datagram_stream.send(&buffer[..oack_size]).await?;
        match read_acknowledge(datagram_stream, buffer, ack_timeout).await {
            Ok(ack_num) if ack_num == oack_index => return Ok(()),
            Ok(ack_num) => {
                let tftp_error = TFTPError::new("Unexpected non-zero ACK", UNDEFINED_ERROR);
                fire_error(tftp_error, datagram_stream, buffer).await;
                return Err(io::Error::other(format!(
                    "Received unexpected ACK {ack_num} while expecting {oack_index}"
                )));
            }
            Err(RecvError::Timeout) => {
                eprintln!("Timeout waiting for ACK {oack_index}, attempt {attempt}");
                continue;
            }
            Err(RecvError::ClientError(code, string)) => {
                return Err(io::Error::other(format!(
                    "Early termination while options negotiation [{code}] {string}"
                )));
            }
            Err(error) => {
                return Err(io::Error::other(format!("ACK read error: {:?}", error)));
            }
        }
    }
    let tftp_error = TFTPError::new("Send timeout occurred", UNDEFINED_ERROR);
    fire_error(tftp_error, datagram_stream, buffer).await;
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        format!("Timeout waiting for ACK {oack_index}"),
    ))
}

async fn negotiate_options(
    datagram_stream: &DatagramStream,
    opened_file: &mut Box<dyn OpenedFile>,
    buffer: &mut [u8],
    options: HashMap<String, String>,
) -> Option<(Window, AckTimeout)> {
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
    if TSize::is_requested(&options) {
        match TSize::obtain(opened_file.as_mut()) {
            Ok(tsize) => oack.push(tsize.as_key_pair()),
            Err(err) => {
                eprintln!("{datagram_stream}: Can't obtain TSize due to {err:?}")
            }
        }
    };
    let window_size = {
        if let Some(window_size) = WindowSize::find_in(&options) {
            oack.push(window_size.as_key_pair());
            window_size
        } else {
            Default::default()
        }
    };
    if oack.has_options()
        && let Err(oack_negotiation_error) =
            send_oack_reliably(&oack, datagram_stream, &ack_timeout, buffer).await
    {
        eprintln!("{datagram_stream}: {oack_negotiation_error}");
        return None;
    };
    let window = Window::new(block_size.get_size() as u16, window_size.get_size() as u16);
    Some((window, ack_timeout))
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

#[derive(Debug)]
struct IrrecoverableError(String);

impl Display for IrrecoverableError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "IrrecoverableFSError: {}", self.0)
    }
}
