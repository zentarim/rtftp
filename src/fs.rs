use crate::local_fs::LocalRoot;
use crate::remote_fs::RemoteRoot;
use std::fmt::{Debug, Display};
use std::io;

pub(super) trait OpenedFile: Display + Debug {
    fn read_to(&mut self, buffer: &mut [u8]) -> io::Result<usize>;

    fn get_size(&mut self) -> io::Result<usize>;
}

pub(super) trait Root: Display + Debug {
    type OpenedFile: OpenedFile;
    fn open(&self, path: &str) -> io::Result<Self::OpenedFile>;
}

pub(super) enum RootKind {
    Local(LocalRoot),
    Remote(RemoteRoot),
}
