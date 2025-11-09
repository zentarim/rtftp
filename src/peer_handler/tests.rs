use crate::datagram_stream::DatagramStream;
use crate::fs::{FileError, OpenedFile};
use crate::options::AckTimeout;
use crate::peer_handler::{ACK, DATA, Window, send_file};
use std::time::Duration;
use std::{fmt, io};
use tokio::join;
use tokio::net::UdpSocket;
use tokio::time::timeout;

fn xorshift64star(index: usize, seed: usize) -> usize {
    let mut x = index ^ seed;
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    (x.wrapping_mul(0x2545F4914F6CDD1D)) >> 56
}

fn weak_pseudo_random_data(len: usize, seed: usize) -> Vec<u8> {
    (0..len).map(|i| xorshift64star(i, seed) as u8).collect()
}

fn generate_data(size: usize) -> Vec<u8> {
    weak_pseudo_random_data(size, size)
}

struct VirtualOpenedFile {
    buffer: Vec<u8>,
    offset: usize,
}

impl VirtualOpenedFile {
    fn new(buffer: Vec<u8>) -> Self {
        Self { buffer, offset: 0 }
    }
}

impl fmt::Display for VirtualOpenedFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "VirtualOpenedFile size {} [{}]",
            self.buffer.len(),
            self.offset
        )
    }
}

impl fmt::Debug for VirtualOpenedFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "VirtualOpenedFile size {} [{}]",
            self.buffer.len(),
            self.offset
        )
    }
}

impl OpenedFile for VirtualOpenedFile {
    fn read_to(&mut self, buffer: &mut [u8]) -> Result<usize, FileError> {
        let slice_length = buffer.len().min(self.buffer.len() - self.offset);
        eprintln!("{}: {} {}", self, slice_length, buffer.len());
        buffer[..slice_length]
            .copy_from_slice(&self.buffer[self.offset..self.offset + slice_length]);
        self.offset += slice_length;
        Ok(slice_length)
    }

    fn get_size(&mut self) -> Result<usize, FileError> {
        Ok(self.buffer.len())
    }
}

async fn make_streams() -> (DatagramStream, DatagramStream) {
    let server_socket = UdpSocket::bind("127.0.0.10:0").await.unwrap();
    let client_socket = UdpSocket::bind("127.0.0.20:0").await.unwrap();
    let server_address = server_socket.local_addr().unwrap();
    let client_address = client_socket.local_addr().unwrap();
    (
        DatagramStream::new(server_socket, client_address),
        DatagramStream::new(client_socket, server_address),
    )
}

#[derive(Debug)]
pub(crate) struct DownloadError(String);

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

async fn download_stream(
    datagram_stream: &DatagramStream,
    block_size: u16,
    window_size: u16,
) -> Result<Vec<u8>, DownloadError> {
    let mut read_data: Vec<u8> = Vec::new();
    let block_header_size = 4;
    let expected_message_size = block_size as usize + block_header_size;
    let mut buffer = vec![0u8; expected_message_size];
    let mut last_received_block_index: u16 = 0;
    let mut done = false;
    while !done {
        for _ in 0..window_size {
            let recv_fut = datagram_stream.recv(&mut buffer, block_header_size);
            let received_bytes = match timeout(Duration::from_secs(5), recv_fut).await {
                Ok(result) => result?,
                Err(_timeout) => return Err(DownloadError("timeout".to_string())),
            };
            let opcode = ((buffer[0] as u16) << 8) | buffer[1] as u16;
            if opcode != DATA {
                return Err(DownloadError("Wrong opcode received: {opcode}".to_string()));
            }
            last_received_block_index = ((buffer[2] as u16) << 8) | (buffer[3] as u16);
            read_data.extend_from_slice(&buffer[block_header_size..received_bytes]);
            if received_bytes < expected_message_size {
                eprintln!(
                    "Received {received_bytes}, expected {expected_message_size} bytes. Break"
                );
                done = true;
                break;
            }
        }

        buffer[0] = 0;
        buffer[1] = ACK as u8;
        buffer[2] = (last_received_block_index >> 8) as u8;
        buffer[3] = (last_received_block_index & 0xFF) as u8;
        datagram_stream.send(&buffer[..block_header_size]).await?;
        eprintln!("Sent ACK for {}", last_received_block_index);
    }
    eprintln!("Done");
    Ok(read_data)
}

#[tokio::test(flavor = "current_thread")]
async fn send_aligned_data() {
    let test_data = generate_data(100);
    let opened_file = VirtualOpenedFile::new(test_data.clone());
    let (server_stream, client_stream) = make_streams().await;
    let ack_timeout = AckTimeout::default();
    let block_size = 100;
    let window_size = 1;
    let window = Window::new(block_size, window_size);
    let mut buffer = vec![0; 1024];
    let send_coro = send_file(
        Box::new(opened_file),
        &server_stream,
        window,
        ack_timeout,
        &mut buffer,
    );
    let recv_coro = download_stream(&client_stream, block_size, window_size);
    let (_send_result, recv_result) = join!(send_coro, recv_coro);
    assert_eq!(recv_result.unwrap(), test_data);
}
#[tokio::test(flavor = "current_thread")]
async fn send_unaligned_data() {
    let test_data = generate_data(512);
    let opened_file = VirtualOpenedFile::new(test_data.clone());
    let (server_stream, client_stream) = make_streams().await;
    let ack_timeout = AckTimeout::default();
    let block_size = 100;
    let window_size = 1;
    let window = Window::new(block_size, window_size);
    let mut buffer = vec![0; 1024];
    let send_coro = send_file(
        Box::new(opened_file),
        &server_stream,
        window,
        ack_timeout,
        &mut buffer,
    );
    let recv_coro = download_stream(&client_stream, block_size, window_size);
    let (_send_result, recv_result) = join!(send_coro, recv_coro);
    assert_eq!(recv_result.unwrap(), test_data);
}
#[tokio::test(flavor = "current_thread")]
async fn send_aligned_data_windowed() {
    let test_data = generate_data(100);
    let opened_file = VirtualOpenedFile::new(test_data.clone());
    let (server_stream, client_stream) = make_streams().await;
    let ack_timeout = AckTimeout::default();
    let block_size = 100;
    let window_size = 5;
    let window = Window::new(block_size, window_size);
    let mut buffer = vec![0; 1024];
    let send_coro = send_file(
        Box::new(opened_file),
        &server_stream,
        window,
        ack_timeout,
        &mut buffer,
    );
    let recv_coro = download_stream(&client_stream, block_size, window_size);
    let (_send_result, recv_result) = join!(send_coro, recv_coro);
    assert_eq!(recv_result.unwrap(), test_data);
}
#[tokio::test(flavor = "current_thread")]
async fn send_unaligned_data_windowed() {
    let test_data = generate_data(512);
    let opened_file = VirtualOpenedFile::new(test_data.clone());
    let (server_stream, client_stream) = make_streams().await;
    let ack_timeout = AckTimeout::default();
    let block_size = 100;
    let window_size = 5;
    let window = Window::new(block_size, window_size);
    let mut buffer = vec![0; 1024];
    let send_coro = send_file(
        Box::new(opened_file),
        &server_stream,
        window,
        ack_timeout,
        &mut buffer,
    );
    let recv_coro = download_stream(&client_stream, block_size, window_size);
    let (_send_result, recv_result) = join!(send_coro, recv_coro);
    assert_eq!(recv_result.unwrap(), test_data);
}
