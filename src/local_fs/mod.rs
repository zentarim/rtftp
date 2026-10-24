use crate::fs::{OpenedFile, Root, contained_join};
use std::fmt::{Debug, Display, Formatter};
use std::io::{self, SeekFrom};
use std::path::PathBuf;
use tokio::fs::File;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeekExt};

#[cfg(test)]
mod tests;

pub(super) struct LocalOpenedFile {
    rd: File,
    display: String,
}

impl Debug for LocalOpenedFile {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "LocalOpenedFile: {:?}", self.rd)
    }
}

impl Display for LocalOpenedFile {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "<OpenedFile {}>", self.display)
    }
}

/// Fill `buffer` from `reader`.
///
/// A short `read` is not end of file. Stop only when the buffer is full or
/// `read` returns 0.
async fn read_until_full(
    reader: &mut (impl AsyncRead + Unpin),
    buffer: &mut [u8],
) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        let read_bytes = reader.read(&mut buffer[filled..]).await?;
        if read_bytes == 0 {
            break;
        }
        filled += read_bytes;
    }
    Ok(filled)
}

impl OpenedFile for LocalOpenedFile {
    async fn read_to(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        read_until_full(&mut self.rd, buffer).await
    }

    async fn get_size(&mut self) -> io::Result<usize> {
        // stream_position is seek(Current(0)). Start(0) always returns 0 and
        // would rewind a file that has already been read.
        let current_pos = self.rd.stream_position().await?;
        let size = self.rd.seek(SeekFrom::End(0)).await?;
        self.rd.seek(SeekFrom::Start(current_pos)).await?;
        Ok(size as usize)
    }
}

pub(super) struct LocalRoot {
    path: PathBuf,
}

impl LocalRoot {
    pub(super) fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl Root for LocalRoot {
    type OpenedFile = LocalOpenedFile;
    async fn open(&self, path: &str) -> io::Result<Self::OpenedFile> {
        let file_path = contained_join(&self.path, path)?;
        let printable_path = file_path.display().to_string();
        let canonical_root = tokio::fs::canonicalize(&self.path).await?;
        // Resolve symlinks so a link cannot leave the configured root.
        let canonical_file = tokio::fs::canonicalize(&file_path).await?;
        if !canonical_file.starts_with(&canonical_root) {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let result = File::open(&canonical_file).await?;
        Ok(LocalOpenedFile {
            rd: result,
            display: printable_path,
        })
    }
}

impl Debug for LocalRoot {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "<LocalRoot: {:?}>", self.path)
    }
}

impl Display for LocalRoot {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "<Local: {:?}>", self.path)
    }
}
