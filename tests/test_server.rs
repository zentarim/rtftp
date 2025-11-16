use crate::common::{make_payload, mk_tmp, run_nbd_server, start_rtftp};
use serde_json::json;
use std::collections::HashMap;
use std::ffi::CStr;
use std::fs::{File, Permissions, set_permissions};
use std::io::{ErrorKind, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::{fs, time};
use tokio::net::UdpSocket;

use crate::common::client::{TFTPClientError, download, download_window};

mod common;

const _BUFFER_SIZE: usize = 1536;

fn _write_file(path: &PathBuf, data: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut file = File::create(path).unwrap();
    file.write_all(data).unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn send_wrong_request_type() {
    let source_ip = "127.0.0.11";
    let server_dir = mk_tmp(send_wrong_request_type);
    let running_server = start_rtftp(server_dir).await;
    let wrong_code_packet = b"\xAAoctet\x00irrelevant\x00";
    let local_socket = UdpSocket::bind((source_ip, 0)).await.unwrap();
    local_socket
        .send_to(wrong_code_packet, running_server.listen_socket)
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
    let running_server = start_rtftp(server_dir).await;
    let wrong_code_packet = b"\x00\x01email\x00irrelevant\x00";
    let local_socket = UdpSocket::bind((source_ip, 0)).await.unwrap();
    local_socket
        .send_to(wrong_code_packet, running_server.listen_socket)
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
    let running_server = start_rtftp(server_dir).await;
    let client = running_server.open_paired_client(source_ip).await;
    let read_result = download(client, file_name).await;
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
    let running_server = start_rtftp(server_dir).await;
    let client = running_server.open_paired_client(source_ip).await;
    let read_data = download(client, file_name).await.unwrap();
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
    let running_server = start_rtftp(server_dir).await;
    let client = running_server.open_paired_client(source_ip).await;
    let read_data = download(client, &file_name_with_leading_slash)
        .await
        .unwrap();
    assert_eq!(read_data, data);
}

#[tokio::test(flavor = "current_thread")]
async fn attempt_download_nonexisting_file() {
    let arbitrary_source_ip = "127.0.0.11";
    let server_dir = mk_tmp(attempt_download_nonexisting_file);
    let nonexisting_file_name = "nonexisting.file";
    let running_server = start_rtftp(server_dir).await;
    let client = running_server.open_paired_client(arbitrary_source_ip).await;
    let sent_request = client
        .send_plain_read_request(nonexisting_file_name)
        .await
        .unwrap();
    let result = sent_request.read_next(5).await;
    assert!(
        matches!(&result, Err(TFTPClientError::ClientError(0x01, msg)) if msg == "File not found"),
        "Unexpected error {result:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn attempt_download_file_default() {
    let arbitrary_source_ip = "127.0.0.11";
    let server_dir = mk_tmp(attempt_download_file_default);
    let data = make_payload(512);
    let file_name = "file.txt";
    let file = server_dir.join("default").join(file_name);
    _write_file(&file, &data);
    let running_server = start_rtftp(server_dir).await;
    let client = running_server.open_paired_client(arbitrary_source_ip).await;
    let read_data = download(client, &file_name).await.unwrap();
    assert_eq!(read_data, data);
}

#[tokio::test(flavor = "current_thread")]
async fn attempt_download_file_peer_takes_precendence() {
    let arbitrary_source_ip = "127.0.0.11";
    let server_dir = mk_tmp(attempt_download_file_peer_takes_precendence);
    let file_name = "file.txt";
    let default_data = make_payload(512);
    let default_file = server_dir.join("default").join(file_name);
    _write_file(&default_file, &default_data);
    let peer_data = make_payload(768);
    let peer_file = server_dir.join(arbitrary_source_ip).join(file_name);
    _write_file(&peer_file, &peer_data);
    let running_server = start_rtftp(server_dir).await;
    let client = running_server.open_paired_client(arbitrary_source_ip).await;
    let read_data = download(client, &file_name).await.unwrap();
    assert_eq!(read_data, peer_data);
}

#[tokio::test(flavor = "current_thread")]
async fn access_violation() {
    let server_dir = mk_tmp(access_violation);
    let arbitrary_source_ip = "127.0.0.11";
    let arbitrary_file_name = "arbitrary.file";
    let running_server = start_rtftp(server_dir.clone()).await;
    let client = running_server.open_paired_client(arbitrary_source_ip).await;
    set_permissions(&server_dir, Permissions::from_mode(0o055)).unwrap();
    let sent_request = client
        .send_plain_read_request(arbitrary_file_name)
        .await
        .unwrap();
    let result = sent_request.read_next(5).await;
    assert!(
        matches!(&result, Err(TFTPClientError::ClientError(0x02, msg)) if msg == "Access violation"),
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
    let running_server = start_rtftp(server_dir.clone()).await;
    let client = running_server.open_paired_client(source_ip).await;
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
    let running_server = start_rtftp(server_dir.clone()).await;
    let client = running_server.open_paired_client(source_ip).await;
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
    let running_server = start_rtftp(server_dir.clone()).await;
    let client = running_server.open_paired_client(source_ip).await;
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
    let running_server = start_rtftp(server_dir.clone()).await;
    let client = running_server.open_paired_client(source_ip).await;
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
        (Vec::new(), 0u64),
    ];
    let local_read_timeout = 2usize;
    let mut buffer = [0u8; _BUFFER_SIZE];
    for (retry_message, timestamp) in &mut retry_buffers {
        if let Ok(read_bytes) = oack
            .datagram_stream
            .recv(&mut buffer, local_read_timeout, 0)
            .await
        {
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
        retry_buffers[4].0, b"\x00\x05\x00\x00Send timeout occurred\x00",
        "5: Received: {:?}, Expected: {:?}",
        retry_buffers[4].0, b"\x00\x05\x00\x00Send timeout occurred\x00"
    );
    assert_eq!(
        retry_buffers[4].1, 5,
        "5: Timestamp mismatch. Received: {}, Expected: {}",
        retry_buffers[4].1, 5
    );
    assert_eq!(
        retry_buffers[5].0, b"",
        "5: Received: {:?}, Expected: {:?}",
        retry_buffers[5].0, b""
    );
    assert_eq!(
        retry_buffers[5].1, 0,
        "5: Timestamp mismatch. Received: {}, Expected: {}",
        retry_buffers[5].1, 0
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
    let running_server = start_rtftp(server_dir.clone()).await;
    let client = running_server.open_paired_client(source_ip).await;
    let existing_file = "aligned.file";
    let read_data = download(client, existing_file).await.unwrap();
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
    let running_server = start_rtftp(server_dir.clone()).await;
    let client = running_server.open_paired_client(source_ip).await;
    let existing_file = "nonaligned.file";
    let read_data = download(client, existing_file).await.unwrap();
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
    let running_server = start_rtftp(server_dir.clone()).await;
    let client = running_server.open_paired_client(source_ip).await;
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
    let running_server = start_rtftp(server_dir.clone()).await;
    let client = running_server.open_paired_client(source_ip).await;
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
    let running_server = start_rtftp(server_dir.clone()).await;
    let client = running_server.open_paired_client(source_ip).await;
    let read_data = download(client, existing_file).await.unwrap();
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
    let running_server = start_rtftp(server_dir.clone()).await;
    let client = running_server.open_paired_client(source_ip).await;
    let read_result = download(client, nonexisting_file).await;
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
    let running_server = start_rtftp(server_dir.clone()).await;
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
    let client = running_server.open_paired_client(source_ip).await;
    let existing_file = "nonaligned.file";
    let read_data = download(client, existing_file).await.unwrap();
    let data = make_payload(4194319);
    assert_eq!(read_data, data);
}

#[tokio::test(flavor = "current_thread")]
async fn download_local_aligned_file_window() {
    let source_ip = "127.0.0.11";
    let server_dir = mk_tmp(download_local_aligned_file_window);
    let payload_size = 4096;
    let data = make_payload(payload_size);
    let file_name = "file.txt";
    let file = server_dir.join(source_ip).join(file_name);
    _write_file(&file, &data);
    let running_server = start_rtftp(server_dir).await;
    let client = running_server.open_paired_client(source_ip).await;
    let read_data = download_window(client, file_name, 5).await.unwrap();
    assert_eq!(read_data, data);
}

#[tokio::test(flavor = "current_thread")]
async fn download_local_unaligned_file_window() {
    let source_ip = "127.0.0.11";
    let server_dir = mk_tmp(download_local_unaligned_file_window);
    let payload_size = 4096;
    let data = make_payload(payload_size);
    let file_name = "file.txt";
    let file = server_dir.join(source_ip).join(file_name);
    _write_file(&file, &data);
    let running_server = start_rtftp(server_dir).await;
    let client = running_server.open_paired_client(source_ip).await;
    let read_data = download_window(client, file_name, 5).await.unwrap();
    assert_eq!(read_data, data);
}

#[tokio::test(flavor = "current_thread")]
async fn file_window_partial_ack() {
    let source_ip = "127.0.0.11";
    let server_dir = mk_tmp(file_window_partial_ack);
    let payload_size = 4096;
    let data = make_payload(payload_size);
    let file_name = "file.txt";
    let file = server_dir.join(source_ip).join(file_name);
    _write_file(&file, &data);
    let running_server = start_rtftp(server_dir).await;
    let client = running_server.open_paired_client(source_ip).await;
    let block_size = 100;
    let send_options = HashMap::from([
        ("windowsize".to_string(), 3.to_string()),
        ("timeout".to_string(), 1.to_string()),
        ("blksize".to_string(), block_size.to_string()),
    ]);
    let sent_request = client
        .send_optioned_read_request(file_name, &send_options)
        .await
        .unwrap();
    let oack = sent_request.read_oack(5).await.unwrap();
    let sent_ack = oack.acknowledge().await.unwrap();
    let first_block = sent_ack.read_next(2).await.unwrap();
    assert_eq!(first_block.data(), data[..block_size].to_vec());
    let second_block = first_block.read_next(2).await.unwrap();
    assert_eq!(
        second_block.data(),
        data[block_size..block_size * 2].to_vec()
    );
    let third_block = second_block.read_next(2).await.unwrap();
    assert_eq!(
        third_block.data(),
        data[block_size * 2..block_size * 3].to_vec()
    );
    let first_block_acknowledge = b"\x00\x04\x00\x01";
    let datagram_stream = third_block.datagram_stream;
    datagram_stream.send(first_block_acknowledge).await.unwrap();
    let mut buffer = [0u8; _BUFFER_SIZE];
    datagram_stream.recv(&mut buffer, 2, 0).await.unwrap();
    let second_block_num = u16::from_be_bytes(buffer[2..4].try_into().unwrap());
    assert_eq!(second_block_num, 2);
    datagram_stream.recv(&mut buffer, 2, 0).await.unwrap();
    let third_block_num = u16::from_be_bytes(buffer[2..4].try_into().unwrap());
    assert_eq!(third_block_num, 3);
    datagram_stream.recv(&mut buffer, 2, 0).await.unwrap();
    let forth_block_num = u16::from_be_bytes(buffer[2..4].try_into().unwrap());
    assert_eq!(forth_block_num, 4);
}
