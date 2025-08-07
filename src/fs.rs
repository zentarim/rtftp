use std::fmt::{Debug, Display};

#[derive(Debug, PartialEq)]
pub(super) enum FileError {
    FileNotFound,
    AccessViolation,
    ReadError,
    UnknownError(String),
}

pub(super) trait OpenedFile: Display + Debug {
    fn read_to(&mut self, buffer: &mut [u8]) -> Result<usize, FileError>;

    fn get_size(&mut self) -> Result<usize, FileError>;
}

pub(super) trait Root: Display + Debug {
    fn open(&self, path: &str) -> Result<Box<dyn OpenedFile>, FileError>;
}
