use super::*;
use crate::_tests::{make_payload, run_nbd_server};
use serde_json::json;
use std::time;

fn read_file(opened: &mut dyn OpenedFile) -> Vec<u8> {
    let mut buffer = vec![];
    let mut chunk = vec![0u8; 512];
    loop {
        let read_size = opened.read_to(&mut chunk).unwrap();
        if read_size == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read_size]);
    }
    buffer
}

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
    let mut disk = result.unwrap();
    let partitions = disk.list_partitions().unwrap();
    eprintln!("{:?}", partitions);
    eprintln!("{}", disk);
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
    assert!(matches!(
        result.err().unwrap(),
        GuestFSError::ShareNotFound(_)
    ))
}

#[test]
fn test_add_invalid_url() {
    let non_existent_nbd_url = "nbd://127.1.1.1:1/invalid";
    let result = attach_nbd_disk(non_existent_nbd_url);
    assert!(result.is_err());
    assert!(matches!(
        result.err().unwrap(),
        GuestFSError::ConnectionRefused(_)
    ))
}

#[test]
fn open_existing_file() {
    let nbd_process = run_nbd_server("127.0.0.2");
    let mut disk = attach_nbd_disk(nbd_process.get_url()).unwrap();
    let partitions = disk.list_partitions().unwrap();
    let boot = partitions.get(0).unwrap();
    let root = partitions.get(1).unwrap();
    assert!(root.mount_ro("/").is_ok());
    assert!(boot.mount_ro("/boot").is_ok());
    let file = "/boot/aligned.file";
    let opened = disk.open(file);
    assert!(opened.is_ok());
    assert_eq!(opened.unwrap().get_size(), Ok(4194304));
}

#[test]
fn open_non_existing_file() {
    let nbd_process = run_nbd_server("127.0.0.2");
    let mut disk = attach_nbd_disk(nbd_process.get_url()).unwrap();
    let partitions = disk.list_partitions().unwrap();
    let root = partitions.get(1).unwrap();
    assert!(root.mount_ro("/").is_ok());
    let file = "/nonexisting/file";
    let opened = disk.open(file);
    assert!(opened.is_err());
    assert!(matches!(opened.err().unwrap(), FileError::FileNotFound))
}

#[test]
fn read_existing_aligned_file() {
    let nbd_process = run_nbd_server("127.0.0.2");
    let mut disk = attach_nbd_disk(nbd_process.get_url()).unwrap();
    let partitions = disk.list_partitions().unwrap();
    let root = partitions.get(1).unwrap();
    let boot = partitions.get(0).unwrap();
    assert!(root.mount_ro("/").is_ok());
    assert!(boot.mount_ro("/boot").is_ok());
    let chroot = RemoteChroot::new(disk, "/boot");
    let file = "aligned.file";
    let mut opened = chroot.open(file).unwrap();
    let expected_data = make_payload(opened.get_size().unwrap());
    let read_data = read_file(opened.as_mut());
    assert_eq!(read_data, expected_data);
}

#[test]
fn read_existing_nonaligned_file() {
    let nbd_process = run_nbd_server("127.0.0.2");
    let mut disk = attach_nbd_disk(nbd_process.get_url()).unwrap();
    let partitions = disk.list_partitions().unwrap();
    let root = partitions.get(1).unwrap();
    let boot = partitions.get(0).unwrap();
    assert!(root.mount_ro("/").is_ok());
    assert!(boot.mount_ro("/boot").is_ok());
    let chroot = RemoteChroot::new(disk, "/boot");
    let file = "nonaligned.file";
    let mut opened = chroot.open(file).unwrap();
    let expected_data = make_payload(opened.get_size().unwrap());
    let read_data = read_file(opened.as_mut());
    assert_eq!(read_data, expected_data);
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
    let running_disk = nbd_config.connect();
    assert!(running_disk.is_ok());
}
