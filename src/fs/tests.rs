use super::contained_join;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

#[test]
fn parent_directory_is_rejected() {
    let root = Path::new("/srv/tftp");
    for request in [
        "..",
        "../etc/passwd",
        "foo/../../etc/passwd",
        "/../etc/passwd",
        "foo/..",
        "foo/bar/../../../etc/passwd",
    ] {
        let error = contained_join(root, request).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::PermissionDenied, "{request}");
    }
}

#[test]
fn leading_slash_stays_under_root() {
    let joined = contained_join(Path::new("/boot"), "/etc/passwd").unwrap();
    assert_eq!(joined, PathBuf::from("/boot/etc/passwd"));
}

#[test]
fn relative_file_stays_under_root() {
    let joined = contained_join(Path::new("/boot"), "aligned.file").unwrap();
    assert_eq!(joined, PathBuf::from("/boot/aligned.file"));
}
