use super::*;
use std::fs::File;
use std::path::PathBuf;
use std::process::Command;
use std::{io, time};

const DATA_PATTERN: &str = "ARBITRARY DATA";

fn get_test_qcow() -> PathBuf {
    get_test_data_dir().join("test_disk_guestfs.qcow2")
}

fn ensure_prerequisite_disk() -> PathBuf {
    let lock = lock_tests_directory().unwrap();
    let qcow_path = get_test_qcow();
    if !qcow_path.exists() {
        create_prerequisite_disk()
    }
    drop(lock);
    qcow_path
}

fn create_prerequisite_disk() {
    let script = get_test_data_dir().join("build_test_qcow_disk.sh");
    let status = Command::new(&script)
        .arg(get_test_qcow().as_path())
        .arg(DATA_PATTERN)
        .status()
        .expect(format!("{:?} failed", script).as_str());
    if !status.success() {
        panic!("{script:?} failed");
    }
}

fn lock_tests_directory() -> io::Result<File> {
    let opened = File::open(get_test_data_dir())?;
    opened.lock()?;
    Ok(opened)
}

fn get_test_data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests")
}

fn read_file(guestfs: &GuestFS, path: &str) -> Vec<u8> {
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

fn make_payload(size: usize) -> Vec<u8> {
    let pattern = DATA_PATTERN.as_bytes();
    pattern.iter().copied().cycle().take(size).collect()
}

#[test]
fn test_add_existing_disk() {
    let test_disk = ensure_prerequisite_disk();
    let guestfs = GuestFS::new();
    let result = guestfs.add_disk(test_disk.to_str().unwrap(), true);
    assert!(
        result.is_ok(),
        "Expected Ok, got Err: {:?}",
        result.unwrap_err()
    );
}

#[test]
fn test_add_non_existing_disk() {
    _ = ensure_prerequisite_disk();
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
    let test_disk = ensure_prerequisite_disk();
    let guestfs = GuestFS::new();
    let add_result = guestfs.add_disk(test_disk.to_str().unwrap(), true);
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
    let test_disk = ensure_prerequisite_disk();
    let guestfs = GuestFS::new();
    guestfs.add_disk(test_disk.to_str().unwrap(), true).unwrap();
    guestfs.launch().unwrap();
    guestfs.mount_ro("/dev/sda2", "/").unwrap();
    guestfs.mount_ro("/dev/sda1", "/boot").unwrap();
    let expected_data = make_payload(4194304);
    let actual_data = read_file(&guestfs, "/boot/aligned.file");
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
    let test_disk = ensure_prerequisite_disk();
    let guestfs = GuestFS::new();
    guestfs.add_disk(test_disk.to_str().unwrap(), true).unwrap();
    guestfs.launch().unwrap();
    guestfs.mount_ro("/dev/sda2", "/").unwrap();
    guestfs.mount_ro("/dev/sda1", "/boot").unwrap();
    let expected_data = make_payload(4194319);
    let actual_data = read_file(&guestfs, "/boot/nonaligned.file");
    assert_eq!(
        actual_data,
        expected_data,
        "Recv: {}, Expected: {}",
        actual_data.len(),
        expected_data.len()
    );
}
