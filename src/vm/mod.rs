use file_protocol::VSOCK_PORT;
use std::fs::{self, File};
use std::io::{self, Read, Seek, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::Once;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

#[cfg(test)]
mod tests;

const READY_TIMEOUT: Duration = Duration::from_secs(30);
const GUEST_CID: u32 = 3;

static VM_SEQ: AtomicU64 = AtomicU64::new(0);
static IGNORE_SIGPIPE: Once = Once::new();

#[derive(Debug)]
pub(super) enum VmError {
    Generic(String),
    DiskNotFound(String),
    ConnectionRefused(String),
    ShareNotFound(String),
}

impl std::fmt::Display for VmError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Generic(text) => write!(formatter, "{text}"),
            Self::DiskNotFound(text) => write!(formatter, "{text}"),
            Self::ConnectionRefused(url) => write!(formatter, "connection refused: {url}"),
            Self::ShareNotFound(text) => write!(formatter, "{text}"),
        }
    }
}

impl std::error::Error for VmError {}

pub(super) struct Vm {
    child: Child,
    dir: PathBuf,
    vsock_path: PathBuf,
    serial_path: PathBuf,
    stderr_path: PathBuf,
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

impl Vm {
    pub(super) fn vsock_path(&self) -> &Path {
        &self.vsock_path
    }
}

impl Drop for Vm {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if matches!(self.child.try_wait(), Ok(None)) {
            let pid = self.child.id();
            unsafe { libc::kill(-(pid as i32), libc::SIGKILL) };
            let _ = self.child.wait();
        }
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
        let _ = fs::remove_dir_all(&self.dir);
    }
}

impl std::fmt::Display for Vm {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "<Vm {}>", self.vsock_path.display())
    }
}

impl std::fmt::Debug for Vm {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "<Vm {}>", self.vsock_path.display())
    }
}

pub(super) fn launch(url: &str, mounts: &[(usize, &str)], peer: &str) -> Result<Vm, VmError> {
    ignore_sigpipe();
    let disk = disk_argument(url)?;
    let hypervisor = find_tool(
        "RTFTP_CLOUD_HYPERVISOR",
        "cloud-hypervisor",
        "release/cloud-hypervisor",
    )?;
    let storekeeper = find_tool(
        "RTFTP_STOREKEEPER",
        "rtftp-storekeeper",
        "x86_64-unknown-linux-musl/release/rtftp-storekeeper",
    )?;
    let initramfs = build_initramfs(&storekeeper)?;
    let kernel = host_kernel()?;
    let dir = vm_dir(peer);
    fs::create_dir_all(&dir).map_err(|error| VmError::Generic(error.to_string()))?;
    let vsock_path = dir.join("vsock.sock");
    let serial_path = dir.join("serial.log");
    let stderr_path = dir.join("hypervisor.log");
    let stderr_file =
        File::create(&stderr_path).map_err(|error| VmError::Generic(error.to_string()))?;
    let mut command = Command::new(&hypervisor);
    command
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(stderr_file))
        .arg("--kernel")
        .arg(&kernel)
        .arg("--initramfs")
        .arg(&initramfs)
        .arg("--cmdline")
        .arg(guest_cmdline(mounts))
        .arg("--cpus")
        .arg("boot=1")
        .arg("--memory")
        .arg("size=256M")
        .arg("--disk")
        .arg(disk)
        .arg("--vsock")
        .arg(format!("cid={GUEST_CID},socket={}", vsock_path.display()))
        .arg("--serial")
        .arg(format!("file={}", serial_path.display()))
        .arg("--console")
        .arg("off");
    eprintln!("launching cloud-hypervisor for {peer} disk {url}");
    let child = command.spawn().map_err(|error| {
        VmError::Generic(format!("failed to spawn {}: {error}", hypervisor.display()))
    })?;
    let stop = Arc::new(AtomicBool::new(false));
    let threads = vec![
        forward_file(serial_path.clone(), Arc::clone(&stop)),
        forward_file(stderr_path.clone(), Arc::clone(&stop)),
    ];
    let mut vm = Vm {
        child,
        dir,
        vsock_path,
        serial_path,
        stderr_path,
        stop,
        threads,
    };
    wait_until_ready(&mut vm, url)?;
    Ok(vm)
}

pub(super) async fn connect_client(vsock_path: &Path) -> io::Result<tokio::net::UnixStream> {
    let deadline = Instant::now() + READY_TIMEOUT;
    handshake(vsock_path, deadline, None).await
}

pub(super) fn disk_argument(url: &str) -> Result<String, VmError> {
    if let Some(rest) = url.strip_prefix("nbd://") {
        if rest.is_empty() {
            return Err(VmError::Generic(format!("Invalid NBD URL: {url}")));
        }
        return Ok(format!("nbd={url},readonly=on"));
    }
    if Path::new(url).is_file() {
        return Ok(format!("path={url},image_type=qcow2,readonly=on"));
    }
    Err(VmError::DiskNotFound(format!(
        "No such file or directory: {url}"
    )))
}

pub(super) fn guest_cmdline(mounts: &[(usize, &str)]) -> String {
    let list = mounts
        .iter()
        .map(|(partition, mountpoint)| format!("/dev/vda{partition}:{mountpoint}"))
        .collect::<Vec<_>>()
        .join(",");
    format!("console=ttyS0 reboot=k panic=-1 mounts={list}")
}

pub(super) fn kernel_path(
    cmdline: &str,
    exists: impl Fn(&Path) -> bool,
) -> Result<PathBuf, VmError> {
    if let Some(token) = cmdline
        .split_whitespace()
        .find_map(|token| token.strip_prefix("BOOT_IMAGE="))
    {
        let image = PathBuf::from(token);
        if exists(&image) {
            return Ok(image);
        }
        let prefixed = PathBuf::from(format!("/boot{token}"));
        if exists(&prefixed) {
            return Ok(prefixed);
        }
    }
    let fallback = PathBuf::from("/boot/vmlinuz");
    if exists(&fallback) {
        return Ok(fallback);
    }
    Err(VmError::Generic(
        "host kernel image was not found".to_string(),
    ))
}

pub(super) fn classify_launch_error(hypervisor_log: &str, serial_log: &str, url: &str) -> VmError {
    if hypervisor_log.contains("Connection refused") {
        return VmError::ConnectionRefused(url.to_string());
    }
    if hypervisor_log.contains("export")
        && (hypervisor_log.contains("not found") || hypervisor_log.contains("not present"))
    {
        return VmError::ShareNotFound(format!("Share is not found on server: {url}"));
    }
    if hypervisor_log.contains("No such file or directory") {
        return VmError::DiskNotFound(hypervisor_log.to_string());
    }
    let mut text = String::new();
    if !serial_log.is_empty() {
        text.push_str(serial_log);
        if !serial_log.ends_with('\n') {
            text.push('\n');
        }
    }
    text.push_str(hypervisor_log);
    VmError::Generic(text)
}

fn wait_until_ready(vm: &mut Vm, url: &str) -> Result<(), VmError> {
    let deadline = Instant::now() + READY_TIMEOUT;
    match blocking_handshake(&vm.vsock_path, deadline, Some(&mut vm.child)) {
        Ok(_stream) => Ok(()),
        Err(error) => {
            let serial = read_log(&vm.serial_path);
            let hypervisor = read_log(&vm.stderr_path);
            let mut hypervisor_text = hypervisor;
            if !hypervisor_text.is_empty() && !hypervisor_text.ends_with('\n') {
                hypervisor_text.push('\n');
            }
            hypervisor_text.push_str(&error.to_string());
            Err(classify_launch_error(&hypervisor_text, &serial, url))
        }
    }
}

async fn handshake(
    vsock_path: &Path,
    deadline: Instant,
    mut child: Option<&mut Child>,
) -> io::Result<tokio::net::UnixStream> {
    let mut last_error = io::Error::new(io::ErrorKind::TimedOut, "guest did not accept vsock");
    while Instant::now() < deadline {
        if let Some(process) = child.as_mut()
            && let Ok(Some(status)) = process.try_wait()
        {
            return Err(io::Error::other(format!(
                "cloud-hypervisor exited: {status}"
            )));
        }
        // Not tokio::fs::try_exists: polling it through the blocking pool during
        // the boot measurably slows the transfer that follows (by about 10%).
        if !vsock_path.exists() {
            tokio::time::sleep(Duration::from_millis(50)).await;
            continue;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        let mut stream = match tokio::time::timeout(
            remaining,
            tokio::net::UnixStream::connect(vsock_path),
        )
        .await
        {
            Ok(Ok(stream)) => stream,
            Ok(Err(error)) => {
                last_error = error;
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
            Err(_elapsed) => break,
        };
        let connect_line = format!("CONNECT {VSOCK_PORT}\n");
        match tokio::time::timeout(remaining, stream.write_all(connect_line.as_bytes())).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                last_error = error;
                continue;
            }
            Err(_elapsed) => break,
        }
        match tokio::time::timeout(remaining, read_async_line(&mut stream, 64)).await {
            Ok(Ok(line)) if line.starts_with("OK ") => return Ok(stream),
            Ok(Ok(line)) => {
                return Err(io::Error::other(format!(
                    "unexpected vsock handshake: {line}"
                )));
            }
            Ok(Err(error)) => {
                let timed_out = error.kind() == io::ErrorKind::TimedOut;
                last_error = error;
                if timed_out || Instant::now() >= deadline {
                    break;
                }
            }
            Err(_elapsed) => break,
        }
    }
    Err(last_error)
}

async fn read_async_line<R: AsyncRead + Unpin>(reader: &mut R, limit: usize) -> io::Result<String> {
    let mut bytes = Vec::new();
    loop {
        let mut byte = [0u8; 1];
        reader.read_exact(&mut byte).await?;
        if byte[0] == b'\n' {
            break;
        }
        if bytes.len() == limit {
            return Err(io::Error::other("vsock handshake line is too long"));
        }
        bytes.push(byte[0]);
    }
    String::from_utf8(bytes).map_err(|error| io::Error::other(error.to_string()))
}

fn blocking_handshake(
    vsock_path: &Path,
    deadline: Instant,
    mut child: Option<&mut Child>,
) -> io::Result<UnixStream> {
    let mut last_error = io::Error::new(io::ErrorKind::TimedOut, "guest did not accept vsock");
    while Instant::now() < deadline {
        if let Some(process) = child.as_mut()
            && let Ok(Some(status)) = process.try_wait()
        {
            return Err(io::Error::other(format!(
                "cloud-hypervisor exited: {status}"
            )));
        }
        if !vsock_path.exists() {
            thread::sleep(Duration::from_millis(50));
            continue;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        let mut stream = match UnixStream::connect(vsock_path) {
            Ok(stream) => stream,
            Err(error) => {
                last_error = error;
                thread::sleep(Duration::from_millis(50));
                continue;
            }
        };
        stream.set_read_timeout(Some(remaining))?;
        stream.set_write_timeout(Some(remaining))?;
        if let Err(error) = stream.write_all(format!("CONNECT {VSOCK_PORT}\n").as_bytes()) {
            last_error = error;
            continue;
        }
        match read_line(&mut stream, 64) {
            Ok(line) if line.starts_with("OK ") => {
                stream.set_read_timeout(None)?;
                stream.set_write_timeout(None)?;
                return Ok(stream);
            }
            Ok(line) => {
                return Err(io::Error::other(format!(
                    "unexpected vsock handshake: {line}"
                )));
            }
            Err(error) => {
                let timed_out = error.kind() == io::ErrorKind::TimedOut;
                last_error = error;
                if timed_out || Instant::now() >= deadline {
                    break;
                }
            }
        }
    }
    Err(last_error)
}

fn read_line<R: Read>(reader: &mut R, limit: usize) -> io::Result<String> {
    let mut bytes = Vec::new();
    loop {
        let mut byte = [0u8; 1];
        reader.read_exact(&mut byte)?;
        if byte[0] == b'\n' {
            break;
        }
        if bytes.len() == limit {
            return Err(io::Error::other("vsock handshake line is too long"));
        }
        bytes.push(byte[0]);
    }
    String::from_utf8(bytes).map_err(|error| io::Error::other(error.to_string()))
}

fn build_initramfs(storekeeper: &Path) -> Result<PathBuf, VmError> {
    let script = find_initramfs_script()?;
    eprintln!("mk-initramfs script {}", script.display());
    let output = runtime_dir().join("initramfs.cpio");
    fs::create_dir_all(output.parent().unwrap_or(Path::new("/var/tmp")))
        .map_err(|error| VmError::Generic(error.to_string()))?;
    let result = Command::new(&script)
        .env("STOREKEEPER", storekeeper)
        .env("OUTPUT", &output)
        .output()
        .map_err(|error| VmError::Generic(format!("mk-initramfs.sh: {error}")))?;
    for stream in [&result.stdout, &result.stderr] {
        for line in String::from_utf8_lossy(stream).lines() {
            if !line.is_empty() {
                eprintln!("{line}");
            }
        }
    }
    if !result.status.success() {
        return Err(VmError::Generic(format!(
            "mk-initramfs.sh failed: {}",
            result.status
        )));
    }
    Ok(output)
}

fn host_kernel() -> Result<PathBuf, VmError> {
    let cmdline = fs::read_to_string("/proc/cmdline").unwrap_or_default();
    kernel_path(&cmdline, |path| path.is_file())
}

fn find_initramfs_script() -> Result<PathBuf, VmError> {
    if let Some(value) = std::env::var_os("RTFTP_MK_INITRAMFS") {
        let path = PathBuf::from(value);
        if path.is_file() {
            return Ok(path);
        }
        return Err(VmError::Generic(
            "RTFTP_MK_INITRAMFS does not point at a file".to_string(),
        ));
    }
    let installed = PathBuf::from("/usr/libexec/rtftp/mk-initramfs.sh");
    if installed.is_file() {
        return Ok(installed);
    }
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/mk-initramfs.sh");
    if source.is_file() {
        return Ok(source);
    }
    Err(VmError::Generic(
        "missing mk-initramfs.sh; set RTFTP_MK_INITRAMFS or install it in /usr/libexec/rtftp"
            .to_string(),
    ))
}

fn find_tool(
    env_var: &str,
    installed_name: &str,
    target_relative: &str,
) -> Result<PathBuf, VmError> {
    if let Some(value) = std::env::var_os(env_var) {
        let path = PathBuf::from(value);
        if path.is_file() {
            return Ok(path);
        }
        return Err(VmError::Generic(format!(
            "{env_var} does not point at a file"
        )));
    }
    let installed = PathBuf::from("/usr/libexec/rtftp").join(installed_name);
    if installed.is_file() {
        return Ok(installed);
    }
    let mut built = Vec::new();
    if let Some(target_dir) = std::env::var_os("CARGO_TARGET_DIR") {
        built.push(PathBuf::from(target_dir).join(target_relative));
    }
    built.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(target_relative),
    );
    for path in &built {
        if path.is_file() {
            return Ok(path.clone());
        }
    }
    Err(VmError::Generic(format!(
        "missing {installed_name}; set {env_var} or install it in /usr/libexec/rtftp"
    )))
}

fn runtime_dir() -> PathBuf {
    let uid = unsafe { libc::geteuid() };
    PathBuf::from(format!("/var/tmp/rtftp-{uid}"))
}

fn vm_dir(peer: &str) -> PathBuf {
    let sequence = VM_SEQ.fetch_add(1, Ordering::Relaxed);
    let safe: String = peer
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '.' || ch == ':' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect();
    runtime_dir().join(format!("vm.{safe}.{}.{sequence}", std::process::id()))
}

fn forward_file(path: PathBuf, stop: Arc<AtomicBool>) -> JoinHandle<()> {
    thread::spawn(move || {
        let mut offset = 0u64;
        let mut buffer = [0u8; 4096];
        while !stop.load(Ordering::Relaxed) {
            let opened = File::open(&path);
            let Ok(mut file) = opened else {
                thread::sleep(Duration::from_millis(50));
                continue;
            };
            if file.seek(std::io::SeekFrom::Start(offset)).is_err() {
                thread::sleep(Duration::from_millis(50));
                continue;
            }
            match file.read(&mut buffer) {
                Ok(0) => thread::sleep(Duration::from_millis(50)),
                Ok(count) => {
                    offset += count as u64;
                    eprint!("{}", String::from_utf8_lossy(&buffer[..count]));
                }
                Err(_) => thread::sleep(Duration::from_millis(50)),
            }
        }
    })
}

fn read_log(path: &Path) -> String {
    fs::read(path)
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_default()
}

fn ignore_sigpipe() {
    IGNORE_SIGPIPE.call_once(|| unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    });
}
