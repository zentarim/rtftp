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

const WINDOW_SIZE: &str = "windowsize";

const BLOCK_SIZE_LIMIT: usize = u16::MAX as usize;

const ACK_TIMEOUT_LIMIT: usize = 255;

const WINDOW_SIZE_LIMIT: usize = u16::MAX as usize;

#[derive(Clone)]
pub(super) struct Blksize {
    block_size: usize,
}

impl Blksize {
    pub(super) fn find_in(options: &HashMap<String, String>) -> Option<Self> {
        if let Some(block_size_string) = options.get(BLKSIZE)
            && let Ok(block_size) = block_size_string.parse::<usize>()
        {
            if (8..=BLOCK_SIZE_LIMIT).contains(&block_size) {
                return Some(Self { block_size });
            } else {
                eprintln!("Requested {block_size} doesn't fit in range 8 .. ={BLOCK_SIZE_LIMIT}");
            }
        }
        None
    }

    pub(super) fn as_key_pair(&self) -> (String, String) {
        (String::from(BLKSIZE), self.block_size.to_string())
    }

    pub(super) fn get_size(&self) -> usize {
        self.block_size
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
            if (1..=ACK_TIMEOUT_LIMIT).contains(&timeout) {
                return Some(Self { timeout });
            } else {
                eprintln!(
                    "Requested timeout {timeout} doesn't fit in range 1 .. ={ACK_TIMEOUT_LIMIT}"
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

pub(super) struct WindowSize(usize);

impl Display for WindowSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[blocks_count: {}]", self.0)
    }
}

impl WindowSize {
    pub(super) fn find_in(options: &HashMap<String, String>) -> Option<Self> {
        if let Some(window_size) = options.get(WINDOW_SIZE)
            && let Ok(window_size) = window_size.parse::<usize>()
        {
            if (1..=WINDOW_SIZE_LIMIT).contains(&window_size) {
                return Some(Self(window_size));
            } else {
                eprintln!(
                    "Requested window size {window_size} doesn't fit in range 1 .. ={WINDOW_SIZE_LIMIT}"
                );
            }
        }
        None
    }

    pub(super) fn get_size(&self) -> usize {
        self.0
    }
    pub(super) fn as_key_pair(&self) -> (String, String) {
        (String::from(WINDOW_SIZE), self.0.to_string())
    }
}

impl Default for WindowSize {
    fn default() -> Self {
        Self(1)
    }
}
