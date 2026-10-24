use super::*;
use crate::fs::{OpenedFile, Root};
use crate::tests_common::{ensure_prerequisite_disk, make_payload, mk_tmp, read_file};
use crate::vm::VmError;
use serde_json::json;
use std::io::ErrorKind;
use std::path::Path;
use std::{fs, time};

include!("../../tests/common/nbd_server.rs");

#[test]
fn test_add_nbd_disk() {
    let nbd_process = run_nbd_server("127.0.0.2");
    let start_time = time::Instant::now();
    let result = attach_nbd_disk(nbd_process.get_url());
    assert!(
        result.is_ok(),
        "Expected Ok, got Err: {:?}",
        result.unwrap_err()
    );
    let disk = result.unwrap();
    eprintln!("{disk}");
    let end_time = time::Instant::now();
    eprintln!(
        "Spent: {:.3} s",
        end_time.duration_since(start_time).as_secs_f64()
    );
    drop(nbd_process);
}

#[test]
fn test_add_non_existing_share_disk() {
    let nbd_process = run_nbd_server("127.0.0.2");
    let non_existing_share = "non_existing_share";
    let (url_prefix, _existing_share) = nbd_process.get_url().rsplit_once("/").unwrap();
    let non_exising_share = vec![url_prefix, non_existing_share].join("/");
    let result = attach_nbd_disk(non_exising_share);
    assert!(result.is_err(), "Unexpected success received");
    assert!(matches!(result.err().unwrap(), VmError::ShareNotFound(_)))
}

#[test]
fn test_add_invalid_url() {
    let non_existent_nbd_url = "nbd://127.1.1.1:1/invalid";
    let result = attach_nbd_disk(non_existent_nbd_url);
    assert!(result.is_err());
    assert!(matches!(
        result.err().unwrap(),
        VmError::ConnectionRefused(_)
    ))
}

#[tokio::test(flavor = "current_thread")]
async fn open_existing_file() {
    let nbd_process = run_nbd_server("127.0.0.2");
    let disk = attach_nbd_disk(nbd_process.get_url()).unwrap();
    let file = "/boot/aligned.file";
    let opened = disk.open(file).await;
    assert!(opened.is_ok());
    assert_eq!(opened.unwrap().get_size().await.unwrap(), 4194304);
}

#[tokio::test(flavor = "current_thread")]
async fn open_non_existing_file() {
    let nbd_process = run_nbd_server("127.0.0.2");
    let disk = attach_nbd_disk(nbd_process.get_url()).unwrap();
    let file = "/nonexisting/file";
    let result = disk.open(file).await;
    assert_eq!(result.err().unwrap().kind(), ErrorKind::NotFound);
}

#[tokio::test(flavor = "current_thread")]
async fn read_existing_aligned_file() {
    let nbd_process = run_nbd_server("127.0.0.2");
    let disk = attach_nbd_disk(nbd_process.get_url()).unwrap();
    let chroot = RemoteRoot::new(disk, "/boot");
    let file = "aligned.file";
    let mut opened = chroot.open(file).await.unwrap();
    let expected_data = make_payload(opened.get_size().await.unwrap());
    let read_data = read_file(&mut opened).await;
    assert_eq!(read_data, expected_data);
}

#[tokio::test(flavor = "current_thread")]
async fn read_existing_nonaligned_file() {
    let nbd_process = run_nbd_server("127.0.0.2");
    let disk = attach_nbd_disk(nbd_process.get_url()).unwrap();
    let chroot = RemoteRoot::new(disk, "/boot");
    let file = "nonaligned.file";
    let mut opened = chroot.open(file).await.unwrap();
    let expected_data = make_payload(opened.get_size().await.unwrap());
    let read_data = read_file(&mut opened).await;
    assert_eq!(read_data, expected_data);
}

#[test]
fn missing_config_is_absent() {
    let tftp_root = mk_tmp(missing_config_is_absent);
    assert!(matches!(
        open_nbd_root(&tftp_root, "10.0.0.1"),
        NbdRoot::Absent
    ));
}

#[test]
fn invalid_config_is_unusable() {
    let tftp_root = mk_tmp(invalid_config_is_unusable);
    fs::write(
        tftp_root.join("10.0.0.1.nbd"),
        r#"{"url":"http://127.0.0.1/disk","mounts":[],"tftp_root":"/boot"}"#,
    )
    .unwrap();
    assert!(matches!(
        open_nbd_root(&tftp_root, "10.0.0.1"),
        NbdRoot::Unusable
    ));
}

#[test]
fn match_ip_requires_exact_config_name() {
    assert!(match_ip(Path::new("10.0.0.1.nbd"), "10.0.0.1"));
    assert!(!match_ip(Path::new("10.0.0.10.nbd"), "10.0.0.1"));
    assert!(!match_ip(Path::new("10.0.0.1.backup"), "10.0.0.1"));
    assert!(match_ip(Path::new("10.0.0.10.nbd"), "10.0.0.10"));
}

#[test]
fn build_config() {
    let config = json!({
        "url": "nbd://127.0.0.1:1000/arbitrary",
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
    }
    );
    let nbd_config = NBDConfig::from_json(&config);
    assert!(nbd_config.is_some());
}

#[test]
fn connect_from_config() {
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
    }
    );
    let nbd_config = NBDConfig::from_json(&config).unwrap();
    let running_disk = nbd_config.connect("10.0.0.1");
    assert!(running_disk.is_ok());
}
