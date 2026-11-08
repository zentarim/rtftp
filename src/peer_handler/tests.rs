use crate::datagram_stream::DatagramStream;
use crate::fs::{OpenedFile, RootKind};
use crate::messages::OptionsAcknowledge;
use crate::options::{AckTimeout, Rollover};
use crate::peer_handler::{
    ACK, AckClass, DATA, Window, classify_ack, roots_for_peer, send_file, send_oack_reliably,
};
use crate::tests_common::mk_tmp;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use std::{fmt, fs, io};
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
    async fn read_to(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let slice_length = buffer.len().min(self.buffer.len() - self.offset);
        eprintln!("{}: {} {}", self, slice_length, buffer.len());
        buffer[..slice_length]
            .copy_from_slice(&self.buffer[self.offset..self.offset + slice_length]);
        self.offset += slice_length;
        Ok(slice_length)
    }

    async fn get_size(&mut self) -> io::Result<usize> {
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
struct DownloadError(String);

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

async fn download_blocks(
    datagram_stream: &DatagramStream,
    block_size: u16,
    window_size: u16,
) -> Result<(Vec<u8>, Vec<u16>), DownloadError> {
    let mut read_data: Vec<u8> = Vec::new();
    let mut block_numbers: Vec<u16> = Vec::new();
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
            let opcode = u16::from_be_bytes(buffer[..2].try_into().unwrap());
            if opcode != DATA {
                return Err(DownloadError(format!("Wrong opcode received: {opcode}")));
            }
            last_received_block_index = u16::from_be_bytes(buffer[2..4].try_into().unwrap());
            block_numbers.push(last_received_block_index);
            read_data.extend_from_slice(&buffer[block_header_size..received_bytes]);
            if received_bytes < expected_message_size {
                done = true;
                break;
            }
        }
        buffer[0] = 0;
        buffer[1] = ACK as u8;
        buffer[2..4].copy_from_slice(&last_received_block_index.to_be_bytes());
        datagram_stream.send(&buffer[..block_header_size]).await?;
    }
    Ok((read_data, block_numbers))
}

#[tokio::test(flavor = "current_thread")]
async fn send_aligned_data() {
    let test_data = generate_data(100);
    let opened_file = VirtualOpenedFile::new(test_data.clone());
    let (server_stream, client_stream) = make_streams().await;
    let ack_timeout = AckTimeout::default();
    let block_size = 100;
    let window_size = 1;
    let window = Window::new(block_size, window_size, Rollover::default());
    let mut buffer = vec![0; 1024];
    let send_coro = send_file(
        opened_file,
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
    let window = Window::new(block_size, window_size, Rollover::default());
    let mut buffer = vec![0; 1024];
    let send_coro = send_file(
        opened_file,
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
    let window = Window::new(block_size, window_size, Rollover::default());
    let mut buffer = vec![0; 1024];
    let send_coro = send_file(
        opened_file,
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
    let window = Window::new(block_size, window_size, Rollover::default());
    let mut buffer = vec![0; 1024];
    let send_coro = send_file(
        opened_file,
        &server_stream,
        window,
        ack_timeout,
        &mut buffer,
    );
    let recv_coro = download_stream(&client_stream, block_size, window_size);
    let (_send_result, recv_result) = join!(send_coro, recv_coro);
    assert_eq!(recv_result.unwrap(), test_data);
}

fn local_root_ends_with(roots: &[RootKind], directory_name: &str) -> bool {
    let suffix = format!("{directory_name}\">");
    roots.iter().any(|root| match root {
        RootKind::Local(local_root) => local_root.to_string().ends_with(&suffix),
        RootKind::Remote(_) => false,
    })
}

#[tokio::test(flavor = "current_thread")]
async fn absent_nbd_config_uses_default_root() {
    let tftp_root = mk_tmp(absent_nbd_config_uses_default_root);
    let roots = roots_for_peer(&tftp_root, "10.0.0.1").await;
    assert!(local_root_ends_with(&roots, "10.0.0.1"));
    assert!(local_root_ends_with(&roots, "default"));
}

#[tokio::test(flavor = "current_thread")]
async fn unusable_nbd_config_skips_default_root() {
    let tftp_root = mk_tmp(unusable_nbd_config_skips_default_root);
    fs::write(
        tftp_root.join("10.0.0.1.nbd"),
        r#"{"url":"http://127.0.0.1/disk","mounts":[],"tftp_root":"/boot"}"#,
    )
    .unwrap();
    let roots = roots_for_peer(&tftp_root, "10.0.0.1").await;
    assert!(local_root_ends_with(&roots, "10.0.0.1"));
    assert!(!local_root_ends_with(&roots, "default"));
}

#[test]
fn window_size_matches_buffer_count() {
    let window = Window::new(16, 3, Rollover::default());
    assert_eq!(window.size(), 3);
}

#[test]
fn ack_inside_window_across_block_wrap() {
    let window_first = 65534u64;
    let count = 4;
    let rollover = Rollover::default();
    assert_eq!(
        classify_ack(65534, window_first, count, rollover),
        AckClass::InWindow(65534)
    );
    assert_eq!(
        classify_ack(65535, window_first, count, rollover),
        AckClass::InWindow(65535)
    );
    assert_eq!(
        classify_ack(0, window_first, count, rollover),
        AckClass::InWindow(65536)
    );
    assert_eq!(
        classify_ack(1, window_first, count, rollover),
        AckClass::InWindow(65537)
    );
    assert_eq!(
        classify_ack(2, window_first, count, rollover),
        AckClass::Other
    );
    assert_eq!(
        classify_ack(65533, window_first, count, rollover),
        AckClass::Previous
    );
}

#[test]
fn ack_inside_window_across_rollover_one() {
    let rollover =
        Rollover::find_in(&HashMap::from([("rollover".to_string(), "1".to_string())])).unwrap();
    let window_first = 65534u64;
    let count = 4;
    assert_eq!(
        classify_ack(1, window_first, count, rollover),
        AckClass::InWindow(65536)
    );
    assert_eq!(
        classify_ack(2, window_first, count, rollover),
        AckClass::InWindow(65537)
    );
    assert_eq!(
        classify_ack(0, window_first, count, rollover),
        AckClass::Other
    );
    assert_eq!(
        classify_ack(3, window_first, count, rollover),
        AckClass::Other
    );
    assert_eq!(
        classify_ack(65533, window_first, count, rollover),
        AckClass::Previous
    );
    assert_eq!(classify_ack(65535, 65536, 1, rollover), AckClass::Previous);
    assert_eq!(classify_ack(0, 1, 1, rollover), AckClass::Previous);
}

#[tokio::test(flavor = "current_thread")]
async fn window_slots_stay_distinct_across_sequence_wrap() {
    let mut window = Window::new(4, 4, Rollover::default());
    let data: Vec<u8> = (0..16).collect();
    let mut opened_file = VirtualOpenedFile::new(data.clone());
    for seq in 65533u64..65537 {
        window
            .push_block(&mut opened_file, seq, seq as u16)
            .await
            .unwrap();
    }
    let expected_slots = [1usize, 2, 3, 0];
    for (offset, seq) in (65533u64..65537).enumerate() {
        let slot = window.slot(seq);
        assert_eq!(slot, expected_slots[offset]);
        let buffer = &window.buffers[slot];
        let block_number = u16::from_be_bytes(buffer[2..4].try_into().unwrap());
        assert_eq!(block_number, seq as u16);
        let payload_start = offset * 4;
        assert_eq!(&buffer[4..8], &data[payload_start..payload_start + 4]);
    }
}

async fn recv_data_block(
    datagram_stream: &DatagramStream,
    buffer: &mut [u8],
) -> Result<(u16, Vec<u8>), DownloadError> {
    let received_bytes =
        match timeout(Duration::from_secs(2), datagram_stream.recv(buffer, 4)).await {
            Ok(result) => result?,
            Err(_elapsed) => return Err(DownloadError("timeout".to_string())),
        };
    let opcode = u16::from_be_bytes(buffer[..2].try_into().unwrap());
    if opcode != DATA {
        return Err(DownloadError(format!("Wrong opcode received: {opcode}")));
    }
    let block_index = u16::from_be_bytes(buffer[2..4].try_into().unwrap());
    Ok((block_index, buffer[4..received_bytes].to_vec()))
}

async fn send_ack(datagram_stream: &DatagramStream, block_index: u16) -> io::Result<()> {
    let mut buffer = [0u8; 4];
    buffer[0] = 0;
    buffer[1] = ACK as u8;
    let block_bytes = block_index.to_be_bytes();
    buffer[2..4].copy_from_slice(&block_bytes);
    datagram_stream.send(&buffer).await
}

#[tokio::test(flavor = "current_thread")]
async fn duplicate_previous_ack_retransmits() {
    let test_data = generate_data(50);
    let opened_file = VirtualOpenedFile::new(test_data.clone());
    let (server_stream, client_stream) = make_streams().await;
    let ack_timeout = AckTimeout::default();
    let block_size = 100;
    let window = Window::new(block_size, 1, Rollover::default());
    let mut buffer = vec![0; 1024];
    let send_coro = send_file(
        opened_file,
        &server_stream,
        window,
        ack_timeout,
        &mut buffer,
    );
    let recv_coro = async {
        let mut read_buffer = vec![0u8; 104];
        let (first_index, first_payload) =
            recv_data_block(&client_stream, &mut read_buffer).await?;
        send_ack(&client_stream, 0).await?;
        let (second_index, second_payload) =
            recv_data_block(&client_stream, &mut read_buffer).await?;
        send_ack(&client_stream, 1).await?;
        assert_eq!(first_index, 1);
        assert_eq!(second_index, 1);
        assert_eq!(first_payload, second_payload);
        Ok::<Vec<u8>, DownloadError>(first_payload)
    };
    let transfer = timeout(Duration::from_secs(2), async {
        join!(send_coro, recv_coro)
    })
    .await;
    let (send_result, recv_result) = transfer.expect("transfer timed out");
    assert!(send_result.is_ok(), "{send_result:?}");
    assert_eq!(recv_result.unwrap(), test_data);
}

#[tokio::test(flavor = "current_thread")]
async fn ack_beyond_sent_blocks_is_ignored() {
    let test_data = generate_data(50);
    let opened_file = VirtualOpenedFile::new(test_data.clone());
    let (server_stream, client_stream) = make_streams().await;
    let ack_timeout = AckTimeout::default();
    let block_size = 100;
    let window = Window::new(block_size, 1, Rollover::default());
    let mut buffer = vec![0; 1024];
    let send_coro = send_file(
        opened_file,
        &server_stream,
        window,
        ack_timeout,
        &mut buffer,
    );
    let recv_coro = async {
        let mut read_buffer = vec![0u8; 104];
        let (block_index, payload) = recv_data_block(&client_stream, &mut read_buffer).await?;
        assert_eq!(block_index, 1);
        send_ack(&client_stream, 9).await?;
        send_ack(&client_stream, 1).await?;
        Ok::<Vec<u8>, DownloadError>(payload)
    };
    let transfer = timeout(Duration::from_secs(2), async {
        join!(send_coro, recv_coro)
    })
    .await;
    let (send_result, recv_result) = transfer.expect("transfer timed out");
    assert!(send_result.is_ok(), "{send_result:?}");
    assert_eq!(recv_result.unwrap(), test_data);
}

#[tokio::test(flavor = "current_thread")]
async fn oack_retransmit_after_dirty_receive_buffer() {
    let options = HashMap::from([(String::from("timeout"), String::from("1"))]);
    let (ack_timeout, pairs) = AckTimeout::negotiate(&options);
    let mut oack = OptionsAcknowledge::new();
    for pair in pairs {
        oack.push(pair);
    }

    let (server_stream, client_stream) = make_streams().await;
    let mut buffer = vec![0xFFu8; 512];
    let send_coro = send_oack_reliably(&oack, &server_stream, &ack_timeout, &mut buffer);
    let recv_coro = async {
        let mut read_buffer = vec![0u8; 512];
        let first_size = match timeout(
            Duration::from_secs(2),
            client_stream.recv(&mut read_buffer, 4),
        )
        .await
        {
            Ok(result) => result?,
            Err(_elapsed) => return Err(DownloadError("timeout".to_string())),
        };
        let first_oack = read_buffer[..first_size].to_vec();
        let opcode = u16::from_be_bytes(first_oack[..2].try_into().unwrap());
        assert_eq!(opcode, 6);
        let runt = [0xABu8];
        client_stream.send(&runt).await?;
        let second_size = match timeout(
            Duration::from_secs(3),
            client_stream.recv(&mut read_buffer, 4),
        )
        .await
        {
            Ok(result) => result?,
            Err(_elapsed) => return Err(DownloadError("timeout".to_string())),
        };
        assert_eq!(&read_buffer[..second_size], first_oack.as_slice());
        send_ack(&client_stream, 0).await?;
        Ok::<(), DownloadError>(())
    };
    let transfer = timeout(Duration::from_secs(5), async {
        join!(send_coro, recv_coro)
    })
    .await;
    let (send_result, recv_result) = transfer.expect("oack negotiation timed out");
    assert!(send_result.is_ok(), "{send_result:?}");
    assert!(recv_result.is_ok(), "{recv_result:?}");
}

#[tokio::test(flavor = "current_thread")]
async fn utimeout_shortens_oack_retransmit() {
    let options = HashMap::from([(String::from("utimeout"), String::from("50000"))]);
    let (ack_timeout, pairs) = AckTimeout::negotiate(&options);
    let mut oack = OptionsAcknowledge::new();
    for pair in pairs {
        oack.push(pair);
    }

    let (server_stream, client_stream) = make_streams().await;
    let mut buffer = vec![0xFFu8; 512];
    let send_coro = send_oack_reliably(&oack, &server_stream, &ack_timeout, &mut buffer);
    let recv_coro = async {
        let mut read_buffer = vec![0u8; 512];
        let first_size = match timeout(
            Duration::from_secs(2),
            client_stream.recv(&mut read_buffer, 4),
        )
        .await
        {
            Ok(result) => result?,
            Err(_elapsed) => return Err(DownloadError("timeout".to_string())),
        };
        let first_oack = read_buffer[..first_size].to_vec();
        let started = Instant::now();
        let second_size = match timeout(
            Duration::from_millis(500),
            client_stream.recv(&mut read_buffer, 4),
        )
        .await
        {
            Ok(result) => result?,
            Err(_elapsed) => return Err(DownloadError("timeout".to_string())),
        };
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_millis(500),
            "retransmit took {elapsed:?}"
        );
        assert_eq!(&read_buffer[..second_size], first_oack.as_slice());
        send_ack(&client_stream, 0).await?;
        Ok::<(), DownloadError>(())
    };
    let transfer = timeout(Duration::from_secs(2), async {
        join!(send_coro, recv_coro)
    })
    .await;
    let (send_result, recv_result) = transfer.expect("oack negotiation timed out");
    assert!(send_result.is_ok(), "{send_result:?}");
    assert!(recv_result.is_ok(), "{recv_result:?}");
}

#[tokio::test(flavor = "current_thread")]
async fn utimeout_backoff_doubles_oack_retransmit() {
    let options = HashMap::from([(String::from("utimeout"), String::from("50000"))]);
    let (ack_timeout, pairs) = AckTimeout::negotiate(&options);
    let mut oack = OptionsAcknowledge::new();
    for pair in pairs {
        oack.push(pair);
    }

    let (server_stream, client_stream) = make_streams().await;
    let mut buffer = vec![0xFFu8; 512];
    let send_coro = send_oack_reliably(&oack, &server_stream, &ack_timeout, &mut buffer);
    let recv_coro = async {
        let mut read_buffer = vec![0u8; 512];
        let first_size = match timeout(
            Duration::from_secs(2),
            client_stream.recv(&mut read_buffer, 4),
        )
        .await
        {
            Ok(result) => result?,
            Err(_elapsed) => return Err(DownloadError("timeout".to_string())),
        };
        let first_oack = read_buffer[..first_size].to_vec();

        let second_started = Instant::now();
        let second_size = match timeout(
            Duration::from_millis(200),
            client_stream.recv(&mut read_buffer, 4),
        )
        .await
        {
            Ok(result) => result?,
            Err(_elapsed) => return Err(DownloadError("timeout".to_string())),
        };
        let first_gap = second_started.elapsed();
        assert!(
            first_gap < Duration::from_millis(200),
            "first retransmit took {first_gap:?}"
        );
        assert_eq!(&read_buffer[..second_size], first_oack.as_slice());

        let third_started = Instant::now();
        let third_size = match timeout(
            Duration::from_millis(400),
            client_stream.recv(&mut read_buffer, 4),
        )
        .await
        {
            Ok(result) => result?,
            Err(_elapsed) => return Err(DownloadError("timeout".to_string())),
        };
        let second_gap = third_started.elapsed();
        assert!(
            second_gap > Duration::from_millis(80) && second_gap < Duration::from_millis(400),
            "second retransmit took {second_gap:?}"
        );
        assert_eq!(&read_buffer[..third_size], first_oack.as_slice());
        send_ack(&client_stream, 0).await?;
        Ok::<(), DownloadError>(())
    };
    let transfer = timeout(Duration::from_secs(2), async {
        join!(send_coro, recv_coro)
    })
    .await;
    let (send_result, recv_result) = transfer.expect("oack negotiation timed out");
    assert!(send_result.is_ok(), "{send_result:?}");
    assert!(recv_result.is_ok(), "{recv_result:?}");
}

async fn transfer_across_block_wrap(rollover: Rollover, after_wrap: u16) {
    let block_size = 8;
    let window_size = 7;
    let test_data = generate_data(block_size as usize * 65540 + 3);
    let opened_file = VirtualOpenedFile::new(test_data.clone());
    let (server_stream, client_stream) = make_streams().await;
    let ack_timeout = AckTimeout::default();
    let window = Window::new(block_size, window_size, rollover);
    let mut buffer = vec![0; 1024];
    let send_coro = send_file(
        opened_file,
        &server_stream,
        window,
        ack_timeout,
        &mut buffer,
    );
    let recv_coro = download_blocks(&client_stream, block_size, window_size);
    let transfer = timeout(Duration::from_secs(60), async {
        join!(send_coro, recv_coro)
    })
    .await;
    let (send_result, recv_result) = transfer.expect("transfer timed out");
    assert!(send_result.is_ok(), "{send_result:?}");
    let (data, block_numbers) = recv_result.unwrap();
    assert_eq!(data, test_data);
    let wrap_at = block_numbers
        .iter()
        .position(|number| *number == 65535)
        .expect("block 65535 was sent");
    assert_eq!(block_numbers[wrap_at + 1], after_wrap);
    if after_wrap == 1 {
        assert!(!block_numbers.contains(&0));
    }
}

#[tokio::test(flavor = "current_thread")]
async fn rollover_zero_wraps_block_numbers_to_zero() {
    transfer_across_block_wrap(Rollover::default(), 0).await;
}

#[tokio::test(flavor = "current_thread")]
async fn rollover_one_wraps_block_numbers_to_one() {
    let rollover =
        Rollover::find_in(&HashMap::from([("rollover".to_string(), "1".to_string())])).unwrap();
    transfer_across_block_wrap(rollover, 1).await;
}
