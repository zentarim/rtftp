use crate::fs::{FileError, OpenedFile, Root};
use crate::guestfs::GuestFSError;
use crate::nbd_disk::{NBDFileReader, Partition};
use serde::Deserialize;
use serde_json::Value;
use std::fmt::{Debug, Display, Formatter};
use std::path::PathBuf;

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
            Ok(opened_file) => Ok(Box::new(opened_file)),
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

    fn open(&self, absolute_path: &str) -> Result<NBDFileReader, FileError>;
}

pub(super) trait Config<'a>: Deserialize<'a> {
    fn from_json(value: &Value) -> Option<Self>;

    fn connect(&self) -> Result<Box<dyn Root>, VirtualRootError>;
}

#[derive(Debug)]
pub(super) enum VirtualRootError {
    ConfigError(String),
    SetupError(GuestFSError),
}
