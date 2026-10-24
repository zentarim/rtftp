use super::{Completion, Connection, FileStat, Steps, Submission};
use file_protocol::{
    FILE_NOT_FOUND, INVALID_SEND, MAX_CHUNK_SIZE, NOT_A_REGULAR_FILE, Reply, read_chunk,
    read_reply, write_request,
};
use std::collections::VecDeque;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::PathBuf;

struct Limits {
    recv: usize,
    read: usize,
    send: usize,
}

fn whole() -> Limits {
    Limits {
        recv: usize::MAX,
        read: usize::MAX,
        send: usize::MAX,
    }
}

fn scratch(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "rtftp-storekeeper-{}-{}-{name}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&path).unwrap();
    path
}

fn append_send(connection: &Connection, output: &mut Vec<u8>, limit: usize) -> usize {
    let pending = connection.pending_send();
    let mut remaining = limit;
    let mut copied = 0;
    for part in pending.parts.into_iter().take(pending.count) {
        let take = part.len().min(remaining);
        output.extend_from_slice(&part[..take]);
        copied += take;
        remaining -= take;
        if remaining == 0 {
            break;
        }
    }
    assert!(copied > 0, "send made no progress");
    copied
}

fn drive(request: &[u8], limits: Limits) -> Vec<u8> {
    let mut connection = Connection::new();
    let mut output = Vec::new();
    let mut request_at = 0;
    let mut file = None;
    let mut pending = VecDeque::from([Submission::Recv]);
    while let Some(submission) = pending.pop_front() {
        let steps = match submission {
            Submission::Recv => recv_step(&mut connection, request, &mut request_at, limits.recv),
            Submission::Open => open_step(&mut connection, &mut file),
            Submission::Stat => stat_step(&mut connection, file.as_ref().expect("open file")),
            Submission::Read { buf, at, len, .. } => read_step(
                &mut connection,
                file.as_mut().expect("open file"),
                buf,
                at,
                len,
                limits.read,
            ),
            Submission::Send => {
                let copied = append_send(&connection, &mut output, limits.send);
                connection.advance(Completion::Send(Ok(copied)))
            }
            Submission::CancelRead => connection.advance(Completion::Cancel(Ok(()))),
            Submission::Close => break,
        };
        extend_pending(&mut pending, steps);
    }
    output
}

fn extend_pending(pending: &mut VecDeque<Submission>, steps: Steps) {
    for submission in steps.into_array().into_iter().flatten() {
        pending.push_back(submission);
    }
}

fn recv_step(
    connection: &mut Connection,
    request: &[u8],
    request_at: &mut usize,
    limit: usize,
) -> Steps {
    let region = connection.recv_region();
    let count = (request.len() - *request_at).min(region.len()).min(limit);
    if count == 0 {
        return connection.advance(Completion::Recv(Ok(0)));
    }
    region[..count].copy_from_slice(&request[*request_at..*request_at + count]);
    *request_at += count;
    connection.advance(Completion::Recv(Ok(count)))
}

fn open_step(connection: &mut Connection, file: &mut Option<File>) -> Steps {
    match File::open(connection.open_path().to_str().expect("utf-8 path")) {
        Ok(opened) => {
            *file = Some(opened);
            connection.advance(Completion::Open(Ok(())))
        }
        Err(error) => connection.advance(Completion::Open(Err(error))),
    }
}

fn stat_step(connection: &mut Connection, file: &File) -> Steps {
    let metadata = file.metadata().expect("metadata");
    connection.advance(Completion::Stat(Ok(FileStat {
        size: metadata.len(),
        regular_file: metadata.is_file(),
    })))
}

fn read_step(
    connection: &mut Connection,
    file: &mut File,
    buf: usize,
    at: usize,
    len: usize,
    limit: usize,
) -> Steps {
    let region = connection.read_region(buf, at, len);
    let take = region.len().min(limit);
    let count = file.read(&mut region[..take]).expect("read");
    connection.advance(Completion::Read(Ok(count)))
}

fn exchange(path: &str, chunk_size: usize, limits: Limits) -> Vec<u8> {
    let mut request = Vec::new();
    write_request(&mut request, path, chunk_size).unwrap();
    drive(&request, limits)
}

fn collect_file(bytes: &[u8]) -> (u64, Vec<u8>) {
    let mut cursor = std::io::Cursor::new(bytes);
    let size = match read_reply(&mut cursor).unwrap() {
        Reply::Info(size) => size,
        Reply::Error(text) => panic!("{text}"),
    };
    let mut collected = Vec::new();
    loop {
        let chunk = read_chunk(&mut cursor).unwrap();
        if chunk.is_empty() {
            break;
        }
        collected.extend_from_slice(&chunk);
    }
    (size, collected)
}

#[test]
fn serves_file_bytes_including_nuls() {
    let dir = scratch("file");
    let path = dir.join("payload");
    let payload = b"a\0b\0c";
    fs::write(&path, payload).unwrap();
    let limits = Limits {
        recv: 1,
        read: 1,
        send: 1,
    };
    let (size, collected) = collect_file(&exchange(path.to_str().unwrap(), 2, limits));
    assert_eq!(size, payload.len() as u64);
    assert_eq!(collected, payload);
}

#[test]
fn empty_file_ends_with_an_empty_chunk() {
    let dir = scratch("empty");
    let path = dir.join("payload");
    fs::write(&path, b"").unwrap();
    let (size, collected) = collect_file(&exchange(path.to_str().unwrap(), 4, whole()));
    assert_eq!(size, 0);
    assert!(collected.is_empty());
}

#[test]
fn missing_file_uses_the_not_found_text() {
    let dir = scratch("missing");
    let path = dir.join("absent");
    let mut cursor =
        std::io::Cursor::new(exchange(path.to_str().unwrap(), MAX_CHUNK_SIZE, whole()));
    match read_reply(&mut cursor).unwrap() {
        Reply::Error(text) => assert_eq!(text, FILE_NOT_FOUND),
        Reply::Info(_) => panic!("expected ERRO"),
    }
}

#[test]
fn directory_is_not_a_regular_file() {
    let dir = scratch("dir");
    let mut cursor = std::io::Cursor::new(exchange(dir.to_str().unwrap(), MAX_CHUNK_SIZE, whole()));
    match read_reply(&mut cursor).unwrap() {
        Reply::Error(text) => assert_eq!(text, NOT_A_REGULAR_FILE),
        Reply::Info(_) => panic!("expected ERRO"),
    }
}

#[test]
fn oversized_send_is_invalid_send() {
    let mut frame = Vec::new();
    frame.extend_from_slice(b"FILE\0/file\0SEND\0");
    frame.extend_from_slice((MAX_CHUNK_SIZE + 1).to_string().as_bytes());
    frame.push(0);
    let output = drive(
        &frame,
        Limits {
            recv: 1,
            read: usize::MAX,
            send: usize::MAX,
        },
    );
    match read_reply(&mut std::io::Cursor::new(output)).unwrap() {
        Reply::Error(text) => assert_eq!(text, INVALID_SEND),
        Reply::Info(_) => panic!("expected ERRO"),
    }
}

#[test]
fn send_failure_cancels_an_in_flight_read() {
    let mut connection = Connection::new();
    let mut request = Vec::new();
    write_request(&mut request, "/file", 4).unwrap();
    connection.recv_region()[..request.len()].copy_from_slice(&request);
    let steps = connection.advance(Completion::Recv(Ok(request.len())));
    assert_eq!(steps.into_array()[0], Some(Submission::Open));
    let steps = connection.advance(Completion::Open(Ok(())));
    assert_eq!(steps.into_array()[0], Some(Submission::Stat));
    let steps = connection.advance(Completion::Stat(Ok(FileStat {
        size: 4,
        regular_file: true,
    })));
    assert_eq!(steps.into_array()[0], Some(Submission::Send));
    let info_len = pending_len(&connection);
    let steps = connection.advance(Completion::Send(Ok(info_len)));
    assert_eq!(
        steps.into_array()[0],
        Some(Submission::Read {
            buf: 0,
            offset: 0,
            at: 0,
            len: 4,
        })
    );
    connection.read_region(0, 0, 4).copy_from_slice(b"abcd");
    let steps = connection.advance(Completion::Read(Ok(4)));
    let armed = steps.into_array();
    assert_eq!(armed[0], Some(Submission::Send));
    assert_eq!(
        armed[1],
        Some(Submission::Read {
            buf: 1,
            offset: 4,
            at: 0,
            len: 4,
        })
    );
    let steps = connection.advance(Completion::Send(Err(io::Error::new(
        io::ErrorKind::BrokenPipe,
        "reset",
    ))));
    assert_eq!(steps.into_array()[0], Some(Submission::CancelRead));
    let steps = connection.advance(Completion::Read(Err(io::Error::from_raw_os_error(
        libc::ECANCELED,
    ))));
    assert_eq!(steps.into_array(), [None, None]);
    let steps = connection.advance(Completion::Cancel(Ok(())));
    assert_eq!(steps.into_array()[0], Some(Submission::Close));
    assert_eq!(connection.take_error_log().as_deref(), Some("reset"));
}

#[test]
fn read_failure_waits_for_the_chunk_already_sending() {
    let mut connection = Connection::new();
    let mut request = Vec::new();
    write_request(&mut request, "/file", 4).unwrap();
    connection.recv_region()[..request.len()].copy_from_slice(&request);
    connection.advance(Completion::Recv(Ok(request.len())));
    connection.advance(Completion::Open(Ok(())));
    connection.advance(Completion::Stat(Ok(FileStat {
        size: 4,
        regular_file: true,
    })));
    let info_len = pending_len(&connection);
    connection.advance(Completion::Send(Ok(info_len)));
    connection.read_region(0, 0, 4).copy_from_slice(b"abcd");
    let steps = connection.advance(Completion::Read(Ok(4)));
    assert_eq!(steps.into_array()[0], Some(Submission::Send));
    let chunk_len = pending_len(&connection);
    let steps = connection.advance(Completion::Read(Err(io::Error::other("disk"))));
    assert_eq!(steps.into_array(), [None, None]);
    let steps = connection.advance(Completion::Send(Ok(chunk_len)));
    assert_eq!(steps.into_array()[0], Some(Submission::Send));
    let mut error_frame = Vec::new();
    append_send(&connection, &mut error_frame, usize::MAX);
    match read_reply(&mut std::io::Cursor::new(error_frame)).unwrap() {
        Reply::Error(text) => assert_eq!(text, "read failed"),
        Reply::Info(_) => panic!("expected ERRO"),
    }
    assert_eq!(connection.take_error_log().as_deref(), Some("disk"));
}

fn pending_len(connection: &Connection) -> usize {
    let pending = connection.pending_send();
    pending
        .parts
        .into_iter()
        .take(pending.count)
        .map(|part| part.len())
        .sum()
}
