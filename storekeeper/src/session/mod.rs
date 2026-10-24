mod connection;

use connection::{Completion, Connection, FileStat, Steps, Submission};
use file_protocol::VSOCK_PORT;
use io_uring::{IoUring, opcode, types};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

const SLOT_COUNT: usize = 32;
const RING_DEPTH: u32 = 256;

const OP_ACCEPT: u8 = 1;
const OP_RECV: u8 = 2;
const OP_SEND: u8 = 3;
const OP_OPEN: u8 = 4;
const OP_STAT: u8 = 5;
const OP_READ: u8 = 6;
const OP_CANCEL: u8 = 7;

const EMPTY_PATH: &[u8] = b"\0";
const OPEN_FLAGS: i32 = libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NONBLOCK;

pub(super) fn listen(root: OwnedFd) -> Result<(), String> {
    let listen_fd = bind_vsock()?;
    let mut reactor = Reactor::new(root)?;
    reactor.listen_fd = Some(listen_fd.as_raw_fd());
    reactor.try_accept()?;
    reactor.run()
}

#[cfg(test)]
fn serve_sockets(root: OwnedFd, sockets: Vec<OwnedFd>) -> Result<(), String> {
    let mut reactor = Reactor::new(root)?;
    for socket in sockets {
        prepare_socket(socket.as_raw_fd())?;
        reactor.adopt(socket)?;
        reactor.run_until_idle()?;
    }
    Ok(())
}

fn bind_vsock() -> Result<OwnedFd, String> {
    let fd = unsafe { libc::socket(libc::AF_VSOCK, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) };
    if fd < 0 {
        return Err(format!("socket: {}", io::Error::last_os_error()));
    }
    // SAFETY: `fd` came from socket(2) and this function owns it from here on.
    let listen_fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let address = libc::sockaddr_vm {
        svm_family: libc::AF_VSOCK as libc::sa_family_t,
        svm_reserved1: 0,
        svm_port: VSOCK_PORT,
        svm_cid: libc::VMADDR_CID_ANY,
        svm_zero: [0; 4],
    };
    let bound = unsafe {
        libc::bind(
            listen_fd.as_raw_fd(),
            &address as *const libc::sockaddr_vm as *const libc::sockaddr,
            size_of::<libc::sockaddr_vm>() as libc::socklen_t,
        )
    };
    if bound != 0 {
        return Err(format!(
            "bind vsock {VSOCK_PORT}: {}",
            io::Error::last_os_error()
        ));
    }
    if unsafe { libc::listen(listen_fd.as_raw_fd(), 128) } != 0 {
        return Err(format!("listen: {}", io::Error::last_os_error()));
    }
    eprintln!("listening on vsock port {VSOCK_PORT}");
    Ok(listen_fd)
}

#[cfg(test)]
fn prepare_socket(fd: RawFd) -> Result<(), String> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(format!("fcntl: {}", io::Error::last_os_error()));
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(format!("fcntl: {}", io::Error::last_os_error()));
    }
    let fd_flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if fd_flags < 0 {
        return Err(format!("fcntl: {}", io::Error::last_os_error()));
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFD, fd_flags | libc::FD_CLOEXEC) } < 0 {
        return Err(format!("fcntl: {}", io::Error::last_os_error()));
    }
    Ok(())
}

struct Reactor {
    ring: IoUring,
    slots: [Slot; SLOT_COUNT],
    root: OwnedFd,
    listen_fd: Option<RawFd>,
}

struct Slot {
    generation: u32,
    kind: Kind,
    socket: Option<OwnedFd>,
    file: Option<OwnedFd>,
    statx: libc::statx,
    open_how: types::OpenHow,
    iovecs: [libc::iovec; 3],
    message: libc::msghdr,
    inflight: Inflight,
}

enum Kind {
    Free,
    Accepting,
    Live(Box<Connection>),
}

#[derive(Default)]
struct Inflight {
    accept: bool,
    recv: bool,
    send: bool,
    open: bool,
    stat: bool,
    read: bool,
    cancel: bool,
}

impl Inflight {
    fn any(&self) -> bool {
        self.accept || self.recv || self.send || self.open || self.stat || self.read || self.cancel
    }
}

impl Reactor {
    fn new(root: OwnedFd) -> Result<Self, String> {
        let ring = IoUring::new(RING_DEPTH).map_err(|error| format!("io_uring: {error}"))?;
        Ok(Self {
            ring,
            slots: std::array::from_fn(|_| Slot::free()),
            root,
            listen_fd: None,
        })
    }

    fn run(&mut self) -> Result<(), String> {
        loop {
            self.wait_and_dispatch()?;
        }
    }

    #[cfg(test)]
    fn run_until_idle(&mut self) -> Result<(), String> {
        loop {
            if !self.slots.iter().any(|slot| slot.inflight.any()) {
                return Ok(());
            }
            self.wait_and_dispatch()?;
        }
    }

    #[cfg(test)]
    fn adopt(&mut self, socket: OwnedFd) -> Result<(), String> {
        let index = self
            .slots
            .iter()
            .position(|slot| matches!(slot.kind, Kind::Free))
            .expect("free connection slot");
        self.slots[index].socket = Some(socket);
        self.slots[index].kind = Kind::Live(Box::new(Connection::new()));
        self.perform(index, Steps::one(Submission::Recv))
    }

    fn wait_and_dispatch(&mut self) -> Result<(), String> {
        loop {
            match self.ring.submit_and_wait(1) {
                Ok(_) => break,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(format!("io_uring_enter: {error}")),
            }
        }
        let mut completed = Vec::new();
        for entry in self.ring.completion() {
            completed.push((entry.user_data(), entry.result()));
        }
        for (userdata, result) in completed {
            self.on_cqe(userdata, result)?;
        }
        Ok(())
    }

    fn on_cqe(&mut self, userdata: u64, result: i32) -> Result<(), String> {
        let (generation, index, op) = unpack_userdata(userdata);
        if index >= SLOT_COUNT || self.slots[index].generation != generation {
            return Ok(());
        }
        match op {
            OP_ACCEPT => self.on_accept(index, result),
            OP_RECV => self.finish_io(index, Op::Recv, result, Completion::Recv),
            OP_SEND => self.finish_io(index, Op::Send, result, Completion::Send),
            OP_READ => self.finish_io(index, Op::Read, result, Completion::Read),
            OP_OPEN => self.on_open(index, result),
            OP_STAT => self.on_stat(index, result),
            OP_CANCEL => self.on_cancel(index),
            _ => Ok(()),
        }
    }

    fn on_accept(&mut self, index: usize, result: i32) -> Result<(), String> {
        if !self.slots[index].inflight.accept {
            return Ok(());
        }
        self.slots[index].inflight.accept = false;
        if result < 0 {
            let error = io::Error::from_raw_os_error(-result);
            self.free_slot(index);
            if accept_is_fatal(&error) {
                return Err(format!("accept: {error}"));
            }
            eprintln!("accept: {error}");
            return self.try_accept();
        }
        // SAFETY: `result` is a new socket from accept and this slot owns it.
        self.slots[index].socket = Some(unsafe { OwnedFd::from_raw_fd(result) });
        self.slots[index].kind = Kind::Live(Box::new(Connection::new()));
        self.perform(index, Steps::one(Submission::Recv))?;
        self.try_accept()
    }

    fn on_open(&mut self, index: usize, result: i32) -> Result<(), String> {
        if !self.slots[index].inflight.open {
            return Ok(());
        }
        self.slots[index].inflight.open = false;
        if result < 0 {
            return self.dispatch(
                index,
                Completion::Open(Err(io::Error::from_raw_os_error(-result))),
            );
        }
        // SAFETY: `result` is a new file descriptor from openat and this slot owns it.
        self.slots[index].file = Some(unsafe { OwnedFd::from_raw_fd(result) });
        self.dispatch(index, Completion::Open(Ok(())))
    }

    fn on_stat(&mut self, index: usize, result: i32) -> Result<(), String> {
        if !self.slots[index].inflight.stat {
            return Ok(());
        }
        self.slots[index].inflight.stat = false;
        let completion = if result < 0 {
            Completion::Stat(Err(io::Error::from_raw_os_error(-result)))
        } else {
            let mode = self.slots[index].statx.stx_mode;
            let regular_file = mode & (libc::S_IFMT as u16) == (libc::S_IFREG as u16);
            Completion::Stat(Ok(FileStat {
                size: self.slots[index].statx.stx_size,
                regular_file,
            }))
        };
        self.dispatch(index, completion)
    }

    fn on_cancel(&mut self, index: usize) -> Result<(), String> {
        if !self.slots[index].inflight.cancel {
            return Ok(());
        }
        self.slots[index].inflight.cancel = false;
        self.dispatch(index, Completion::Cancel(Ok(())))
    }

    fn finish_io(
        &mut self,
        index: usize,
        op: Op,
        result: i32,
        completion: fn(io::Result<usize>) -> Completion,
    ) -> Result<(), String> {
        if !self.slots[index].inflight_flag(op) {
            return Ok(());
        }
        self.slots[index].set_inflight(op, false);
        self.dispatch(index, completion(cqe_result(result)))
    }

    fn dispatch(&mut self, index: usize, completion: Completion) -> Result<(), String> {
        let steps = {
            let connection = self.slots[index].connection_mut();
            let steps = connection.advance(completion);
            if let Some(message) = connection.take_error_log() {
                eprintln!("connection: {message}");
            }
            steps
        };
        self.perform(index, steps)
    }

    fn perform(&mut self, index: usize, steps: Steps) -> Result<(), String> {
        for submission in steps.into_array().into_iter().flatten() {
            self.perform_one(index, submission)?;
        }
        Ok(())
    }

    fn perform_one(&mut self, index: usize, submission: Submission) -> Result<(), String> {
        match submission {
            Submission::Recv => self.submit_recv(index),
            Submission::Open => self.submit_open(index),
            Submission::Stat => self.submit_stat(index),
            Submission::Read {
                buf,
                offset,
                at,
                len,
            } => self.submit_read(index, buf, offset, at, len),
            Submission::Send => self.submit_send(index),
            Submission::CancelRead => self.submit_cancel(index),
            Submission::Close => {
                self.free_slot(index);
                self.try_accept()
            }
        }
    }

    fn try_accept(&mut self) -> Result<(), String> {
        let Some(listen_fd) = self.listen_fd else {
            return Ok(());
        };
        if self.slots.iter().any(|slot| slot.inflight.accept) {
            return Ok(());
        }
        let Some(index) = self
            .slots
            .iter()
            .position(|slot| matches!(slot.kind, Kind::Free))
        else {
            return Ok(());
        };
        let generation = self.slots[index].generation;
        let entry = opcode::Accept::new(
            types::Fd(listen_fd),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
        .flags(libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK)
        .build()
        .user_data(pack_userdata(generation, index, OP_ACCEPT));
        push(&mut self.ring, &entry)?;
        self.slots[index].kind = Kind::Accepting;
        self.slots[index].inflight.accept = true;
        Ok(())
    }

    fn submit_recv(&mut self, index: usize) -> Result<(), String> {
        let fd = socket_fd(&self.slots[index])?;
        let generation = self.slots[index].generation;
        let (ptr, len) = {
            let region = self.slots[index].connection_mut().recv_region();
            (region.as_mut_ptr(), region.len())
        };
        let entry = opcode::Recv::new(types::Fd(fd), ptr, len_u32(len))
            .build()
            .user_data(pack_userdata(generation, index, OP_RECV));
        push(&mut self.ring, &entry)?;
        self.slots[index].inflight.recv = true;
        Ok(())
    }

    fn submit_open(&mut self, index: usize) -> Result<(), String> {
        let generation = self.slots[index].generation;
        let path = self.slots[index].connection_mut().open_path().as_ptr();
        self.slots[index].open_how = types::OpenHow::new()
            .flags(OPEN_FLAGS as u64)
            .resolve(libc::RESOLVE_IN_ROOT);
        let how = std::ptr::addr_of!(self.slots[index].open_how);
        let root = self.root.as_raw_fd();
        let entry = opcode::OpenAt2::new(types::Fd(root), path, how)
            .build()
            .user_data(pack_userdata(generation, index, OP_OPEN));
        push(&mut self.ring, &entry)?;
        self.slots[index].inflight.open = true;
        Ok(())
    }

    fn submit_stat(&mut self, index: usize) -> Result<(), String> {
        let fd = file_fd(&self.slots[index])?;
        let generation = self.slots[index].generation;
        // SAFETY: the buffer is a valid `statx` and stays in this slot until the completion.
        self.slots[index].statx = unsafe { std::mem::zeroed() };
        let statx_ptr = std::ptr::addr_of_mut!(self.slots[index].statx).cast();
        let entry = opcode::Statx::new(types::Fd(fd), EMPTY_PATH.as_ptr().cast(), statx_ptr)
            .flags(libc::AT_EMPTY_PATH)
            .mask(libc::STATX_TYPE | libc::STATX_SIZE)
            .build()
            .user_data(pack_userdata(generation, index, OP_STAT));
        push(&mut self.ring, &entry)?;
        self.slots[index].inflight.stat = true;
        Ok(())
    }

    fn submit_read(
        &mut self,
        index: usize,
        buf: usize,
        offset: u64,
        at: usize,
        len: usize,
    ) -> Result<(), String> {
        let fd = file_fd(&self.slots[index])?;
        let generation = self.slots[index].generation;
        let ptr = self.slots[index]
            .connection_mut()
            .read_region(buf, at, len)
            .as_mut_ptr();
        let entry = opcode::Read::new(types::Fd(fd), ptr, len_u32(len))
            .offset(offset)
            .build()
            .user_data(pack_userdata(generation, index, OP_READ));
        push(&mut self.ring, &entry)?;
        self.slots[index].inflight.read = true;
        Ok(())
    }

    fn submit_send(&mut self, index: usize) -> Result<(), String> {
        let fd = socket_fd(&self.slots[index])?;
        let generation = self.slots[index].generation;
        let mut raw_parts = [(std::ptr::null::<u8>(), 0usize); 3];
        let mut count = 0;
        {
            let pending = self.slots[index].connection().pending_send();
            for part in pending.parts.into_iter().take(pending.count) {
                if part.is_empty() {
                    continue;
                }
                raw_parts[count] = (part.as_ptr(), part.len());
                count += 1;
            }
        }
        if count == 0 {
            return Err("send has no bytes".to_string());
        }
        for (part_index, (ptr, len)) in raw_parts.into_iter().take(count).enumerate() {
            self.slots[index].iovecs[part_index] = libc::iovec {
                iov_base: ptr.cast_mut().cast(),
                iov_len: len,
            };
        }
        // SAFETY: `message` only points at `iovecs` in this same slot.
        self.slots[index].message = unsafe { std::mem::zeroed() };
        self.slots[index].message.msg_iov = self.slots[index].iovecs.as_mut_ptr();
        // musl stores this count as `c_int`; glibc stores it as `size_t`.
        self.slots[index].message.msg_iovlen = count as _;
        let entry = opcode::SendMsg::new(types::Fd(fd), &self.slots[index].message)
            .build()
            .user_data(pack_userdata(generation, index, OP_SEND));
        push(&mut self.ring, &entry)?;
        self.slots[index].inflight.send = true;
        Ok(())
    }

    fn submit_cancel(&mut self, index: usize) -> Result<(), String> {
        let generation = self.slots[index].generation;
        let target = pack_userdata(generation, index, OP_READ);
        let entry = opcode::AsyncCancel::new(target)
            .build()
            .user_data(pack_userdata(generation, index, OP_CANCEL));
        push(&mut self.ring, &entry)?;
        self.slots[index].inflight.cancel = true;
        Ok(())
    }

    fn free_slot(&mut self, index: usize) {
        let slot = &mut self.slots[index];
        if slot.inflight.any() {
            eprintln!("connection: slot {index} closed with work in flight");
            return;
        }
        slot.socket = None;
        slot.file = None;
        slot.kind = Kind::Free;
        slot.generation = slot.generation.wrapping_add(1);
        if slot.generation == 0 {
            slot.generation = 1;
        }
    }
}

impl Slot {
    fn free() -> Self {
        Self {
            generation: 1,
            kind: Kind::Free,
            socket: None,
            file: None,
            // SAFETY: `statx` is a plain data buffer and zero is a valid initial value.
            statx: unsafe { std::mem::zeroed() },
            open_how: types::OpenHow::new(),
            iovecs: [libc::iovec {
                iov_base: std::ptr::null_mut(),
                iov_len: 0,
            }; 3],
            // SAFETY: `msghdr` is a plain data buffer and zero is a valid initial value.
            message: unsafe { std::mem::zeroed() },
            inflight: Inflight::default(),
        }
    }

    fn connection(&self) -> &Connection {
        match &self.kind {
            Kind::Live(connection) => connection,
            Kind::Free | Kind::Accepting => unreachable!("completion for an idle slot"),
        }
    }

    fn connection_mut(&mut self) -> &mut Connection {
        match &mut self.kind {
            Kind::Live(connection) => connection,
            Kind::Free | Kind::Accepting => unreachable!("completion for an idle slot"),
        }
    }

    fn inflight_flag(&self, op: Op) -> bool {
        match op {
            Op::Recv => self.inflight.recv,
            Op::Send => self.inflight.send,
            Op::Read => self.inflight.read,
        }
    }

    fn set_inflight(&mut self, op: Op, value: bool) {
        match op {
            Op::Recv => self.inflight.recv = value,
            Op::Send => self.inflight.send = value,
            Op::Read => self.inflight.read = value,
        }
    }
}

#[derive(Clone, Copy)]
enum Op {
    Recv,
    Send,
    Read,
}

fn socket_fd(slot: &Slot) -> Result<RawFd, String> {
    slot.socket
        .as_ref()
        .map(AsRawFd::as_raw_fd)
        .ok_or_else(|| "missing socket".to_string())
}

fn file_fd(slot: &Slot) -> Result<RawFd, String> {
    slot.file
        .as_ref()
        .map(AsRawFd::as_raw_fd)
        .ok_or_else(|| "missing file".to_string())
}

fn push(ring: &mut IoUring, entry: &io_uring::squeue::Entry) -> Result<(), String> {
    unsafe {
        // SAFETY: the caller keeps every buffer and descriptor referenced by `entry`
        // alive and unmodified until the matching completion is consumed.
        ring.submission()
            .push(entry)
            .map_err(|_| "io_uring submission queue is full".to_string())
    }
}

fn cqe_result(result: i32) -> io::Result<usize> {
    if result < 0 {
        Err(io::Error::from_raw_os_error(-result))
    } else {
        Ok(usize::try_from(result).unwrap_or(0))
    }
}

fn len_u32(len: usize) -> u32 {
    u32::try_from(len).expect("transfer length fits in u32")
}

fn pack_userdata(generation: u32, slot: usize, op: u8) -> u64 {
    (u64::from(generation) << 16) | ((slot as u64) << 8) | u64::from(op)
}

fn unpack_userdata(userdata: u64) -> (u32, usize, u8) {
    let generation = (userdata >> 16) as u32;
    let slot = ((userdata >> 8) & 0xff) as usize;
    let op = (userdata & 0xff) as u8;
    (generation, slot, op)
}

fn accept_is_fatal(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(libc::EBADF | libc::ENOTSOCK | libc::EINVAL | libc::EFAULT)
    )
}

#[cfg(test)]
mod tests;
