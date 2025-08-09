use crate::fs::{FileError, OpenedFile, Root};
use crate::guestfs::{GuestFS, GuestFSError};
use serde::Deserialize;
use serde_json::Value;
use std::fmt::{Debug, Display, Formatter};
use std::path::PathBuf;
use std::rc::Rc;

pub(super) struct RemoteChroot<T: ConnectedDisk> {
    disk: T,
    path: PathBuf,
}

impl<T: ConnectedDisk> RemoteChroot<T> {
    pub(super) fn new(disk: T, path: &str) -> Self {
        Self {
            disk,
            path: PathBuf::from(path),
        }
    }
}

impl<T: ConnectedDisk> Root for RemoteChroot<T> {
    fn open(&self, path: &str) -> Result<Box<dyn OpenedFile>, FileError> {
        match self.disk.open(self.path.join(path).to_str().unwrap()) {
            Ok(opened_file) => Ok(opened_file),
            Err(err) => Err(err),
        }
    }
}

impl<T: ConnectedDisk> Debug for RemoteChroot<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write! {f, "<{:?} in {}>", self.path, self.disk}
    }
}

impl<T: ConnectedDisk> Display for RemoteChroot<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write! {f, "<{:?} in {}>", self.path, self.disk}
    }
}

pub(super) trait ConnectedDisk: Display {
    fn list_partitions(&mut self) -> Result<Vec<Partition>, GuestFSError>;

    fn open(&self, absolute_path: &str) -> Result<Box<dyn OpenedFile>, FileError>;
}

pub(super) trait Config<'a>: Deserialize<'a> {
    type ConnectedRoot: Root;
    fn from_json(value: &Value) -> Option<Self>;

    fn connect(&self) -> Result<Self::ConnectedRoot, VirtualRootError>;
}

#[derive(Debug)]
pub(super) enum VirtualRootError {
    ConfigError(String),
    SetupError(GuestFSError),
}

pub(super) struct Partition {
    handle: Rc<GuestFS>,
    device: String,
}

impl Partition {
    pub(crate) fn new(handle: Rc<GuestFS>, device: String) -> Self {
        Self { handle, device }
    }
}

impl Display for Partition {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write! {f, "<Partition: {}>", self.device}
    }
}

impl Debug for Partition {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write! {f, "<Partition: {}>", self.device}
    }
}

impl Partition {
    pub(crate) fn mount_ro(&self, mountpoint: &str) -> Result<(), GuestFSError> {
        eprintln!("{self}: Mounting to {mountpoint}");
        self.handle.mount_ro(self.device.as_str(), mountpoint)
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct Mount {
    partition: usize,
    mountpoint: String,
}

impl Mount {
    pub(super) fn mount_suitable(&self, available: &[Partition]) -> Result<(), VirtualRootError> {
        if let Some(partition) = available.get(self.partition - 1) {
            if let Err(guestfs_error) = partition.mount_ro(self.mountpoint.as_str()) {
                Err(VirtualRootError::SetupError(guestfs_error))
            } else {
                Ok(())
            }
        } else {
            Err(VirtualRootError::ConfigError(format!(
                "Can't find a config for partition {}",
                self.partition
            )))
        }
    }
}

pub(super) struct FileChunk {
    buffer: Vec<u8>,
    offset: usize,
}

impl Debug for FileChunk {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "<FileChunk {}, offset {}>",
            self.buffer.len(),
            self.offset
        )
    }
}

impl FileChunk {
    pub(super) fn new(buffer: Vec<u8>) -> Self {
        Self { buffer, offset: 0 }
    }
    pub(super) fn fill(&mut self, buffer: &mut [u8]) -> usize {
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
    handle: Rc<GuestFS>,
    path: String,
    file_size: usize,
    current_offset: usize,
    chunk: FileChunk,
    display: String,
}

impl FileReader {
    pub(super) fn open(
        handle: Rc<GuestFS>,
        path: String,
        file_size: usize,
        display: String,
    ) -> Result<Self, GuestFSError> {
        let first_chunk = handle.read_chunk(&path, 0)?;
        Ok(Self {
            handle,
            path,
            file_size,
            current_offset: 0,
            chunk: FileChunk::new(first_chunk),
            display,
        })
    }

    fn buffer_new_chunk(&mut self) -> Result<bool, GuestFSError> {
        let next_chunk = self
            .handle
            .read_chunk(self.path.as_str(), self.current_offset)?;
        if next_chunk.is_empty() {
            Ok(false)
        } else {
            self.chunk = FileChunk::new(next_chunk);
            Ok(true)
        }
    }
}

impl Display for FileReader {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write! {f, "{}", self.display}
    }
}

impl OpenedFile for FileReader {
    fn read_to(&mut self, buffer: &mut [u8]) -> Result<usize, FileError> {
        let mut read: usize = 0;
        while self.current_offset < self.file_size && read < buffer.len() {
            let copied = self.chunk.fill(&mut buffer[read..]);
            if copied == 0 {
                let chunk_has_data = match self.buffer_new_chunk() {
                    Ok(result) => result,
                    Err(guestfs_error) => {
                        return Err(FileError::UnknownError(guestfs_error.to_string()));
                    }
                };
                if !chunk_has_data {
                    break;
                }
            };
            read += copied;
            self.current_offset += copied;
        }
        Ok(read)
    }

    fn get_size(&mut self) -> Result<usize, FileError> {
        Ok(self.file_size)
    }
}
