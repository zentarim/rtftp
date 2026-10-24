use super::{OP_READ, SLOT_COUNT, pack_userdata, serve_sockets, unpack_userdata};
use file_protocol::{Reply, read_chunk, read_reply, write_request};
use std::fs::{self, File};
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::symlink;
use std::path::PathBuf;
use std::thread;

fn scratch(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "rtftp-storekeeper-ring-{}-{}-{name}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&path).unwrap();
    path
}

fn open_directory(path: &str) -> OwnedFd {
    File::open(path)
        .unwrap_or_else(|error| panic!("open {path}: {error}"))
        .into()
}

fn socketpair() -> (OwnedFd, OwnedFd) {
    let mut fds = [0; 2];
    let result = unsafe {
        libc::socketpair(
            libc::AF_UNIX,
            libc::SOCK_STREAM | libc::SOCK_CLOEXEC,
            0,
            fds.as_mut_ptr(),
        )
    };
    assert_eq!(result, 0, "{}", std::io::Error::last_os_error());
    // SAFETY: socketpair filled both descriptors and this test owns them.
    unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) }
}

fn set_timeouts(fd: std::os::fd::RawFd) {
    let timeout = libc::timeval {
        tv_sec: 5,
        tv_usec: 0,
    };
    let result = unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_RCVTIMEO,
            &timeout as *const libc::timeval as *const libc::c_void,
            size_of::<libc::timeval>() as libc::socklen_t,
        )
    };
    assert_eq!(result, 0, "{}", std::io::Error::last_os_error());
    let result = unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_SNDTIMEO,
            &timeout as *const libc::timeval as *const libc::c_void,
            size_of::<libc::timeval>() as libc::socklen_t,
        )
    };
    assert_eq!(result, 0, "{}", std::io::Error::last_os_error());
}

fn exchange(socket: OwnedFd, path: &str, chunk_size: usize) -> Vec<u8> {
    set_timeouts(socket.as_raw_fd());
    let mut file = File::from(socket);
    write_request(&mut file, path, chunk_size).unwrap();
    let mut output = Vec::new();
    file.read_to_end(&mut output).unwrap();
    output
}

#[test]
fn userdata_round_trip_covers_every_slot() {
    for slot in 0..SLOT_COUNT {
        let packed = pack_userdata(0x1_0000, slot, OP_READ);
        assert_eq!(unpack_userdata(packed), (0x1_0000, slot, OP_READ));
    }
}

#[test]
fn ring_serves_a_file_and_a_missing_path() {
    let dir = scratch("ring");
    let payload_path = dir.join("payload");
    let payload = b"a\0b\0c";
    fs::write(&payload_path, payload).unwrap();
    let missing = dir.join("absent");
    let (client_ok, server_ok) = socketpair();
    let (client_missing, server_missing) = socketpair();
    let root = open_directory("/");
    let server = thread::spawn(move || serve_sockets(root, vec![server_ok, server_missing]));

    let ok_bytes = exchange(client_ok, payload_path.to_str().unwrap(), 2);
    let mut cursor = std::io::Cursor::new(ok_bytes);
    match read_reply(&mut cursor).unwrap() {
        Reply::Info(size) => assert_eq!(size, payload.len() as u64),
        Reply::Error(text) => panic!("{text}"),
    }
    let mut collected = Vec::new();
    loop {
        let chunk = read_chunk(&mut cursor).unwrap();
        if chunk.is_empty() {
            break;
        }
        collected.extend_from_slice(&chunk);
    }
    assert_eq!(collected, payload);

    let missing_bytes = exchange(client_missing, missing.to_str().unwrap(), 4);
    match read_reply(&mut std::io::Cursor::new(missing_bytes)).unwrap() {
        Reply::Error(text) => assert_eq!(text, file_protocol::FILE_NOT_FOUND),
        Reply::Info(_) => panic!("expected ERRO"),
    }
    server.join().unwrap().unwrap();
}

#[test]
fn symlink_stays_inside_the_root_fd() {
    let root = scratch("root");
    let outside = scratch("outside");
    let secret = b"secret-outside";
    fs::write(outside.join("secret"), secret).unwrap();
    symlink(outside.join("secret"), root.join("link")).unwrap();
    fs::write(root.join("inside"), b"inside").unwrap();

    let (client_link, server_link) = socketpair();
    let (client_inside, server_inside) = socketpair();
    let root_fd = open_directory(root.to_str().unwrap());
    let server = thread::spawn(move || serve_sockets(root_fd, vec![server_link, server_inside]));

    let link_bytes = exchange(client_link, "/link", 8);
    match read_reply(&mut std::io::Cursor::new(&link_bytes)).unwrap() {
        Reply::Error(text) => assert_eq!(text, file_protocol::FILE_NOT_FOUND),
        Reply::Info(_) => panic!("symlink escaped the root: {link_bytes:?}"),
    }

    let inside_bytes = exchange(client_inside, "/inside", 8);
    let mut cursor = std::io::Cursor::new(inside_bytes);
    match read_reply(&mut cursor).unwrap() {
        Reply::Info(size) => assert_eq!(size, 6),
        Reply::Error(text) => panic!("{text}"),
    }
    let mut collected = Vec::new();
    loop {
        let chunk = read_chunk(&mut cursor).unwrap();
        if chunk.is_empty() {
            break;
        }
        collected.extend_from_slice(&chunk);
    }
    assert_eq!(collected, b"inside");
    server.join().unwrap().unwrap();
}
