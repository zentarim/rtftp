use std::fmt::{Display, Formatter};
use std::io;

#[cfg(test)]
mod tests;

pub(super) struct ReadCursor<'a> {
    datagram: &'a [u8],
    index: usize,
}

impl<'a> ReadCursor<'a> {
    pub(super) fn new(datagram: &'a [u8]) -> Self {
        Self { datagram, index: 0 }
    }

    pub(super) fn extract_ushort(&mut self) -> io::Result<u16> {
        let end_index = self.index + 2;
        if end_index > self.datagram.len() {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        let result = u16::from_be_bytes([self.datagram[self.index], self.datagram[self.index + 1]]);
        self.index = end_index;
        Ok(result)
    }

    pub(super) fn extract_string(&mut self) -> io::Result<String> {
        if self.index >= self.datagram.len() {
            return Err(io::ErrorKind::UnexpectedEof.into());
        };
        if let Some(relative_null_index) = self.datagram[self.index..].iter().position(|&b| b == 0)
        {
            let absolute_null_index = self.index + relative_null_index;
            match String::from_utf8(self.datagram[self.index..absolute_null_index].to_vec()) {
                Ok(string) => {
                    self.index = absolute_null_index + 1;
                    Ok(string)
                }
                Err(_) => Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Can't parse UTF-8",
                )),
            }
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Null-terminated string is not found",
            ))
        }
    }
}

pub(super) struct WriteCursor<'a> {
    buffer: &'a mut [u8],
    offset: usize,
}

impl<'a> WriteCursor<'a> {
    pub fn new(buffer: &'a mut [u8]) -> Self {
        Self { buffer, offset: 0 }
    }

    pub fn put_ushort(&mut self, value: u16) -> Result<usize, BufferError> {
        let end_index = self.offset + 2;
        if end_index > self.buffer.len() {
            return Err(BufferError::new("Too little data left to write u16"));
        }
        self.buffer[self.offset] = ((value & 0xFF00) >> 8) as u8;
        self.buffer[self.offset + 1] = (value & 0xFF) as u8;
        self.offset = end_index;
        Ok(self.offset)
    }

    pub fn put_string(&mut self, string: &str) -> Result<usize, BufferError> {
        let string_size = string.len();
        let end_index = self.offset + string_size + 1;
        if end_index > self.buffer.len() {
            return Err(BufferError::new(&format!(
                "Too little data left to write a string {string_size} bytes size"
            )));
        }
        self.buffer[self.offset..end_index - 1].copy_from_slice(string.as_bytes());
        self.buffer[end_index] = 0x0;
        self.offset = end_index;
        Ok(self.offset)
    }
}

#[derive(Debug, PartialEq)]
pub(super) struct BufferError {
    message: String,
}

impl Display for BufferError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "<{}>", self.message)
    }
}

impl std::error::Error for BufferError {}

impl BufferError {
    pub fn new(message: &str) -> Self {
        Self {
            message: message.to_string(),
        }
    }
}
