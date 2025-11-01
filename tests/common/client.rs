use std::collections::HashMap;
use std::fs::File;
use std::io::{ErrorKind, Write};
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::time::Duration;
use std::{fmt, fs, io, time};
use tokio::net::UdpSocket;
const _BUFFER_SIZE: usize = 1536;
const _U16_SIZE: usize = size_of::<u16>();
const _RRQ: u16 = 0x01;
const _DATA: u16 = 0x03;
const _ACK: u16 = 0x04;
const _ERR: u16 = 0x05;
const _OACK: u16 = 0x06;

#[derive(Debug)]
struct _SendError<T> {
    message: String,
    sent: T,
}

#[derive(Debug)]
pub(crate) enum TFTPClientError<T: fmt::Debug> {
    IO(io::Error),
    Timeout(T),
    ClientError(u16, String),
    ParseError(String),
    UnexpectedData(Vec<u8>),
    UnexpectedPeer(IpAddr, Vec<u8>),
}

impl<T: fmt::Debug> fmt::Display for TFTPClientError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TFTPClientError::Timeout(msg) => write!(f, "Timeout: {msg:?}"),
            TFTPClientError::ClientError(code, msg) => write!(f, "ACK: [{code}] {msg}"),
            TFTPClientError::IO(error) => write!(f, "IO: {error:?} "),
            TFTPClientError::ParseError(error) => write!(f, "ParseError: {error}"),
            TFTPClientError::UnexpectedData(data) => write!(f, "UnexpectedData: {data:?}"),
            TFTPClientError::UnexpectedPeer(remote_ip, data) => {
                write!(f, "Unexpected peer {remote_ip}: {}", data.len())
            }
        }
    }
}

impl<T: fmt::Debug> std::error::Error for TFTPClientError<T> {}

impl<T: fmt::Debug> TFTPClientError<T> {
    #[allow(dead_code)]
    fn unpack(self) -> Option<T> {
        if let TFTPClientError::Timeout(sent) = self {
            Some(sent)
        } else {
            None
        }
    }

    #[allow(dead_code)]
    fn client_error(self) -> Option<(u16, String)> {
        if let TFTPClientError::ClientError(code, message) = self {
            Some((code, message))
        } else {
            None
        }
    }
}

#[derive(Debug)]
pub(crate) struct TFTPClient {
    local_socket: UdpSocket,
    remote_addr: SocketAddr,
    read_buffer: [u8; _BUFFER_SIZE],
    write_buffer: [u8; _BUFFER_SIZE],
}

impl TFTPClient {
    pub(super) fn new(local_socket: UdpSocket, remote_addr: SocketAddr) -> Self {
        Self {
            local_socket,
            remote_addr,
            read_buffer: [0; _BUFFER_SIZE],
            write_buffer: [0; _BUFFER_SIZE],
        }
    }

    pub(crate) async fn send_plain_read_request(
        mut self,
        file_name: &str,
    ) -> io::Result<SentPlainReadRequest> {
        let (_write_cursor, buffer_size) = self.make_read_request(file_name);
        self.local_socket
            .send_to(&self.write_buffer[..buffer_size], &self.remote_addr)
            .await?;
        Ok(SentPlainReadRequest {
            file_name: file_name.to_string(),
            local_socket: self.local_socket,
            remote_addr: self.remote_addr,
            read_buffer: self.read_buffer,
            write_buffer: self.write_buffer,
            sent_bytes: buffer_size,
        })
    }

    pub(crate) async fn send_optioned_read_request(
        mut self,
        file_name: &str,
        options: &HashMap<String, String>,
    ) -> io::Result<SentReadRequestWithOpts> {
        let (mut write_cursor, mut buffer_size) = self.make_read_request(file_name);
        for (option_name, option_value) in options {
            _ = write_cursor.put_string(option_name).unwrap();
            buffer_size = write_cursor.put_string(option_value).unwrap();
        }
        self.local_socket
            .send_to(&self.write_buffer[..buffer_size], &self.remote_addr)
            .await?;
        Ok(SentReadRequestWithOpts {
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

pub(crate) struct DatagramStream {
    local_socket: UdpSocket,
    peer_address: SocketAddr,
    display: String,
}

impl fmt::Debug for DatagramStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.display)
    }
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

    pub(crate) async fn recv(
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

impl fmt::Display for DatagramStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.display)
    }
}

pub(crate) struct SentReadRequestWithOpts {
    file_name: String,
    options: HashMap<String, String>,
    local_socket: UdpSocket,
    remote_addr: SocketAddr,
    read_buffer: [u8; _BUFFER_SIZE],
    write_buffer: [u8; _BUFFER_SIZE],
    sent_bytes: usize,
}

impl fmt::Debug for SentReadRequestWithOpts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "_SentReadRequestWithOpts")
    }
}

impl SentReadRequestWithOpts {
    pub(crate) async fn read_oack(
        mut self,
        read_timeout: usize,
    ) -> Result<OACK, TFTPClientError<Self>> {
        let duration = time::Duration::from_secs(read_timeout as u64);
        let read_future = self.local_socket.recv_from(&mut self.read_buffer);
        match tokio::time::timeout(duration, read_future).await {
            Ok(Ok((read_bytes, remote_address)))
                if remote_address.ip() == self.remote_addr.ip() =>
            {
                let mut read_cursor = ReadCursor::new(&mut self.read_buffer[..read_bytes]);
                match read_cursor.extract_ushort() {
                    Ok(code) if code == _OACK => Ok(OACK {
                        datagram_stream: DatagramStream::new(self.local_socket, remote_address),
                        read_buffer: self.read_buffer,
                        write_buffer: self.write_buffer,
                        read_bytes,
                    }),
                    Ok(code) if code == _ERR => {
                        let error_code = read_cursor.extract_ushort().unwrap();
                        let message = read_cursor.extract_string().unwrap();
                        Err(TFTPClientError::ClientError(error_code, message))
                    }
                    Ok(_code) => Err(TFTPClientError::UnexpectedData(
                        self.read_buffer[..read_bytes].to_vec(),
                    )),
                    Err(parse_error) => {
                        Err(TFTPClientError::ParseError(format!("{parse_error:?}")))
                    }
                }
            }
            Ok(Ok((read_bytes, remote_address))) => Err(TFTPClientError::UnexpectedPeer(
                remote_address.ip(),
                self.read_buffer[..read_bytes].to_vec(),
            )),
            Ok(Err(error)) => Err(TFTPClientError::IO(error)),
            Err(_timeout_error) => Err(TFTPClientError::Timeout(SentReadRequestWithOpts {
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

pub(crate) struct SentPlainReadRequest {
    file_name: String,
    local_socket: UdpSocket,
    remote_addr: SocketAddr,
    read_buffer: [u8; _BUFFER_SIZE],
    write_buffer: [u8; _BUFFER_SIZE],
    sent_bytes: usize,
}

impl fmt::Debug for SentPlainReadRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:?} => {} {}",
            self.local_socket, self.remote_addr, self.file_name
        )
    }
}

impl SentPlainReadRequest {
    pub(crate) async fn read_next(
        mut self,
        read_timeout: usize,
    ) -> Result<Block, TFTPClientError<Self>> {
        let duration = time::Duration::from_secs(read_timeout as u64);
        let read_future = self.local_socket.recv_from(&mut self.read_buffer);
        match tokio::time::timeout(duration, read_future).await {
            Ok(Ok((read_bytes, remote_address)))
                if remote_address.ip() == self.remote_addr.ip() =>
            {
                let mut read_cursor = ReadCursor::new(&mut self.read_buffer[..read_bytes]);
                match read_cursor.extract_ushort() {
                    Ok(code) if code == _DATA => Ok(Block {
                        datagram_stream: DatagramStream::new(self.local_socket, remote_address),
                        read_buffer: self.read_buffer,
                        write_buffer: self.write_buffer,
                        read_bytes,
                    }),
                    Ok(code) if code == _ERR => {
                        let error_code = read_cursor.extract_ushort().unwrap();
                        let message = read_cursor.extract_string().unwrap();
                        Err(TFTPClientError::ClientError(error_code, message))
                    }
                    Ok(_code) => Err(TFTPClientError::UnexpectedData(
                        self.read_buffer[..read_bytes].to_vec(),
                    )),
                    Err(parse_error) => {
                        Err(TFTPClientError::ParseError(format!("{parse_error:?}")))
                    }
                }
            }
            Ok(Ok((read_bytes, remote_address))) => Err(TFTPClientError::UnexpectedPeer(
                remote_address.ip(),
                self.read_buffer[..read_bytes].to_vec(),
            )),
            Ok(Err(error)) => Err(TFTPClientError::IO(error)),
            Err(_timeout_error) => Err(TFTPClientError::Timeout(SentPlainReadRequest {
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

pub(crate) struct OACK {
    pub(crate) datagram_stream: DatagramStream,
    read_buffer: [u8; _BUFFER_SIZE],
    write_buffer: [u8; _BUFFER_SIZE],
    read_bytes: usize,
}

impl fmt::Debug for OACK {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "OACK")
    }
}

impl OACK {
    pub(crate) fn fields(&self) -> HashMap<String, String> {
        let mut fields: HashMap<String, String> = HashMap::new();
        let mut cursor = ReadCursor::new(&self.read_buffer[2..self.read_bytes]);
        while let Ok(option) = cursor.extract_string() {
            fields.insert(option, cursor.extract_string().unwrap());
        }
        fields
    }
    pub(crate) async fn acknowledge(mut self) -> Result<SentACK, TFTPClientError<Self>> {
        let mut write_cursor = WriteCursor::new(&mut self.write_buffer);
        _ = write_cursor.put_ushort(_ACK).unwrap();
        let buffer_size = write_cursor.put_ushort(0u16).unwrap();
        self.datagram_stream
            .send(&self.write_buffer[..buffer_size])
            .await
            .or_else(|error| Err(TFTPClientError::IO(error)))?;
        Ok(SentACK {
            datagram_stream: self.datagram_stream,
            read_buffer: self.read_buffer,
            write_buffer: self.write_buffer,
            buffer_size,
        })
    }
}

pub(crate) struct Block {
    datagram_stream: DatagramStream,
    read_buffer: [u8; _BUFFER_SIZE],
    write_buffer: [u8; _BUFFER_SIZE],
    read_bytes: usize,
}

impl fmt::Debug for Block {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Block size [{}]", self.read_bytes - _U16_SIZE)
    }
}

impl Block {
    pub(crate) fn data(&self) -> &[u8] {
        &self.read_buffer[_U16_SIZE * 2..self.read_bytes]
    }
    async fn acknowledge(mut self) -> Result<SentACK, TFTPClientError<Self>> {
        let mut write_cursor = WriteCursor::new(&mut self.write_buffer);
        _ = write_cursor.put_ushort(_ACK).unwrap();
        let block_num = u16::from_be_bytes([self.read_buffer[2], self.read_buffer[3]]);
        let buffer_size = write_cursor.put_ushort(block_num).unwrap();
        self.datagram_stream
            .send(&self.write_buffer[..buffer_size])
            .await
            .unwrap();
        Ok(SentACK {
            datagram_stream: self.datagram_stream,
            read_buffer: self.read_buffer,
            write_buffer: self.write_buffer,
            buffer_size,
        })
    }

    pub(crate) async fn send_error(
        mut self,
        code: u16,
        message: &str,
    ) -> Result<SentError, TFTPClientError<Self>> {
        let mut write_cursor = WriteCursor::new(&mut self.write_buffer);
        _ = write_cursor.put_ushort(_ERR).unwrap();
        _ = write_cursor.put_ushort(code).unwrap();
        let buffer_size = write_cursor.put_string(message).unwrap();
        self.datagram_stream
            .send(&self.write_buffer[..buffer_size])
            .await
            .unwrap();
        Ok(SentError {
            datagram_stream: self.datagram_stream,
            read_buffer: self.read_buffer,
            write_buffer: self.write_buffer,
            write_bytes: buffer_size,
        })
    }
}

pub(crate) struct SentACK {
    datagram_stream: DatagramStream,
    read_buffer: [u8; _BUFFER_SIZE],
    write_buffer: [u8; _BUFFER_SIZE],
    buffer_size: usize,
}

impl fmt::Debug for SentACK {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "<SentACK {:?} ({})>",
            self.datagram_stream, self.buffer_size
        )
    }
}

impl SentACK {
    pub(crate) async fn read_next(
        mut self,
        read_timeout: usize,
    ) -> Result<Block, TFTPClientError<Self>> {
        let duration = time::Duration::from_secs(read_timeout as u64);
        let read_future = self
            .datagram_stream
            .recv(&mut self.read_buffer, read_timeout, 4);
        match tokio::time::timeout(duration, read_future).await {
            Ok(Ok(read_bytes)) => {
                let mut read_cursor = ReadCursor::new(&mut self.read_buffer[..read_bytes]);
                match read_cursor.extract_ushort() {
                    Ok(code) if code == _DATA => Ok(Block {
                        datagram_stream: self.datagram_stream,
                        read_buffer: self.read_buffer,
                        write_buffer: self.write_buffer,
                        read_bytes,
                    }),
                    Ok(code) if code == _ERR => {
                        let error_code = read_cursor.extract_ushort().unwrap();
                        let message = read_cursor.extract_string().unwrap();
                        Err(TFTPClientError::ClientError(error_code, message))
                    }
                    Ok(_code) => Err(TFTPClientError::UnexpectedData(
                        self.read_buffer[..read_bytes].to_vec(),
                    )),
                    Err(parse_error) => {
                        Err(TFTPClientError::ParseError(format!("{parse_error:?}")))
                    }
                }
            }
            Ok(Err(err)) => Err(TFTPClientError::IO(err)),
            Err(_timeout_error) => Err(TFTPClientError::Timeout(self)),
        }
    }
}

#[allow(dead_code)]
pub(crate) struct SentError {
    datagram_stream: DatagramStream,
    read_buffer: [u8; _BUFFER_SIZE],
    write_buffer: [u8; _BUFFER_SIZE],
    write_bytes: usize,
}

impl fmt::Debug for SentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "<SentError {:?} ({})>",
            self.datagram_stream, self.write_bytes
        )
    }
}

impl SentError {
    pub(crate) async fn read_some(&mut self, read_timeout: usize) -> io::Result<&[u8]> {
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

pub(crate) async fn download(client: TFTPClient, file: &str) -> Result<Vec<u8>, DownloadError> {
    let default_timeout: usize = 5;
    let default_block_size: usize = 512;
    let mut read_data: Vec<u8> = Vec::new();
    let sent_request = client
        .send_plain_read_request(file)
        .await
        .map_err(|error| DownloadError::from(error))?;
    let mut block: Option<Block> = Some(
        sent_request
            .read_next(default_timeout)
            .await
            .map_err(|error| DownloadError::from(error))?,
    );
    while let Some(_block) = block.take() {
        let recv_block_len = _block.data().len();
        read_data.extend(_block.data());
        let sent_ack = _block
            .acknowledge()
            .await
            .map_err(|error| DownloadError::from(error))?;
        if recv_block_len == default_block_size {
            block = Some(
                sent_ack
                    .read_next(default_timeout)
                    .await
                    .map_err(|error| DownloadError::from(error))?,
            );
        }
    }
    Ok(read_data)
}

#[derive(Debug)]
pub(crate) struct DownloadError(String);

impl<T: fmt::Debug> From<TFTPClientError<T>> for DownloadError {
    fn from(value: TFTPClientError<T>) -> Self {
        match value {
            TFTPClientError::Timeout(msg) => DownloadError(format!("{:?}", msg)),
            error => DownloadError(error.to_string()),
        }
    }
}

impl fmt::Display for DownloadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.clone())
    }
}

impl From<io::Error> for DownloadError {
    fn from(value: io::Error) -> Self {
        DownloadError(value.to_string())
    }
}

struct ReadCursor<'a> {
    datagram: &'a [u8],
    index: usize,
}

impl<'a> ReadCursor<'a> {
    fn new(datagram: &'a [u8]) -> Self {
        Self { datagram, index: 0 }
    }

    fn extract_ushort(&mut self) -> Result<u16, ParseError> {
        let end_index = self.index + 2;
        if end_index > self.datagram.len() {
            return Err(ParseError::NotEnoughData);
        }
        let result = u16::from_be_bytes([self.datagram[self.index], self.datagram[self.index + 1]]);
        self.index = end_index;
        Ok(result)
    }

    fn extract_string(&mut self) -> Result<String, ParseError> {
        if self.index >= self.datagram.len() {
            return Err(ParseError::NotEnoughData);
        };
        if let Some(relative_null_index) = self.datagram[self.index..].iter().position(|&b| b == 0)
        {
            let absolute_null_index = self.index + relative_null_index;
            match String::from_utf8(self.datagram[self.index..absolute_null_index].to_vec()) {
                Ok(string) => {
                    self.index = absolute_null_index + 1;
                    Ok(string)
                }
                Err(_) => Err(ParseError::generic("Can't parse UTF-8")),
            }
        } else {
            Err(ParseError::generic("Null-terminated string is not found"))
        }
    }
}

#[allow(dead_code)]
#[derive(Debug)]
enum ParseError {
    Generic(String),
    NotEnoughData,
}

impl ParseError {
    pub fn generic<T: Into<String>>(msg: T) -> Self {
        ParseError::Generic(msg.into())
    }
}

struct WriteCursor<'a> {
    buffer: &'a mut [u8],
    offset: usize,
}

impl<'a> WriteCursor<'a> {
    fn new(buffer: &'a mut [u8]) -> Self {
        Self { buffer, offset: 0 }
    }

    fn put_ushort(&mut self, value: u16) -> Result<usize, BufferError> {
        let end_index = self.offset + 2;
        if end_index > self.buffer.len() {
            return Err(BufferError::new("Too little data left to write u16"));
        }
        self.buffer[self.offset..end_index].copy_from_slice(&value.to_be_bytes());
        self.offset = end_index;
        Ok(self.offset)
    }

    fn put_string(&mut self, string: &str) -> Result<usize, BufferError> {
        let string_size = string.len();
        let end_index = self.offset + string_size + 1;
        if end_index > self.buffer.len() {
            return Err(BufferError::new(&format!(
                "Too little data left to write a string {string_size} bytes size"
            )));
        }
        self.buffer[self.offset..end_index - 1].copy_from_slice(string.as_bytes());
        self.buffer[end_index] = 0x0;
        self.offset = end_index;
        Ok(self.offset)
    }
}

#[derive(Debug, PartialEq)]
struct BufferError {
    message: String,
}

impl fmt::Display for BufferError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<{}>", self.message)
    }
}

impl std::error::Error for BufferError {}

impl BufferError {
    fn new(message: &str) -> Self {
        Self {
            message: message.to_string(),
        }
    }
}
