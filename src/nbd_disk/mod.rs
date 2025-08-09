use crate::fs::{FileError, OpenedFile};
use crate::guestfs::{GuestFS, GuestFSError};
use crate::remote_fs::{
    Config, ConnectedDisk, FileChunk, Mount, Partition, RemoteChroot, VirtualRootError,
};
use serde::Deserialize;
use serde_json::{Value, from_value};
use std::fmt::{Debug, Display, Formatter};
use std::rc::Rc;

#[cfg(test)]
mod tests;

fn attach_nbd_disk<U: AsRef<str>>(url: U) -> Result<NBDDisk, GuestFSError> {
    let owned_url = String::from(url.as_ref());
    let handle = GuestFS::new();
    disable_appliance_log_color(&handle)?;
    add_stub_disk(&handle)?;
    add_nbd_device_read_only(&handle, owned_url.as_str())?;
    if let Err(_launch_result) = handle.launch() {
        let mut appliance_errors: Vec<String> = vec![];
        for error in handle.retrieve_appliance_stderr() {
            if error.contains("Failed to connect to") && error.contains("Connection refused") {
                return Err(GuestFSError::ConnectionRefused(owned_url));
            }
            if error.contains("server reported: export ") && error.contains("not present") {
                return Err(GuestFSError::ShareNotFound(format!(
                    "Share is not found on server: {owned_url}"
                )));
            }
            appliance_errors.push(error);
        }
        Err(GuestFSError::Generic(appliance_errors.join("\n")))
    } else {
        _ = handle.retrieve_appliance_stderr();
        Ok(NBDDisk {
            handle: Rc::new(handle),
            url: owned_url,
        })
    }
}

fn disable_appliance_log_color(handle: &GuestFS) -> Result<(), GuestFSError> {
    handle.set_append("SYSTEMD_COLORS=0")
}

fn add_stub_disk(handle: &GuestFS) -> Result<(), GuestFSError> {
    // guestfs_launch() does not allow a qemu appliance to be run without explicitly provided device.
    handle.add_disk("/dev/null", true)
}

fn add_nbd_device_read_only(handle: &GuestFS, url: &str) -> Result<(), GuestFSError> {
    handle.add_qemu_option("-device", "scsi-hd,drive=nbd0")?;
    handle.add_qemu_option(
        "-drive",
        &format!("id=nbd0,file={url},format=raw,if=none,readonly=on"),
    )
}

#[derive(Debug)]
pub(super) struct NBDDisk {
    handle: Rc<GuestFS>,
    url: String,
}

impl Display for NBDDisk {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write! {f, "<NBDDisk: {} [{}]>", self.url, self.handle}
    }
}

impl ConnectedDisk for NBDDisk {
    fn list_partitions(&mut self) -> Result<Vec<Partition>, GuestFSError> {
        let partitions = self.handle.list_partitions()?;
        eprintln!("{self}: Found partitions: {partitions:?}");
        let mut result: Vec<Partition> = Vec::new();
        for partition_name in partitions {
            result.push(Partition::new(self.handle.clone(), partition_name));
        }
        for warning in self.handle.retrieve_appliance_stderr() {
            eprintln!("{self}: {warning}");
        }
        Ok(result)
    }

    fn open(&self, absolute_path: &str) -> Result<Box<dyn OpenedFile>, FileError> {
        let file_size = match self.handle.get_size(absolute_path) {
            Ok(file_size) => file_size,
            Err(guestfs_error) => {
                return if guestfs_error
                    .to_string()
                    .contains("No such file or directory")
                {
                    Err(FileError::FileNotFound)
                } else {
                    Err(FileError::UnknownError(guestfs_error.to_string()))
                };
            }
        };
        let display = format!("<{absolute_path} on {self}>");
        match NBDFileReader::open(
            self.handle.clone(),
            absolute_path.to_string(),
            file_size,
            display,
        ) {
            Ok(file_reader) => Ok(Box::new(file_reader)),
            Err(guestfs_error) => Err(FileError::UnknownError(guestfs_error.to_string())),
        }
    }
}

#[derive(Debug)]
pub(super) struct NBDFileReader {
    handle: Rc<GuestFS>,
    path: String,
    file_size: usize,
    current_offset: usize,
    chunk: FileChunk,
    display: String,
}

impl NBDFileReader {
    fn open(
        handle: Rc<GuestFS>,
        path: String,
        file_size: usize,
        display: String,
    ) -> Result<Self, GuestFSError> {
        let first_chunk = handle.read_chunk(&path, 0)?;
        Ok(Self {
            handle,
            path,
            file_size,
            current_offset: 0,
            chunk: FileChunk::new(first_chunk),
            display,
        })
    }

    fn buffer_new_chunk(&mut self) -> Result<bool, GuestFSError> {
        let next_chunk = self
            .handle
            .read_chunk(self.path.as_str(), self.current_offset)?;
        if next_chunk.is_empty() {
            Ok(false)
        } else {
            self.chunk = FileChunk::new(next_chunk);
            Ok(true)
        }
    }
}

impl Display for NBDFileReader {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write! {f, "{}", self.display}
    }
}

impl OpenedFile for NBDFileReader {
    fn read_to(&mut self, buffer: &mut [u8]) -> Result<usize, FileError> {
        let mut read: usize = 0;
        while self.current_offset < self.file_size && read < buffer.len() {
            let copied = self.chunk.fill(&mut buffer[read..]);
            if copied == 0 {
                let chunk_has_data = match self.buffer_new_chunk() {
                    Ok(result) => result,
                    Err(guestfs_error) => {
                        return Err(FileError::UnknownError(guestfs_error.to_string()));
                    }
                };
                if !chunk_has_data {
                    break;
                }
            };
            read += copied;
            self.current_offset += copied;
        }
        Ok(read)
    }

    fn get_size(&mut self) -> Result<usize, FileError> {
        Ok(self.file_size)
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct NBDConfig {
    url: String,
    mounts: Vec<Mount>,
    tftp_root: String,
}

impl<'a> Config<'a> for NBDConfig {
    type ConnectedRoot = RemoteChroot<NBDDisk>;
    fn from_json(value: &Value) -> Option<Self> {
        match from_value::<Self>(value.clone()) {
            Ok(config) => Some(config),
            Err(error) => {
                eprintln!("Can't parse config {value:?} as NBD: {error}");
                None
            }
        }
    }
    fn connect(&self) -> Result<Self::ConnectedRoot, VirtualRootError> {
        if !self.url.starts_with("nbd://") {
            return Err(VirtualRootError::ConfigError(format!(
                "Invalid NBD URL: {}",
                self.url
            )));
        };
        let mut disk = match attach_nbd_disk(&self.url) {
            Ok(disk) => disk,
            Err(error) => return Err(VirtualRootError::SetupError(error)),
        };
        let partitions = match disk.list_partitions() {
            Ok(partitions) => partitions,
            Err(error) => return Err(VirtualRootError::SetupError(error)),
        };
        for mountpoint_config in &self.mounts {
            mountpoint_config.mount_suitable(&partitions)?;
        }
        Ok(RemoteChroot::new(disk, &self.tftp_root))
    }
}
