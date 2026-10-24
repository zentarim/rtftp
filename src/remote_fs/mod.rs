use crate::fs::{OpenedFile, Root, contained_join};
use crate::vm::{Vm, VmError, connect_client};
use file_protocol::{FILE_NOT_FOUND, MAX_CHUNK_SIZE, Reply, read_chunk, read_reply, write_request};
use serde::Deserialize;
use serde_json::Value;
use std::fmt::{Debug, Display, Formatter};
use std::io::{self, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

#[cfg(test)]
mod tests;

pub(super) struct RemoteRoot {
    disk: ConnectedDisk,
    chroot_path: PathBuf,
}

impl RemoteRoot {
    pub(super) fn new(disk: ConnectedDisk, chroot_path: &str) -> Self {
        Self {
            disk,
            chroot_path: PathBuf::from(chroot_path),
        }
    }
}

impl Root for RemoteRoot {
    type OpenedFile = FileReader;
    fn open(&self, path: &str) -> io::Result<Self::OpenedFile> {
        let full_path = contained_join(&self.chroot_path, path)?;
        self.disk.open(full_path.to_str().unwrap())
    }
}

impl Debug for RemoteRoot {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "<{:?} in {}>", self.chroot_path, self.disk)
    }
}

impl Display for RemoteRoot {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "<{:?} in {}>", self.chroot_path, self.disk)
    }
}

pub(super) trait Config<'a>: Deserialize<'a> {
    fn from_json(value: &Value) -> Option<Self>;

    fn connect(&self, peer: &str) -> Result<RemoteRoot, VirtualRootError>;
}

#[derive(Debug)]
pub(super) enum VirtualRootError {
    ConfigError(String),
    SetupError(VmError),
}

#[derive(Debug, Deserialize)]
pub(super) struct Mount {
    partition: usize,
    mountpoint: String,
}

impl Mount {
    pub(super) fn partition(&self) -> usize {
        self.partition
    }

    pub(super) fn mountpoint(&self) -> &str {
        &self.mountpoint
    }
}

struct FileChunk {
    buffer: Vec<u8>,
    offset: usize,
}

impl Debug for FileChunk {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "<FileChunk {}, offset {}>",
            self.buffer.len(),
            self.offset
        )
    }
}

impl FileChunk {
    fn new(buffer: Vec<u8>) -> Self {
        Self { buffer, offset: 0 }
    }

    fn fill(&mut self, buffer: &mut [u8]) -> usize {
        let available_bytes = &self.buffer[self.offset..];
        if available_bytes.is_empty() {
            return 0;
        }
        if available_bytes.len() <= buffer.len() {
            buffer[..available_bytes.len()].copy_from_slice(available_bytes);
            self.offset += available_bytes.len();
            available_bytes.len()
        } else {
            buffer.copy_from_slice(&available_bytes[..buffer.len()]);
            self.offset += buffer.len();
            buffer.len()
        }
    }
}

#[derive(Debug)]
pub(super) struct FileReader {
    stream: UnixStream,
    file_size: usize,
    current_offset: usize,
    chunk: FileChunk,
    display: String,
}

impl FileReader {
    fn open(vsock_path: &std::path::Path, path: &str, display: String) -> io::Result<Self> {
        let mut stream = connect_client(vsock_path)?;
        write_request(&mut stream, path, MAX_CHUNK_SIZE)?;
        stream.flush()?;
        let file_size = match read_reply(&mut stream)? {
            Reply::Info(size) => {
                usize::try_from(size).map_err(|_| io::Error::other("file size exceeds usize"))?
            }
            Reply::Error(text) if text == FILE_NOT_FOUND => {
                return Err(io::ErrorKind::NotFound.into());
            }
            Reply::Error(text) => return Err(io::Error::other(text)),
        };
        let first_chunk = read_chunk(&mut stream)?;
        Ok(Self {
            stream,
            file_size,
            current_offset: 0,
            chunk: FileChunk::new(first_chunk),
            display,
        })
    }

    fn buffer_new_chunk(&mut self) -> io::Result<bool> {
        let next_chunk = read_chunk(&mut self.stream)?;
        if next_chunk.is_empty() {
            Ok(false)
        } else {
            self.chunk = FileChunk::new(next_chunk);
            Ok(true)
        }
    }
}

impl Display for FileReader {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.display)
    }
}

impl OpenedFile for FileReader {
    fn read_to(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let mut read = 0;
        while self.current_offset < self.file_size && read < buffer.len() {
            let copied = self.chunk.fill(&mut buffer[read..]);
            if copied == 0 {
                let chunk_has_data = self.buffer_new_chunk()?;
                if !chunk_has_data {
                    break;
                }
            }
            read += copied;
            self.current_offset += copied;
        }
        Ok(read)
    }

    fn get_size(&mut self) -> io::Result<usize> {
        Ok(self.file_size)
    }
}

#[derive(Debug)]
pub(super) struct ConnectedDisk {
    vm: Option<Box<Vm>>,
    url: String,
}

impl Display for ConnectedDisk {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "<NBDDisk: {}>", self.url)
    }
}

impl ConnectedDisk {
    pub(super) fn new(vm: Vm, url: String) -> Self {
        Self {
            vm: Some(Box::new(vm)),
            url,
        }
    }

    #[cfg(test)]
    pub(super) fn unconnected(url: String) -> Self {
        Self { vm: None, url }
    }

    pub(super) fn open(&self, absolute_path: &str) -> io::Result<FileReader> {
        let Some(vm) = &self.vm else {
            return Err(io::Error::other("disk is not connected"));
        };
        let display = format!("<{absolute_path} on {self}>");
        FileReader::open(vm.vsock_path(), absolute_path, display)
    }
}
