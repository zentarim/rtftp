use crate::remote_fs::{Config, ConnectedDisk, Mount, RemoteRoot, VirtualRootError};
use crate::vm::{VmError, launch};
use serde::Deserialize;
use serde_json::{Value, from_value};
use std::fmt::Debug;
use std::path::{Path, PathBuf};

#[cfg(test)]
mod tests;

#[cfg(test)]
const BOOT_MOUNTS: [(usize, &str); 2] = [(2, "/"), (1, "/boot")];

#[cfg(test)]
async fn attach_nbd_disk<U: AsRef<str>>(url: U) -> Result<ConnectedDisk, VmError> {
    attach_mounted(url.as_ref(), "test", &BOOT_MOUNTS).await
}

async fn attach_mounted(
    url: &str,
    peer: &str,
    mounts: &[(usize, &str)],
) -> Result<ConnectedDisk, VmError> {
    let vm = launch(url, mounts, peer).await?;
    Ok(ConnectedDisk::new(vm, url.to_string()))
}

#[derive(Debug, Deserialize)]
struct NBDConfig {
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
    async fn connect(&self, peer: &str) -> Result<RemoteRoot, VirtualRootError> {
        if !self.url.starts_with("nbd://") {
            return Err(VirtualRootError::ConfigError(format!(
                "Invalid NBD URL: {}",
                self.url
            )));
        };
        let mounts = self
            .mounts
            .iter()
            .map(|mount| (mount.partition(), mount.mountpoint()))
            .collect::<Vec<_>>();
        let disk = match attach_mounted(&self.url, peer, &mounts).await {
            Ok(disk) => disk,
            Err(error) => return Err(VirtualRootError::SetupError(error)),
        };
        Ok(RemoteRoot::new(disk, &self.tftp_root))
    }
}

pub(super) enum NbdRoot {
    Absent,
    Connected(RemoteRoot),
    Unusable,
}

pub(super) async fn open_nbd_root(tftp_root: &Path, ip: &str) -> NbdRoot {
    eprintln!("Looking for TFTP root configs in {tftp_root:?} ...");
    let Some(file_path) = files_sorted(tftp_root)
        .await
        .into_iter()
        .find(|file_path| match_ip(file_path, ip))
    else {
        return NbdRoot::Absent;
    };
    eprintln!("Found TFTP root config {file_path:?}");
    let Some(json_struct) = read_json(&file_path).await else {
        eprintln!("Can't read JSON config {file_path:?}");
        return NbdRoot::Unusable;
    };
    eprintln!("Found JSON file {file_path:?}");
    let Some(nbd_config) = NBDConfig::from_json(&json_struct) else {
        return NbdRoot::Unusable;
    };
    eprintln!("Found NBD TFTP root config {file_path:?}");
    match nbd_config.connect(ip).await {
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

async fn files_sorted<P: AsRef<Path>>(parent: P) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let Ok(mut entries) = tokio::fs::read_dir(parent).await else {
        return files;
    };
    loop {
        match entries.next_entry().await {
            Ok(Some(entry)) => {
                let path = entry.path();
                if tokio::fs::metadata(&path)
                    .await
                    .is_ok_and(|metadata| metadata.is_file())
                {
                    files.push(path);
                }
            }
            Ok(None) => break,
            Err(_error) => continue,
        }
    }
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

async fn read_json(path: &Path) -> Option<Value> {
    let Ok(content) = tokio::fs::read_to_string(path).await else {
        return None;
    };
    serde_json::from_str::<Value>(&content).ok()
}
