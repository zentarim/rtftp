use super::{
    MAX_CHUNK_SIZE, ProtocolError, Reply, read_chunk, read_reply, read_request, write_chunk,
    write_error, write_info, write_request,
};
use std::io::Cursor;

#[test]
fn request_round_trip() {
    let mut buffer = Vec::new();
    write_request(&mut buffer, "/boot/grub.cfg", MAX_CHUNK_SIZE).unwrap();
    let request = read_request(&mut Cursor::new(buffer)).unwrap();
    assert_eq!(request.path, "/boot/grub.cfg");
    assert_eq!(request.chunk_size, MAX_CHUNK_SIZE);
}

#[test]
fn oversized_send_is_rejected() {
    assert!(matches!(
        write_request(&mut Vec::new(), "/file", MAX_CHUNK_SIZE + 1),
        Err(ProtocolError::InvalidSend)
    ));
    let mut frame = Vec::new();
    frame.extend_from_slice(b"FILE\0/file\0SEND\0");
    frame.extend_from_slice((MAX_CHUNK_SIZE + 1).to_string().as_bytes());
    frame.push(0);
    assert!(matches!(
        read_request(&mut Cursor::new(frame)),
        Err(ProtocolError::InvalidSend)
    ));
}

#[test]
fn zero_and_padded_send_are_rejected() {
    for size in ["0", "00", "01", "1048576 "] {
        let mut frame = Vec::new();
        frame.extend_from_slice(b"FILE\0/file\0SEND\0");
        frame.extend_from_slice(size.as_bytes());
        frame.push(0);
        assert!(
            matches!(
                read_request(&mut Cursor::new(frame)),
                Err(ProtocolError::InvalidSend)
            ),
            "{size}"
        );
    }
}

#[test]
fn chunk_preserves_embedded_nuls() {
    let payload = b"a\0b\0c";
    let mut buffer = Vec::new();
    write_chunk(&mut buffer, payload).unwrap();
    let decoded = read_chunk(&mut Cursor::new(buffer)).unwrap();
    assert_eq!(decoded, payload);
}

#[test]
fn zero_length_chunk_is_the_final_frame() {
    let mut buffer = Vec::new();
    write_chunk(&mut buffer, b"").unwrap();
    assert_eq!(buffer, b"CHNK\x000\x00\x00");
    let decoded = read_chunk(&mut Cursor::new(buffer)).unwrap();
    assert!(decoded.is_empty());
}

#[test]
fn info_and_error_round_trip() {
    let mut info = Vec::new();
    write_info(&mut info, 0).unwrap();
    assert!(matches!(
        read_reply(&mut Cursor::new(info)).unwrap(),
        Reply::Info(0)
    ));

    let mut error = Vec::new();
    write_error(&mut error, "No such file or directory").unwrap();
    match read_reply(&mut Cursor::new(error)).unwrap() {
        Reply::Error(text) => assert_eq!(text, "No such file or directory"),
        Reply::Info(_) => panic!("expected ERRO"),
    }
}

#[test]
fn oversized_chunk_is_rejected() {
    let mut frame = Vec::new();
    frame.extend_from_slice(b"CHNK\0");
    frame.extend_from_slice((MAX_CHUNK_SIZE + 1).to_string().as_bytes());
    frame.push(0);
    assert!(matches!(
        read_chunk(&mut Cursor::new(frame)),
        Err(ProtocolError::Malformed(_))
    ));
}
