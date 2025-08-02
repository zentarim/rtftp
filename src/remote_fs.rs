use crate::fs::{FileError, OpenedFile, Root};
use crate::guestfs::GuestFSError;
use crate::nbd_disk::{NBDFileReader, Partition};
use std::fmt::{Debug, Display, Formatter};
use std::path::PathBuf;

pub(super) struct RemoteChroot {
    disk: Box<dyn ConnectedDisk>,
    path: PathBuf,
}

impl RemoteChroot {
    pub(super) fn new(disk: Box<dyn ConnectedDisk>, path: &str) -> Self {
        Self {
            disk,
            path: PathBuf::from(path),
        }
    }
}

impl Root for RemoteChroot {
    fn open(&self, path: &str) -> Result<Box<dyn OpenedFile>, FileError> {
        match self.disk.open(self.path.join(path).to_str().unwrap()) {
            Ok(opened_file) => Ok(Box::new(opened_file)),
            Err(err) => Err(err),
        }
    }
}

impl Debug for RemoteChroot {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write! {f, "<{:?} in {}>", self.path, self.disk}
    }
}

impl Display for RemoteChroot {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write! {f, "<{:?} in {}>", self.path, self.disk}
    }
}

pub(super) trait ConnectedDisk: Display {
    fn list_partitions(&mut self) -> Result<Vec<Partition>, GuestFSError>;

    fn open(&self, absolute_path: &str) -> Result<NBDFileReader, FileError>;
}
