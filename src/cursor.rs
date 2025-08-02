use std::fmt::{Display, Formatter};

pub(super) struct ReadCursor<'a> {
    datagram: &'a [u8],
    index: usize,
}

impl<'a> ReadCursor<'a> {
    pub(super) fn new(datagram: &'a [u8]) -> Self {
        Self { datagram, index: 0 }
    }

    pub(super) fn extract_ushort(&mut self) -> Result<u16, ParseError> {
        let end_index = self.index + 2;
        if end_index > self.datagram.len() {
            return Err(ParseError::NotEnoughData);
        }
        let result = u16::from_be_bytes([self.datagram[self.index], self.datagram[self.index + 1]]);
        self.index = end_index;
        Ok(result)
    }

    pub(super) fn extract_string(&mut self) -> Result<String, ParseError> {
        if self.index >= self.datagram.len() {
            return Err(ParseError::NotEnoughData);
        };
        if let Some(relative_null_index) = self.datagram[self.index..].iter().position(|&b| b == 0)
        {
            let absolute_null_index = self.index + relative_null_index;
            match String::from_utf8(self.datagram[self.index..absolute_null_index].to_vec()) {
                Ok(string) => {
                    self.index = absolute_null_index + 1;
                    Ok(string)
                }
                Err(_) => Err(ParseError::generic("Can't parse UTF-8")),
            }
        } else {
            Err(ParseError::generic("Null-terminated string is not found"))
        }
    }
}

#[derive(Debug)]
pub(super) enum ParseError {
    Generic(String),
    NotEnoughData,
}

impl ParseError {
    pub fn generic<T: Into<String>>(msg: T) -> Self {
        ParseError::Generic(msg.into())
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
        self.buffer[self.offset..end_index].copy_from_slice(&value.to_be_bytes());
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_ushort() {
        let buffer: Vec<u8> = vec![0x00, 0x0A, 0x00, 0x00, 0x00, 0xab, 0xcd, 0xef];
        let mut cursor = ReadCursor::new(&buffer);
        let result = cursor.extract_ushort();
        assert_eq!(result.unwrap(), 0x0A);
    }

    #[test]
    fn extract_ushort_not_enough_data() {
        let buffer: Vec<u8> = vec![0x00, 0x0A, 0xFF];
        let mut cursor = ReadCursor::new(&buffer);
        cursor.extract_ushort().unwrap();
        let result = cursor.extract_ushort();
        assert!(matches!(result.unwrap_err(), ParseError::NotEnoughData));
    }

    #[test]
    fn extract_string() {
        let buffer: Vec<u8> = b"Arbitrary_string\x00\x0A".to_vec();
        let mut cursor = ReadCursor::new(&buffer);
        let result = cursor.extract_string();
        assert_eq!(result.unwrap(), "Arbitrary_string");
    }

    #[test]
    fn extract_string_not_enough_data() {
        let buffer: Vec<u8> = b"Arbitrary_string\x00".to_vec();
        let mut cursor = ReadCursor::new(&buffer);
        let result = cursor.extract_string();
        assert_eq!(result.unwrap(), "Arbitrary_string");
        let error = cursor.extract_string();
        assert!(matches!(error.unwrap_err(), ParseError::NotEnoughData));
    }

    #[test]
    fn extract_string_non_utf() {
        let buffer: Vec<u8> = b"Arbitrary_\xFFstring\x00\x0A".to_vec();
        let mut cursor = ReadCursor::new(&buffer);
        let result = cursor.extract_string();
        assert!(matches!(result.unwrap_err(), ParseError::Generic(_)));
    }

    #[test]
    fn extract_non_terminated_string() {
        let buffer: Vec<u8> = b"Arbitrary_string".to_vec();
        let mut cursor = ReadCursor::new(&buffer);
        let result = cursor.extract_string();
        assert!(matches!(result.unwrap_err(), ParseError::Generic(_)));
    }
}
