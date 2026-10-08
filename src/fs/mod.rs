use crate::local_fs::LocalRoot;
use crate::remote_fs::RemoteRoot;
use std::fmt::{Debug, Display};
use std::io;
use std::path::{Component, Path, PathBuf};

#[cfg(test)]
mod tests;

/// Join `request` onto `root` when every component stays inside that root.
///
/// A leading `/` is the TFTP root. `..` and any remaining absolute component
/// are rejected so `Path::join` cannot replace `root` or walk out of it.
pub(super) fn contained_join(root: &Path, request: &str) -> io::Result<PathBuf> {
    let relative = Path::new(request.trim_start_matches('/'));
    let stays_inside = !relative.is_absolute()
        && relative
            .components()
            .all(|component| matches!(component, Component::Normal(_) | Component::CurDir));
    if !stays_inside {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    let joined = root.join(relative);
    if !joined.starts_with(root) {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    Ok(joined)
}

pub(super) trait OpenedFile: Display + Debug {
    fn read_to(&mut self, buffer: &mut [u8]) -> io::Result<usize>;

    fn get_size(&mut self) -> io::Result<usize>;
}

pub(super) trait Root: Display + Debug {
    type OpenedFile: OpenedFile;
    fn open(&self, path: &str) -> io::Result<Self::OpenedFile>;
}

pub(super) enum RootKind {
    Local(LocalRoot),
    Remote(RemoteRoot),
}
