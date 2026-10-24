use file_protocol::{
    ChunkHeader, FILE_NOT_FOUND, FileRequest, INVALID_SEND, NOT_A_REGULAR_FILE, ProtocolError,
    read_request, write_error, write_info,
};
use std::ffi::{CStr, CString};
use std::io::{self, Cursor};

#[cfg(test)]
mod tests;

const RECV_CAP: usize = 8192;
const CHUNK_TRAILER: [u8; 1] = [0];

pub(super) struct Connection {
    stage: Stage,
    recv_buf: Vec<u8>,
    recv_filled: usize,
    path: CString,
    chunk_size: usize,
    chunks: [Vec<u8>; 2],
    filled: [usize; 2],
    buf_status: [BufStatus; 2],
    next_fill: usize,
    next_send: usize,
    filling: usize,
    file_offset: u64,
    saw_eof: bool,
    read_inflight: bool,
    cancel_inflight: bool,
    header: ChunkHeader,
    trailer: [u8; 1],
    send: Option<Outgoing>,
    follow_up: FollowUp,
    read_error: Option<io::Error>,
    log_error: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Stage {
    Request,
    Open,
    Stat,
    Body,
    Drain,
    Done,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BufStatus {
    Empty,
    Filling,
    Ready,
    Sending,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FollowUp {
    Close,
    StartBody,
    Pump,
}

struct Outgoing {
    kind: OutgoingKind,
    sent: usize,
    total: usize,
}

enum OutgoingKind {
    Linear(Vec<u8>),
    Chunk { buf: usize, body_len: usize },
}

enum SendShape {
    None,
    Linear,
    Chunk,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct FileStat {
    pub size: u64,
    pub regular_file: bool,
}

pub(super) enum Completion {
    Recv(io::Result<usize>),
    Open(io::Result<()>),
    Stat(io::Result<FileStat>),
    Read(io::Result<usize>),
    Send(io::Result<usize>),
    Cancel(io::Result<()>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Submission {
    Recv,
    Open,
    Stat,
    Read {
        buf: usize,
        offset: u64,
        at: usize,
        len: usize,
    },
    Send,
    CancelRead,
    Close,
}

pub(super) struct Steps {
    first: Option<Submission>,
    second: Option<Submission>,
}

impl Steps {
    pub(super) fn none() -> Self {
        Self {
            first: None,
            second: None,
        }
    }

    pub(super) fn one(submission: Submission) -> Self {
        Self {
            first: Some(submission),
            second: None,
        }
    }

    fn push(&mut self, submission: Submission) {
        if self.first.is_none() {
            self.first = Some(submission);
        } else if self.second.is_none() {
            self.second = Some(submission);
        } else {
            unreachable!("a connection schedules at most two submissions");
        }
    }

    pub(super) fn into_array(self) -> [Option<Submission>; 2] {
        [self.first, self.second]
    }
}

pub(super) struct PendingSend<'a> {
    pub parts: [&'a [u8]; 3],
    pub count: usize,
}

impl Connection {
    pub(super) fn new() -> Self {
        Self {
            stage: Stage::Request,
            recv_buf: vec![0; RECV_CAP],
            recv_filled: 0,
            path: CString::default(),
            chunk_size: 0,
            chunks: [Vec::new(), Vec::new()],
            filled: [0, 0],
            buf_status: [BufStatus::Empty, BufStatus::Empty],
            next_fill: 0,
            next_send: 0,
            filling: 0,
            file_offset: 0,
            saw_eof: false,
            read_inflight: false,
            cancel_inflight: false,
            header: ChunkHeader::new(0).expect("empty chunk header"),
            trailer: CHUNK_TRAILER,
            send: None,
            follow_up: FollowUp::Close,
            read_error: None,
            log_error: None,
        }
    }

    pub(super) fn recv_region(&mut self) -> &mut [u8] {
        let filled = self.recv_filled;
        &mut self.recv_buf[filled..]
    }

    pub(super) fn read_region(&mut self, buf: usize, at: usize, len: usize) -> &mut [u8] {
        &mut self.chunks[buf][at..at + len]
    }

    pub(super) fn open_path(&self) -> &CStr {
        &self.path
    }

    pub(super) fn pending_send(&self) -> PendingSend<'_> {
        match self.send_shape() {
            SendShape::None => PendingSend {
                parts: [&[], &[], &[]],
                count: 0,
            },
            SendShape::Linear => self.pending_linear(),
            SendShape::Chunk => self.pending_chunk(),
        }
    }

    pub(super) fn take_error_log(&mut self) -> Option<String> {
        self.log_error.take()
    }

    pub(super) fn advance(&mut self, completion: Completion) -> Steps {
        match completion {
            Completion::Recv(result) => self.on_recv(result),
            Completion::Open(result) => self.on_open(result),
            Completion::Stat(result) => self.on_stat(result),
            Completion::Read(result) => self.on_read(result),
            Completion::Send(result) => self.on_send(result),
            Completion::Cancel(result) => self.on_cancel(result),
        }
    }

    fn on_recv(&mut self, result: io::Result<usize>) -> Steps {
        if self.stage != Stage::Request {
            return self.fail("unexpected recv");
        }
        let read_bytes = match result {
            Ok(0) => return self.quiet_close(),
            Ok(read_bytes) => read_bytes,
            Err(error) if is_retry(&error) => return Steps::one(Submission::Recv),
            Err(error) => return self.fail_io(error),
        };
        self.recv_filled += read_bytes;
        match read_request(&mut Cursor::new(&self.recv_buf[..self.recv_filled])) {
            Ok(request) => self.begin_open(request),
            Err(ProtocolError::Io(error)) if error.kind() == io::ErrorKind::UnexpectedEof => {
                if self.recv_filled == self.recv_buf.len() {
                    self.fail_with_reply("malformed request", "request is too long")
                } else {
                    Steps::one(Submission::Recv)
                }
            }
            Err(ProtocolError::InvalidSend) => self.queue_error(INVALID_SEND),
            Err(error) => self.fail_with_reply("malformed request", &error.to_string()),
        }
    }

    fn on_open(&mut self, result: io::Result<()>) -> Steps {
        if self.stage != Stage::Open {
            return self.fail("unexpected open");
        }
        match result {
            Ok(()) => {
                self.stage = Stage::Stat;
                Steps::one(Submission::Stat)
            }
            Err(error) if is_retry(&error) => Steps::one(Submission::Open),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                self.queue_error(FILE_NOT_FOUND)
            }
            Err(error) => self.queue_error(&error.to_string()),
        }
    }

    fn on_stat(&mut self, result: io::Result<FileStat>) -> Steps {
        if self.stage != Stage::Stat {
            return self.fail("unexpected stat");
        }
        match result {
            Ok(stat) if !stat.regular_file => self.queue_error(NOT_A_REGULAR_FILE),
            Ok(stat) => self.queue_info(stat.size),
            Err(error) if is_retry(&error) => Steps::one(Submission::Stat),
            Err(error) => self.queue_error(&error.to_string()),
        }
    }

    fn on_read(&mut self, result: io::Result<usize>) -> Steps {
        if self.stage == Stage::Drain {
            self.read_inflight = false;
            return self.finish_drain();
        }
        if self.stage != Stage::Body || !self.read_inflight {
            return self.fail("unexpected read");
        }
        let read_bytes = match result {
            Ok(read_bytes) => read_bytes,
            Err(error) if is_retry(&error) => return self.continue_read(),
            Err(error) => return self.read_failed(error),
        };
        let buf = self.filling;
        if read_bytes == 0 {
            self.read_inflight = false;
            self.saw_eof = true;
            if self.filled[buf] == 0 {
                self.buf_status[buf] = BufStatus::Empty;
            } else {
                self.finish_fill(buf);
            }
            return self.pump();
        }
        self.filled[buf] += read_bytes;
        self.file_offset += read_bytes as u64;
        if self.filled[buf] == self.chunk_size {
            self.finish_fill(buf);
            return self.pump();
        }
        self.continue_read()
    }

    fn on_send(&mut self, result: io::Result<usize>) -> Steps {
        if self.send.is_none() {
            return self.fail("unexpected send");
        }
        let sent_now = match result {
            Ok(0) => return self.fail("broken pipe"),
            Ok(sent_now) => sent_now,
            Err(error) if is_retry(&error) => return Steps::one(Submission::Send),
            Err(error) => return self.fail_io(error),
        };
        let finished = {
            let outgoing = self.send.as_mut().expect("send is in flight");
            outgoing.sent += sent_now;
            outgoing.sent >= outgoing.total
        };
        if finished {
            self.finish_send()
        } else {
            Steps::one(Submission::Send)
        }
    }

    fn on_cancel(&mut self, _result: io::Result<()>) -> Steps {
        if self.stage != Stage::Drain {
            return self.fail("unexpected cancel");
        }
        self.cancel_inflight = false;
        self.finish_drain()
    }

    fn begin_open(&mut self, request: FileRequest) -> Steps {
        self.chunk_size = request.chunk_size;
        self.chunks = [vec![0; request.chunk_size], vec![0; request.chunk_size]];
        self.path = CString::new(request.path).expect("request path has no NUL");
        self.stage = Stage::Open;
        Steps::one(Submission::Open)
    }

    fn queue_info(&mut self, size: u64) -> Steps {
        let mut bytes = Vec::new();
        if let Err(error) = write_info(&mut bytes, size) {
            return self.fail_io_protocol(error);
        }
        self.arm_linear(bytes, FollowUp::StartBody);
        Steps::one(Submission::Send)
    }

    fn queue_error(&mut self, text: &str) -> Steps {
        let mut bytes = Vec::new();
        if let Err(error) = write_error(&mut bytes, text) {
            return self.fail_io_protocol(error);
        }
        self.arm_linear(bytes, FollowUp::Close);
        Steps::one(Submission::Send)
    }

    fn fail_with_reply(&mut self, text: &str, log: &str) -> Steps {
        self.log_error = Some(log.to_string());
        self.queue_error(text)
    }

    fn read_failed(&mut self, error: io::Error) -> Steps {
        self.read_inflight = false;
        self.saw_eof = true;
        let buf = self.filling;
        self.buf_status[buf] = BufStatus::Empty;
        self.filled[buf] = 0;
        if self.send.is_some() {
            self.read_error = Some(error);
            return Steps::none();
        }
        self.log_error = Some(error.to_string());
        self.queue_error("read failed")
    }

    fn finish_fill(&mut self, buf: usize) {
        self.read_inflight = false;
        self.buf_status[buf] = BufStatus::Ready;
        self.next_fill = (buf + 1) % 2;
    }

    fn pump(&mut self) -> Steps {
        let mut steps = Steps::none();
        if self.send.is_none() && self.buf_status[self.next_send] == BufStatus::Ready {
            self.arm_chunk(self.next_send);
            steps.push(Submission::Send);
        } else if self.send.is_none() && self.saw_eof && !self.read_inflight && self.buffers_quiet()
        {
            self.arm_empty();
            steps.push(Submission::Send);
        }
        if !self.read_inflight
            && !self.saw_eof
            && self.buf_status[self.next_fill] == BufStatus::Empty
        {
            steps.push(self.start_read(self.next_fill));
        }
        steps
    }

    fn start_read(&mut self, buf: usize) -> Submission {
        self.filling = buf;
        self.filled[buf] = 0;
        self.buf_status[buf] = BufStatus::Filling;
        self.read_inflight = true;
        Submission::Read {
            buf,
            offset: self.file_offset,
            at: 0,
            len: self.chunk_size,
        }
    }

    fn continue_read(&mut self) -> Steps {
        let buf = self.filling;
        let at = self.filled[buf];
        Steps::one(Submission::Read {
            buf,
            offset: self.file_offset,
            at,
            len: self.chunk_size - at,
        })
    }

    fn arm_chunk(&mut self, buf: usize) {
        let body_len = self.filled[buf];
        let header = ChunkHeader::new(body_len).expect("chunk length was accepted");
        let total = header.as_bytes().len() + body_len + 1;
        self.header = header;
        self.buf_status[buf] = BufStatus::Sending;
        self.follow_up = FollowUp::Pump;
        self.send = Some(Outgoing {
            kind: OutgoingKind::Chunk { buf, body_len },
            sent: 0,
            total,
        });
    }

    fn arm_empty(&mut self) {
        let header = ChunkHeader::new(0).expect("empty chunk header");
        let total = header.as_bytes().len() + 1;
        self.header = header;
        self.follow_up = FollowUp::Close;
        self.send = Some(Outgoing {
            kind: OutgoingKind::Chunk {
                buf: 0,
                body_len: 0,
            },
            sent: 0,
            total,
        });
    }

    fn arm_linear(&mut self, bytes: Vec<u8>, follow_up: FollowUp) {
        let total = bytes.len();
        self.follow_up = follow_up;
        self.send = Some(Outgoing {
            kind: OutgoingKind::Linear(bytes),
            sent: 0,
            total,
        });
    }

    fn finish_send(&mut self) -> Steps {
        let kind = self.send.take().expect("send is in flight").kind;
        if let OutgoingKind::Chunk { buf, body_len } = kind
            && body_len > 0
        {
            self.buf_status[buf] = BufStatus::Empty;
            self.filled[buf] = 0;
            self.next_send = (buf + 1) % 2;
        }
        if let Some(error) = self.read_error.take() {
            self.log_error = Some(error.to_string());
            return self.queue_error("read failed");
        }
        match self.follow_up {
            FollowUp::Close => self.quiet_close(),
            FollowUp::StartBody => {
                self.stage = Stage::Body;
                self.follow_up = FollowUp::Pump;
                self.pump()
            }
            FollowUp::Pump => self.pump(),
        }
    }

    fn finish_drain(&mut self) -> Steps {
        if self.read_inflight || self.cancel_inflight {
            return Steps::none();
        }
        self.stage = Stage::Done;
        Steps::one(Submission::Close)
    }

    fn quiet_close(&mut self) -> Steps {
        if self.read_inflight {
            self.stage = Stage::Drain;
            self.cancel_inflight = true;
            return Steps::one(Submission::CancelRead);
        }
        self.stage = Stage::Done;
        Steps::one(Submission::Close)
    }

    fn fail(&mut self, message: &str) -> Steps {
        self.log_error = Some(message.to_string());
        self.send = None;
        self.quiet_close()
    }

    fn fail_io(&mut self, error: io::Error) -> Steps {
        let message = error.to_string();
        self.fail(&message)
    }

    fn fail_io_protocol(&mut self, error: ProtocolError) -> Steps {
        let message = error.to_string();
        self.fail(&message)
    }

    fn buffers_quiet(&self) -> bool {
        self.buf_status
            .iter()
            .all(|status| *status == BufStatus::Empty)
    }

    fn send_shape(&self) -> SendShape {
        match self.send.as_ref().map(|outgoing| &outgoing.kind) {
            None => SendShape::None,
            Some(OutgoingKind::Linear(_)) => SendShape::Linear,
            Some(OutgoingKind::Chunk { .. }) => SendShape::Chunk,
        }
    }

    fn pending_linear(&self) -> PendingSend<'_> {
        let outgoing = self.send.as_ref().expect("linear send");
        let OutgoingKind::Linear(bytes) = &outgoing.kind else {
            unreachable!("linear send");
        };
        skip_parts([bytes.as_slice(), &[], &[]], outgoing.sent)
    }

    fn pending_chunk(&self) -> PendingSend<'_> {
        let (buf, body_len, sent) = {
            let outgoing = self.send.as_ref().expect("chunk send");
            let OutgoingKind::Chunk { buf, body_len } = outgoing.kind else {
                unreachable!("chunk send");
            };
            (buf, body_len, outgoing.sent)
        };
        let body = if body_len == 0 {
            &[][..]
        } else {
            &self.chunks[buf][..body_len]
        };
        skip_parts([self.header.as_bytes(), body, &self.trailer], sent)
    }
}

fn skip_parts(parts: [&[u8]; 3], mut skip: usize) -> PendingSend<'_> {
    let mut kept = [&[][..]; 3];
    let mut count = 0;
    for part in parts {
        if part.is_empty() {
            continue;
        }
        if skip >= part.len() {
            skip -= part.len();
            continue;
        }
        kept[count] = &part[skip..];
        count += 1;
        skip = 0;
    }
    PendingSend { parts: kept, count }
}

fn is_retry(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
    )
}
