use crate::fs::{OpenedFile, Root, contained_join};
use crate::vm::{Vm, VmError, connect_client};
use file_protocol::{FILE_NOT_FOUND, MAX_CHUNK_SIZE};
use serde::Deserialize;
use serde_json::Value;
use std::fmt::{Debug, Display, Formatter};
use std::io;
use std::path::PathBuf;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::UnixStream;

#[cfg(test)]
mod tests;

pub(super) struct RemoteRoot {
    disk: ConnectedDisk,
    chroot_path: PathBuf,
}

impl RemoteRoot {
    pub(super) fn new(disk: ConnectedDisk, chroot_path: &str) -> Self {
        Self {
            disk,
            chroot_path: PathBuf::from(chroot_path),
        }
    }
}

impl Root for RemoteRoot {
    type OpenedFile = FileReader;
    async fn open(&self, path: &str) -> io::Result<Self::OpenedFile> {
        let full_path = contained_join(&self.chroot_path, path)?;
        self.disk.open(full_path.to_str().unwrap()).await
    }
}

impl Debug for RemoteRoot {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "<{:?} in {}>", self.chroot_path, self.disk)
    }
}

impl Display for RemoteRoot {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "<{:?} in {}>", self.chroot_path, self.disk)
    }
}

pub(super) trait Config<'a>: Deserialize<'a> {
    fn from_json(value: &Value) -> Option<Self>;

    fn connect(&self, peer: &str) -> Result<RemoteRoot, VirtualRootError>;
}

#[derive(Debug)]
pub(super) enum VirtualRootError {
    ConfigError(String),
    SetupError(VmError),
}

#[derive(Debug, Deserialize)]
pub(super) struct Mount {
    partition: usize,
    mountpoint: String,
}

impl Mount {
    pub(super) fn partition(&self) -> usize {
        self.partition
    }

    pub(super) fn mountpoint(&self) -> &str {
        &self.mountpoint
    }
}

struct FileChunk {
    buffer: Vec<u8>,
    offset: usize,
}

impl Debug for FileChunk {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "<FileChunk {}, offset {}>",
            self.buffer.len(),
            self.offset
        )
    }
}

impl FileChunk {
    fn new(buffer: Vec<u8>) -> Self {
        Self { buffer, offset: 0 }
    }

    fn fill(&mut self, buffer: &mut [u8]) -> usize {
        let available_bytes = &self.buffer[self.offset..];
        if available_bytes.is_empty() {
            return 0;
        }
        if available_bytes.len() <= buffer.len() {
            buffer[..available_bytes.len()].copy_from_slice(available_bytes);
            self.offset += available_bytes.len();
            available_bytes.len()
        } else {
            buffer.copy_from_slice(&available_bytes[..buffer.len()]);
            self.offset += buffer.len();
            buffer.len()
        }
    }
}

#[derive(Debug)]
pub(super) struct FileReader {
    stream: UnixStream,
    file_size: usize,
    current_offset: usize,
    chunk: FileChunk,
    display: String,
}

impl FileReader {
    async fn open(vsock_path: &std::path::Path, path: &str, display: String) -> io::Result<Self> {
        let mut stream = connect_client(vsock_path).await?;
        write_request(&mut stream, path, MAX_CHUNK_SIZE).await?;
        stream.flush().await?;
        let size = read_reply(&mut stream).await?;
        let file_size =
            usize::try_from(size).map_err(|_| io::Error::other("file size exceeds usize"))?;
        let first_chunk = read_chunk(&mut stream).await?;
        Ok(Self {
            stream,
            file_size,
            current_offset: 0,
            chunk: FileChunk::new(first_chunk),
            display,
        })
    }

    async fn buffer_new_chunk(&mut self) -> io::Result<bool> {
        let next_chunk = read_chunk(&mut self.stream).await?;
        if next_chunk.is_empty() {
            Ok(false)
        } else {
            self.chunk = FileChunk::new(next_chunk);
            Ok(true)
        }
    }
}

impl Display for FileReader {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.display)
    }
}

impl OpenedFile for FileReader {
    async fn read_to(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let mut read = 0;
        while self.current_offset < self.file_size && read < buffer.len() {
            let copied = self.chunk.fill(&mut buffer[read..]);
            if copied == 0 {
                let chunk_has_data = self.buffer_new_chunk().await?;
                if !chunk_has_data {
                    break;
                }
            }
            read += copied;
            self.current_offset += copied;
        }
        Ok(read)
    }

    async fn get_size(&mut self) -> io::Result<usize> {
        Ok(self.file_size)
    }
}

#[derive(Debug)]
pub(super) struct ConnectedDisk {
    vm: Option<Box<Vm>>,
    url: String,
}

impl Display for ConnectedDisk {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "<NBDDisk: {}>", self.url)
    }
}

impl ConnectedDisk {
    pub(super) fn new(vm: Vm, url: String) -> Self {
        Self {
            vm: Some(Box::new(vm)),
            url,
        }
    }

    #[cfg(test)]
    pub(super) fn unconnected(url: String) -> Self {
        Self { vm: None, url }
    }

    pub(super) async fn open(&self, absolute_path: &str) -> io::Result<FileReader> {
        let Some(vm) = &self.vm else {
            return Err(io::Error::other("disk is not connected"));
        };
        let display = format!("<{absolute_path} on {self}>");
        FileReader::open(vm.vsock_path(), absolute_path, display).await
    }
}

const PATH_LIMIT: usize = 4096;
const TEXT_LIMIT: usize = 1024;
const SIZE_LIMIT: usize = 20;

async fn write_request<W: AsyncWrite + Unpin>(
    writer: &mut W,
    path: &str,
    chunk_size: usize,
) -> io::Result<()> {
    if path.is_empty() || path.contains('\0') {
        return Err(io::Error::other("path is empty or contains NUL"));
    }
    if path.len() > PATH_LIMIT {
        return Err(io::Error::other("path is too long"));
    }
    if chunk_size == 0 || chunk_size > MAX_CHUNK_SIZE {
        return Err(io::Error::other("invalid SEND"));
    }
    write_opcode(writer, b"FILE").await?;
    write_cstr(writer, path).await?;
    write_opcode(writer, b"SEND").await?;
    write_cstr(writer, &chunk_size.to_string()).await?;
    Ok(())
}

async fn read_reply<R: AsyncRead + Unpin>(reader: &mut R) -> io::Result<u64> {
    let opcode = read_opcode(reader).await?;
    if opcode == *b"INFO" {
        let size_text = read_cstr(reader, SIZE_LIMIT).await?;
        return parse_size(&size_text).ok_or_else(|| io::Error::other("invalid INFO size"));
    }
    if opcode == *b"ERRO" {
        let text = read_cstr(reader, TEXT_LIMIT).await?;
        if text == FILE_NOT_FOUND {
            return Err(io::ErrorKind::NotFound.into());
        }
        return Err(io::Error::other(text));
    }
    Err(io::Error::other(format!(
        "expected INFO or ERRO, got {}",
        opcode_text(&opcode)
    )))
}

async fn read_chunk<R: AsyncRead + Unpin>(reader: &mut R) -> io::Result<Vec<u8>> {
    let opcode = read_opcode(reader).await?;
    if opcode == *b"ERRO" {
        let text = read_cstr(reader, TEXT_LIMIT).await?;
        return Err(io::Error::other(text));
    }
    if opcode != *b"CHNK" {
        return Err(io::Error::other(format!(
            "expected CHNK, got {}",
            opcode_text(&opcode)
        )));
    }
    let size_text = read_cstr(reader, SIZE_LIMIT).await?;
    let size = parse_size(&size_text).ok_or_else(|| io::Error::other("invalid chunk size"))?;
    if size > MAX_CHUNK_SIZE as u64 {
        return Err(io::Error::other("chunk exceeds MAX_CHUNK_SIZE"));
    }
    let mut data = vec![0u8; size as usize];
    if size > 0 {
        reader.read_exact(&mut data).await?;
    }
    let mut trailer = [0u8; 1];
    reader.read_exact(&mut trailer).await?;
    if trailer[0] != 0 {
        return Err(io::Error::other("chunk is missing its trailing NUL"));
    }
    Ok(data)
}

async fn write_opcode<W: AsyncWrite + Unpin>(writer: &mut W, opcode: &[u8; 4]) -> io::Result<()> {
    writer.write_all(opcode).await?;
    writer.write_all(&[0]).await?;
    Ok(())
}

async fn write_cstr<W: AsyncWrite + Unpin>(writer: &mut W, text: &str) -> io::Result<()> {
    writer.write_all(text.as_bytes()).await?;
    writer.write_all(&[0]).await?;
    Ok(())
}

async fn read_opcode<R: AsyncRead + Unpin>(reader: &mut R) -> io::Result<[u8; 4]> {
    let mut opcode = [0u8; 4];
    reader.read_exact(&mut opcode).await?;
    let mut terminator = [0u8; 1];
    reader.read_exact(&mut terminator).await?;
    if terminator[0] != 0 {
        return Err(io::Error::other("opcode is not NUL terminated"));
    }
    Ok(opcode)
}

async fn read_cstr<R: AsyncRead + Unpin>(reader: &mut R, limit: usize) -> io::Result<String> {
    let mut bytes = Vec::new();
    loop {
        let mut byte = [0u8; 1];
        reader.read_exact(&mut byte).await?;
        if byte[0] == 0 {
            break;
        }
        if bytes.len() == limit {
            return Err(io::Error::other("field is too long"));
        }
        bytes.push(byte[0]);
    }
    String::from_utf8(bytes).map_err(|_| io::Error::other("field is not utf-8"))
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
