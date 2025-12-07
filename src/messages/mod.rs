use crate::cursor::{BufferError, ReadCursor, WriteCursor};
use crate::error::TFTPError;
use crate::fs::{OpenedFile, Root};
use std::collections::HashMap;
use std::fmt::{Debug, Display};
use std::{fmt, io};

#[cfg(test)]
mod tests;

const RRQ: u16 = 0x01;
const OACK: u16 = 0x06;
static OCTET: &str = "octet";

pub(super) struct ReadRequest {
    filename: String,
    pub options: HashMap<String, String>,
}

impl Display for ReadRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RRQ: '{}' ({:?})", self.filename, self.options)
    }
}

impl Debug for ReadRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RRQ: '{}' ({:?})", self.filename, self.options)
    }
}

impl ReadRequest {
    pub(super) fn parse(raw: &[u8]) -> Result<Self, TFTPError> {
        let mut cursor = ReadCursor::new(raw);
        let opcode = cursor
            .extract_ushort()
            .map_err(|_| TFTPError::undefined("Bad format"))?;
        if opcode != RRQ {
            return Err(TFTPError::illegal_operation("Only RRQ is supported"));
        }
        let filename = cursor
            .extract_string()
            .map_err(|_| TFTPError::undefined("Can't obtain filename"))?;
        if let Ok(mode) = cursor.extract_string() {
            if mode != OCTET {
                if mode.is_empty() {
                    return Err(TFTPError::undefined("Bad format"));
                }
                return Err(TFTPError::undefined("Only octet mode is supported"));
            }
        } else {
            return Err(TFTPError::undefined("Bad format"));
        }
        let mut options: HashMap<String, String> = HashMap::new();
        loop {
            let option_name = match cursor.extract_string() {
                Ok(name) => name,
                Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(_error) => {
                    return Err(TFTPError::undefined("Bad format"));
                }
            };
            let option_value = match cursor.extract_string() {
                Ok(name) => name,
                Err(_) => return Err(TFTPError::undefined("Bad format")),
            };
            options.insert(option_name, option_value);
        }
        Ok(ReadRequest { filename, options })
    }
    pub(super) fn open_in(&self, filesystem: &dyn Root) -> io::Result<Box<dyn OpenedFile>> {
        let normalized_path = self.filename.trim_start_matches('/');
        eprintln!("Opening {normalized_path} in {filesystem} ...");
        filesystem.open(normalized_path)
    }
}

#[derive(Debug)]
pub(super) struct OptionsAcknowledge {
    options: Vec<(String, String)>,
}

impl OptionsAcknowledge {
    pub fn new() -> Self {
        Self {
            options: Vec::new(),
        }
    }

    pub(super) fn serialize(&self, buffer: &mut [u8]) -> Result<usize, BufferError> {
        if buffer.is_empty() {
            return Ok(0);
        }
        let mut datagram = WriteCursor::new(buffer);
        datagram.put_ushort(OACK)?;
        let offset = {
            let mut offset: usize = 0;
            for (key, value) in &self.options {
                datagram.put_string(key.as_str())?;
                offset = datagram.put_string(value.as_str())?;
            }
            offset
        };
        Ok(offset)
    }
    pub fn push(&mut self, option: (String, String)) {
        self.options.push(option)
    }

    pub fn has_options(&self) -> bool {
        !self.options.is_empty()
    }
}

impl Display for OptionsAcknowledge {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let display = self
            .options
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join(",");
        write!(f, "OACK: [{display}]")
    }
}
