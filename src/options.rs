use crate::fs::{FileError, OpenedFile};
use std::collections::HashMap;
use std::fmt;
use std::fmt::Display;
use std::time::Duration;
use tokio::time::timeout;

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
        if let Some(block_size_string) = options.get(BLKSIZE) {
            if let Ok(block_size) = block_size_string.parse::<usize>() {
                if block_size < BLOCK_SIZE_LIMIT {
                    return Some(Self { block_size });
                } else {
                    eprintln!(
                        "Requested block size {block_size} exceeds \
                        maximum allowed block size {BLOCK_SIZE_LIMIT}"
                    );
                }
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
        if let Some(timeout_string) = options.get(TIMEOUT) {
            if let Ok(timeout) = timeout_string.parse::<usize>() {
                if timeout <= ACK_TIMEOUT_LIMIT {
                    return Some(Self { timeout });
                } else {
                    eprintln!(
                        "Requested timeout {timeout} exceeds maximum allowed {ACK_TIMEOUT_LIMIT}"
                    );
                }
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

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn find_block_size() {
        let mut options = HashMap::new();
        options.insert("blksize".to_string(), "1468".to_string());
        let blk_size = Blksize::find_in(&options).unwrap();
        assert_eq!(blk_size.block_size, 1468);
        assert_eq!(
            blk_size.as_key_pair(),
            (BLKSIZE.to_string(), "1468".to_string())
        );
    }

    #[test]
    fn find_tsize() {
        let mut options = HashMap::new();
        options.insert("tsize".to_string(), "0".to_string());
        assert!(TSize::is_requested(&options));
    }

    #[test]
    fn find_timeout() {
        let mut options = HashMap::new();
        let timeout_value: usize = 10;
        options.insert("timeout".to_string(), timeout_value.to_string());
        let timeout = AckTimeout::find_in(&options).unwrap();
        assert_eq!(timeout.timeout, timeout_value);
    }

    #[test]
    fn test_timeout_cap() {
        let mut options = HashMap::new();
        options.insert("timeout".to_string(), (ACK_TIMEOUT_LIMIT + 1).to_string());
        let find_result = AckTimeout::find_in(&options);
        assert!(find_result.is_none());
    }

    #[test]
    fn test_block_size_cap() {
        let mut options = HashMap::new();
        options.insert("blksize".to_string(), (BLOCK_SIZE_LIMIT + 1).to_string());
        let find_result = Blksize::find_in(&options);
        assert!(find_result.is_none());
    }
}
