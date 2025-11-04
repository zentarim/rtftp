use crate::fs::{FileError, OpenedFile, Root};
use std::fmt::{Debug, Display, Formatter};
use std::fs::{File, OpenOptions};
use std::io;
use std::io::{ErrorKind, Read, Seek, SeekFrom};
use std::path::PathBuf;

#[cfg(test)]
mod tests;

struct LocalOpenedFile {
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
        write! {f, "<OpenedFile {}>", self.display}
    }
}

impl OpenedFile for LocalOpenedFile {
    fn read_to(&mut self, buffer: &mut [u8]) -> Result<usize, FileError> {
        let result = self.rd.read(buffer).map_err(local_error_map)?;
        Ok(result)
    }

    fn get_size(&mut self) -> Result<usize, FileError> {
        let current_pos = self.rd.seek(SeekFrom::Start(0)).map_err(local_error_map)?;
        let result = self.rd.seek(SeekFrom::End(0)).map_err(local_error_map)?;
        self.rd
            .seek(SeekFrom::Start(current_pos))
            .map_err(local_error_map)?;
        Ok(result as usize)
    }
}

fn local_error_map(err: io::Error) -> FileError {
    match err.kind() {
        ErrorKind::UnexpectedEof | ErrorKind::Unsupported => FileError::ReadError,
        ErrorKind::NotFound => FileError::FileNotFound,
        ErrorKind::PermissionDenied => FileError::AccessViolation,
        _ => FileError::UnknownError(err.to_string()),
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
    fn open(&self, path: &str) -> Result<Box<dyn OpenedFile>, FileError> {
        let file_path = self.path.join(path.trim_start_matches('/'));
        let printable_path = file_path.display().to_string();
        if !file_path.starts_with(&self.path) {
            return Err(FileError::AccessViolation);
        }
        let result = OpenOptions::new()
            .read(true)
            .open(&file_path)
            .map_err(local_error_map)?;
        Ok(Box::new(LocalOpenedFile {
            rd: result,
            display: printable_path,
        }))
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
