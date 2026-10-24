use std::ffi::CString;
use std::fs::{self, File};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

#[cfg(test)]
mod tests;

const RAMFS_MAGIC: i64 = 0x8584_58f6;
const MOUNT_ROOT: &str = "/mnt";
const DEVICE_WAIT: Duration = Duration::from_secs(5);

pub(super) fn prepare() -> Result<OwnedFd, String> {
    mount_pseudofs("proc", "/proc", "proc")?;
    mount_pseudofs("devtmpfs", "/dev", "devtmpfs")?;
    let modules = read_lines("/modules.list")?;
    let filesystems = read_lines("/filesystems.list")?;
    let cmdline = fs::read_to_string("/proc/cmdline").map_err(|error| error.to_string())?;
    for module in &modules {
        let (required, path) = module_entry(module);
        if let Err(error) = load_module(path) {
            if required {
                return Err(error);
            }
            eprintln!("{error}");
        }
    }
    let mounts = mounts_from_cmdline(&cmdline)?;
    match fs::create_dir(MOUNT_ROOT) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(format!("mkdir {MOUNT_ROOT}: {error}")),
    }
    for (device, mountpoint) in &mounts {
        let target = staged_mountpoint(mountpoint);
        ensure_mountpoint(&target)?;
        wait_for_device(device)?;
        mount_readonly(device, &target, &filesystems)?;
    }
    open_mount_root()
}

pub(super) fn mounts_from_cmdline(cmdline: &str) -> Result<Vec<(String, String)>, String> {
    let Some(token) = cmdline
        .split_whitespace()
        .find(|token| token.starts_with("mounts="))
    else {
        return Err("cmdline has no mounts=".to_string());
    };
    let value = &token["mounts=".len()..];
    if value.is_empty() {
        return Ok(Vec::new());
    }
    let mut mounts = Vec::new();
    for entry in value.split(',') {
        let Some((device, mountpoint)) = entry.split_once(':') else {
            return Err(format!("mount entry has no colon: {entry}"));
        };
        if device.is_empty() || mountpoint.is_empty() {
            return Err(format!("mount entry is incomplete: {entry}"));
        }
        mounts.push((device.to_string(), mountpoint.to_string()));
    }
    Ok(mounts)
}

pub(super) fn mount_options(fstype: &str) -> Vec<Option<&'static str>> {
    if matches!(fstype, "ext2" | "ext3" | "ext4") {
        vec![None, Some("noload")]
    } else {
        vec![None]
    }
}

pub(super) fn is_ramfs_magic(magic: i64) -> bool {
    magic == RAMFS_MAGIC
}

pub(super) fn staged_mountpoint(mountpoint: &str) -> String {
    if mountpoint == "/" {
        return MOUNT_ROOT.to_string();
    }
    format!("{MOUNT_ROOT}{}", mountpoint.trim_end_matches('/'))
}

fn read_lines(path: &str) -> Result<Vec<String>, String> {
    let text = fs::read_to_string(path).map_err(|error| format!("read {path}: {error}"))?;
    Ok(text
        .lines()
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect())
}

pub(super) fn module_entry(line: &str) -> (bool, &str) {
    match line.split_once(' ') {
        Some(("required", path)) if !path.is_empty() => (true, path),
        Some(("optional", path)) if !path.is_empty() => (false, path),
        _ => (true, line),
    }
}

fn load_module(path: &str) -> Result<(), String> {
    let file = File::open(path).map_err(|error| format!("open {path}: {error}"))?;
    let params = CString::new("").expect("empty module params");
    let result =
        unsafe { libc::syscall(libc::SYS_finit_module, file.as_raw_fd(), params.as_ptr(), 0) };
    if result == 0 {
        eprintln!("loaded {path}");
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::EEXIST) {
        eprintln!("already loaded {path}");
        return Ok(());
    }
    Err(format!("finit_module {path}: {error}"))
}

fn mount_pseudofs(source: &str, target: &str, fstype: &str) -> Result<(), String> {
    if !Path::new(target).exists() {
        fs::create_dir(target).map_err(|error| format!("mkdir {target}: {error}"))?;
    }
    mount_once(source, target, fstype, 0, None)
}

fn open_mount_root() -> Result<OwnedFd, String> {
    let path = CString::new(MOUNT_ROOT).expect("mount root path");
    let fd = unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(format!("open {MOUNT_ROOT}: {}", io::Error::last_os_error()));
    }
    // SAFETY: `fd` came from open(2) and this function owns it from here on.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn ensure_mountpoint(mountpoint: &str) -> Result<(), String> {
    if mountpoint == "/" {
        return Ok(());
    }
    let path = mountpoint.trim_end_matches('/');
    if path.is_empty() {
        return Ok(());
    }
    match fs::metadata(path) {
        Ok(_) => return Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("stat {mountpoint}: {error}")),
    }
    let parent = Path::new(path).parent().unwrap_or(Path::new("/"));
    let parent_text = parent.to_str().unwrap_or("/");
    if parent_text != "/" && !parent.exists() {
        ensure_mountpoint(parent_text)?;
    }
    if !is_ramfs_magic(filesystem_magic(parent_text)?) {
        return Err(format!(
            "missing mountpoint {mountpoint} on a mounted filesystem"
        ));
    }
    fs::create_dir(path).map_err(|error| format!("mkdir {mountpoint}: {error}"))
}

fn wait_for_device(device: &str) -> Result<(), String> {
    let deadline = Instant::now() + DEVICE_WAIT;
    while Instant::now() < deadline {
        if Path::new(device).exists() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(50));
    }
    Err(format!("device {device} did not appear"))
}

fn mount_readonly(device: &str, mountpoint: &str, filesystems: &[String]) -> Result<(), String> {
    if filesystems.is_empty() {
        return Err("filesystem try-list is empty".to_string());
    }
    let mut failures = Vec::new();
    for fstype in filesystems {
        for data in mount_options(fstype) {
            match mount_once(device, mountpoint, fstype, libc::MS_RDONLY, data) {
                Ok(()) => {
                    eprintln!("mounted {device} on {mountpoint} type {fstype}");
                    return Ok(());
                }
                Err(error) => failures.push(format!("{fstype} {data:?}: {error}")),
            }
        }
    }
    Err(format!(
        "failed to mount {device} on {mountpoint}: {}",
        failures.join("; ")
    ))
}

fn mount_once(
    source: &str,
    target: &str,
    fstype: &str,
    flags: libc::c_ulong,
    data: Option<&str>,
) -> Result<(), String> {
    let source = CString::new(source).map_err(|error| error.to_string())?;
    let target = CString::new(target).map_err(|error| error.to_string())?;
    let fstype = CString::new(fstype).map_err(|error| error.to_string())?;
    let data_owned = data
        .map(CString::new)
        .transpose()
        .map_err(|error| error.to_string())?;
    let data_ptr = data_owned
        .as_ref()
        .map(|value| value.as_ptr())
        .unwrap_or(std::ptr::null());
    let result = unsafe {
        libc::mount(
            source.as_ptr(),
            target.as_ptr(),
            fstype.as_ptr(),
            flags,
            data_ptr as *const libc::c_void,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error().to_string())
    }
}

fn filesystem_magic(path: &str) -> Result<i64, String> {
    let c_path = CString::new(path).map_err(|error| error.to_string())?;
    let mut stat = unsafe { std::mem::zeroed::<libc::statfs>() };
    let result = unsafe { libc::statfs(c_path.as_ptr(), &mut stat) };
    if result != 0 {
        return Err(format!("statfs {path}: {}", io::Error::last_os_error()));
    }
    Ok(stat.f_type as i64)
}
