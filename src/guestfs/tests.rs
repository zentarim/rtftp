use super::*;
use crate::_tests::{ensure_prerequisite_disk, get_test_qcow, make_payload};
use std::time;

fn _read_file(guestfs: &GuestFS, path: &str) -> Vec<u8> {
    let mut result = vec![];
    let mut offset = 0;
    loop {
        let chunk = guestfs.read_chunk(path, offset).unwrap();
        if !chunk.is_empty() {
            result.extend_from_slice(&chunk);
            offset += chunk.len();
        } else {
            break;
        }
    }
    result
}

#[test]
fn test_add_existing_disk() {
    ensure_prerequisite_disk();
    let guestfs = GuestFS::new();
    let result = guestfs.add_disk(get_test_qcow().to_str().unwrap(), true);
    assert!(
        result.is_ok(),
        "Expected Ok, got Err: {:?}",
        result.unwrap_err()
    );
}

#[test]
fn test_add_non_existing_disk() {
    ensure_prerequisite_disk();
    let guestfs = GuestFS::new();
    let result = guestfs.add_disk("/nonexisting.qcow2", true);
    assert!(result.is_err(), "Unexpected success received");
    assert!(matches!(
        result.err().unwrap(),
        GuestFSError::DiskNotFound(_)
    ));
}

#[test]
fn test_open_existing_disk() {
    ensure_prerequisite_disk();
    let guestfs = GuestFS::new();
    let add_result = guestfs.add_disk(get_test_qcow().to_str().unwrap(), true);
    assert!(
        add_result.is_ok(),
        "Expected Ok, got Err: {:?}",
        add_result.unwrap_err()
    );
    let start_time = time::Instant::now();
    let launch_result = guestfs.launch();
    assert!(
        launch_result.is_ok(),
        "Expected Ok, got Err: {:?}",
        launch_result.unwrap_err()
    );
    let end_time = time::Instant::now();
    eprintln!(
        "Spent: {:.3} s",
        end_time.duration_since(start_time).as_secs_f64()
    );
}

#[test]
fn test_read_aligned_file() {
    ensure_prerequisite_disk();
    let guestfs = GuestFS::new();
    guestfs
        .add_disk(get_test_qcow().to_str().unwrap(), true)
        .unwrap();
    guestfs.launch().unwrap();
    guestfs.mount_ro("/dev/sda2", "/").unwrap();
    guestfs.mount_ro("/dev/sda1", "/boot").unwrap();
    let expected_data = make_payload(4194304);
    let actual_data = _read_file(&guestfs, "/boot/aligned.file");
    assert_eq!(
        actual_data,
        expected_data,
        "Recv: {}, Expected: {}",
        actual_data.len(),
        expected_data.len()
    );
}

#[test]
fn test_read_nonaligned_file() {
    ensure_prerequisite_disk();
    let guestfs = GuestFS::new();
    guestfs
        .add_disk(get_test_qcow().to_str().unwrap(), true)
        .unwrap();
    guestfs.launch().unwrap();
    guestfs.mount_ro("/dev/sda2", "/").unwrap();
    guestfs.mount_ro("/dev/sda1", "/boot").unwrap();
    let expected_data = make_payload(4194319);
    let actual_data = _read_file(&guestfs, "/boot/nonaligned.file");
    assert_eq!(
        actual_data,
        expected_data,
        "Recv: {}, Expected: {}",
        actual_data.len(),
        expected_data.len()
    );
}
