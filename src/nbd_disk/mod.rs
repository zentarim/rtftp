use crate::fs::{Config, FileError, OpenedFile, Root, VirtualRootError};
use crate::guestfs::{GuestFS, GuestFSError};
use crate::remote_fs::{ConnectedDisk, RemoteChroot};
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
            result.push(Partition {
                handle: self.handle.clone(),
                device: partition_name,
            });
        }
        for warning in self.handle.retrieve_appliance_stderr() {
            eprintln!("{self}: {warning}");
        }
        Ok(result)
    }

    fn open(&self, absolute_path: &str) -> Result<NBDFileReader, FileError> {
        match self.handle.get_size(absolute_path) {
            Ok(file_size) => Ok(NBDFileReader {
                handle: self.handle.clone(),
                path: absolute_path.to_string(),
                file_size,
                current_offset: 0,
                repr: format!("<{absolute_path} on {self}>"),
            }),
            Err(guestfs_error) => {
                if guestfs_error
                    .to_string()
                    .contains("No such file or directory")
                {
                    Err(FileError::FileNotFound)
                } else {
                    Err(FileError::UnknownError(guestfs_error.to_string()))
                }
            }
        }
    }
}

pub(super) struct Partition {
    handle: Rc<GuestFS>,
    device: String,
}

impl Display for Partition {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write! {f, "<Partition: {}>", self.device}
    }
}

impl Debug for Partition {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write! {f, "<Partition: {}>", self.device}
    }
}

impl Partition {
    fn mount_ro(&self, mountpoint: &str) -> Result<(), GuestFSError> {
        eprintln!("{self}: Mounting to {mountpoint}");
        self.handle.mount_ro(self.device.as_str(), mountpoint)
    }
}

#[derive(Debug)]
pub(super) struct NBDFileReader {
    handle: Rc<GuestFS>,
    path: String,
    file_size: usize,
    current_offset: usize,
    repr: String,
}

impl Display for NBDFileReader {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write! {f, "<{}>", self.repr}
    }
}

impl OpenedFile for NBDFileReader {
    fn read_to(&mut self, buffer: &mut [u8]) -> Result<usize, FileError> {
        // It is extremely inefficient to request every chunk by a specific command but
        // of guestfs_download() is not really convenient to use due it's locking nature.
        match self
            .handle
            .read_to(self.path.as_str(), buffer, self.current_offset)
        {
            Ok(read_bytes) => {
                self.current_offset += read_bytes;
                Ok(read_bytes)
            }
            Err(guestfs_error) => Err(FileError::UnknownError(guestfs_error.to_string())),
        }
    }

    fn get_size(&mut self) -> Result<usize, FileError> {
        Ok(self.file_size)
    }
}

#[derive(Debug, Deserialize)]
struct Mount {
    partition: usize,
    mountpoint: String,
}

impl Mount {
    fn mount(&self, available: &[Partition]) -> Result<(), VirtualRootError> {
        if let Some(partition) = available.get(self.partition - 1) {
            if let Err(guestfs_error) = partition.mount_ro(self.mountpoint.as_str()) {
                Err(VirtualRootError::SetupError(guestfs_error))
            } else {
                Ok(())
            }
        } else {
            Err(VirtualRootError::ConfigError(format!(
                "Can't find a config for partition {}",
                self.partition
            )))
        }
    }
}
#[derive(Debug, Deserialize)]
pub(super) struct NBDConfig {
    url: String,
    mounts: Vec<Mount>,
    tftp_root: String,
}

impl<'a> Config<'a> for NBDConfig {
    fn from_json(value: &Value) -> Option<Self> {
        match from_value::<Self>(value.clone()) {
            Ok(config) => Some(config),
            Err(error) => {
                eprintln!("Can't parse config {value:?} as NBD: {error}");
                None
            }
        }
    }
    fn connect(&self) -> Result<Box<dyn Root>, VirtualRootError> {
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
            mountpoint_config.mount(&partitions)?;
        }
        Ok(Box::new(RemoteChroot::new(Box::new(disk), &self.tftp_root)))
    }
}
