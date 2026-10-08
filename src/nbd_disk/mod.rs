use crate::guestfs::{GuestFS, GuestFSError};
use crate::remote_fs::{Config, ConnectedDisk, Mount, RemoteRoot, VirtualRootError};
use serde::Deserialize;
use serde_json::{Value, from_value};
use std::fmt::Debug;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;

#[cfg(test)]
mod tests;

fn attach_nbd_disk<U: AsRef<str>>(url: U) -> Result<ConnectedDisk, GuestFSError> {
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
        Ok(ConnectedDisk::new(Rc::new(handle), owned_url))
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
    fn connect(&self) -> Result<RemoteRoot, VirtualRootError> {
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
        Ok(RemoteRoot::new(disk, &self.tftp_root))
    }
}

pub(super) enum NbdRoot {
    Absent,
    Connected(RemoteRoot),
    Unusable,
}

pub(super) fn open_nbd_root(tftp_root: &Path, ip: &str) -> NbdRoot {
    eprintln!("Looking for TFTP root configs in {tftp_root:?} ...");
    let Some(file_path) = files_sorted(tftp_root)
        .into_iter()
        .find(|file_path| match_ip(file_path, ip))
    else {
        return NbdRoot::Absent;
    };
    eprintln!("Found TFTP root config {file_path:?}");
    let Some(json_struct) = read_json(&file_path) else {
        eprintln!("Can't read JSON config {file_path:?}");
        return NbdRoot::Unusable;
    };
    eprintln!("Found JSON file {file_path:?}");
    let Some(nbd_config) = NBDConfig::from_json(&json_struct) else {
        return NbdRoot::Unusable;
    };
    eprintln!("Found NBD TFTP root config {file_path:?}");
    match nbd_config.connect() {
        Ok(disk) => {
            eprintln!("Connected config {file_path:?}");
            NbdRoot::Connected(disk)
        }
        Err(VirtualRootError::ConfigError(error)) => {
            eprintln!("Invalid config {file_path:?}: {error}");
            NbdRoot::Unusable
        }
        Err(VirtualRootError::SetupError(error)) => {
            eprintln!("Failed to connect disk using config {file_path:?}: {error:?}");
            NbdRoot::Unusable
        }
    }
}

fn files_sorted<P: AsRef<Path>>(parent: P) -> Vec<PathBuf> {
    let mut files = fs::read_dir(parent)
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            if let Ok(entry) = entry {
                let path = entry.path();
                if path.is_file() {
                    return Some(path);
                };
            };
            None
        })
        .collect::<Vec<_>>();
    files.sort();
    files
}

fn match_ip(path: &Path, ip: &str) -> bool {
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let Some(address) = file_name.strip_suffix(".nbd") else {
        return false;
    };
    address == ip
}

fn read_json(path: &Path) -> Option<Value> {
    if let Ok(content) = fs::read_to_string(path)
        && let Ok(json_struct) = serde_json::from_str::<Value>(&content)
    {
        return Some(json_struct);
    }
    None
}
