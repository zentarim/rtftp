use super::{ConnectedDisk, RemoteRoot, read_chunk, read_reply, write_request};
use crate::fs::Root;
use file_protocol::{
    FILE_NOT_FOUND, MAX_CHUNK_SIZE, write_chunk, write_error, write_info,
    write_request as sync_write_request,
};
use std::io::{Cursor, ErrorKind};

#[tokio::test(flavor = "current_thread")]
async fn open_rejects_parent_directory() {
    let disk = ConnectedDisk::unconnected("nbd://unused".to_string());
    let root = RemoteRoot::new(disk, "/boot");
    for request in [
        "..",
        "../etc/passwd",
        "foo/../../etc/passwd",
        "/../etc/passwd",
    ] {
        let error = root.open(request).await.unwrap_err();
        assert_eq!(error.kind(), ErrorKind::PermissionDenied, "{request}");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn async_request_matches_sync_bytes() {
    let path = "/boot/grub.cfg";
    let mut encoded = Cursor::new(Vec::new());
    write_request(&mut encoded, path, MAX_CHUNK_SIZE)
        .await
        .unwrap();
    let mut expected = Vec::new();
    sync_write_request(&mut expected, path, MAX_CHUNK_SIZE).unwrap();
    assert_eq!(encoded.into_inner(), expected);
}

#[tokio::test(flavor = "current_thread")]
async fn async_read_reply_decodes_sync_info_and_error() {
    let mut info = Vec::new();
    write_info(&mut info, 42).unwrap();
    assert_eq!(read_reply(&mut Cursor::new(info)).await.unwrap(), 42);

    let mut missing = Vec::new();
    write_error(&mut missing, FILE_NOT_FOUND).unwrap();
    let missing_error = read_reply(&mut Cursor::new(missing)).await.unwrap_err();
    assert_eq!(missing_error.kind(), ErrorKind::NotFound);

    let mut other = Vec::new();
    write_error(&mut other, "disk busy").unwrap();
    let other_error = read_reply(&mut Cursor::new(other)).await.unwrap_err();
    assert_eq!(other_error.kind(), ErrorKind::Other);
    assert_eq!(other_error.to_string(), "disk busy");
}

#[tokio::test(flavor = "current_thread")]
async fn async_read_chunk_decodes_sync_chunk() {
    let payload = b"a\0b";
    let mut frame = Vec::new();
    write_chunk(&mut frame, payload).unwrap();
    let decoded = read_chunk(&mut Cursor::new(frame)).await.unwrap();
    assert_eq!(decoded, payload);
}

#[tokio::test(flavor = "current_thread")]
async fn async_read_chunk_rejects_error_bad_trailer_and_oversize() {
    let mut error_frame = Vec::new();
    write_error(&mut error_frame, "read failed").unwrap();
    let error = read_chunk(&mut Cursor::new(error_frame)).await.unwrap_err();
    assert_eq!(error.to_string(), "read failed");

    let mut bad_trailer = Vec::new();
    write_chunk(&mut bad_trailer, b"abc").unwrap();
    let trailer_at = bad_trailer.len() - 1;
    bad_trailer[trailer_at] = b'x';
    let trailer_error = read_chunk(&mut Cursor::new(bad_trailer)).await.unwrap_err();
    assert_eq!(
        trailer_error.to_string(),
        "chunk is missing its trailing NUL"
    );

    let mut oversized = Vec::new();
    oversized.extend_from_slice(b"CHNK\0");
    oversized.extend_from_slice((MAX_CHUNK_SIZE + 1).to_string().as_bytes());
    oversized.push(0);
    let oversized_error = read_chunk(&mut Cursor::new(oversized)).await.unwrap_err();
    assert_eq!(oversized_error.to_string(), "chunk exceeds MAX_CHUNK_SIZE");
}
