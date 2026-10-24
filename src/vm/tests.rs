use super::{VmError, classify_launch_error, disk_argument, guest_cmdline, kernel_path, launch};
use crate::fs::{OpenedFile, Root};
use crate::remote_fs::{ConnectedDisk, RemoteRoot};
use crate::tests_common::{ensure_prerequisite_disk, make_payload, read_file};
use std::path::Path;

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

#[test]
fn test_open_existing_disk() {
    let (test_disk, _lock) = ensure_prerequisite_disk();
    launch(test_disk.to_str().unwrap(), &boot_mounts(), "qcow-open").unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn test_read_aligned_file() {
    let (test_disk, _lock) = ensure_prerequisite_disk();
    let vm = launch(test_disk.to_str().unwrap(), &boot_mounts(), "qcow-aligned").unwrap();
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
    .unwrap();
    let root = RemoteRoot::new(
        ConnectedDisk::new(vm, test_disk.display().to_string()),
        "/boot",
    );
    let mut opened = root.open("nonaligned.file").await.unwrap();
    let expected = make_payload(opened.get_size().await.unwrap());
    assert_eq!(read_file(&mut opened).await, expected);
}
