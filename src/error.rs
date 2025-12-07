use crate::cursor::{BufferError, WriteCursor};
use std::fmt;
use std::fmt::Display;

pub(super) const ERROR: u16 = 0x05;
const UNDEFINED_ERROR: u16 = 0x00;

const FILE_NOT_FOUND: u16 = 0x01;

const ACCESS_VIOLATION: u16 = 0x02;
const ILLEGAL_OPERATION: u16 = 0x04;

#[derive(Debug)]
pub(super) enum TFTPError {
    UndefinedError(String),
    FileNotFound(String),
    AccessViolation(String),
    IllegalOperation(String),
}

impl TFTPError {
    pub(super) fn undefined<M: Into<String>>(message: M) -> Self {
        Self::UndefinedError(message.into())
    }

    pub(super) fn file_not_found() -> Self {
        Self::FileNotFound("File not found".to_string())
    }

    pub(super) fn access_violation() -> Self {
        Self::AccessViolation("Access violation".to_string())
    }

    pub(super) fn illegal_operation<M: Into<String>>(message: M) -> Self {
        Self::IllegalOperation(message.into())
    }

    pub(super) fn serialize(&self, buffer: &mut [u8]) -> Result<usize, BufferError> {
        let mut cursor = WriteCursor::new(buffer);
        let (code, message) = self.parse();
        cursor.put_ushort(ERROR)?;
        cursor.put_ushort(code)?;
        cursor.put_string(message)
    }

    fn parse(&self) -> (u16, &str) {
        match self {
            TFTPError::UndefinedError(string) => (UNDEFINED_ERROR, string),
            TFTPError::FileNotFound(string) => (FILE_NOT_FOUND, string),
            TFTPError::AccessViolation(string) => (ACCESS_VIOLATION, string),
            TFTPError::IllegalOperation(string) => (ILLEGAL_OPERATION, string),
        }
    }
}

impl Display for TFTPError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (code, message) = self.parse();
        write!(f, "TFTP ERROR: [0x{:02x}] {}", code, message)
    }
}

impl std::error::Error for TFTPError {}
