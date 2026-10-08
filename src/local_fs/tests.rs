use super::*;
use crate::tests_common::mk_tmp;
use std::fs::{Permissions, create_dir, set_permissions, write};
use std::io::ErrorKind;
use std::os::unix::fs::{PermissionsExt, symlink};
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

#[test]
fn reject_parent_directory() {
    let root_dir = mk_tmp(reject_parent_directory);
    let outside = root_dir.with_file_name("rtftp_parent_escape_secret");
    write(&outside, b"secret").unwrap();
    let local_root = LocalRoot { path: root_dir };
    let escaped = format!("../{}", outside.file_name().unwrap().to_str().unwrap());
    for request in [escaped.as_str(), "../etc/passwd", "foo/../../etc/passwd"] {
        let result = local_root.open(request);
        assert_eq!(
            result.err().unwrap().kind(),
            ErrorKind::PermissionDenied,
            "{request}"
        );
    }
}

#[test]
fn reject_symlink_outside_root() {
    let root_dir = mk_tmp(reject_symlink_outside_root);
    let outside = root_dir.with_file_name(format!(
        "{}_outside",
        root_dir.file_name().unwrap().to_str().unwrap()
    ));
    create_dir(&outside).unwrap();
    let secret = outside.join("secret");
    write(&secret, b"secret").unwrap();
    symlink(&secret, root_dir.join("file_link")).unwrap();
    symlink(&outside, root_dir.join("dir_link")).unwrap();
    let local_root = LocalRoot {
        path: root_dir.clone(),
    };
    for request in ["file_link", "dir_link/secret"] {
        let result = local_root.open(request);
        assert_eq!(
            result.err().unwrap().kind(),
            ErrorKind::PermissionDenied,
            "{request}"
        );
    }
}

#[test]
fn open_symlink_inside_root() {
    let root_dir = mk_tmp(open_symlink_inside_root);
    let inside = root_dir.join("inside.txt");
    write(&inside, b"inside").unwrap();
    symlink(&inside, root_dir.join("link")).unwrap();
    let local_root = LocalRoot { path: root_dir };
    let mut opened = local_root.open("link").unwrap();
    let mut buffer = [0u8; 16];
    let read_size = opened.read_to(&mut buffer).unwrap();
    assert_eq!(&buffer[..read_size], b"inside");
}
