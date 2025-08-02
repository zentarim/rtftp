use crate::cursor::{BufferError, ParseError, ReadCursor, WriteCursor};
use crate::fs::{FileError, OpenedFile, Root};
use std::collections::HashMap;
use std::fmt;
use std::fmt::{Debug, Display};

const RRQ: u16 = 0x01;
const ERROR: u16 = 0x05;
const OACK: u16 = 0x06;
pub(super) const UNDEFINED_ERROR: u16 = 0x00;

pub(super) const ILLEGAL_OPERATION: u16 = 0x04;
static OCTET: &str = "octet";

#[derive(Debug)]
pub(super) struct TFTPError {
    message: String,
    error_code: u16,
}

impl TFTPError {
    pub(super) fn new<M: Into<String>>(message: M, error_code: u16) -> Self {
        Self {
            message: message.into(),
            error_code,
        }
    }

    pub(super) fn serialize(&self, buffer: &mut [u8]) -> Result<usize, BufferError> {
        let mut cursor = WriteCursor::new(buffer);
        cursor.put_ushort(ERROR)?;
        cursor.put_ushort(self.error_code)?;
        cursor.put_string(self.message.as_str())
    }
}

impl Display for TFTPError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "TFTP ERROR: [0x{:02x}] {}",
            self.error_code, self.message
        )
    }
}

impl std::error::Error for TFTPError {}

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
            .map_err(|_| TFTPError::new("Bad format", UNDEFINED_ERROR))?;
        if opcode != RRQ {
            return Err(TFTPError::new("Only RRQ is supported", ILLEGAL_OPERATION));
        }
        let filename = cursor
            .extract_string()
            .map_err(|_| TFTPError::new("Can't obtain filename", UNDEFINED_ERROR))?;
        if let Ok(mode) = cursor.extract_string() {
            if mode != OCTET {
                if mode.is_empty() {
                    return Err(TFTPError::new("Bad format", UNDEFINED_ERROR));
                }
                return Err(TFTPError::new(
                    "Only octet mode is supported",
                    UNDEFINED_ERROR,
                ));
            }
        } else {
            return Err(TFTPError::new("Bad format", UNDEFINED_ERROR));
        }
        let mut options: HashMap<String, String> = HashMap::new();
        loop {
            let option_name = match cursor.extract_string() {
                Ok(name) => name,
                Err(ParseError::NotEnoughData) => break,
                Err(ParseError::Generic(_error)) => {
                    return Err(TFTPError::new("Bad format", UNDEFINED_ERROR));
                }
            };
            let option_value = match cursor.extract_string() {
                Ok(name) => name,
                Err(_) => return Err(TFTPError::new("Bad format", UNDEFINED_ERROR)),
            };
            options.insert(option_name, option_value);
        }
        Ok(ReadRequest { filename, options })
    }
    pub(super) fn open_in(&self, filesystem: &dyn Root) -> Result<Box<dyn OpenedFile>, FileError> {
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

    pub(super) fn serialize(&self, buffer: &mut [u8]) -> Result<(usize, u16), BufferError> {
        if buffer.is_empty() {
            return Ok((0, 0));
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
        Ok((offset, 0))
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
        let repr = self
            .options
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join(",");
        write!(f, "OACK: [{repr}]")
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn parse_rrq() {
        let filename = "irrelevant.file";
        let binding = vec![
            RRQ.to_be_bytes().to_vec(),
            filename.as_bytes().to_vec(),
            vec![0x00],
            OCTET.as_bytes().to_vec(),
            vec![0x00],
        ];
        let raw: Vec<u8> = binding.iter().flatten().copied().collect();
        let rrq = ReadRequest::parse(&raw);
        assert!(rrq.is_ok());
    }
    #[test]
    fn parse_incomplete_rrq() {
        let filename = "irrelevant.file";
        let binding = vec![
            RRQ.to_be_bytes().to_vec(),
            filename.as_bytes().to_vec(),
            vec![0x00],
        ];
        let raw: Vec<u8> = binding.iter().flatten().copied().collect();
        let error = ReadRequest::parse(&raw).err().unwrap();
        assert!(error.to_string().contains("Bad format"));
    }

    #[test]
    fn parse_empty_rrq() {
        let error = ReadRequest::parse(&vec![]).err().unwrap();
        assert!(error.to_string().contains("Bad format"));
    }
}
