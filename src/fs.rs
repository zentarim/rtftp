use std::fmt::{Debug, Display};
use std::io;

pub(super) trait OpenedFile: Display + Debug {
    fn read_to(&mut self, buffer: &mut [u8]) -> io::Result<usize>;

    fn get_size(&mut self) -> io::Result<usize>;
}

pub(super) trait Root: Display + Debug {
    fn open(&self, path: &str) -> io::Result<Box<dyn OpenedFile>>;
}
