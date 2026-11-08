use crate::fs::OpenedFile;
use std::collections::HashMap;
use std::fmt::Display;
use std::time::Duration;
use std::{fmt, io};
use tokio::time::timeout;

#[cfg(test)]
mod tests;

static TSIZE: &str = "tsize";

static TIMEOUT: &str = "timeout";

static UTIMEOUT: &str = "utimeout";

static BLKSIZE: &str = "blksize";

const WINDOW_SIZE: &str = "windowsize";

const ROLLOVER: &str = "rollover";

const ROLLOVER_UPPER_CAP: usize = 1;

const BLOCK_SIZE_BOTTOM_CAP: usize = 8;
const BLOCK_SIZE_UPPER_CAP: usize = u16::MAX as usize;

const ACK_TIMEOUT_BOTTOM_CAP: u64 = 1;
const ACK_TIMEOUT_UPPER_CAP: u64 = 255;

const UTIMEOUT_BOTTOM_CAP: u64 = 10_000;
const UTIMEOUT_UPPER_CAP: u64 = 255_000_000;

const WINDOW_SIZE_BOTTOM_CAP: usize = 1;
const WINDOW_SIZE_UPPER_CAP: usize = u16::MAX as usize;

#[derive(Clone)]
pub(super) struct Blksize {
    block_size: usize,
}

impl Blksize {
    pub(super) fn find_in(options: &HashMap<String, String>) -> Option<Self> {
        if let Some(block_size_string) = options.get(BLKSIZE)
            && let Ok(block_size) = block_size_string.parse::<usize>()
        {
            if (BLOCK_SIZE_BOTTOM_CAP..=BLOCK_SIZE_UPPER_CAP).contains(&block_size) {
                return Some(Self { block_size });
            } else {
                eprintln!(
                    "Requested {block_size} doesn't fit in range {BLOCK_SIZE_BOTTOM_CAP} .. ={BLOCK_SIZE_UPPER_CAP}"
                );
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
    timeout: Duration,
}

impl Default for AckTimeout {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(5),
        }
    }
}

impl AckTimeout {
    pub(super) async fn timeout<T, F: Future<Output = T>>(
        &self,
        fut: F,
    ) -> Result<T, tokio::time::error::Elapsed> {
        timeout(self.timeout, fut).await
    }

    pub(super) fn negotiate(options: &HashMap<String, String>) -> (Self, Vec<(String, String)>) {
        let seconds = requested_seconds(options);
        let micros = requested_micros(options);
        let mut pairs = Vec::new();
        let timeout = match micros {
            Some(micros) => {
                if let Some(seconds) = seconds {
                    if seconds * 1_000_000 == micros {
                        pairs.push((String::from(TIMEOUT), seconds.to_string()));
                    } else {
                        eprintln!(
                            "Requested timeout {seconds} ignored in favor of utimeout {micros}"
                        );
                    }
                }
                pairs.push((String::from(UTIMEOUT), micros.to_string()));
                Duration::from_micros(micros)
            }
            None => match seconds {
                Some(seconds) => {
                    pairs.push((String::from(TIMEOUT), seconds.to_string()));
                    Duration::from_secs(seconds)
                }
                None => Duration::from_secs(5),
            },
        };
        (Self { timeout }, pairs)
    }
}

fn requested_seconds(options: &HashMap<String, String>) -> Option<u64> {
    if let Some(timeout_string) = options.get(TIMEOUT)
        && let Ok(timeout) = timeout_string.parse::<u64>()
    {
        if (ACK_TIMEOUT_BOTTOM_CAP..=ACK_TIMEOUT_UPPER_CAP).contains(&timeout) {
            return Some(timeout);
        }
        eprintln!(
            "Requested timeout {timeout} doesn't fit in range {ACK_TIMEOUT_BOTTOM_CAP} .. ={ACK_TIMEOUT_UPPER_CAP}"
        );
    }
    None
}

fn requested_micros(options: &HashMap<String, String>) -> Option<u64> {
    if let Some(timeout_string) = options.get(UTIMEOUT)
        && let Ok(timeout) = timeout_string.parse::<u64>()
    {
        if (UTIMEOUT_BOTTOM_CAP..=UTIMEOUT_UPPER_CAP).contains(&timeout) {
            return Some(timeout);
        }
        eprintln!(
            "Requested utimeout {timeout} doesn't fit in range {UTIMEOUT_BOTTOM_CAP} .. ={UTIMEOUT_UPPER_CAP}"
        );
    }
    None
}

impl Display for AckTimeout {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[timeout: {}us]", self.timeout.as_micros())
    }
}

pub(super) struct TSize {
    file_size: usize,
}

impl TSize {
    pub(super) fn is_requested(options: &HashMap<String, String>) -> bool {
        options.contains_key(TSIZE)
    }

    pub(super) async fn obtain<O: OpenedFile>(opened_file: &mut O) -> io::Result<Self> {
        let file_size = opened_file.get_size().await?;
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
            if (WINDOW_SIZE_BOTTOM_CAP..=WINDOW_SIZE_UPPER_CAP).contains(&window_size) {
                return Some(Self(window_size));
            } else {
                eprintln!(
                    "Requested window size {window_size} doesn't fit in range {WINDOW_SIZE_BOTTOM_CAP} .. ={WINDOW_SIZE_UPPER_CAP}"
                );
            }
        }
        None
    }

    pub(super) fn get_size(&self) -> usize {
        self.0
    }

    pub(super) fn limited_to(self, max: usize) -> Self {
        Self(self.0.min(max))
    }

    pub(super) fn as_key_pair(&self) -> (String, String) {
        (String::from(WINDOW_SIZE), self.0.to_string())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Rollover(u16);

impl Rollover {
    pub(super) fn find_in(options: &HashMap<String, String>) -> Option<Self> {
        if let Some(rollover_string) = options.get(ROLLOVER)
            && let Ok(rollover) = rollover_string.parse::<usize>()
        {
            if (0..=ROLLOVER_UPPER_CAP).contains(&rollover) {
                return Some(Self(rollover as u16));
            }
            eprintln!(
                "Requested rollover {rollover} doesn't fit in range 0 .. ={ROLLOVER_UPPER_CAP}"
            );
        }
        None
    }

    pub(super) fn as_key_pair(&self) -> (String, String) {
        (String::from(ROLLOVER), self.0.to_string())
    }

    pub(super) fn block_number(self, seq: u64) -> u16 {
        if seq == 0 {
            return 0;
        }
        let cycle = self.cycle();
        if self.0 == 0 {
            (seq % cycle) as u16
        } else {
            (((seq - 1) % cycle) + 1) as u16
        }
    }

    pub(super) fn max_window_size(self) -> usize {
        if self.0 == 0 {
            WINDOW_SIZE_UPPER_CAP
        } else {
            WINDOW_SIZE_UPPER_CAP - 1
        }
    }

    pub(super) fn distance_in_cycle(self, received: u16, window_first: u64) -> Option<u64> {
        let position = self.wire_position(received)?;
        let cycle = self.cycle();
        let first_position = self.seq_position(window_first);
        Some((position + cycle - first_position) % cycle)
    }

    fn cycle(self) -> u64 {
        if self.0 == 0 { 65_536 } else { 65_535 }
    }

    fn wire_position(self, block: u16) -> Option<u64> {
        if self.0 == 0 {
            Some(u64::from(block))
        } else if block == 0 {
            None
        } else {
            Some(u64::from(block) - 1)
        }
    }

    fn seq_position(self, seq: u64) -> u64 {
        seq.wrapping_sub(u64::from(self.0)) % self.cycle()
    }
}

impl Default for WindowSize {
    fn default() -> Self {
        Self(1)
    }
}
