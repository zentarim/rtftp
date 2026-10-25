// Shared by src/nbd_disk/tests.rs and tests/common/mod.rs via include!.
// qemu-nbd adopts the already-listening socket from fd 3 when LISTEN_FDS=1
// and LISTEN_PID is this process. See QEMU util/systemd.c.

const ACTIVATED_SOCKET_FD: libc::c_int = 3;
const ENV_CAPACITY: usize = 4096;
const LISTEN_PID_PREFIX: &[u8] = b"LISTEN_PID=";
const LISTEN_PID_SLOT_LEN: usize = LISTEN_PID_PREFIX.len() + 10 + 1;

// Filled in only after fork. Each child has its own address space, and the
// parent never touches these, so test threads do not share the writes.
struct ForkSlot<T>(std::cell::UnsafeCell<T>);
unsafe impl<T> Sync for ForkSlot<T> {}

static LISTEN_PID_SLOT: ForkSlot<[u8; LISTEN_PID_SLOT_LEN]> =
    ForkSlot(std::cell::UnsafeCell::new([0; LISTEN_PID_SLOT_LEN]));
static ENV_SLOTS: ForkSlot<[*const libc::c_char; ENV_CAPACITY]> =
    ForkSlot(std::cell::UnsafeCell::new([std::ptr::null(); ENV_CAPACITY]));

unsafe extern "C" {
    static mut environ: *const *const libc::c_char;
}
static LISTEN_FDS_ENTRY: [u8; 13] = *b"LISTEN_FDS=1\0";

pub(super) struct NBDServerProcess {
    process: std::process::Child,
    url: String,
}

impl NBDServerProcess {
    pub(super) fn get_url(&self) -> &str {
        &self.url
    }
}

impl Drop for NBDServerProcess {
    fn drop(&mut self) {
        self.process.kill().unwrap();
        self.process.wait().unwrap();
    }
}

pub(super) fn run_nbd_server(listen_ip: &str) -> NBDServerProcess {
    let (test_disk, locked_tests_directory) = ensure_prerequisite_disk();
    let server = spawn_nbd_server(listen_ip, &test_disk);
    drop(locked_tests_directory);
    server
}

fn spawn_nbd_server(listen_ip: &str, test_disk: &std::path::Path) -> NBDServerProcess {
    let export_name = "disk";
    let listener = bind_listen_socket(listen_ip);
    let listen_port = listener
        .local_addr()
        .unwrap_or_else(|err| panic!("bound socket has no local address: {err}"))
        .port();
    let url = format!("nbd://{listen_ip}:{listen_port}/{export_name}");
    let mut command = std::process::Command::new("qemu-nbd");
    command
        .arg(format!("--export-name={export_name}"))
        .arg("--read-only")
        .arg("--shared=100")
        .arg("--persistent")
        .arg(test_disk);
    let listener_fd = {
        use std::os::fd::AsRawFd;
        listener.as_raw_fd()
    };
    configure_child_cleanup(&mut command, listener_fd);
    let process = command.spawn().unwrap();
    // The child duplicates this socket onto fd 3 before exec. Closing the
    // parent fd leaves the child's copy listening.
    drop(listener);
    let mut server = NBDServerProcess { process, url };
    wait_until_listening(&mut server.process, listen_ip, listen_port).unwrap_or_else(|err| {
        panic!("qemu-nbd failed to listen on {listen_ip}:{listen_port}: {err}")
    });
    eprintln!("Started NBD server on {}", server.url);
    server
}

fn bind_listen_socket(listen_ip: &str) -> std::net::TcpListener {
    std::net::TcpListener::bind((listen_ip, 0))
        .unwrap_or_else(|err| panic!("bind {listen_ip}:0 failed: {err}"))
}

fn configure_child_cleanup(command: &mut std::process::Command, listener_fd: std::os::fd::RawFd) {
    use std::os::unix::process::CommandExt;
    let parent_pid = std::process::id();
    // Runs after fork and before exec. Only async-signal-safe calls belong
    // here: the parent is multi-threaded, so allocating in the child can deadlock.
    // Command applies its own environment after this hook, so activation
    // variables are installed by replacing environ directly and Command::env
    // stays unused.
    unsafe {
        command.pre_exec(move || {
            arm_parent_death_signal(parent_pid)?;
            move_listener_to_activation_fd(listener_fd)?;
            install_activation_environment()?;
            close_inherited_fds()?;
            Ok(())
        });
    }
}

fn arm_parent_death_signal(parent_pid: u32) -> std::io::Result<()> {
    let armed = unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL as libc::c_ulong) };
    if armed != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let parent_alive = unsafe { libc::getppid() } == parent_pid as libc::pid_t;
    if !parent_alive {
        unsafe { libc::raise(libc::SIGKILL) };
        return Err(std::io::Error::from_raw_os_error(libc::ESRCH));
    }
    Ok(())
}

fn move_listener_to_activation_fd(listener_fd: std::os::fd::RawFd) -> std::io::Result<()> {
    if unsafe { libc::dup2(listener_fd, ACTIVATED_SOCKET_FD) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let flags = unsafe { libc::fcntl(ACTIVATED_SOCKET_FD, libc::F_GETFD) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let kept_open = flags & !libc::FD_CLOEXEC;
    if unsafe { libc::fcntl(ACTIVATED_SOCKET_FD, libc::F_SETFD, kept_open) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

fn install_activation_environment() -> std::io::Result<()> {
    let pid_slot = unsafe { &mut *LISTEN_PID_SLOT.0.get() };
    write_listen_pid(pid_slot, unsafe { libc::getpid() });
    let env_slots = unsafe { &mut *ENV_SLOTS.0.get() };
    let mut count = 0usize;
    unsafe {
        if !environ.is_null() {
            let mut entry = environ;
            while !(*entry).is_null() {
                let bytes = std::ffi::CStr::from_ptr(*entry).to_bytes();
                let replaced =
                    bytes.starts_with(b"LISTEN_PID=") || bytes.starts_with(b"LISTEN_FDS=");
                if !replaced {
                    if count + 3 >= ENV_CAPACITY {
                        return Err(std::io::Error::from_raw_os_error(libc::ENOMEM));
                    }
                    env_slots[count] = *entry;
                    count += 1;
                }
                entry = entry.add(1);
            }
        }
        env_slots[count] = pid_slot.as_ptr().cast();
        count += 1;
        env_slots[count] = LISTEN_FDS_ENTRY.as_ptr().cast();
        count += 1;
        env_slots[count] = std::ptr::null();
        environ = env_slots.as_ptr();
    }
    Ok(())
}

fn write_listen_pid(slot: &mut [u8], pid: libc::pid_t) {
    slot[..LISTEN_PID_PREFIX.len()].copy_from_slice(LISTEN_PID_PREFIX);
    let mut value = pid as u32;
    let mut reversed = [0u8; 10];
    let mut length = 0usize;
    loop {
        reversed[length] = b'0' + (value % 10) as u8;
        length += 1;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    for index in 0..length {
        slot[LISTEN_PID_PREFIX.len() + index] = reversed[length - 1 - index];
    }
    slot[LISTEN_PID_PREFIX.len() + length] = 0;
}

fn close_inherited_fds() -> std::io::Result<()> {
    if unsafe {
        libc::close_range(
            (ACTIVATED_SOCKET_FD + 1) as libc::c_uint,
            libc::c_uint::MAX,
            0,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

fn wait_until_listening(
    process: &mut std::process::Child,
    listen_ip: &str,
    port: u16,
) -> std::io::Result<()> {
    let local = proc_net_tcp_local(listen_ip, port)?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if process.try_wait()?.is_some() {
            return Err(std::io::Error::other(format!(
                "qemu-nbd exited before listening on {listen_ip}:{port}"
            )));
        }
        if command_is_qemu_nbd(process.id()) {
            match listener_state(process.id(), &local)? {
                ListenerState::Expected => {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                    if process.try_wait()?.is_some() {
                        return Err(std::io::Error::other(format!(
                            "qemu-nbd exited before listening on {listen_ip}:{port}"
                        )));
                    }
                    if command_is_qemu_nbd(process.id())
                        && listener_state(process.id(), &local)? == ListenerState::Expected
                    {
                        return Ok(());
                    }
                }
                ListenerState::Unexpected(locals) => {
                    return Err(std::io::Error::other(format!(
                        "qemu-nbd listen sockets {locals:?}, expected {local}"
                    )));
                }
                ListenerState::Pending => {}
            }
        }
        if std::time::Instant::now() >= deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("timed out waiting for {listen_ip}:{port}"),
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

#[derive(PartialEq, Eq)]
enum ListenerState {
    Pending,
    Expected,
    Unexpected(Vec<String>),
}

fn listener_state(pid: u32, local: &str) -> std::io::Result<ListenerState> {
    let locals = listening_locals(pid)?;
    if locals.is_empty() {
        Ok(ListenerState::Pending)
    } else if locals.len() == 1 && locals[0] == local {
        Ok(ListenerState::Expected)
    } else {
        Ok(ListenerState::Unexpected(locals))
    }
}

fn command_is_qemu_nbd(pid: u32) -> bool {
    let Ok(bytes) = std::fs::read(format!("/proc/{pid}/cmdline")) else {
        return false;
    };
    let Some(argv0) = bytes.split(|byte| *byte == 0).next() else {
        return false;
    };
    argv0 == b"qemu-nbd" || argv0.ends_with(b"/qemu-nbd")
}

fn listening_locals(pid: u32) -> std::io::Result<Vec<String>> {
    let mut inodes = Vec::new();
    for target in fd_targets(pid)? {
        if let Some(inode) = target
            .strip_prefix("socket:[")
            .and_then(|text| text.strip_suffix(']'))
        {
            inodes.push(inode.to_string());
        }
    }
    let mut locals = Vec::new();
    let file = std::fs::File::open("/proc/net/tcp")?;
    use std::io::BufRead;
    for line in std::io::BufReader::new(file).lines().skip(1) {
        if let Some(local) = listen_local_for_inodes(&line?, &inodes) {
            locals.push(local);
        }
    }
    Ok(locals)
}

fn listen_local_for_inodes(line: &str, inodes: &[String]) -> Option<String> {
    let fields: Vec<&str> = line.split_whitespace().collect();
    if fields.len() < 10 || fields[3] != "0A" {
        return None;
    }
    if inodes.iter().any(|inode| inode == fields[9]) {
        Some(fields[1].to_string())
    } else {
        None
    }
}

fn proc_net_tcp_local(listen_ip: &str, port: u16) -> std::io::Result<String> {
    let ip: std::net::Ipv4Addr = listen_ip.parse().map_err(|err| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{listen_ip} is not an IPv4 address: {err}"),
        )
    })?;
    Ok(format!(
        "{:08X}:{:04X}",
        u32::from_ne_bytes(ip.octets()),
        port
    ))
}

fn tcp_port_is_listening(local: &str) -> std::io::Result<bool> {
    use std::io::BufRead;
    let file = std::fs::File::open("/proc/net/tcp")?;
    for line in std::io::BufReader::new(file).lines().skip(1) {
        if line_is_listen(&line?, local) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn line_is_listen(line: &str, local: &str) -> bool {
    let mut fields = line.split_whitespace();
    let _slot = fields.next();
    let local_address = fields.next();
    let _remote_address = fields.next();
    let state = fields.next();
    local_address == Some(local) && state == Some("0A")
}

fn fd_targets(pid: u32) -> std::io::Result<Vec<String>> {
    let mut targets = Vec::new();
    for entry in std::fs::read_dir(format!("/proc/{pid}/fd"))? {
        let entry = entry?;
        match std::fs::read_link(entry.path()) {
            Ok(target) => targets.push(target.to_string_lossy().into_owned()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => return Err(err),
        }
    }
    Ok(targets)
}

struct RawSocket(libc::c_int);

impl Drop for RawSocket {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.0);
        }
    }
}

#[test]
fn listen_pid_entry_is_decimal() {
    let mut slot = [0u8; LISTEN_PID_SLOT_LEN];
    write_listen_pid(&mut slot, 12345);
    assert_eq!(&slot[..17], b"LISTEN_PID=12345\0");
    write_listen_pid(&mut slot, 4194304);
    assert_eq!(&slot[..19], b"LISTEN_PID=4194304\0");
}

#[test]
fn proc_net_tcp_local_matches_bound_socket() {
    let listener = std::net::TcpListener::bind(("127.0.0.2", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let local = proc_net_tcp_local("127.0.0.2", port).unwrap();
    assert!(tcp_port_is_listening(&local).unwrap());
    assert!(!line_is_listen(
        &format!("0: {local} 00000000:0000 01 00000000:00000000"),
        &local
    ));
}

#[test]
fn spawned_qemu_nbd_drops_inherited_sockets() {
    let extra_fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0) };
    assert!(extra_fd >= 0, "socket failed");
    let _extra = RawSocket(extra_fd);
    let inherited = std::fs::read_link(format!("/proc/self/fd/{extra_fd}")).unwrap();
    let inherited = inherited.to_string_lossy().into_owned();
    assert!(inherited.starts_with("socket:["));
    let server = run_nbd_server("127.0.0.2");
    let child_fds = fd_targets(server.process.id()).unwrap();
    let activated = std::fs::read_link(format!(
        "/proc/{}/fd/{ACTIVATED_SOCKET_FD}",
        server.process.id()
    ))
    .unwrap();
    assert!(
        activated.to_string_lossy().starts_with("socket:["),
        "activation fd is {activated:?}"
    );
    assert!(
        child_fds
            .iter()
            .any(|target| target.starts_with("socket:[")),
        "qemu-nbd has no listen socket: {child_fds:?}"
    );
    assert!(
        !child_fds.iter().any(|target| target == &inherited),
        "qemu-nbd inherited {inherited}"
    );
}
