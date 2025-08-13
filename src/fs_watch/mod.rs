use crate::String;
use crate::fs_watch::async_channel::TX;
use std::ffi::{CStr, CString};
use std::fmt::{Debug, Formatter};
use std::fs::File;
use std::io;
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd};
use std::pin::Pin;
use std::rc::Rc;
use tokio::io::unix::AsyncFd;
use tokio::task::JoinHandle;

mod async_channel;
#[cfg(test)]
mod tests;

const EVENT_HEADER_SIZE: usize = size_of::<InotifyEventHeader>();
const EVENT_BUFFER_SIZE: usize = EVENT_HEADER_SIZE + libc::PATH_MAX as usize + 1;

pub(super) trait Event: Debug {
    fn file_name(&self) -> String;
    fn is_modify(&self) -> bool;
    #[allow(dead_code)]
    fn is_removal(&self) -> bool;
}
pub(super) trait Observer: Debug {
    type E: Event;
    fn next<'a>(&'a self) -> Pin<Box<dyn Future<Output = Self::E> + 'a>>;
}

#[repr(C)]
struct InotifyEventHeader {
    wd: libc::c_int,
    mask: libc::c_uint,
    cookie: libc::c_uint,
    len: libc::c_uint,
}

#[derive(Clone)]
pub(super) struct InotifyEvent {
    mask: u32,
    file_name: Option<String>,
}

impl InotifyEvent {
    fn from(buffer: &[u8]) -> Result<(Self, usize), ParseError> {
        if buffer.len() < EVENT_HEADER_SIZE {
            return Err(ParseError::NotEnoughBytes);
        }
        let event_header =
            unsafe { &*(buffer[0..EVENT_HEADER_SIZE].as_ptr() as *const InotifyEventHeader) };
        let mut file_name: Option<String> = None;
        let mut message_offset = EVENT_HEADER_SIZE;
        if event_header.len > 0 {
            message_offset += event_header.len as usize;
            if buffer.len() < message_offset {
                return Err(ParseError::NotEnoughBytes);
            }
            file_name = Some(
                CStr::from_bytes_until_nul(&buffer[EVENT_HEADER_SIZE..message_offset])
                    .unwrap()
                    .to_string_lossy()
                    .to_string(),
            );
        }
        Ok((
            Self {
                mask: event_header.mask,
                file_name,
            },
            message_offset,
        ))
    }
}

impl Debug for InotifyEvent {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let file_name = {
            if let Some(ref file_name) = self.file_name {
                file_name.clone()
            } else {
                String::new()
            }
        };
        write! {f, "<InotifyEvent: mask=0x{:x}, file_name='{}'>", self.mask, file_name}
    }
}

impl Event for InotifyEvent {
    fn file_name(&self) -> String {
        if self.file_name.is_some() {
            self.file_name.clone().unwrap()
        } else {
            String::new()
        }
    }
    fn is_modify(&self) -> bool {
        (self.mask & (libc::IN_MOVED_TO | libc::IN_CLOSE_WRITE)) > 0
    }

    fn is_removal(&self) -> bool {
        (self.mask & libc::IN_DELETE) > 0
    }
}

pub(super) enum ParseError {
    NotEnoughBytes,
}

pub(super) struct INotifyObserver {
    fd: Rc<AsyncFd<File>>,
    wd: i32,
    join_handle: JoinHandle<()>,
    rx: async_channel::RX<InotifyEvent>,
    display: String,
}

impl Observer for INotifyObserver {
    type E = InotifyEvent;

    fn next<'a>(&'a self) -> Pin<Box<dyn Future<Output = Self::E> + 'a>> {
        Box::pin(self.rx.next())
    }
}

impl Debug for INotifyObserver {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write! {f, "<Observer on: {:?}>", self.display}
    }
}

pub struct Watch(u32);

impl Watch {
    pub(super) fn new() -> Self {
        Watch(0)
    }
    pub(super) fn change(self) -> Self {
        Self(self.0 | libc::IN_CLOSE_WRITE | libc::IN_MOVED_TO)
    }

    #[allow(dead_code)]
    pub(super) fn removal(self) -> Self {
        Self(self.0 | libc::IN_DELETE)
    }

    pub(super) fn observe(&self, directory: &str) -> io::Result<INotifyObserver> {
        eprintln!("Observe {directory}");
        let path = CString::new(directory)?;
        let raw_fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK) };
        if raw_fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let wd = unsafe { libc::inotify_add_watch(raw_fd, path.as_ptr(), self.0) };
        if wd < 0 {
            return Err(io::Error::last_os_error());
        }
        let file = unsafe { File::from_raw_fd(raw_fd) };
        let (tx, rx) = async_channel::new::<InotifyEvent>();
        let async_fd = Rc::new(AsyncFd::new(file)?);
        let join_handle = tokio::task::spawn_local(read_loop(async_fd.clone(), tx));
        Ok(INotifyObserver {
            fd: async_fd,
            wd,
            join_handle,
            rx,
            display: directory.to_string(),
        })
    }
}

impl Drop for INotifyObserver {
    fn drop(&mut self) {
        self.join_handle.abort();
        let result = unsafe { libc::inotify_rm_watch(self.fd.as_raw_fd(), self.wd) };
        if result != 0 {
            eprintln!(
                "Error closing the fs_watch fd: {}",
                std::io::Error::last_os_error()
            );
        }
    }
}

async fn read_loop(fd: Rc<AsyncFd<File>>, mut tx: TX<InotifyEvent>) {
    let mut buffer: [u8; EVENT_BUFFER_SIZE] = [0; EVENT_BUFFER_SIZE];
    loop {
        let mut guard = match fd.readable().await {
            Ok(guard) => guard,
            Err(error) => panic!("Error reading from fs_watch fd: {error}"),
        };
        match guard.try_io(|inner| inner.get_ref().read(&mut buffer)) {
            Ok(Ok(0)) => return,
            Ok(Ok(read_bytes)) => {
                for event in parse_events(&buffer, read_bytes) {
                    eprintln!("Sending fs_watch event: {event:?} ...");
                    tx.push(event);
                }
            }
            Ok(Err(error)) => {
                panic!("Error reading from fs_watch fd: {error}")
            }
            Err(_try_io_error) => continue,
        }
    }
}

fn parse_events(buffer: &[u8], bytes_read: usize) -> Vec<InotifyEvent> {
    let mut result = Vec::new();
    let mut offset: usize = 0;
    loop {
        match InotifyEvent::from(&buffer[offset..bytes_read]) {
            Ok((event, event_size)) => {
                offset += event_size;
                result.push(event);
            }
            Err(ParseError::NotEnoughBytes) => {
                return result;
            }
        };
    }
}
