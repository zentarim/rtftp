use super::*;
use libc::{LOCK_EX, O_DIRECTORY, O_RDONLY, flock, open};
use std::fs::File;
use std::os::fd::{FromRawFd, RawFd};
use std::path::PathBuf;
use std::process::Command;
use std::{io, time};

const _DATA_PATTERN: &str = "ARBITRARY DATA";

fn _get_test_data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests")
}

fn _get_test_qcow() -> PathBuf {
    _get_test_data_dir().join("test_disk.qcow2")
}

fn _ensure_prerequisite_disk() {
    let lock = _explicit_lock().unwrap();
    if !_get_test_qcow().exists() {
        let script = _get_test_data_dir().join("build_test_disk.sh");
        let status = Command::new(&script)
            .arg(_get_test_qcow().as_path())
            .arg(_DATA_PATTERN)
            .status()
            .expect(format!("{:?} failed", script).as_str());
        if !status.success() {
            panic!("{script:?} failed");
        }
    }
    drop(lock);
}

fn _explicit_lock() -> io::Result<File> {
    let cwd_fd = _open_dir_ro(_get_test_data_dir().to_str().unwrap())?;
    if unsafe { flock(cwd_fd, LOCK_EX) } != 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { File::from_raw_fd(cwd_fd) })
    }
}

fn _open_dir_ro(path: &str) -> io::Result<RawFd> {
    let c_path = CString::new(path)?;
    let fd = unsafe { open(c_path.as_ptr(), O_RDONLY | O_DIRECTORY) as RawFd };
    if fd != 0 {
        Ok(fd)
    } else {
        Err(io::Error::last_os_error())
    }
}

#[test]
fn test_add_existing_disk() {
    _ensure_prerequisite_disk();
    let guestfs = GuestFS::new();
    let result = guestfs.add_disk(_get_test_qcow().to_str().unwrap(), true);
    assert!(
        result.is_ok(),
        "Expected Ok, got Err: {:?}",
        result.unwrap_err()
    );
}

#[test]
fn test_add_non_existing_disk() {
    _ensure_prerequisite_disk();
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
    _ensure_prerequisite_disk();
    let guestfs = GuestFS::new();
    let add_result = guestfs.add_disk(_get_test_qcow().to_str().unwrap(), true);
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
