use std::fmt::{Display, Formatter};
use std::io::{self, Read, Write};

#[cfg(test)]
mod tests;

pub const MAX_CHUNK_SIZE: usize = 1024 * 1024;
pub const VSOCK_PORT: u32 = 1234;
pub const FILE_NOT_FOUND: &str = "No such file or directory";
pub const NOT_A_REGULAR_FILE: &str = "Not a regular file";
pub const INVALID_SEND: &str = "invalid SEND";

const PATH_LIMIT: usize = 4096;
const TEXT_LIMIT: usize = 1024;
const SIZE_LIMIT: usize = 20;

pub struct FileRequest {
    pub path: String,
    pub chunk_size: usize,
}

pub enum Reply {
    Info(u64),
    Error(String),
}

#[derive(Debug)]
pub enum ProtocolError {
    Io(io::Error),
    InvalidSend,
    Malformed(String),
}

impl Display for ProtocolError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "{error}"),
            Self::InvalidSend => write!(formatter, "{INVALID_SEND}"),
            Self::Malformed(text) => write!(formatter, "{text}"),
        }
    }
}

impl std::error::Error for ProtocolError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::InvalidSend | Self::Malformed(_) => None,
        }
    }
}

impl From<io::Error> for ProtocolError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<ProtocolError> for io::Error {
    fn from(error: ProtocolError) -> Self {
        match error {
            ProtocolError::Io(source) => source,
            other => io::Error::other(other),
        }
    }
}

pub fn write_request<W: Write>(
    writer: &mut W,
    path: &str,
    chunk_size: usize,
) -> Result<(), ProtocolError> {
    if path.is_empty() || path.contains('\0') {
        return Err(ProtocolError::Malformed(
            "path is empty or contains NUL".to_string(),
        ));
    }
    if chunk_size == 0 || chunk_size > MAX_CHUNK_SIZE {
        return Err(ProtocolError::InvalidSend);
    }
    write_opcode(writer, b"FILE")?;
    write_cstr(writer, path)?;
    write_opcode(writer, b"SEND")?;
    write_cstr(writer, &chunk_size.to_string())?;
    Ok(())
}

pub fn read_request<R: Read>(reader: &mut R) -> Result<FileRequest, ProtocolError> {
    let opcode = read_opcode(reader)?;
    if opcode != *b"FILE" {
        return Err(ProtocolError::Malformed("expected FILE".to_string()));
    }
    let path = read_cstr(reader, PATH_LIMIT)?;
    if path.is_empty() {
        return Err(ProtocolError::Malformed("path is empty".to_string()));
    }
    let opcode = read_opcode(reader)?;
    if opcode != *b"SEND" {
        return Err(ProtocolError::Malformed("expected SEND".to_string()));
    }
    let size_text = read_cstr(reader, SIZE_LIMIT)?;
    let chunk_size = parse_send_size(&size_text)?;
    Ok(FileRequest { path, chunk_size })
}

pub fn write_info<W: Write>(writer: &mut W, size: u64) -> Result<(), ProtocolError> {
    write_opcode(writer, b"INFO")?;
    write_cstr(writer, &size.to_string())?;
    Ok(())
}

pub fn write_error<W: Write>(writer: &mut W, text: &str) -> Result<(), ProtocolError> {
    if text.is_empty() || text.contains('\0') {
        return Err(ProtocolError::Malformed(
            "error text is empty or contains NUL".to_string(),
        ));
    }
    write_opcode(writer, b"ERRO")?;
    write_cstr(writer, text)?;
    Ok(())
}

pub fn read_reply<R: Read>(reader: &mut R) -> Result<Reply, ProtocolError> {
    let opcode = read_opcode(reader)?;
    if opcode == *b"INFO" {
        let size_text = read_cstr(reader, SIZE_LIMIT)?;
        let size = parse_size(&size_text)
            .ok_or_else(|| ProtocolError::Malformed("invalid INFO size".to_string()))?;
        return Ok(Reply::Info(size));
    }
    if opcode == *b"ERRO" {
        return Ok(Reply::Error(read_cstr(reader, TEXT_LIMIT)?));
    }
    Err(ProtocolError::Malformed(format!(
        "expected INFO or ERRO, got {}",
        opcode_text(&opcode)
    )))
}

pub fn write_chunk<W: Write>(writer: &mut W, data: &[u8]) -> Result<(), ProtocolError> {
    if data.len() > MAX_CHUNK_SIZE {
        return Err(ProtocolError::Malformed(
            "chunk exceeds MAX_CHUNK_SIZE".to_string(),
        ));
    }
    write_opcode(writer, b"CHNK")?;
    write_cstr(writer, &data.len().to_string())?;
    writer.write_all(data)?;
    writer.write_all(&[0])?;
    Ok(())
}

pub fn read_chunk<R: Read>(reader: &mut R) -> Result<Vec<u8>, ProtocolError> {
    let opcode = read_opcode(reader)?;
    if opcode == *b"ERRO" {
        let text = read_cstr(reader, TEXT_LIMIT)?;
        return Err(ProtocolError::Malformed(text));
    }
    if opcode != *b"CHNK" {
        return Err(ProtocolError::Malformed(format!(
            "expected CHNK, got {}",
            opcode_text(&opcode)
        )));
    }
    let size_text = read_cstr(reader, SIZE_LIMIT)?;
    let size = parse_size(&size_text)
        .ok_or_else(|| ProtocolError::Malformed("invalid chunk size".to_string()))?;
    if size > MAX_CHUNK_SIZE as u64 {
        return Err(ProtocolError::Malformed(
            "chunk exceeds MAX_CHUNK_SIZE".to_string(),
        ));
    }
    let mut data = vec![0u8; size as usize];
    if size > 0 {
        reader.read_exact(&mut data)?;
    }
    let mut trailer = [0u8; 1];
    reader.read_exact(&mut trailer)?;
    if trailer[0] != 0 {
        return Err(ProtocolError::Malformed(
            "chunk is missing its trailing NUL".to_string(),
        ));
    }
    Ok(data)
}

fn write_opcode<W: Write>(writer: &mut W, opcode: &[u8; 4]) -> Result<(), ProtocolError> {
    writer.write_all(opcode)?;
    writer.write_all(&[0])?;
    Ok(())
}

fn write_cstr<W: Write>(writer: &mut W, text: &str) -> Result<(), ProtocolError> {
    writer.write_all(text.as_bytes())?;
    writer.write_all(&[0])?;
    Ok(())
}

fn read_opcode<R: Read>(reader: &mut R) -> Result<[u8; 4], ProtocolError> {
    let mut opcode = [0u8; 4];
    reader.read_exact(&mut opcode)?;
    let mut terminator = [0u8; 1];
    reader.read_exact(&mut terminator)?;
    if terminator[0] != 0 {
        return Err(ProtocolError::Malformed(
            "opcode is not NUL terminated".to_string(),
        ));
    }
    Ok(opcode)
}

fn read_cstr<R: Read>(reader: &mut R, limit: usize) -> Result<String, ProtocolError> {
    let mut bytes = Vec::new();
    loop {
        let mut byte = [0u8; 1];
        reader.read_exact(&mut byte)?;
        if byte[0] == 0 {
            break;
        }
        if bytes.len() == limit {
            return Err(ProtocolError::Malformed("field is too long".to_string()));
        }
        bytes.push(byte[0]);
    }
    String::from_utf8(bytes).map_err(|_| ProtocolError::Malformed("field is not utf-8".to_string()))
}

fn parse_send_size(text: &str) -> Result<usize, ProtocolError> {
    let size = parse_size(text).ok_or(ProtocolError::InvalidSend)?;
    if size == 0 || size > MAX_CHUNK_SIZE as u64 {
        return Err(ProtocolError::InvalidSend);
    }
    Ok(size as usize)
}

fn parse_size(text: &str) -> Option<u64> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    if text.len() > 1 && text.starts_with('0') {
        return None;
    }
    text.parse().ok()
}

fn opcode_text(opcode: &[u8; 4]) -> String {
    String::from_utf8_lossy(opcode).into_owned()
}
