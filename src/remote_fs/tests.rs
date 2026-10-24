use super::{ConnectedDisk, RemoteRoot};
use crate::fs::Root;
use std::io::ErrorKind;

#[test]
fn open_rejects_parent_directory() {
    let disk = ConnectedDisk::unconnected("nbd://unused".to_string());
    let root = RemoteRoot::new(disk, "/boot");
    for request in [
        "..",
        "../etc/passwd",
        "foo/../../etc/passwd",
        "/../etc/passwd",
    ] {
        let error = root.open(request).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::PermissionDenied, "{request}");
    }
}
