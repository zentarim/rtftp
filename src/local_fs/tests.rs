use super::*;
use crate::tests_common::mk_tmp;
use std::fs::{Permissions, set_permissions};
use std::io::ErrorKind;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

#[test]
fn open_non_existent() {
    let local_root = LocalRoot {
        path: PathBuf::from("/nonexistent"),
    };
    let result = local_root.open("nonexistent.file");
    assert_eq!(result.err().unwrap().kind(), ErrorKind::NotFound);
}

#[test]
fn open_access_denied() {
    let unreadable_directory = mk_tmp(open_access_denied);
    set_permissions(&unreadable_directory, Permissions::from_mode(0o055)).unwrap();
    let local_root = LocalRoot {
        path: unreadable_directory,
    };
    let result = local_root.open("nonexistent");
    assert_eq!(result.err().unwrap().kind(), ErrorKind::PermissionDenied);
}

#[test]
fn get_size() {
    let local_root = LocalRoot {
        path: PathBuf::from(env!("CARGO_MANIFEST_DIR")),
    };
    let mut result = local_root.open("Cargo.toml").unwrap();
    let size = result.get_size().unwrap();
    assert!(size > 0);
}

#[test]
fn read() {
    let mut buffer = [0u8; 1024];
    let local_root = LocalRoot {
        path: PathBuf::from(env!("CARGO_MANIFEST_DIR")),
    };
    let mut result = local_root.open("Cargo.toml").unwrap();
    let read_size = result.read_to(&mut buffer).unwrap();
    let string = String::from_utf8(buffer[..read_size].to_vec()).unwrap();
    assert!(string.contains("libc"));
}

#[test]
fn read_leading_slash() {
    let mut buffer = [0u8; 1024];
    let local_root = LocalRoot {
        path: PathBuf::from(env!("CARGO_MANIFEST_DIR")),
    };
    let mut result = local_root.open("/Cargo.toml").unwrap();
    let read_size = result.read_to(&mut buffer).unwrap();
    let string = String::from_utf8(buffer[..read_size].to_vec()).unwrap();
    assert!(string.contains("libc"));
}
