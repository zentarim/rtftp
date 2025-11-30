use crate::fs::{OpenedFile, Root};
use std::fmt::{Debug, Display, Formatter};
use std::fs::{File, OpenOptions};
use std::io;
use std::io::{Read, Seek, SeekFrom};
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
    fn read_to(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let result = self.rd.read(buffer)?;
        Ok(result)
    }

    fn get_size(&mut self) -> io::Result<usize> {
        let current_pos = self.rd.seek(SeekFrom::Start(0))?;
        let result = self.rd.seek(SeekFrom::End(0))?;
        self.rd.seek(SeekFrom::Start(current_pos))?;
        Ok(result as usize)
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
    fn open(&self, path: &str) -> io::Result<Box<dyn OpenedFile>> {
        let file_path = self.path.join(path.trim_start_matches('/'));
        let printable_path = file_path.display().to_string();
        if !file_path.starts_with(&self.path) {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let result = OpenOptions::new().read(true).open(&file_path)?;
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
