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
