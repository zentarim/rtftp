use super::{
    VmError, classify_launch_error, connect_client, disk_argument, guest_cmdline, kernel_path,
    launch, take_prefixed_lines, take_prefixed_remainder,
};
use crate::fs::{OpenedFile, Root};
use crate::remote_fs::{ConnectedDisk, RemoteRoot};
use crate::tests_common::{ensure_prerequisite_disk, make_payload, mk_tmp, read_file};
use file_protocol::VSOCK_PORT;
use std::path::Path;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixListener;

fn boot_mounts() -> [(usize, &'static str); 2] {
    [(2, "/"), (1, "/boot")]
}

#[test]
fn disk_argument_uses_nbd_for_urls_and_qcow_for_files() {
    assert_eq!(
        disk_argument("nbd://127.0.0.1:10809/disk").unwrap(),
        "nbd=nbd://127.0.0.1:10809/disk,readonly=on"
    );
    let (test_disk, _lock) = ensure_prerequisite_disk();
    let argument = disk_argument(test_disk.to_str().unwrap()).unwrap();
    assert_eq!(
        argument,
        format!("path={},image_type=qcow2,readonly=on", test_disk.display())
    );
}

#[test]
fn missing_qcow_is_disk_not_found() {
    let error = disk_argument("/nonexisting.qcow2").unwrap_err();
    assert!(matches!(error, VmError::DiskNotFound(_)));
}

#[test]
fn cmdline_lists_json_mounts_in_order() {
    assert_eq!(
        guest_cmdline(&boot_mounts()),
        "console=ttyS0 reboot=k panic=-1 mounts=/dev/vda2:/,/dev/vda1:/boot"
    );
}

#[test]
fn kernel_path_prefers_boot_image_then_prefixed_then_vmlinuz() {
    let exists = |path: &Path| path == Path::new("/boot/vmlinuz-test");
    assert_eq!(
        kernel_path("BOOT_IMAGE=/boot/vmlinuz-test quiet", exists).unwrap(),
        Path::new("/boot/vmlinuz-test")
    );
    assert_eq!(
        kernel_path("BOOT_IMAGE=/vmlinuz-test", exists).unwrap(),
        Path::new("/boot/vmlinuz-test")
    );
    let fallback = |path: &Path| path == Path::new("/boot/vmlinuz");
    assert_eq!(
        kernel_path("quiet", fallback).unwrap(),
        Path::new("/boot/vmlinuz")
    );
}

#[test]
fn prefixed_lines_split_chunks_and_keep_a_partial_tail() {
    let peer = "10.1.0.225";
    let mut pending = Vec::new();
    let first = take_prefixed_lines(
        &mut pending,
        b"cloud-hypervisor: warn\nError: boot\n[    0.000000] Linux",
        peer,
    );
    assert_eq!(
        first,
        vec![
            "10.1.0.225: cloud-hypervisor: warn".to_string(),
            "10.1.0.225: Error: boot".to_string(),
        ]
    );
    assert_eq!(pending, b"[    0.000000] Linux");

    let second = take_prefixed_lines(&mut pending, b" version\npartial", peer);
    assert_eq!(
        second,
        vec!["10.1.0.225: [    0.000000] Linux version".to_string()]
    );
    assert_eq!(
        take_prefixed_remainder(&mut pending, peer).as_deref(),
        Some("10.1.0.225: partial")
    );
    assert!(pending.is_empty());
    assert_eq!(take_prefixed_remainder(&mut pending, peer), None);
}

#[test]
fn launch_errors_keep_the_nbd_failure_classes() {
    assert!(matches!(
        classify_launch_error(
            "Failed to connect to NBD server: Connection refused",
            "",
            "nbd://x"
        ),
        VmError::ConnectionRefused(_)
    ));
    assert!(matches!(
        classify_launch_error("NBD export \"disk\" not found", "", "nbd://x"),
        VmError::ShareNotFound(_)
    ));
    assert!(matches!(
        classify_launch_error("No such file or directory", "", "/missing.qcow2"),
        VmError::DiskNotFound(_)
    ));
    assert!(matches!(
        classify_launch_error(
            "guest did not accept vsock",
            "finit_module ubifs.ko: No such file or directory",
            "/disk.qcow2"
        ),
        VmError::Generic(_)
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn test_open_existing_disk() {
    let (test_disk, _lock) = ensure_prerequisite_disk();
    launch(test_disk.to_str().unwrap(), &boot_mounts(), "qcow-open")
        .await
        .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn test_read_aligned_file() {
    let (test_disk, _lock) = ensure_prerequisite_disk();
    let vm = launch(test_disk.to_str().unwrap(), &boot_mounts(), "qcow-aligned")
        .await
        .unwrap();
    let root = RemoteRoot::new(
        ConnectedDisk::new(vm, test_disk.display().to_string()),
        "/boot",
    );
    let mut opened = root.open("aligned.file").await.unwrap();
    let expected = make_payload(opened.get_size().await.unwrap());
    assert_eq!(read_file(&mut opened).await, expected);
}

#[tokio::test(flavor = "current_thread")]
async fn test_read_nonaligned_file() {
    let (test_disk, _lock) = ensure_prerequisite_disk();
    let vm = launch(
        test_disk.to_str().unwrap(),
        &boot_mounts(),
        "qcow-nonaligned",
    )
    .await
    .unwrap();
    let root = RemoteRoot::new(
        ConnectedDisk::new(vm, test_disk.display().to_string()),
        "/boot",
    );
    let mut opened = root.open("nonaligned.file").await.unwrap();
    let expected = make_payload(opened.get_size().await.unwrap());
    assert_eq!(read_file(&mut opened).await, expected);
}

#[tokio::test(flavor = "current_thread")]
async fn connect_client_accepts_ok_and_rejects_other_lines() {
    let directory = mk_tmp(connect_client_accepts_ok_and_rejects_other_lines);
    let ok_path = directory.join("ok.sock");
    let listener = UnixListener::bind(&ok_path).unwrap();
    let client = tokio::spawn(async move { connect_client(&ok_path).await });
    let (mut server, _address) = listener.accept().await.unwrap();
    let request = read_handshake_line(&mut server).await;
    assert_eq!(request, format!("CONNECT {VSOCK_PORT}"));
    server.write_all(b"OK 1234\n").await.unwrap();
    client.await.unwrap().unwrap();

    let bad_path = directory.join("bad.sock");
    let listener = UnixListener::bind(&bad_path).unwrap();
    let client = tokio::spawn(async move { connect_client(&bad_path).await });
    let (mut server, _address) = listener.accept().await.unwrap();
    let _request = read_handshake_line(&mut server).await;
    server.write_all(b"NOPE\n").await.unwrap();
    let error = client.await.unwrap().unwrap_err();
    assert!(error.to_string().contains("unexpected vsock handshake"));
}

async fn read_handshake_line(stream: &mut tokio::net::UnixStream) -> String {
    let mut request = Vec::new();
    loop {
        let mut byte = [0u8; 1];
        stream.read_exact(&mut byte).await.unwrap();
        if byte[0] == b'\n' {
            break;
        }
        request.push(byte[0]);
    }
    String::from_utf8(request).unwrap()
}
