use crate::cursor::ReadCursor;
use crate::datagram_stream::DatagramStream;
use crate::error::{ERROR, TFTPError};
use crate::fs::{OpenedFile, RootKind};
use crate::local_fs::LocalRoot;
use crate::messages::{OptionsAcknowledge, ReadRequest};
use crate::nbd_disk::{NbdRoot, open_nbd_root};
use crate::options::{AckTimeout, Blksize, Rollover, TSize, WindowSize};
use std::borrow::Borrow;
use std::collections::HashMap;
use std::fmt;
use std::fmt::{Debug, Display, Formatter};
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::task::JoinHandle;
use tokio::time::timeout;

#[cfg(test)]
mod tests;

const ACK: u16 = 0x04;
const DATA: u16 = 0x03;
const MAX_SESSIONS_PER_IP: usize = 128;
const SEND_ATTEMPTS: u16 = 5;

async fn fire_error<D: Borrow<DatagramStream>>(
    error: TFTPError,
    datagram_stream: D,
    mut buffer: impl AsMut<[u8]>,
) {
    let borrowed_stream = datagram_stream.borrow();
    let borrowed_buffer = buffer.as_mut();
    match error.serialize(borrowed_buffer) {
        Ok(to_send) => {
            if let Err(send_error) = borrowed_stream.send(&borrowed_buffer[..to_send]).await {
                eprintln!("{borrowed_stream}: Error sending {error}: {send_error}");
            } else {
                eprintln!("{borrowed_stream}: Sent {error}");
            }
        }
        Err(buffer_error) => {
            eprintln!("{borrowed_stream}: Error serializing {error}: {buffer_error}")
        }
    }
}

struct Window {
    block_size: u16,
    buffers: Vec<Vec<u8>>,
    rollover: Rollover,
}

impl Window {
    fn new(block_size: u16, window_size: u16, rollover: Rollover) -> Self {
        Self {
            block_size,
            buffers: (0..window_size)
                .map(|_| vec![0; block_size as usize + 2 * size_of::<u16>()])
                .collect(),
            rollover,
        }
    }

    fn rollover(&self) -> Rollover {
        self.rollover
    }

    fn size(&self) -> u16 {
        self.buffers.len() as u16
    }

    async fn push_block<O: OpenedFile>(
        &mut self,
        opened_file: &mut O,
        seq: u64,
        block_number: u16,
    ) -> io::Result<(usize, bool)> {
        let buffer = self.buffer(seq);
        buffer[0] = 0;
        buffer[1] = DATA as u8;
        buffer[2..4].copy_from_slice(&block_number.to_be_bytes());
        let read_bytes = opened_file.read_to(&mut buffer[4..]).await?;
        buffer.truncate(read_bytes + 4);
        Ok((read_bytes, read_bytes < self.block_size as usize))
    }

    fn slot(&self, seq: u64) -> usize {
        (seq % self.buffers.len() as u64) as usize
    }

    fn buffer(&mut self, seq: u64) -> &mut Vec<u8> {
        let slot = self.slot(seq);
        let buffer = &mut self.buffers[slot];
        unsafe { buffer.set_len(buffer.capacity()) }
        buffer
    }

    async fn send(&mut self, seq: u64, datagram_stream: &DatagramStream) -> io::Result<()> {
        let slot = self.slot(seq);
        let buffer = &self.buffers[slot];
        datagram_stream.send(buffer).await
    }
}

async fn send_file<O: OpenedFile>(
    mut opened_file: O,
    datagram_stream: &DatagramStream,
    mut window: Window,
    ack_timeout: AckTimeout,
    buffer: &mut [u8],
) -> Result<(usize, usize), TFTPError> {
    let mut bytes_sent: usize = 0;
    let mut blocks_sent: usize = 0;
    let mut last_acknowledged_seq: u64 = 0;
    let mut last_read_seq: u64 = 0;
    let mut done = false;
    while !done {
        let unacknowledged_count = (last_read_seq - last_acknowledged_seq) as u16;
        debug_assert!(unacknowledged_count <= window.size());
        let mut to_send = unacknowledged_count;
        while to_send < window.size() {
            last_read_seq += 1;
            let block_number = window.rollover().block_number(last_read_seq);
            if let Ok((read_bytes, is_last)) = window
                .push_block(&mut opened_file, last_read_seq, block_number)
                .await
            {
                to_send += 1;
                bytes_sent += read_bytes;
                if is_last {
                    done = true;
                    break;
                }
            } else {
                return Err(TFTPError::undefined("Read file error occurred"));
            }
        }
        debug_assert!(to_send <= window.size());
        last_acknowledged_seq = match send_reliably(
            &mut window,
            &ack_timeout,
            datagram_stream,
            buffer,
            last_acknowledged_seq + 1,
            to_send,
        )
        .await
        {
            Ok(received_acknowledged) => received_acknowledged,
            Err(SendError::Timeout) => {
                return Err(TFTPError::undefined("Send timeout occurred"));
            }
            Err(SendError::ClientError(code, string)) => {
                eprintln!("{datagram_stream}: Early termination [{code}] {string}");
                blocks_sent += to_send as usize;
                return Ok((bytes_sent, blocks_sent));
            }
            Err(_) => {
                return Err(TFTPError::undefined("Unknown error occurred"));
            }
        };
    }
    Ok((bytes_sent, blocks_sent))
}

async fn read_acknowledge(
    datagram_stream: &DatagramStream,
    buffer: &mut [u8],
) -> Result<u16, RecvError> {
    if let Err(err) = datagram_stream.recv(buffer, 4).await {
        eprintln!("{datagram_stream}: Read error: {:?}", err);
        return Err(RecvError::Network);
    }
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
}

enum WindowWait {
    Accepted(u64),
    Retransmit,
}

async fn wait_for_window_ack(
    datagram_stream: &DatagramStream,
    buffer: &mut [u8],
    window_first: u64,
    count: u16,
    rollover: Rollover,
) -> Result<WindowWait, RecvError> {
    let window_end = window_first + count as u64;
    loop {
        match read_acknowledge(datagram_stream, buffer).await {
            Ok(received_ack) => match classify_ack(received_ack, window_first, count, rollover) {
                AckClass::InWindow(acknowledged) => {
                    return Ok(WindowWait::Accepted(acknowledged));
                }
                AckClass::Previous => {
                    eprintln!(
                        "{datagram_stream}: Duplicate ACK {received_ack}, retransmitting {}",
                        rollover.block_number(window_first)
                    );
                    return Ok(WindowWait::Retransmit);
                }
                AckClass::Other => {
                    eprintln!(
                        "{datagram_stream}: Ignore ACK {received_ack} outside {} .. {}",
                        rollover.block_number(window_first),
                        rollover.block_number(window_end)
                    );
                }
            },
            Err(error) => return Err(error),
        }
    }
}

#[derive(Debug)]
enum SendError {
    Network,
    Timeout,
    ClientError(u16, String),
}

#[derive(Debug, PartialEq, Eq)]
enum AckClass {
    InWindow(u64),
    Previous,
    Other,
}

fn classify_ack(received_ack: u16, window_first: u64, count: u16, rollover: Rollover) -> AckClass {
    if let Some(distance) = rollover.distance_in_cycle(received_ack, window_first)
        && distance < count as u64
    {
        return AckClass::InWindow(window_first + distance);
    }
    if received_ack == rollover.block_number(window_first.wrapping_sub(1)) {
        AckClass::Previous
    } else {
        AckClass::Other
    }
}

#[derive(Debug)]
enum RecvError {
    Network,
    ClientError(u16, String),
    ACKError,
}

pub(super) struct PeerHandler {
    sender_address: IpAddr,
    requests_channel: Option<Sender<(u16, ReadRequest)>>,
    task: Option<JoinHandle<()>>,
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
        let task = tokio::task::spawn_local(peer_requests_handler(
            peer,
            local_address,
            tftp_root,
            rx,
            idle_timeout,
        ));
        Self {
            sender_address: peer,
            requests_channel: Some(tx),
            task: Some(task),
        }
    }

    pub(super) async fn shutdown(mut self) {
        eprintln!("{self}: Shutdown requested");
        drop(self.requests_channel.take());
        if let Some(handle) = self.task.take() {
            let _ = handle.await;
        }
    }

    pub(super) async fn feed(&mut self, sender_port: u16, request: ReadRequest) -> bool {
        let Some(sender) = &self.requests_channel else {
            return false;
        };
        sender.send((sender_port, request)).await.is_ok()
    }

    pub(super) fn is_finished(&self) -> bool {
        self.task.as_ref().is_none_or(|handle| handle.is_finished())
    }
}

impl Drop for PeerHandler {
    fn drop(&mut self) {
        if let Some(handle) = self.task.take() {
            handle.abort();
        }
    }
}

async fn roots_for_peer(tftp_root: &Path, peer: &str) -> Vec<RootKind> {
    let mut available_roots = vec![RootKind::Local(LocalRoot::new(tftp_root.join(peer)))];
    match open_nbd_root(tftp_root, peer).await {
        NbdRoot::Connected(remote_root) => {
            available_roots.push(RootKind::Remote(remote_root));
        }
        NbdRoot::Absent => {
            available_roots.push(RootKind::Local(LocalRoot::new(tftp_root.join("default"))));
        }
        NbdRoot::Unusable => {}
    }
    available_roots
}

async fn peer_requests_handler(
    peer: IpAddr,
    local_address: IpAddr,
    tftp_root: PathBuf,
    mut rx_channel: Receiver<(u16, ReadRequest)>,
    idle_timeout: Duration,
) {
    let available_roots = Rc::new(roots_for_peer(&tftp_root, &peer.to_string()).await);
    let mut send_sessions: HashMap<u16, JoinHandle<()>> =
        HashMap::with_capacity(MAX_SESSIONS_PER_IP);
    let mut last_active = Instant::now();
    loop {
        let (peer_port, request) = match timeout(Duration::from_secs(1), rx_channel.recv()).await {
            Ok(Some(result)) => result,
            Ok(None) => {
                eprintln!("{peer}: Handler shutdown is requested");
                break;
            }
            Err(_elapsed) => {
                send_sessions.retain(|_peer_port, handle| !handle.is_finished());
                if send_sessions.is_empty() {
                    if Instant::now() - last_active > idle_timeout {
                        eprintln!("{peer}: Handler inactive, shutting down");
                        break;
                    }
                } else {
                    last_active = Instant::now();
                }
                continue;
            }
        };
        eprintln!("{peer}: sessions: {:?}", send_sessions.len());
        if send_sessions.contains_key(&peer_port) {
            eprintln!("{peer}: Ignore repeated request from port {peer_port}");
            continue;
        };
        let local_socket = UdpSocket::bind(SocketAddr::new(local_address, 0))
            .await
            .unwrap_or_else(|err| {
                panic!("Can't bind to address {local_address} to random port dues to {err}")
            });
        let datagram_stream = DatagramStream::new(local_socket, SocketAddr::new(peer, peer_port));
        let buffer: Vec<u8> = vec![0; u16::MAX as usize];
        send_sessions.retain(|_peer_port, handle| !handle.is_finished());
        if send_sessions.len() >= MAX_SESSIONS_PER_IP {
            let error_message = "Maximum sessions per IP exceeded";
            eprintln!("{peer}: {error_message}");
            let tftp_error = TFTPError::undefined(error_message);
            fire_error(tftp_error, &datagram_stream, buffer).await;
            continue;
        };
        let roots = Rc::clone(&available_roots);
        send_sessions.insert(
            peer_port,
            tokio::task::spawn_local(serve_request(request, datagram_stream, roots, buffer)),
        );
    }
    rx_channel.close();
    if !send_sessions.is_empty() {
        eprintln!("{peer}: Waiting sessions to finish ...");
    }
    for (_peer_port, handle) in send_sessions {
        _ = handle.await;
    }
    eprintln!("{peer}: Handler closed");
}

async fn serve_request(
    request: ReadRequest,
    datagram_stream: DatagramStream,
    available_roots: Rc<Vec<RootKind>>,
    buffer: Vec<u8>,
) {
    for root in available_roots.iter() {
        let error = match root {
            RootKind::Local(local_root) => match request.open_in(local_root).await {
                Ok(opened_file) => {
                    send(
                        opened_file,
                        datagram_stream,
                        request.yield_options(),
                        buffer,
                    )
                    .await;
                    return;
                }
                Err(err) => err,
            },
            RootKind::Remote(remote_root) => match request.open_in(remote_root).await {
                Ok(opened_file) => {
                    send(
                        opened_file,
                        datagram_stream,
                        request.yield_options(),
                        buffer,
                    )
                    .await;
                    return;
                }
                Err(err) => err,
            },
        };
        match error.kind() {
            io::ErrorKind::NotFound => continue,
            io::ErrorKind::PermissionDenied => {
                fire_error(TFTPError::access_violation(), datagram_stream, buffer).await;
                return;
            }
            _error => {
                fire_error(
                    TFTPError::undefined("Server Error"),
                    datagram_stream,
                    buffer,
                )
                .await;
                return;
            }
        }
    }
    fire_error(TFTPError::file_not_found(), datagram_stream, buffer).await;
}

async fn send<O: OpenedFile>(
    mut opened_file: O,
    datagram_stream: DatagramStream,
    options: HashMap<String, String>,
    mut buffer: Vec<u8>,
) {
    if let Some((window, ack_timeout)) =
        negotiate_options(&datagram_stream, &mut opened_file, &mut buffer, &options).await
    {
        match send_file(
            opened_file,
            &datagram_stream,
            window,
            ack_timeout,
            &mut buffer,
        )
        .await
        {
            Ok((sent_bytes, sent_blocks)) => {
                eprintln!("{datagram_stream}: Sent {sent_bytes} bytes, {sent_blocks} blocks")
            }
            Err(tftp_error) => fire_error(tftp_error, &datagram_stream, &mut buffer).await,
        };
        drop(buffer);
        drop(datagram_stream);
    }
}

async fn send_reliably(
    window: &mut Window,
    ack_timeout: &AckTimeout,
    datagram_stream: &DatagramStream,
    buffer: &mut [u8],
    window_first: u64,
    count: u16,
) -> Result<u64, SendError> {
    let window_end = window_first + count as u64;
    let rollover = window.rollover();
    for attempt in 1..=SEND_ATTEMPTS {
        for block_seq in window_first..window_end {
            if let Err(send_error) = window.send(block_seq, datagram_stream).await {
                eprintln!(
                    "{datagram_stream}: Network error while sending block {}: {send_error}",
                    rollover.block_number(block_seq)
                );
                return Err(SendError::Network);
            }
        }
        match ack_timeout
            .timeout(wait_for_window_ack(
                datagram_stream,
                buffer,
                window_first,
                count,
                rollover,
            ))
            .await
        {
            Ok(Ok(WindowWait::Accepted(acknowledged))) => return Ok(acknowledged),
            Ok(Ok(WindowWait::Retransmit)) => {}
            Ok(Err(RecvError::ClientError(error_code, error_message))) => {
                return Err(SendError::ClientError(error_code, error_message));
            }
            Ok(Err(_)) => return Err(SendError::Network),
            Err(_elapsed) => {
                eprintln!(
                    "{datagram_stream}: Timeout waiting for {} .. {}, attempt {attempt}",
                    rollover.block_number(window_first),
                    rollover.block_number(window_end)
                );
            }
        }
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
    for attempt in 1..=SEND_ATTEMPTS {
        // Recv writes the ACK into this same buffer, so rebuild before every send.
        let oack_size = match oack.serialize(buffer) {
            Ok(size) => size,
            Err(buffer_error) => {
                let tftp_error = TFTPError::undefined("OACK build error");
                fire_error(tftp_error, datagram_stream, buffer).await;
                return Err(io::Error::other(format!(
                    "Error building options: {buffer_error}"
                )));
            }
        };
        datagram_stream.send(&buffer[..oack_size]).await?;
        match ack_timeout
            .timeout(read_acknowledge(datagram_stream, buffer))
            .await
        {
            Ok(Ok(ack_num)) if ack_num == oack_index => return Ok(()),
            Ok(Ok(ack_num)) => {
                let tftp_error = TFTPError::undefined("Unexpected non-zero ACK");
                fire_error(tftp_error, datagram_stream, buffer).await;
                return Err(io::Error::other(format!(
                    "Received unexpected ACK {ack_num} while expecting {oack_index}"
                )));
            }
            Err(_elapsed) => {
                eprintln!("Timeout waiting for ACK {oack_index}, attempt {attempt}");
                continue;
            }
            Ok(Err(RecvError::ClientError(code, string))) => {
                return Err(io::Error::other(format!(
                    "Early termination while options negotiation [{code}] {string}"
                )));
            }
            Ok(Err(error)) => {
                return Err(io::Error::other(format!("ACK read error: {:?}", error)));
            }
        }
    }
    let tftp_error = TFTPError::undefined("Send timeout occurred");
    fire_error(tftp_error, datagram_stream, buffer).await;
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        format!("Timeout waiting for ACK {oack_index}"),
    ))
}

async fn negotiate_options<O: OpenedFile>(
    datagram_stream: &DatagramStream,
    opened_file: &mut O,
    buffer: &mut [u8],
    options: &HashMap<String, String>,
) -> Option<(Window, AckTimeout)> {
    let mut oack = OptionsAcknowledge::new();
    let ack_timeout = {
        if let Some(timeout) = AckTimeout::find_in(options) {
            oack.push(timeout.as_key_pair());
            timeout
        } else {
            Default::default()
        }
    };
    let block_size = {
        if let Some(block_size) = Blksize::find_in(options) {
            oack.push(block_size.as_key_pair());
            block_size
        } else {
            Default::default()
        }
    };
    if TSize::is_requested(options) {
        match TSize::obtain(opened_file).await {
            Ok(tsize) => oack.push(tsize.as_key_pair()),
            Err(err) => {
                eprintln!("{datagram_stream}: Can't obtain TSize due to {err:?}")
            }
        }
    };
    let rollover = Rollover::find_in(options);
    let rollover_value = rollover.unwrap_or_default();
    let window_size = {
        if let Some(window_size) = WindowSize::find_in(options) {
            let requested = window_size.get_size();
            let limited = window_size.limited_to(rollover_value.max_window_size());
            if limited.get_size() != requested {
                eprintln!(
                    "{datagram_stream}: Requested window size {requested} capped to {} for rollover {}",
                    limited.get_size(),
                    rollover_value.as_key_pair().1
                );
            }
            oack.push(limited.as_key_pair());
            limited
        } else {
            Default::default()
        }
    };
    if let Some(rollover) = rollover {
        oack.push(rollover.as_key_pair());
    }
    if oack.has_options()
        && let Err(oack_negotiation_error) =
            send_oack_reliably(&oack, datagram_stream, &ack_timeout, buffer).await
    {
        eprintln!("{datagram_stream}: {oack_negotiation_error}");
        return None;
    };
    let window = Window::new(
        block_size.get_size() as u16,
        window_size.get_size() as u16,
        rollover_value,
    );
    Some((window, ack_timeout))
}
