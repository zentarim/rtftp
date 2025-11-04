use crate::fs::{FileError, OpenedFile};
use std::collections::HashMap;
use std::fmt;
use std::fmt::Display;
use std::time::Duration;
use tokio::time::timeout;

#[cfg(test)]
mod tests;

static TSIZE: &str = "tsize";

static TIMEOUT: &str = "timeout";

static BLKSIZE: &str = "blksize";

const BLOCK_SIZE_LIMIT: usize = u16::MAX as usize;

const ACK_TIMEOUT_LIMIT: usize = 60;

#[derive(Clone)]
pub(super) struct Blksize {
    block_size: usize,
}

impl Blksize {
    pub(super) fn find_in(options: &HashMap<String, String>) -> Option<Self> {
        if let Some(block_size_string) = options.get(BLKSIZE)
            && let Ok(block_size) = block_size_string.parse::<usize>()
        {
            if block_size < BLOCK_SIZE_LIMIT {
                return Some(Self { block_size });
            } else {
                eprintln!(
                    "Requested block size {block_size} exceeds \
                        maximum allowed block size {BLOCK_SIZE_LIMIT}"
                );
            }
        }
        None
    }

    pub(super) fn as_key_pair(&self) -> (String, String) {
        (String::from(BLKSIZE), self.block_size.to_string())
    }

    pub(super) fn is_last(&self, chunk_size: usize) -> bool {
        chunk_size < self.block_size
    }

    pub(super) fn read_chunk(
        &self,
        opened_file: &mut dyn OpenedFile,
        buffer: &mut [u8],
    ) -> Result<usize, FileError> {
        opened_file.read_to(&mut buffer[..self.block_size])
    }
}

impl Default for Blksize {
    fn default() -> Self {
        Self { block_size: 512 }
    }
}

#[derive(Clone)]
pub(super) struct AckTimeout {
    timeout: usize,
}

impl Default for AckTimeout {
    fn default() -> Self {
        Self { timeout: 5 }
    }
}

impl AckTimeout {
    pub(super) async fn timeout<T, F: Future<Output = T>>(
        &self,
        fut: F,
    ) -> Result<T, tokio::time::error::Elapsed> {
        timeout(Duration::from_secs(self.timeout as u64), fut).await
    }

    pub(super) fn find_in(options: &HashMap<String, String>) -> Option<Self> {
        if let Some(timeout_string) = options.get(TIMEOUT)
            && let Ok(timeout) = timeout_string.parse::<usize>()
        {
            if timeout <= ACK_TIMEOUT_LIMIT {
                return Some(Self { timeout });
            } else {
                eprintln!(
                    "Requested timeout {timeout} exceeds maximum allowed {ACK_TIMEOUT_LIMIT}"
                );
            }
        }
        None
    }

    pub(super) fn as_key_pair(&self) -> (String, String) {
        (String::from(TIMEOUT), self.timeout.to_string())
    }
}

impl Display for AckTimeout {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[timeout: {}]", self.timeout)
    }
}

pub(super) struct TSize {
    file_size: usize,
}

impl TSize {
    pub(super) fn is_requested(options: &HashMap<String, String>) -> bool {
        options.contains_key(TSIZE)
    }

    pub(super) fn obtain(opened_file: &mut dyn OpenedFile) -> Result<Self, FileError> {
        let file_size = opened_file.get_size()?;
        Ok(Self { file_size })
    }

    pub(super) fn as_key_pair(&self) -> (String, String) {
        (String::from(TSIZE), self.file_size.to_string())
    }
}
