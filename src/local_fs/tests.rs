use super::*;
use std::any::type_name;
use std::env;
use std::fs::{Permissions, create_dir, set_permissions};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

fn get_fn_name<T>(_: T) -> &'static str {
    type_name::<T>()
}

fn mk_tmp<T>(test_func: T) -> PathBuf {
    let test_dir_name = get_fn_name(test_func).replace("::", "_");
    let pid = std::process::id();
    let test_tmp_dir = env::temp_dir().join(format!("rtftp_{pid}_{test_dir_name}"));
    create_dir(&test_tmp_dir).unwrap();
    test_tmp_dir
}

#[test]
fn open_non_existent() {
    let local_root = LocalRoot {
        path: PathBuf::from("/nonexistent"),
    };
    let result = local_root.open("nonexistent.file");
    assert_eq!(result.err().unwrap(), FileError::FileNotFound);
}

#[test]
fn open_access_denied() {
    let unreadable_directory = mk_tmp(open_access_denied);
    set_permissions(&unreadable_directory, Permissions::from_mode(0o055)).unwrap();
    let local_root = LocalRoot {
        path: unreadable_directory,
    };
    let result = local_root.open("nonexistent");
    assert_eq!(result.err().unwrap(), FileError::AccessViolation);
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
