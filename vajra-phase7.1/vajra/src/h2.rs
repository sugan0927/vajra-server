//! HTTP/2 (RFC 9113), server side, sans-I/O.
//!
//! The worker owns the sockets and TLS. It hands this module decrypted bytes
//! ([`H2Conn::feed`]) and sends whatever lands in the `out` buffer.
//!
//! ## What is implemented
//! * Connection preface, SETTINGS (+ACK), PING, GOAWAY, RST_STREAM, PRIORITY (ignored),
//!   WINDOW_UPDATE, HEADERS/CONTINUATION (padding + priority fields handled), DATA.
//! * HPACK decoding via the `hpack` crate (Huffman + dynamic table). Encoding is
//!   hand-written and stateless (literals without indexing), so the encoder can
//!   never desynchronise from the peer's table.
//! * Receive-side flow control (batched WINDOW_UPDATEs) and send-side flow
//!   control at connection and stream level, including SETTINGS_INITIAL_WINDOW_SIZE
//!   changes. Response bodies are scheduled round-robin across streams.
//! * Request bodies are buffered (bounded by `max_body`) until END_STREAM, then the
//!   request is surfaced via [`H2Conn::take_ready`].
//! * Abuse limits: concurrent streams, header block size, RST_STREAM count.
//! * The RFC 9113 §5.1 stream state machine (see [`StreamState`]): frames that are
//!   illegal in a stream's current state get the prescribed stream or connection
//!   error, and a response for a stream the peer already reset is discarded.
//! * Request validation (§8.1.2): pseudo-header rules, `content-length` must match
//!   the DATA actually received, PRIORITY frames must not make a stream depend on itself.
//!
//! ## Deliberately not implemented
//! Server push (never sent), PRIORITY scheduling, extended CONNECT, h2c upgrade.
//!
//! ## Concurrency model
//! `feed` stops after at most one request becomes ready, so the worker can
//! dispatch it (possibly starting an asynchronous proxy job) before parsing
//! further frames.

use crate::static_files::OpenFile;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::os::fd::RawFd;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Diagnostics: when on, every inbound frame, every connection/stream error we
/// answer with (and why) and every response head is written to stderr. Enabled
/// with `VAJRA_H2_TRACE=1` (see `main.rs`); it replaces a tcpdump + tshark
/// session when hunting a browser-only HTTP/2 failure.
static TRACE: AtomicBool = AtomicBool::new(false);

pub fn set_trace(on: bool) {
    TRACE.store(on, Ordering::Relaxed);
}

#[inline]
fn tracing() -> bool {
    TRACE.load(Ordering::Relaxed)
}

fn frame_name(ty: u8) -> &'static str {
    match ty {
        0x0 => "DATA",
        0x1 => "HEADERS",
        0x2 => "PRIORITY",
        0x3 => "RST_STREAM",
        0x4 => "SETTINGS",
        0x5 => "PUSH_PROMISE",
        0x6 => "PING",
        0x7 => "GOAWAY",
        0x8 => "WINDOW_UPDATE",
        0x9 => "CONTINUATION",
        0x10 => "PRIORITY_UPDATE",
        _ => "UNKNOWN",
    }
}

fn error_name(code: u32) -> &'static str {
    match code {
        0x0 => "NO_ERROR",
        0x1 => "PROTOCOL_ERROR",
        0x2 => "INTERNAL_ERROR",
        0x3 => "FLOW_CONTROL_ERROR",
        0x4 => "SETTINGS_TIMEOUT",
        0x5 => "STREAM_CLOSED",
        0x6 => "FRAME_SIZE_ERROR",
        0x7 => "REFUSED_STREAM",
        0x8 => "CANCEL",
        0x9 => "COMPRESSION_ERROR",
        0xa => "CONNECT_ERROR",
        0xb => "ENHANCE_YOUR_CALM",
        _ => "?",
    }
}

const PREFACE: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";

// Frame types.
const DATA: u8 = 0x0;
const HEADERS: u8 = 0x1;
const PRIORITY: u8 = 0x2;
const RST_STREAM: u8 = 0x3;
const SETTINGS: u8 = 0x4;
const PUSH_PROMISE: u8 = 0x5;
const PING: u8 = 0x6;
const GOAWAY: u8 = 0x7;
const WINDOW_UPDATE: u8 = 0x8;
const CONTINUATION: u8 = 0x9;

// Flags.
const END_STREAM: u8 = 0x1;
const ACK: u8 = 0x1;
const END_HEADERS: u8 = 0x4;
const PADDED: u8 = 0x8;
const PRIORITY_FLAG: u8 = 0x20;

// Error codes.
pub const NO_ERROR: u32 = 0x0;
pub const PROTOCOL_ERROR: u32 = 0x1;
pub const INTERNAL_ERROR: u32 = 0x2;
pub const FLOW_CONTROL_ERROR: u32 = 0x3;
pub const STREAM_CLOSED: u32 = 0x5;
pub const FRAME_SIZE_ERROR: u32 = 0x6;
pub const REFUSED_STREAM: u32 = 0x7;
pub const CANCEL: u32 = 0x8;
pub const COMPRESSION_ERROR: u32 = 0x9;
pub const ENHANCE_YOUR_CALM: u32 = 0xb;

/// Largest frame payload we accept (the protocol default).
const MAX_FRAME: usize = 16_384;
/// Largest frame payload we *send*, regardless of what the peer allows.
const SEND_FRAME: usize = 16_384;
const MAX_HEADER_BLOCK: usize = 64 * 1024;
const MAX_CONCURRENT: u32 = 100;
const DEFAULT_WINDOW: i64 = 65_535;
const MAX_WINDOW: i64 = 0x7fff_ffff;
const WINDOW_BATCH: u32 = 16_384;
const MAX_RST: u32 = 1_000;

/// Bytes read from a file per `IORING_OP_READ` for HTTP/2 and TLS bodies.
pub const FILE_CHUNK: usize = 64 * 1024;

pub struct H2Request {
    pub stream: u32,
    pub method: String,
    /// Path plus optional `?query`.
    pub path: String,
    pub authority: Vec<u8>,
    /// Regular (non-pseudo) headers; names are lowercase. Cookies are merged.
    pub headers: Vec<(Vec<u8>, Vec<u8>)>,
    pub body: Vec<u8>,
}

pub enum H2Body {
    Empty,
    Mem(Vec<u8>),
    File(Rc<OpenFile>),
}

pub struct H2Response {
    pub status: u16,
    /// Complete header list (lowercase names), excluding `:status`.
    pub headers: Vec<(Vec<u8>, Vec<u8>)>,
    pub body: H2Body,
}

/// A file read the worker must perform; its result goes to [`H2Conn::file_data`].
#[derive(Clone, Copy, Debug)]
pub struct FileRead {
    pub stream: u32,
    pub fd: RawFd,
    pub off: u64,
    pub len: u32,
}

pub struct Feed {
    /// Bytes of input fully consumed.
    pub consumed: usize,
    /// Connection error: GOAWAY was queued, close after flushing.
    pub fatal: bool,
}

struct OpenReq {
    content_length: Option<u64>,
    method: String,
    path: String,
    authority: Vec<u8>,
    headers: Vec<(Vec<u8>, Vec<u8>)>,
    body: Vec<u8>,
    unacked: u32,
}

impl OpenReq {
    fn into_request(self, stream: u32) -> H2Request {
        H2Request {
            stream,
            method: self.method,
            path: self.path,
            authority: self.authority,
            headers: self.headers,
            body: self.body,
        }
    }
}

/// How a HEADERS frame (and its CONTINUATIONs) is to be treated once decoded.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum HKind {
    /// First HEADERS of a new request.
    New,
    /// New request we will not serve (draining or too many streams).
    Refuse,
    /// Trailers on a stream that is still receiving its request.
    Trailer,
    /// Stream error: answer with RST_STREAM(code) after decoding.
    Reject(u32),
    /// Stream we already reset ourselves: decode and drop silently.
    Ignore,
}

struct Cont {
    stream: u32,
    end_stream: bool,
    kind: HKind,
    block: Vec<u8>,
}

/// Stream states of RFC 9113 §5.1 as seen from the server. Server push is never
/// used, so there are no reserved states, and a response is only produced after
/// the whole request has arrived, so *half-closed (local)* is never observable
/// while the peer can still send; it is folded into `Closed`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum StreamState {
    /// Never opened (or so old that its record was evicted): any frame but PRIORITY is a connection error.
    Idle,
    /// Request headers/body still arriving.
    Active,
    /// Request complete; response pending or being sent.
    HalfClosedRemote,
    Closed(CloseKind),
}

/// Why a stream is closed; decides how late frames are answered.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum CloseKind {
    /// Both directions finished normally.
    Normal,
    /// We sent RST_STREAM: in-flight frames from the peer are ignored.
    ResetByUs,
    /// The peer sent RST_STREAM.
    ResetByPeer,
}

/// Closed streams remembered for late-frame handling (oldest evicted first).
const CLOSED_CAP: usize = 4096;

enum SendBody {
    Mem {
        data: Vec<u8>,
        pos: usize,
    },
    File {
        file: Rc<OpenFile>,
        off: u64,
        remaining: u64,
        inflight: bool,
    },
}

struct SendStream {
    id: u32,
    /// Remaining stream credit *after* subtracting `reserved`.
    window: i64,
    body: SendBody,
    /// Credit (stream and connection) set aside for a file read in flight.
    /// It is deducted when the read is requested, so concurrent reads and
    /// in-memory bodies can never together exceed what the peer granted.
    reserved: i64,
}

pub struct H2Conn {
    got_preface: bool,
    dec: hpack::Decoder<'static>,
    peer_max_frame: usize,
    /// Description of the frame being processed (diagnostics only).
    cur: String,
    peer_init_window: i64,
    conn_window: i64,
    recv_unacked: u32,
    last_stream: u32,
    cont: Option<Cont>,
    open: HashMap<u32, OpenReq>,
    /// Streams whose request is complete and whose response is not yet fully sent.
    hc_remote: HashSet<u32>,
    closed: BTreeMap<u32, CloseKind>,
    ready: Option<H2Request>,
    sends: Vec<SendStream>,
    max_body: usize,
    rst_seen: u32,
    draining: bool,
}

// ───────────────────────── frame writers ─────────────────────────

fn put_frame_header(out: &mut Vec<u8>, len: usize, ty: u8, flags: u8, stream: u32) {
    out.extend_from_slice(&[(len >> 16) as u8, (len >> 8) as u8, len as u8, ty, flags]);
    out.extend_from_slice(&(stream & 0x7fff_ffff).to_be_bytes());
}

fn put_rst(out: &mut Vec<u8>, stream: u32, code: u32) {
    put_frame_header(out, 4, RST_STREAM, 0, stream);
    out.extend_from_slice(&code.to_be_bytes());
}

fn put_window_update(out: &mut Vec<u8>, stream: u32, inc: u32) {
    put_frame_header(out, 4, WINDOW_UPDATE, 0, stream);
    out.extend_from_slice(&inc.to_be_bytes());
}

fn put_goaway(out: &mut Vec<u8>, last: u32, code: u32) {
    put_frame_header(out, 8, GOAWAY, 0, 0);
    out.extend_from_slice(&last.to_be_bytes());
    out.extend_from_slice(&code.to_be_bytes());
}

fn push_setting(buf: &mut Vec<u8>, id: u16, val: u32) {
    buf.extend_from_slice(&id.to_be_bytes());
    buf.extend_from_slice(&val.to_be_bytes());
}

// ───────────────────────── HPACK (encoder side) ─────────────────────────

/// HPACK integer with an N-bit prefix (RFC 7541 5.1). `flags` are the high bits.
fn put_int(out: &mut Vec<u8>, prefix_bits: u8, flags: u8, mut v: usize) {
    let max = (1usize << prefix_bits) - 1;
    if v < max {
        out.push(flags | v as u8);
        return;
    }
    out.push(flags | max as u8);
    v -= max;
    while v >= 128 {
        out.push((v % 128) as u8 | 0x80);
        v /= 128;
    }
    out.push(v as u8);
}

/// String literal without Huffman coding.
fn put_str(out: &mut Vec<u8>, s: &[u8]) {
    put_int(out, 7, 0, s.len());
    out.extend_from_slice(s);
}

/// "Literal header field without indexing - new name".
fn put_literal(out: &mut Vec<u8>, name: &[u8], value: &[u8]) {
    out.push(0x00);
    put_str(out, name);
    put_str(out, value);
}

fn encode_status(out: &mut Vec<u8>, status: u16) {
    // Static table entries 8..=14 are :status 200,204,206,304,400,404,500.
    match status {
        200 => out.push(0x88),
        204 => out.push(0x89),
        206 => out.push(0x8a),
        304 => out.push(0x8b),
        400 => out.push(0x8c),
        404 => out.push(0x8d),
        500 => out.push(0x8e),
        _ => {
            out.push(0x08); // literal without indexing, name = static index 8 (:status)
            put_str(out, status.to_string().as_bytes());
        }
    }
}

/// HEADERS + CONTINUATION frames for one header block.
fn write_headers(out: &mut Vec<u8>, sid: u32, block: &[u8], end_stream: bool, max: usize) {
    let total = block.len();
    let mut pos = 0;
    let mut first = true;
    loop {
        let end = (pos + max).min(total);
        let last = end == total;
        let mut flags = if last { END_HEADERS } else { 0 };
        let ty = if first {
            if end_stream {
                flags |= END_STREAM;
            }
            HEADERS
        } else {
            CONTINUATION
        };
        put_frame_header(out, end - pos, ty, flags, sid);
        out.extend_from_slice(&block[pos..end]);
        pos = end;
        first = false;
        if last {
            break;
        }
    }
}

fn strip_padding(flags: u8, p: &[u8]) -> Result<&[u8], u32> {
    if flags & PADDED == 0 {
        return Ok(p);
    }
    let (&pad, rest) = p.split_first().ok_or(PROTOCOL_ERROR)?;
    let pad = pad as usize;
    if pad > rest.len() {
        return Err(PROTOCOL_ERROR);
    }
    Ok(&rest[..rest.len() - pad])
}

/// Validate a decoded header list and turn it into a request (None = malformed).
fn build_open_req(headers: Vec<(Vec<u8>, Vec<u8>)>) -> Result<OpenReq, &'static str> {
    let mut method = None;
    let mut path = None;
    let mut scheme = false;
    let mut authority: Option<Vec<u8>> = None;
    let mut content_length: Option<u64> = None;
    let mut regular: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    let mut cookies: Vec<Vec<u8>> = Vec::new();
    let mut seen_regular = false;

    for (n, v) in headers {
        if v.iter().any(|&b| b == b'\r' || b == b'\n' || b == 0) {
            return Err("header value contains CR, LF or NUL");
        }
        if n.first() == Some(&b':') {
            if seen_regular {
                return Err("pseudo-header after a regular header");
            }
            match &n[..] {
                b":method" => {
                    if method.is_some() {
                        return Err("duplicate :method");
                    }
                    method = Some(String::from_utf8(v).map_err(|_| "non-UTF-8 :method")?);
                }
                b":path" => {
                    if path.is_some() {
                        return Err("duplicate :path");
                    }
                    path = Some(String::from_utf8(v).map_err(|_| "non-UTF-8 :path")?);
                }
                b":authority" => {
                    if authority.is_some() {
                        return Err("duplicate :authority");
                    }
                    authority = Some(v);
                }
                b":scheme" => {
                    if scheme {
                        return Err("duplicate :scheme");
                    }
                    if v.is_empty() {
                        return Err("empty :scheme");
                    }
                    scheme = true;
                }
                _ => return Err("unknown pseudo-header"),
            }
        } else {
            seen_regular = true;
            if n.is_empty() || n.iter().any(|b| b.is_ascii_uppercase() || *b <= 0x20) {
                return Err("header name is empty, has upper-case letters or control characters");
            }
            match &n[..] {
                b"connection" | b"keep-alive" | b"proxy-connection" | b"transfer-encoding"
                | b"upgrade" => return Err("connection-specific header field in a request"),
                b"te" => {
                    if v != b"trailers" {
                        return Err("te header other than \"trailers\"");
                    }
                }
                b"cookie" => cookies.push(v),
                b"content-length" => {
                    if v.is_empty() || !v.iter().all(u8::is_ascii_digit) {
                        return Err("invalid content-length");
                    }
                    let len: u64 = std::str::from_utf8(&v)
                        .ok()
                        .and_then(|t| t.parse().ok())
                        .ok_or("invalid content-length")?;
                    if content_length.is_some_and(|prev| prev != len) {
                        return Err("conflicting content-length values");
                    }
                    content_length = Some(len);
                    regular.push((n, v));
                }
                _ => regular.push((n, v)),
            }
        }
    }

    let method = method.ok_or("missing :method")?;
    let path = path.ok_or("missing :path")?;
    if !scheme {
        return Err("missing :scheme");
    }
    if method.is_empty() || path.is_empty() || path.bytes().any(|b| b <= 0x20 || b == 0x7f) {
        return Err("empty :method/:path or control character in :path");
    }
    if !cookies.is_empty() {
        regular.push((b"cookie".to_vec(), cookies.join(&b"; "[..])));
    }
    Ok(OpenReq {
        content_length,
        method,
        path,
        authority: authority.unwrap_or_default(),
        headers: regular,
        body: Vec::new(),
        unacked: 0,
    })
}

impl H2Conn {
    /// Creates the connection state and queues our SETTINGS frame.
    pub fn new(max_body: usize, out: &mut Vec<u8>) -> Self {
        let mut payload = Vec::with_capacity(12);
        push_setting(&mut payload, 3, MAX_CONCURRENT); // MAX_CONCURRENT_STREAMS
        push_setting(&mut payload, 6, MAX_HEADER_BLOCK as u32); // MAX_HEADER_LIST_SIZE
        put_frame_header(out, payload.len(), SETTINGS, 0, 0);
        out.extend_from_slice(&payload);

        Self {
            got_preface: false,
            dec: hpack::Decoder::new(),
            peer_max_frame: 16_384,
            cur: String::new(),
            peer_init_window: DEFAULT_WINDOW,
            conn_window: DEFAULT_WINDOW,
            recv_unacked: 0,
            last_stream: 0,
            cont: None,
            open: HashMap::new(),
            hc_remote: HashSet::new(),
            closed: BTreeMap::new(),
            ready: None,
            sends: Vec::new(),
            max_body,
            rst_seen: 0,
            draining: false,
        }
    }

    pub fn take_ready(&mut self) -> Option<H2Request> {
        self.ready.take()
    }

    /// Graceful shutdown: tell the peer no new streams will be accepted
    /// (GOAWAY with NO_ERROR) and refuse any that arrive anyway. Streams that
    /// are already open keep running.
    pub fn start_shutdown(&mut self, out: &mut Vec<u8>) {
        if !self.draining {
            self.draining = true;
            put_goaway(out, self.last_stream, NO_ERROR);
        }
    }

    /// No request is being received or answered on this connection.
    pub fn is_idle(&self) -> bool {
        self.open.is_empty() && self.sends.is_empty() && self.ready.is_none() && self.cont.is_none()
    }

    fn fail(&mut self, code: u32, out: &mut Vec<u8>, total: usize) -> Feed {
        if tracing() {
            eprintln!(
                "vajra h2: CONNECTION ERROR {} (GOAWAY, last stream {}) while handling {}",
                error_name(code),
                self.last_stream,
                self.cur
            );
        }
        put_goaway(out, self.last_stream, code);
        Feed {
            consumed: total,
            fatal: true,
        }
    }

    /// Consume as many complete frames from `input` as possible, stopping
    /// early once a request is ready. Responses to control frames are appended
    /// to `out`.
    pub fn feed(&mut self, input: &[u8], out: &mut Vec<u8>) -> Feed {
        let total = input.len();
        let mut rest = input;

        if !self.got_preface {
            if rest.len() < PREFACE.len() {
                if PREFACE.starts_with(rest) {
                    return Feed {
                        consumed: 0,
                        fatal: false,
                    };
                }
                return self.fail(PROTOCOL_ERROR, out, total);
            }
            if &rest[..PREFACE.len()] != PREFACE {
                return self.fail(PROTOCOL_ERROR, out, total);
            }
            rest = &rest[PREFACE.len()..];
            self.got_preface = true;
        }

        while self.ready.is_none() && rest.len() >= 9 {
            let len = (rest[0] as usize) << 16 | (rest[1] as usize) << 8 | rest[2] as usize;
            if len > MAX_FRAME {
                return self.fail(FRAME_SIZE_ERROR, out, total);
            }
            if rest.len() < 9 + len {
                break;
            }
            let ty = rest[3];
            let flags = rest[4];
            let sid = u32::from_be_bytes([rest[5] & 0x7f, rest[6], rest[7], rest[8]]);
            let payload = &rest[9..9 + len];
            if tracing() {
                self.cur = format!(
                    "{} flags=0x{:02x} stream={} len={}{}",
                    frame_name(ty),
                    flags,
                    sid,
                    len,
                    if ty == PRIORITY && len == 5 {
                        let dep = u32::from_be_bytes([
                            payload[0] & 0x7f,
                            payload[1],
                            payload[2],
                            payload[3],
                        ]);
                        format!(" depends_on={dep} weight={}", payload[4] as u32 + 1)
                    } else {
                        String::new()
                    }
                );
                eprintln!("vajra h2: <- {}", self.cur);
            }

            // While a header block is open, only its CONTINUATIONs may arrive.
            if let Some(c) = &self.cont {
                if ty != CONTINUATION || sid != c.stream {
                    return self.fail(PROTOCOL_ERROR, out, total);
                }
            }
            if let Err(code) = self.frame(ty, flags, sid, payload, out) {
                return self.fail(code, out, total);
            }
            rest = &rest[9 + len..];
        }
        Feed {
            consumed: total - rest.len(),
            fatal: false,
        }
    }

    // ───────────────────────── stream state machine (RFC 9113 §5.1) ─────────────────────────

    fn state_of(&self, sid: u32) -> StreamState {
        if sid == 0 || sid % 2 == 0 || sid > self.last_stream {
            return StreamState::Idle;
        }
        if self.open.contains_key(&sid) {
            StreamState::Active
        } else if self.hc_remote.contains(&sid) {
            StreamState::HalfClosedRemote
        } else if let Some(k) = self.closed.get(&sid) {
            StreamState::Closed(*k)
        } else {
            StreamState::Idle // implicitly closed by a higher id, or evicted from `closed`
        }
    }

    /// Move a stream to Closed(kind), discarding everything we hold for it.
    /// A stream that already has a close record keeps it.
    fn close_stream(&mut self, sid: u32, kind: CloseKind) {
        self.open.remove(&sid);
        self.hc_remote.remove(&sid);
        self.drop_send(sid);
        if self.ready.as_ref().is_some_and(|r| r.stream == sid) {
            self.ready = None;
        }
        if sid % 2 == 1 && sid <= self.last_stream {
            self.closed.entry(sid).or_insert(kind);
            while self.closed.len() > CLOSED_CAP {
                self.closed.pop_first();
            }
        }
    }

    /// Stream error: RST_STREAM(code), and the stream is closed by us.
    fn reset_stream(&mut self, sid: u32, code: u32, why: &str, out: &mut Vec<u8>) {
        if tracing() {
            eprintln!(
                "vajra h2: STREAM ERROR on stream {sid}: RST_STREAM {} ({why})",
                error_name(code)
            );
        }
        put_rst(out, sid, code);
        self.close_stream(sid, CloseKind::ResetByUs);
    }

    /// The response for `sid` has been fully queued: the stream is closed.
    fn response_done(&mut self, sid: u32) {
        self.hc_remote.remove(&sid);
        self.closed.entry(sid).or_insert(CloseKind::Normal);
        while self.closed.len() > CLOSED_CAP {
            self.closed.pop_first();
        }
    }

    /// The request on `sid` is complete (END_STREAM seen): validate and surface it.
    fn request_done(&mut self, sid: u32, r: OpenReq, out: &mut Vec<u8>) {
        if let Some(n) = r.content_length {
            if n != r.body.len() as u64 {
                self.reset_stream(
                    sid,
                    PROTOCOL_ERROR,
                    "content-length does not match the DATA received",
                    out,
                );
                return;
            }
        }
        self.hc_remote.insert(sid);
        self.ready = Some(r.into_request(sid));
    }

    // ───────────────────────── frames ─────────────────────────

    fn frame(
        &mut self,
        ty: u8,
        flags: u8,
        sid: u32,
        p: &[u8],
        out: &mut Vec<u8>,
    ) -> Result<(), u32> {
        match ty {
            DATA => self.on_data(flags, sid, p, out),
            HEADERS => self.on_headers(flags, sid, p, out),
            PRIORITY => {
                if sid == 0 {
                    return Err(PROTOCOL_ERROR);
                }
                if p.len() != 5 {
                    // §6.3: a stream error, not a connection error.
                    self.reset_stream(sid, FRAME_SIZE_ERROR, "PRIORITY frame length is not 5", out);
                    return Ok(());
                }
                let dep = u32::from_be_bytes([p[0] & 0x7f, p[1], p[2], p[3]]);
                if dep == sid {
                    self.reset_stream(sid, PROTOCOL_ERROR, "stream depends on itself", out);
                }
                Ok(())
            }
            RST_STREAM => {
                if sid == 0 {
                    return Err(PROTOCOL_ERROR);
                }
                if p.len() != 4 {
                    return Err(FRAME_SIZE_ERROR);
                }
                match self.state_of(sid) {
                    StreamState::Idle => return Err(PROTOCOL_ERROR),
                    StreamState::Active | StreamState::HalfClosedRemote => {
                        self.close_stream(sid, CloseKind::ResetByPeer);
                    }
                    StreamState::Closed(_) => {}
                }
                self.rst_seen += 1;
                if self.rst_seen > MAX_RST {
                    return Err(ENHANCE_YOUR_CALM); // rapid-reset style abuse
                }
                Ok(())
            }
            SETTINGS => self.on_settings(flags, sid, p, out),
            PUSH_PROMISE => Err(PROTOCOL_ERROR), // clients must not send it
            PING => {
                if sid != 0 {
                    return Err(PROTOCOL_ERROR);
                }
                if p.len() != 8 {
                    return Err(FRAME_SIZE_ERROR);
                }
                if flags & ACK == 0 {
                    put_frame_header(out, 8, PING, ACK, 0);
                    out.extend_from_slice(p);
                }
                Ok(())
            }
            GOAWAY => {
                if sid != 0 {
                    Err(PROTOCOL_ERROR)
                } else {
                    Ok(())
                }
            }
            WINDOW_UPDATE => self.on_window_update(sid, p, out),
            CONTINUATION => self.on_continuation(flags, sid, p, out),
            _ => Ok(()), // unknown frame types must be ignored
        }
    }

    fn on_settings(
        &mut self,
        flags: u8,
        sid: u32,
        p: &[u8],
        _out: &mut Vec<u8>,
    ) -> Result<(), u32> {
        if sid != 0 {
            return Err(PROTOCOL_ERROR);
        }
        if flags & ACK != 0 {
            return if p.is_empty() {
                Ok(())
            } else {
                Err(FRAME_SIZE_ERROR)
            };
        }
        if p.len() % 6 != 0 {
            return Err(FRAME_SIZE_ERROR);
        }
        let mut new_window = None;
        let mut new_frame = None;
        for e in p.chunks_exact(6) {
            let id = u16::from_be_bytes([e[0], e[1]]);
            let val = u32::from_be_bytes([e[2], e[3], e[4], e[5]]);
            match id {
                2 => {
                    if val > 1 {
                        return Err(PROTOCOL_ERROR);
                    }
                }
                4 => {
                    if val as i64 > MAX_WINDOW {
                        return Err(FLOW_CONTROL_ERROR);
                    }
                    new_window = Some(val as i64); // several in one frame: the last one wins
                }
                5 => {
                    if !(16_384..=16_777_215).contains(&val) {
                        return Err(PROTOCOL_ERROR);
                    }
                    new_frame = Some(val as usize);
                }
                _ => {} // header table size (our encoder is stateless), concurrency, unknown
            }
        }
        // Settings in a frame take effect together, in order: apply the net result once.
        if let Some(w) = new_window {
            let delta = w - self.peer_init_window;
            self.peer_init_window = w;
            for s in self.sends.iter_mut() {
                s.window += delta;
                if s.window + s.reserved > MAX_WINDOW {
                    return Err(FLOW_CONTROL_ERROR);
                }
            }
        }
        if let Some(f) = new_frame {
            self.peer_max_frame = f;
        }
        put_frame_header(_out, 0, SETTINGS, ACK, 0);
        Ok(())
    }

    fn on_window_update(&mut self, sid: u32, p: &[u8], out: &mut Vec<u8>) -> Result<(), u32> {
        if p.len() != 4 {
            return Err(FRAME_SIZE_ERROR);
        }
        let inc = (u32::from_be_bytes([p[0], p[1], p[2], p[3]]) & 0x7fff_ffff) as i64;
        if sid == 0 {
            if inc == 0 {
                return Err(PROTOCOL_ERROR);
            }
            self.conn_window += inc;
            if self.conn_window > MAX_WINDOW {
                return Err(FLOW_CONTROL_ERROR);
            }
            return Ok(());
        }
        match self.state_of(sid) {
            StreamState::Idle => Err(PROTOCOL_ERROR),
            StreamState::Closed(_) => Ok(()), // late WINDOW_UPDATE: ignored (§5.1)
            StreamState::Active | StreamState::HalfClosedRemote => {
                if inc == 0 {
                    self.reset_stream(
                        sid,
                        PROTOCOL_ERROR,
                        "WINDOW_UPDATE with a zero increment",
                        out,
                    );
                } else if let Some(i) = self.sends.iter().position(|s| s.id == sid) {
                    self.sends[i].window += inc;
                    if self.sends[i].window + self.sends[i].reserved > MAX_WINDOW {
                        self.reset_stream(sid, FLOW_CONTROL_ERROR, "stream window overflow", out);
                    }
                }
                Ok(())
            }
        }
    }

    fn on_headers(&mut self, flags: u8, sid: u32, p: &[u8], out: &mut Vec<u8>) -> Result<(), u32> {
        if sid == 0 || sid % 2 == 0 {
            return Err(PROTOCOL_ERROR);
        }
        let mut p = strip_padding(flags, p)?;
        let mut self_dep = false;
        if flags & PRIORITY_FLAG != 0 {
            if p.len() < 5 {
                return Err(PROTOCOL_ERROR);
            }
            self_dep = u32::from_be_bytes([p[0] & 0x7f, p[1], p[2], p[3]]) == sid;
            p = &p[5..];
        }

        let kind = match self.state_of(sid) {
            StreamState::Active => {
                if self_dep {
                    HKind::Reject(PROTOCOL_ERROR)
                } else {
                    HKind::Trailer
                }
            }
            StreamState::HalfClosedRemote => HKind::Reject(STREAM_CLOSED),
            StreamState::Closed(CloseKind::Normal) => return Err(STREAM_CLOSED),
            StreamState::Closed(CloseKind::ResetByPeer) => HKind::Reject(STREAM_CLOSED),
            StreamState::Closed(CloseKind::ResetByUs) => HKind::Ignore,
            StreamState::Idle => {
                if sid <= self.last_stream {
                    return Err(PROTOCOL_ERROR); // ids must increase
                }
                self.last_stream = sid;
                if self_dep {
                    HKind::Reject(PROTOCOL_ERROR)
                } else if self.draining
                    || self.open.len() + self.sends.len() >= MAX_CONCURRENT as usize
                {
                    HKind::Refuse
                } else {
                    HKind::New
                }
            }
        };

        let end_stream = flags & END_STREAM != 0;
        if flags & END_HEADERS != 0 {
            self.header_block(sid, end_stream, kind, p, out)
        } else {
            if p.len() > MAX_HEADER_BLOCK {
                return Err(ENHANCE_YOUR_CALM);
            }
            self.cont = Some(Cont {
                stream: sid,
                end_stream,
                kind,
                block: p.to_vec(),
            });
            Ok(())
        }
    }

    fn on_continuation(
        &mut self,
        flags: u8,
        sid: u32,
        p: &[u8],
        out: &mut Vec<u8>,
    ) -> Result<(), u32> {
        let Some(c) = self.cont.as_mut() else {
            // No header block is open.
            return match self.state_of(sid) {
                StreamState::Closed(CloseKind::Normal) => Err(STREAM_CLOSED),
                _ => Err(PROTOCOL_ERROR),
            };
        };
        if sid != c.stream {
            return Err(PROTOCOL_ERROR);
        }
        if c.block.len() + p.len() > MAX_HEADER_BLOCK {
            return Err(ENHANCE_YOUR_CALM);
        }
        c.block.extend_from_slice(p);
        if flags & END_HEADERS != 0 {
            let c = self.cont.take().expect("checked above");
            self.header_block(c.stream, c.end_stream, c.kind, &c.block, out)
        } else {
            Ok(())
        }
    }

    /// A complete header block. It is *always* decoded, even when the stream
    /// is refused or rejected, so the HPACK dynamic table stays in sync with the peer.
    fn header_block(
        &mut self,
        sid: u32,
        end_stream: bool,
        kind: HKind,
        block: &[u8],
        out: &mut Vec<u8>,
    ) -> Result<(), u32> {
        let headers = self.dec.decode(block).map_err(|_| COMPRESSION_ERROR)?;
        let size: usize = headers.iter().map(|(n, v)| n.len() + v.len() + 32).sum();

        match kind {
            HKind::Ignore => return Ok(()),
            HKind::Reject(code) => {
                self.reset_stream(sid, code, "HEADERS not allowed in this stream state", out);
                return Ok(());
            }
            HKind::Refuse => {
                self.reset_stream(
                    sid,
                    REFUSED_STREAM,
                    "too many streams or shutting down",
                    out,
                );
                return Ok(());
            }
            _ => {}
        }
        if size > MAX_HEADER_BLOCK {
            self.reset_stream(sid, ENHANCE_YOUR_CALM, "header list too large", out);
            return Ok(());
        }
        if kind == HKind::Trailer {
            if !end_stream {
                self.reset_stream(sid, PROTOCOL_ERROR, "trailers without END_STREAM", out);
                return Ok(());
            }
            if headers.iter().any(|(n, _)| n.first() == Some(&b':')) {
                self.reset_stream(sid, PROTOCOL_ERROR, "pseudo-header in trailers", out);
                return Ok(());
            }
            if let Some(r) = self.open.remove(&sid) {
                self.request_done(sid, r, out);
            }
            return Ok(());
        }
        match build_open_req(headers) {
            Err(why) => self.reset_stream(sid, PROTOCOL_ERROR, why, out),
            Ok(r) => {
                if end_stream {
                    self.request_done(sid, r, out);
                } else {
                    self.open.insert(sid, r);
                }
            }
        }
        Ok(())
    }

    fn on_data(&mut self, flags: u8, sid: u32, p: &[u8], out: &mut Vec<u8>) -> Result<(), u32> {
        if sid == 0 {
            return Err(PROTOCOL_ERROR);
        }
        let flow_len = p.len(); // padding counts against flow control
        let data = strip_padding(flags, p)?;
        let end_stream = flags & END_STREAM != 0;

        // Connection-level credit, batched (DATA on any stream consumes it).
        self.recv_unacked += flow_len as u32;
        if self.recv_unacked >= WINDOW_BATCH {
            put_window_update(out, 0, self.recv_unacked);
            self.recv_unacked = 0;
        }

        match self.state_of(sid) {
            StreamState::Idle => return Err(PROTOCOL_ERROR),
            StreamState::HalfClosedRemote => {
                self.reset_stream(
                    sid,
                    STREAM_CLOSED,
                    "DATA on a half-closed (remote) stream",
                    out,
                );
                return Ok(());
            }
            StreamState::Closed(CloseKind::Normal) => return Err(STREAM_CLOSED),
            StreamState::Closed(CloseKind::ResetByPeer) => {
                put_rst(out, sid, STREAM_CLOSED);
                return Ok(());
            }
            StreamState::Closed(CloseKind::ResetByUs) => return Ok(()), // in flight when we reset
            StreamState::Active => {}
        }

        let Some(r) = self.open.get_mut(&sid) else {
            return Ok(());
        };
        if r.body.len() + data.len() > self.max_body {
            self.reset_stream(
                sid,
                CANCEL,
                "request body exceeds the configured maximum",
                out,
            );
            return Ok(());
        }
        r.body.extend_from_slice(data);
        if r.content_length.is_some_and(|n| r.body.len() as u64 > n) {
            self.reset_stream(sid, PROTOCOL_ERROR, "more DATA than content-length", out);
            return Ok(());
        }
        r.unacked += flow_len as u32;
        if r.unacked >= WINDOW_BATCH && !end_stream {
            put_window_update(out, sid, r.unacked);
            r.unacked = 0;
        }
        if end_stream {
            if let Some(r) = self.open.remove(&sid) {
                self.request_done(sid, r, out);
            }
        }
        Ok(())
    }

    /// Forget a response stream, returning any connection credit reserved for
    /// a file read that will now never be sent.
    fn drop_send(&mut self, sid: u32) {
        if let Some(i) = self.sends.iter().position(|s| s.id == sid) {
            let s = self.sends.remove(i);
            self.conn_window += s.reserved;
        }
    }

    // ───────────────────────── responses ─────────────────────────

    /// Queue a response: HEADERS now, body scheduled under flow control.
    pub fn respond(&mut self, stream: u32, resp: H2Response, out: &mut Vec<u8>) {
        // The peer reset (or we reset) this stream while the request was being served: nothing to send.
        if !self.hc_remote.contains(&stream) {
            if tracing() {
                eprintln!("vajra h2: response for stream {stream} dropped (stream no longer open)");
            }
            return;
        }
        let mut block = Vec::with_capacity(128);
        encode_status(&mut block, resp.status);
        for (n, v) in &resp.headers {
            put_literal(&mut block, n, v);
        }
        let end_now = matches!(resp.body, H2Body::Empty);
        if tracing() {
            eprintln!(
                "vajra h2: -> HEADERS stream={} status={} fields={} block={}B body={}",
                stream,
                resp.status,
                resp.headers.len(),
                block.len(),
                match &resp.body {
                    H2Body::Empty => "none".to_string(),
                    H2Body::Mem(d) => format!("{}B", d.len()),
                    H2Body::File(f) => format!("{}B (file)", f.size),
                }
            );
        }
        write_headers(
            out,
            stream,
            &block,
            end_now,
            self.peer_max_frame.min(SEND_FRAME),
        );

        match resp.body {
            H2Body::Empty => self.response_done(stream),
            H2Body::Mem(data) => {
                if data.is_empty() {
                    put_frame_header(out, 0, DATA, END_STREAM, stream);
                    self.response_done(stream);
                } else {
                    self.sends.push(SendStream {
                        id: stream,
                        window: self.peer_init_window,
                        body: SendBody::Mem { data, pos: 0 },
                        reserved: 0,
                    });
                }
            }
            H2Body::File(file) => {
                if file.size == 0 {
                    put_frame_header(out, 0, DATA, END_STREAM, stream);
                    self.response_done(stream);
                } else {
                    let remaining = file.size;
                    self.sends.push(SendStream {
                        id: stream,
                        window: self.peer_init_window,
                        body: SendBody::File {
                            file,
                            off: 0,
                            remaining,
                            inflight: false,
                        },
                        reserved: 0,
                    });
                }
            }
        }
    }

    /// Emit as much in-memory body data as the windows allow, then report the
    /// next file read needed (if any). Call after anything that may have
    /// opened a window or queued a response.
    pub fn poll_output(&mut self, out: &mut Vec<u8>) -> Option<FileRead> {
        let fs = self.peer_max_frame.min(SEND_FRAME);

        let mut progress = true;
        while progress && self.conn_window > 0 {
            progress = false;
            for s in self.sends.iter_mut() {
                if let SendBody::Mem { data, pos } = &mut s.body {
                    if s.window <= 0 || self.conn_window <= 0 {
                        continue;
                    }
                    let remaining = data.len() - *pos;
                    let n = remaining
                        .min(s.window as usize)
                        .min(self.conn_window as usize)
                        .min(fs);
                    if n == 0 {
                        continue;
                    }
                    let last = n == remaining;
                    put_frame_header(out, n, DATA, if last { END_STREAM } else { 0 }, s.id);
                    out.extend_from_slice(&data[*pos..*pos + n]);
                    *pos += n;
                    s.window -= n as i64;
                    self.conn_window -= n as i64;
                    progress = true;
                }
            }
            let mut done = Vec::new();
            self.sends.retain(|s| {
                let keep = match &s.body {
                    SendBody::Mem { data, pos } => *pos < data.len(),
                    SendBody::File { .. } => true,
                };
                if !keep {
                    done.push(s.id);
                }
                keep
            });
            for id in done {
                self.response_done(id);
            }
        }

        if self.conn_window > 0 {
            let mut pick: Option<(usize, FileRead)> = None;
            for (i, s) in self.sends.iter_mut().enumerate() {
                if let SendBody::File {
                    file,
                    off,
                    remaining,
                    inflight,
                } = &mut s.body
                {
                    if *inflight || s.window <= 0 {
                        continue;
                    }
                    let len = (*remaining)
                        .min(s.window as u64)
                        .min(self.conn_window as u64)
                        .min(FILE_CHUNK as u64) as u32;
                    if len == 0 {
                        continue;
                    }
                    *inflight = true;
                    // Reserve the credit now: other streams must not spend it while the read is pending.
                    s.window -= len as i64;
                    s.reserved = len as i64;
                    self.conn_window -= len as i64;
                    pick = Some((
                        i,
                        FileRead {
                            stream: s.id,
                            fd: file.fd(),
                            off: *off,
                            len,
                        },
                    ));
                    break;
                }
            }
            if let Some((i, fr)) = pick {
                // Round-robin: the stream we just served goes to the back.
                self.sends.rotate_left(i + 1);
                return Some(fr);
            }
        }
        None
    }

    /// Deliver the result of a [`FileRead`]. An empty `data` means the read
    /// failed or hit EOF early: the stream is reset.
    pub fn file_data(&mut self, stream: u32, data: &[u8], out: &mut Vec<u8>) {
        // A stream reset while its read was pending was already dropped (and its reservation refunded).
        let Some(i) = self.sends.iter().position(|s| s.id == stream) else {
            return;
        };
        let fs = self.peer_max_frame.min(SEND_FRAME);

        let finished = {
            let SendStream {
                window,
                body,
                reserved,
                ..
            } = &mut self.sends[i];
            let SendBody::File {
                off,
                remaining,
                inflight,
                ..
            } = body
            else {
                return;
            };
            *inflight = false;
            let held = std::mem::take(reserved);

            if data.is_empty() {
                put_rst(out, stream, INTERNAL_ERROR);
                self.conn_window += held;
                true
            } else {
                // Never send more than was reserved, nor more than the stream credit that is
                // still valid (SETTINGS_INITIAL_WINDOW_SIZE may have shrunk it meanwhile).
                // Unsent bytes are simply read again later: reads are positional.
                let credit = (*window + held).max(0) as usize;
                let n = data
                    .len()
                    .min(*remaining as usize)
                    .min(held as usize)
                    .min(credit);
                let whole = n as u64 == *remaining;
                let mut sent = 0;
                while sent < n {
                    let m = (n - sent).min(fs);
                    let last = whole && sent + m == n;
                    put_frame_header(out, m, DATA, if last { END_STREAM } else { 0 }, stream);
                    out.extend_from_slice(&data[sent..sent + m]);
                    sent += m;
                }
                *off += n as u64;
                *remaining -= n as u64;
                let refund = held - n as i64;
                *window += refund;
                self.conn_window += refund;
                *remaining == 0
            }
        };
        if finished {
            self.sends.remove(i);
            self.response_done(stream);
        }
    }

    /// Number of response streams still being sent.
    pub fn active_sends(&self) -> usize {
        self.sends.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(ty: u8, flags: u8, sid: u32, payload: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        put_frame_header(&mut v, payload.len(), ty, flags, sid);
        v.extend_from_slice(payload);
        v
    }

    /// Split a byte stream into (type, flags, stream, payload).
    fn frames(mut b: &[u8]) -> Vec<(u8, u8, u32, Vec<u8>)> {
        let mut v = Vec::new();
        while b.len() >= 9 {
            let len = (b[0] as usize) << 16 | (b[1] as usize) << 8 | b[2] as usize;
            let sid = u32::from_be_bytes([b[5] & 0x7f, b[6], b[7], b[8]]);
            v.push((b[3], b[4], sid, b[9..9 + len].to_vec()));
            b = &b[9 + len..];
        }
        assert!(b.is_empty(), "trailing partial frame");
        v
    }

    fn encode_req(headers: &[(&str, &str)]) -> Vec<u8> {
        let mut enc = hpack::Encoder::new();
        enc.encode(headers.iter().map(|(n, v)| (n.as_bytes(), v.as_bytes())))
    }

    fn get_headers(path: &str) -> Vec<u8> {
        encode_req(&[
            (":method", "GET"),
            (":scheme", "https"),
            (":path", path),
            (":authority", "localhost"),
        ])
    }

    fn started() -> (H2Conn, Vec<u8>) {
        let mut out = Vec::new();
        let mut h = H2Conn::new(1 << 20, &mut out);
        let mut input = PREFACE.to_vec();
        input.extend(frame(SETTINGS, 0, 0, &[]));
        let f = h.feed(&input, &mut out);
        assert!(!f.fatal);
        assert_eq!(f.consumed, input.len());
        (h, out)
    }

    /// Deliver a complete GET on `sid` and take it, as the worker would.
    fn open_stream(h: &mut H2Conn, sid: u32) {
        let mut sink = Vec::new();
        let f = h.feed(
            &frame(HEADERS, END_HEADERS | END_STREAM, sid, &get_headers("/")),
            &mut sink,
        );
        assert!(!f.fatal);
        assert_eq!(h.take_ready().map(|r| r.stream), Some(sid));
    }

    #[test]
    fn handshake_emits_settings_and_ack() {
        let (_h, out) = started();
        let fs = frames(&out);
        assert_eq!(fs[0].0, SETTINGS);
        assert_eq!(fs[0].1, 0);
        assert!(fs.iter().any(|f| f.0 == SETTINGS && f.1 == ACK));
    }

    #[test]
    fn bad_preface_is_goaway() {
        let mut out = Vec::new();
        let mut h = H2Conn::new(1024, &mut out);
        out.clear();
        let f = h.feed(b"GET / HTTP/1.1\r\n\r\nGET / HTTP/1.1\r\n\r\n", &mut out);
        assert!(f.fatal);
        let fs = frames(&out);
        assert_eq!(fs[0].0, GOAWAY);
    }

    #[test]
    fn partial_preface_waits() {
        let mut out = Vec::new();
        let mut h = H2Conn::new(1024, &mut out);
        let f = h.feed(&PREFACE[..10], &mut out);
        assert!(!f.fatal);
        assert_eq!(f.consumed, 0);
    }

    #[test]
    fn simple_get_becomes_ready() {
        let (mut h, mut out) = started();
        out.clear();
        let block = get_headers("/hello?x=1");
        let input = frame(HEADERS, END_HEADERS | END_STREAM, 1, &block);
        let f = h.feed(&input, &mut out);
        assert_eq!(f.consumed, input.len());
        let r = h.take_ready().expect("request");
        assert_eq!(
            (r.stream, r.method.as_str(), r.path.as_str()),
            (1, "GET", "/hello?x=1")
        );
        assert_eq!(r.authority, b"localhost");
        assert!(r.body.is_empty());
    }

    #[test]
    fn feed_stops_after_one_ready_request() {
        let (mut h, mut out) = started();
        let b = get_headers("/a");
        let mut input = frame(HEADERS, END_HEADERS | END_STREAM, 1, &b);
        let first_len = input.len();
        input.extend(frame(
            HEADERS,
            END_HEADERS | END_STREAM,
            3,
            &get_headers("/b"),
        ));
        let f = h.feed(&input, &mut out);
        assert_eq!(f.consumed, first_len);
        assert_eq!(h.take_ready().unwrap().path, "/a");
        let f2 = h.feed(&input[first_len..], &mut out);
        assert_eq!(f2.consumed, input.len() - first_len);
        assert_eq!(h.take_ready().unwrap().path, "/b");
    }

    #[test]
    fn request_body_is_buffered_until_end_stream() {
        let (mut h, mut out) = started();
        let block = encode_req(&[
            (":method", "POST"),
            (":scheme", "https"),
            (":path", "/p"),
            (":authority", "x"),
            ("content-length", "5"),
        ]);
        h.feed(&frame(HEADERS, END_HEADERS, 1, &block), &mut out);
        assert!(h.take_ready().is_none());
        h.feed(&frame(DATA, 0, 1, b"he"), &mut out);
        assert!(h.take_ready().is_none());
        h.feed(&frame(DATA, END_STREAM, 1, b"llo"), &mut out);
        let r = h.take_ready().unwrap();
        assert_eq!(r.method, "POST");
        assert_eq!(r.body, b"hello");
    }

    #[test]
    fn continuation_frames_are_joined() {
        let (mut h, mut out) = started();
        let block = get_headers("/split");
        let (a, b) = block.split_at(block.len() / 2);
        let mut input = frame(HEADERS, END_STREAM, 1, a);
        input.extend(frame(CONTINUATION, END_HEADERS, 1, b));
        h.feed(&input, &mut out);
        assert_eq!(h.take_ready().unwrap().path, "/split");
    }

    #[test]
    fn interleaving_during_header_block_is_an_error() {
        let (mut h, mut out) = started();
        let block = get_headers("/x");
        let mut input = frame(HEADERS, 0, 1, &block[..2]);
        input.extend(frame(PING, 0, 0, &[0; 8]));
        let f = h.feed(&input, &mut out);
        assert!(f.fatal);
    }

    #[test]
    fn cookies_are_merged_and_uppercase_names_rejected() {
        let (mut h, mut out) = started();
        let block = encode_req(&[
            (":method", "GET"),
            (":scheme", "https"),
            (":path", "/"),
            ("cookie", "a=1"),
            ("cookie", "b=2"),
        ]);
        h.feed(
            &frame(HEADERS, END_HEADERS | END_STREAM, 1, &block),
            &mut out,
        );
        let r = h.take_ready().unwrap();
        let c = r.headers.iter().find(|(n, _)| n == b"cookie").unwrap();
        assert_eq!(c.1, b"a=1; b=2");

        out.clear();
        let bad = encode_req(&[
            (":method", "GET"),
            (":scheme", "https"),
            (":path", "/"),
            ("X-Upper", "v"),
        ]);
        let f = h.feed(&frame(HEADERS, END_HEADERS | END_STREAM, 3, &bad), &mut out);
        assert!(!f.fatal);
        assert!(h.take_ready().is_none());
        let fs = frames(&out);
        assert_eq!((fs[0].0, fs[0].2), (RST_STREAM, 3));
    }

    #[test]
    fn ping_is_acked_and_stream_ids_must_increase() {
        let (mut h, mut out) = started();
        out.clear();
        h.feed(&frame(PING, 0, 0, b"12345678"), &mut out);
        let fs = frames(&out);
        assert_eq!((fs[0].0, fs[0].1), (PING, ACK));
        assert_eq!(fs[0].3, b"12345678");

        out.clear();
        h.feed(
            &frame(HEADERS, END_HEADERS | END_STREAM, 5, &get_headers("/")),
            &mut out,
        );
        h.take_ready();
        let f = h.feed(
            &frame(HEADERS, END_HEADERS | END_STREAM, 3, &get_headers("/")),
            &mut out,
        );
        assert!(f.fatal);
    }

    fn mem_response(len: usize) -> H2Response {
        H2Response {
            status: 200,
            headers: vec![(b"content-type".to_vec(), b"x/y".to_vec())],
            body: H2Body::Mem(vec![b'z'; len]),
        }
    }

    fn data_bytes(out: &[u8]) -> (usize, bool) {
        let mut n = 0;
        let mut ended = false;
        for (ty, flags, _, p) in frames(out) {
            if ty == DATA {
                n += p.len();
                ended |= flags & END_STREAM != 0;
            }
        }
        (n, ended)
    }

    #[test]
    fn response_headers_use_static_status_index() {
        let (mut h, mut out) = started();
        open_stream(&mut h, 1);
        open_stream(&mut h, 3);
        out.clear();
        h.respond(
            1,
            H2Response {
                status: 404,
                headers: vec![],
                body: H2Body::Empty,
            },
            &mut out,
        );
        let fs = frames(&out);
        assert_eq!(fs[0].0, HEADERS);
        assert_eq!(fs[0].1, END_HEADERS | END_STREAM);
        assert_eq!(fs[0].3, vec![0x8d]);

        out.clear();
        h.respond(
            3,
            H2Response {
                status: 502,
                headers: vec![],
                body: H2Body::Empty,
            },
            &mut out,
        );
        let fs = frames(&out);
        assert_eq!(fs[0].3, vec![0x08, 0x03, b'5', b'0', b'2']);
    }

    #[test]
    fn send_side_flow_control_blocks_then_resumes() {
        let (mut h, mut out) = started();
        open_stream(&mut h, 1);
        out.clear();
        h.respond(1, mem_response(100_000), &mut out);
        assert!(h.poll_output(&mut out).is_none());
        let (n, ended) = data_bytes(&out);
        assert_eq!(n, 65_535, "limited by the 65535-byte windows");
        assert!(!ended);

        out.clear();
        h.feed(
            &frame(WINDOW_UPDATE, 0, 0, &65_535u32.to_be_bytes()),
            &mut out,
        );
        h.feed(
            &frame(WINDOW_UPDATE, 0, 1, &65_535u32.to_be_bytes()),
            &mut out,
        );
        h.poll_output(&mut out);
        let (n, ended) = data_bytes(&out);
        assert_eq!(n, 100_000 - 65_535);
        assert!(ended);
        assert_eq!(h.active_sends(), 0);
    }

    #[test]
    fn initial_window_setting_adjusts_open_streams() {
        let (mut h, mut out) = started();
        open_stream(&mut h, 1);
        out.clear();
        h.respond(1, mem_response(200_000), &mut out);
        h.poll_output(&mut out);
        out.clear();
        // Raise the peer's per-stream window by 1 MiB and the connection window too.
        let mut s = Vec::new();
        push_setting(&mut s, 4, 1 << 20);
        h.feed(&frame(SETTINGS, 0, 0, &s), &mut out);
        h.feed(
            &frame(WINDOW_UPDATE, 0, 0, &(1u32 << 20).to_be_bytes()),
            &mut out,
        );
        h.poll_output(&mut out);
        let (n, ended) = data_bytes(&out);
        assert_eq!(n, 200_000 - 65_535);
        assert!(ended);
    }

    #[test]
    fn frames_are_capped_at_16k() {
        let (mut h, mut out) = started();
        out.clear();
        h.respond(1, mem_response(40_000), &mut out);
        h.poll_output(&mut out);
        for (ty, _, _, p) in frames(&out) {
            if ty == DATA {
                assert!(p.len() <= 16_384);
            }
        }
    }

    #[test]
    fn rst_stream_cancels_pending_body() {
        let (mut h, mut out) = started();
        h.respond(1, mem_response(100_000), &mut out);
        h.feed(&frame(RST_STREAM, 0, 1, &CANCEL.to_be_bytes()), &mut out);
        assert_eq!(h.active_sends(), 0);
    }

    #[test]
    fn data_triggers_window_updates() {
        let (mut h, mut out) = started();
        let block = encode_req(&[
            (":method", "POST"),
            (":scheme", "https"),
            (":path", "/p"),
            (":authority", "x"),
        ]);
        h.feed(&frame(HEADERS, END_HEADERS, 1, &block), &mut out);
        out.clear();
        h.feed(&frame(DATA, 0, 1, &vec![0u8; 16_000]), &mut out);
        h.feed(&frame(DATA, 0, 1, &vec![0u8; 16_000]), &mut out);
        let fs = frames(&out);
        assert!(fs.iter().any(|f| f.0 == WINDOW_UPDATE && f.2 == 0));
        assert!(fs.iter().any(|f| f.0 == WINDOW_UPDATE && f.2 == 1));
    }

    #[test]
    fn oversized_body_resets_stream() {
        let mut out = Vec::new();
        let mut h = H2Conn::new(10, &mut out);
        let mut input = PREFACE.to_vec();
        input.extend(frame(SETTINGS, 0, 0, &[]));
        h.feed(&input, &mut out);
        let block = encode_req(&[
            (":method", "POST"),
            (":scheme", "https"),
            (":path", "/p"),
            (":authority", "x"),
        ]);
        h.feed(&frame(HEADERS, END_HEADERS, 1, &block), &mut out);
        out.clear();
        h.feed(&frame(DATA, END_STREAM, 1, &[0u8; 64]), &mut out);
        assert!(h.take_ready().is_none());
        assert_eq!(frames(&out)[0].0, RST_STREAM);
    }

    #[test]
    fn graceful_shutdown_sends_goaway_and_refuses_new_streams() {
        let (mut h, mut out) = started();
        assert!(h.is_idle());
        out.clear();
        h.start_shutdown(&mut out);
        h.start_shutdown(&mut out); // idempotent
        let fs = frames(&out);
        assert_eq!(fs.len(), 1);
        assert_eq!(fs[0].0, GOAWAY);
        assert_eq!(&fs[0].3[4..8], &NO_ERROR.to_be_bytes());

        out.clear();
        let f = h.feed(
            &frame(HEADERS, END_HEADERS | END_STREAM, 1, &get_headers("/late")),
            &mut out,
        );
        assert!(!f.fatal, "refusing a stream is not a connection error");
        assert!(h.take_ready().is_none());
        let fs = frames(&out);
        assert_eq!((fs[0].0, fs[0].2), (RST_STREAM, 1));
        assert_eq!(&fs[0].3[..], &REFUSED_STREAM.to_be_bytes());
    }

    #[test]
    fn idle_tracking() {
        let (mut h, mut out) = started();
        assert!(h.is_idle());
        open_stream(&mut h, 1);
        h.respond(1, mem_response(100_000), &mut out);
        assert!(!h.is_idle(), "response still being sent");
        h.feed(&frame(RST_STREAM, 0, 1, &CANCEL.to_be_bytes()), &mut out);
        assert!(h.is_idle());
    }

    #[test]
    fn hpack_int_encoding() {
        let mut v = Vec::new();
        put_int(&mut v, 7, 0, 10);
        assert_eq!(v, [10]);
        v.clear();
        put_int(&mut v, 7, 0, 1337 - 0);
        // 1337 with a 7-bit prefix: 127, then 1210 as base-128 varint.
        assert_eq!(v, [127, (1210 % 128) as u8 | 0x80, (1210 / 128) as u8]);
    }

    // ---- real-browser connection openings --------------------------------

    fn settings(entries: &[(u16, u32)]) -> Vec<u8> {
        let mut p = Vec::new();
        for (id, v) in entries {
            p.extend_from_slice(&id.to_be_bytes());
            p.extend_from_slice(&v.to_be_bytes());
        }
        frame(SETTINGS, 0, 0, &p)
    }

    fn priority(sid: u32, dep: u32, weight: u8, exclusive: bool) -> Vec<u8> {
        let mut p = (dep | if exclusive { 0x8000_0000 } else { 0 })
            .to_be_bytes()
            .to_vec();
        p.push(weight);
        frame(PRIORITY, 0, sid, &p)
    }

    /// HEADERS with the PRIORITY flag (what Chrome and Firefox send for every request).
    fn headers_with_priority(sid: u32, block: &[u8], extra_flags: u8) -> Vec<u8> {
        let mut p = vec![0x80, 0, 0, 0, 219]; // exclusive, depends on stream 0, weight 220
        p.extend_from_slice(block);
        frame(HEADERS, END_HEADERS | PRIORITY_FLAG | extra_flags, sid, &p)
    }

    fn browser_get(path: &str) -> Vec<u8> {
        encode_req(&[
            (":method", "GET"),
            (":authority", "example.test"),
            (":scheme", "https"),
            (":path", path),
            ("te", "trailers"),
            ("cookie", "wordpress_test_cookie=WP%20Cookie%20check"),
            ("cookie", "a=b"),
            ("accept-encoding", "gzip, deflate, br, zstd"),
            ("user-agent", "Mozilla/5.0"),
        ])
    }

    #[test]
    fn firefox_opening_with_priority_frames_is_accepted() {
        let mut out = Vec::new();
        let mut h = H2Conn::new(1 << 20, &mut out);
        let mut input = PREFACE.to_vec();
        input.extend(settings(&[(1, 65536), (2, 0), (4, 131072), (5, 16384)]));
        input.extend(frame(WINDOW_UPDATE, 0, 0, &12_517_377u32.to_be_bytes()));
        // Firefox creates idle "group" streams with PRIORITY frames before any request.
        for (sid, dep, w) in [
            (3, 0, 200),
            (5, 0, 100),
            (7, 0, 0),
            (9, 7, 0),
            (11, 3, 0),
            (13, 0, 240),
        ] {
            input.extend(priority(sid, dep, w, false));
        }
        input.extend(headers_with_priority(
            15,
            &browser_get("/wp-login.php"),
            END_STREAM,
        ));
        let f = h.feed(&input, &mut out);
        assert!(!f.fatal, "PRIORITY frames must not be a protocol error");
        assert_eq!(f.consumed, input.len());
        let r = h.take_ready().expect("request after PRIORITY frames");
        assert_eq!(
            (r.stream, r.method.as_str(), r.path.as_str()),
            (15, "GET", "/wp-login.php")
        );
        assert!(
            r.headers
                .iter()
                .any(|(n, v)| n == b"cookie"
                    && v == b"wordpress_test_cookie=WP%20Cookie%20check; a=b")
        );
        assert!(frames(&out)
            .iter()
            .all(|f| f.0 != GOAWAY && f.0 != RST_STREAM));
    }

    #[test]
    fn chrome_opening_and_reprioritisation_is_accepted() {
        let mut out = Vec::new();
        let mut h = H2Conn::new(1 << 20, &mut out);
        let mut input = PREFACE.to_vec();
        input.extend(settings(&[
            (1, 65536),
            (2, 0),
            (4, 6_291_456),
            (6, 262_144),
        ]));
        input.extend(frame(WINDOW_UPDATE, 0, 0, &15_663_105u32.to_be_bytes()));
        input.extend(headers_with_priority(
            1,
            &browser_get("/wp-login.php"),
            END_STREAM,
        ));
        // Re-prioritising an open or finished stream, a PRIORITY_UPDATE (0x10) and an unknown type.
        input.extend(priority(1, 0, 255, true));
        input.extend(frame(0x10, 0, 0, &[0, 0, 0, 1, b'u', b'=', b'3']));
        input.extend(frame(0xfa, 0, 0, b"grease"));
        let f = h.feed(&input, &mut out);
        assert!(!f.fatal);
        assert!(h.take_ready().is_some());
        // The rest of the buffer is consumed on the next call (one request at a time).
        let f2 = h.feed(&input[f.consumed..], &mut out);
        assert!(!f2.fatal);
        assert_eq!(f.consumed + f2.consumed, input.len());
    }

    #[test]
    fn priority_frame_errors_are_still_detected() {
        let (mut h, mut out) = started();
        assert!(
            h.feed(&frame(PRIORITY, 0, 0, &[0; 5]), &mut out).fatal,
            "stream 0"
        );
        let (mut h, mut out) = started();
        let f = h.feed(&frame(PRIORITY, 0, 3, &[0; 4]), &mut out);
        assert!(!f.fatal, "wrong length is a stream error (RFC 9113 §6.3)");
        assert_eq!(rst_codes(&out), vec![(3, FRAME_SIZE_ERROR)]);
        let (mut h, mut out) = started();
        assert!(!h.feed(&frame(PRIORITY, 0, 3, &[0; 5]), &mut out).fatal);
    }

    #[test]
    fn padded_headers_with_priority_are_parsed() {
        let (mut h, mut out) = started();
        let block = get_headers("/x");
        let mut p = vec![3u8]; // pad length
        p.extend_from_slice(&[0, 0, 0, 0, 15]); // priority
        p.extend_from_slice(&block);
        p.extend_from_slice(&[0, 0, 0]);
        let f = h.feed(
            &frame(
                HEADERS,
                END_HEADERS | END_STREAM | PADDED | PRIORITY_FLAG,
                1,
                &p,
            ),
            &mut out,
        );
        assert!(!f.fatal);
        assert_eq!(h.take_ready().unwrap().path, "/x");
    }

    /// Decode our own response HEADERS block with the strictness of a browser.
    fn assert_browser_valid_response(wire: &[u8], stream: u32, expect_body: usize) {
        let fs = frames(wire);
        let hdr_idx = fs
            .iter()
            .position(|f| f.0 == HEADERS && f.2 == stream)
            .expect("HEADERS frame");
        assert_eq!(hdr_idx, 0, "HEADERS must come first");
        let mut block = fs[hdr_idx].3.clone();
        let mut i = hdr_idx + 1;
        let mut ended = fs[hdr_idx].1 & END_HEADERS != 0;
        while !ended {
            let f = &fs[i];
            assert_eq!(
                (f.0, f.2),
                (CONTINUATION, stream),
                "CONTINUATION must follow immediately"
            );
            block.extend_from_slice(&f.3);
            ended = f.1 & END_HEADERS != 0;
            i += 1;
        }
        let hs = hpack::Decoder::new().decode(&block).expect("valid HPACK");
        assert_eq!(hs[0].0, b":status", "status first");
        let mut cl: Option<usize> = None;
        for (n, v) in &hs[1..] {
            assert!(
                !n.iter().any(|b| b.is_ascii_uppercase()),
                "lowercase names: {:?}",
                String::from_utf8_lossy(n)
            );
            assert!(n.first() != Some(&b':'), "no pseudo-headers after :status");
            for banned in [
                "connection",
                "keep-alive",
                "proxy-connection",
                "transfer-encoding",
                "upgrade",
            ] {
                assert_ne!(
                    n.as_slice(),
                    banned.as_bytes(),
                    "connection-specific header {banned}"
                );
            }
            assert!(!v.iter().any(|&b| b == b'\r' || b == b'\n' || b == 0));
            if n == b"content-length" {
                assert!(cl.is_none(), "duplicate content-length");
                cl = Some(String::from_utf8_lossy(v).parse().unwrap());
            }
        }
        let data: Vec<_> = fs[i..]
            .iter()
            .filter(|f| f.0 == DATA && f.2 == stream)
            .collect();
        let total: usize = data.iter().map(|f| f.3.len()).sum();
        assert_eq!(total, expect_body);
        if let Some(c) = cl {
            assert_eq!(c, total, "content-length must equal DATA total");
        }
        assert!(data.iter().all(|f| f.3.len() <= 16_384));
        if expect_body > 0 {
            assert_eq!(
                data.iter().filter(|f| f.1 & END_STREAM != 0).count(),
                1,
                "exactly one END_STREAM"
            );
            assert!(
                data.last().unwrap().1 & END_STREAM != 0,
                "END_STREAM on the last DATA frame"
            );
        }
    }

    #[test]
    fn php_style_response_with_many_cookies_is_valid_for_a_strict_client() {
        let mut out = Vec::new();
        let mut h = H2Conn::new(1 << 20, &mut out);
        let mut input = PREFACE.to_vec();
        input.extend(settings(&[(1, 65536), (2, 0), (4, 6_291_456)]));
        input.extend(frame(WINDOW_UPDATE, 0, 0, &15_663_105u32.to_be_bytes()));
        input.extend(headers_with_priority(
            1,
            &browser_get("/wp-login.php"),
            END_STREAM,
        ));
        assert!(!h.feed(&input, &mut out).fatal);
        let req = h.take_ready().unwrap();
        out.clear();
        let long_cookie = format!(
            "wordpress_logged_in_x={}; path=/; secure; HttpOnly",
            "a".repeat(300)
        );
        let body = vec![b'<'; 40_000];
        let resp = H2Response {
            status: 200,
            headers: vec![
                (
                    b"content-type".to_vec(),
                    b"text/html; charset=UTF-8".to_vec(),
                ),
                (
                    b"set-cookie".to_vec(),
                    b"wordpress_test_cookie=WP%20Cookie%20check; path=/; secure".to_vec(),
                ),
                (b"set-cookie".to_vec(), long_cookie.into_bytes()),
                (b"set-cookie".to_vec(), b"c=3".to_vec()),
                (
                    b"link".to_vec(),
                    b"<https://example.test/wp-json/>; rel=\"https://api.w.org/\"".to_vec(),
                ),
                (
                    b"content-length".to_vec(),
                    body.len().to_string().into_bytes(),
                ),
            ],
            body: H2Body::Mem(body.clone()),
        };
        h.respond(req.stream, resp, &mut out);
        assert!(h.poll_output(&mut out).is_none());
        assert_eq!(h.active_sends(), 0, "whole body fits the browser's windows");
        assert_browser_valid_response(&out, 1, body.len());
    }

    #[test]
    fn response_to_a_small_window_client_resumes_correctly() {
        let (mut h, mut out) = started();
        assert!(
            !h.feed(
                &headers_with_priority(1, &get_headers("/big"), END_STREAM),
                &mut out
            )
            .fatal
        );
        let req = h.take_ready().unwrap();
        out.clear();
        let body = vec![7u8; 200_000];
        h.respond(
            req.stream,
            H2Response {
                status: 200,
                headers: vec![],
                body: H2Body::Mem(body.clone()),
            },
            &mut out,
        );
        h.poll_output(&mut out);
        let sent: usize = frames(&out)
            .iter()
            .filter(|f| f.0 == DATA)
            .map(|f| f.3.len())
            .sum();
        assert_eq!(sent, 65_535, "stops at the default connection window");
        // Browser grants more credit, in several updates, interleaved with PRIORITY.
        let mut more = frame(WINDOW_UPDATE, 0, 0, &1_000_000u32.to_be_bytes());
        more.extend(priority(1, 0, 10, false));
        more.extend(frame(WINDOW_UPDATE, 0, 1, &1_000_000u32.to_be_bytes()));
        assert!(!h.feed(&more, &mut out).fatal);
        h.poll_output(&mut out);
        assert_browser_valid_response(&out, 1, 200_000);
    }

    #[test]
    fn several_pipelined_requests_are_released_one_by_one() {
        let (mut h, mut out) = started();
        let mut input = Vec::new();
        for sid in [1u32, 3, 5] {
            input.extend(headers_with_priority(sid, &browser_get("/p"), END_STREAM));
            input.extend(priority(sid, 0, 1, false));
        }
        let mut off = 0;
        let mut seen = Vec::new();
        while off < input.len() {
            let f = h.feed(&input[off..], &mut out);
            assert!(!f.fatal);
            off += f.consumed;
            if let Some(r) = h.take_ready() {
                seen.push(r.stream);
            } else {
                assert_eq!(off, input.len());
            }
        }
        assert_eq!(seen, [1, 3, 5]);
    }

    // ---- flow control under mixed traffic ------------------------------------

    /// What a strict peer tracks: every DATA frame must fit the connection and
    /// stream credit it has granted so far.
    struct Peer {
        conn: i64,
        init: i64,
        streams: HashMap<u32, i64>,
        ended: HashSet<u32>,
        reset: HashSet<u32>,
        seen: usize,
    }

    impl Peer {
        fn observe(&mut self, out: &[u8]) {
            let fs = frames(&out[self.seen..]);
            self.seen = out.len();
            for (ty, flags, sid, payload) in fs {
                if ty != DATA {
                    continue;
                }
                assert!(
                    payload.len() <= 16_384,
                    "DATA frame over the maximum frame size"
                );
                if self.reset.contains(&sid) {
                    continue; // frames already in flight when we reset are legal
                }
                assert!(
                    !self.ended.contains(&sid),
                    "DATA after END_STREAM on stream {sid}"
                );
                self.conn -= payload.len() as i64;
                let c = self
                    .streams
                    .get_mut(&sid)
                    .expect("DATA for a stream we never opened");
                *c -= payload.len() as i64;
                assert!(
                    self.conn >= 0,
                    "connection flow-control window exceeded ({})",
                    self.conn
                );
                assert!(*c >= 0, "stream {sid} flow-control window exceeded ({c})");
                if flags & END_STREAM != 0 {
                    self.ended.insert(sid);
                }
            }
        }
    }

    use std::collections::HashSet;

    #[test]
    fn flow_control_is_never_exceeded_under_mixed_file_and_memory_streams() {
        use crate::static_files::OpenFile;
        let mut x = 0x9e37_79b9_7f4a_7c15u64;
        let mut rnd = move |n: usize| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x % n.max(1) as u64) as usize
        };
        for _round in 0..400 {
            let (mut h, mut out) = started();
            out.clear();
            let mut peer = Peer {
                conn: 65_535,
                init: 65_535,
                streams: HashMap::new(),
                ended: HashSet::new(),
                reset: HashSet::new(),
                seen: 0,
            };
            let mut pending: Vec<FileRead> = Vec::new();
            let mut next_sid = 1u32;
            for _step in 0..60 {
                match rnd(7) {
                    0 | 1 => {
                        let sid = next_sid;
                        next_sid += 2;
                        assert!(
                            !h.feed(
                                &headers_with_priority(sid, &get_headers("/x"), END_STREAM),
                                &mut out
                            )
                            .fatal
                        );
                        let req = h.take_ready().unwrap();
                        let size = [0usize, 10, 5_000, 70_000, 300_000][rnd(5)];
                        let body = if rnd(2) == 0 {
                            H2Body::Mem(vec![1; size])
                        } else {
                            H2Body::File(Rc::new(OpenFile::for_test(size as u64)))
                        };
                        h.respond(
                            req.stream,
                            H2Response {
                                status: 200,
                                headers: vec![],
                                body,
                            },
                            &mut out,
                        );
                        peer.streams.insert(sid, peer.init);
                    }
                    2 => {
                        let inc = 1 + rnd(200_000) as u32;
                        peer.conn += inc as i64;
                        assert!(
                            !h.feed(&frame(WINDOW_UPDATE, 0, 0, &inc.to_be_bytes()), &mut out)
                                .fatal
                        );
                    }
                    3 => {
                        if let Some(&sid) = peer.streams.keys().nth(rnd(peer.streams.len().max(1)))
                        {
                            if !peer.ended.contains(&sid) {
                                let inc = 1 + rnd(100_000) as u32;
                                *peer.streams.get_mut(&sid).unwrap() += inc as i64;
                                assert!(
                                    !h.feed(
                                        &frame(WINDOW_UPDATE, 0, sid, &inc.to_be_bytes()),
                                        &mut out
                                    )
                                    .fatal
                                );
                            }
                        }
                    }
                    4 => {
                        if let Some(&sid) = peer.streams.keys().nth(rnd(peer.streams.len().max(1)))
                        {
                            if !peer.ended.contains(&sid) {
                                peer.reset.insert(sid);
                                assert!(
                                    !h.feed(
                                        &frame(RST_STREAM, 0, sid, &8u32.to_be_bytes()),
                                        &mut out
                                    )
                                    .fatal
                                );
                            }
                        }
                    }
                    5 => {
                        let new = [1_000i64, 65_535, 131_072, 6_291_456][rnd(4)];
                        let delta = new - peer.init;
                        peer.init = new;
                        for c in peer.streams.values_mut() {
                            *c += delta;
                        }
                        assert!(!h.feed(&settings(&[(4, new as u32)]), &mut out).fatal);
                    }
                    _ => {}
                }
                // The disk answers some reads late, in any order.
                while let Some(fr) = h.poll_output(&mut out) {
                    pending.push(fr);
                }
                if !pending.is_empty() && rnd(3) != 0 {
                    let fr = pending.remove(rnd(pending.len()));
                    h.file_data(fr.stream, &vec![2u8; fr.len as usize], &mut out);
                }
                peer.observe(&out);
            }
        }
    }

    // ───────────── stream state machine / h2spec conformance ─────────────

    /// (stream, code) of every RST_STREAM in `out`.
    fn rst_codes(out: &[u8]) -> Vec<(u32, u32)> {
        frames(out)
            .into_iter()
            .filter(|f| f.0 == RST_STREAM)
            .map(|f| (f.2, u32::from_be_bytes([f.3[0], f.3[1], f.3[2], f.3[3]])))
            .collect()
    }

    /// Error code of the GOAWAY in `out`, if any.
    fn goaway_code(out: &[u8]) -> Option<u32> {
        frames(out)
            .into_iter()
            .find(|f| f.0 == GOAWAY)
            .map(|f| u32::from_be_bytes([f.3[4], f.3[5], f.3[6], f.3[7]]))
    }

    fn post_headers() -> Vec<u8> {
        encode_req(&[
            (":method", "POST"),
            (":scheme", "https"),
            (":path", "/"),
            (":authority", "x"),
        ])
    }

    /// Open stream `sid` with a request body still to come.
    fn open_with_body_pending(h: &mut H2Conn, sid: u32) {
        let mut sink = Vec::new();
        assert!(
            !h.feed(
                &frame(HEADERS, END_HEADERS, sid, &post_headers()),
                &mut sink
            )
            .fatal
        );
    }

    fn conn_error(frames_in: &[Vec<u8>]) -> Option<u32> {
        let (mut h, mut out) = started();
        out.clear();
        let mut fatal = false;
        for f in frames_in {
            fatal |= h.feed(f, &mut out).fatal;
        }
        if fatal {
            goaway_code(&out)
        } else {
            None
        }
    }

    #[test]
    fn idle_stream_frames_are_connection_errors() {
        // §5.1 idle: DATA, RST_STREAM, WINDOW_UPDATE, CONTINUATION
        assert_eq!(conn_error(&[frame(DATA, 0, 1, b"x")]), Some(PROTOCOL_ERROR));
        assert_eq!(
            conn_error(&[frame(RST_STREAM, 0, 1, &CANCEL.to_be_bytes())]),
            Some(PROTOCOL_ERROR)
        );
        assert_eq!(
            conn_error(&[frame(WINDOW_UPDATE, 0, 1, &1u32.to_be_bytes())]),
            Some(PROTOCOL_ERROR)
        );
        assert_eq!(
            conn_error(&[frame(CONTINUATION, END_HEADERS, 1, &[])]),
            Some(PROTOCOL_ERROR)
        );
        // PRIORITY is allowed on idle streams.
        assert_eq!(conn_error(&[priority(1, 0, 15, false)]), None);
    }

    #[test]
    fn data_or_headers_on_half_closed_remote_is_a_stream_error() {
        for late in [
            frame(DATA, 0, 1, b"x"),
            frame(HEADERS, END_HEADERS | END_STREAM, 1, &get_headers("/")),
        ] {
            let (mut h, mut out) = started();
            open_stream(&mut h, 1);
            out.clear();
            let f = h.feed(&late, &mut out);
            assert!(!f.fatal);
            assert_eq!(rst_codes(&out), vec![(1, STREAM_CLOSED)]);
            // The reset cancelled the request: a late response is discarded.
            out.clear();
            h.respond(1, mem_response(10), &mut out);
            assert!(out.is_empty());
            // WINDOW_UPDATE and PRIORITY on a half-closed stream are fine.
        }
        let (mut h, mut out) = started();
        open_stream(&mut h, 1);
        out.clear();
        assert!(
            !h.feed(&frame(WINDOW_UPDATE, 0, 1, &1u32.to_be_bytes()), &mut out)
                .fatal
        );
        assert!(!h.feed(&priority(1, 0, 1, false), &mut out).fatal);
        assert!(out.is_empty());
    }

    #[test]
    fn half_closed_remote_headers_keep_hpack_in_sync() {
        let (mut h, mut out) = started();
        open_stream(&mut h, 1);
        out.clear();
        // Split across CONTINUATION; the block must still be decoded (a dynamic-table entry is added).
        let blk = encode_req(&[
            (":method", "GET"),
            (":scheme", "https"),
            (":path", "/"),
            ("x-a", "1"),
        ]);
        h.feed(&frame(HEADERS, END_STREAM, 1, &blk[..3]), &mut out);
        h.feed(&frame(CONTINUATION, END_HEADERS, 1, &blk[3..]), &mut out);
        assert_eq!(rst_codes(&out), vec![(1, STREAM_CLOSED)]);
        out.clear();
        h.feed(
            &frame(HEADERS, END_HEADERS | END_STREAM, 3, &get_headers("/ok")),
            &mut out,
        );
        assert_eq!(h.take_ready().map(|r| r.path), Some("/ok".to_string()));
    }

    #[test]
    fn frames_after_peer_reset_get_stream_closed() {
        for late in [
            frame(DATA, 0, 1, b"x"),
            frame(HEADERS, END_HEADERS | END_STREAM, 1, &get_headers("/")),
        ] {
            let (mut h, mut out) = started();
            open_with_body_pending(&mut h, 1);
            h.feed(&frame(RST_STREAM, 0, 1, &CANCEL.to_be_bytes()), &mut out);
            out.clear();
            let f = h.feed(&late, &mut out);
            assert!(!f.fatal);
            assert_eq!(rst_codes(&out), vec![(1, STREAM_CLOSED)]);
        }
    }

    #[test]
    fn frames_on_a_normally_closed_stream_are_connection_errors() {
        for late in [
            frame(DATA, 0, 1, b"x"),
            frame(HEADERS, END_HEADERS | END_STREAM, 1, &get_headers("/")),
            frame(CONTINUATION, END_HEADERS, 1, &[]),
        ] {
            let (mut h, mut out) = started();
            open_stream(&mut h, 1);
            h.respond(
                1,
                H2Response {
                    status: 204,
                    headers: vec![],
                    body: H2Body::Empty,
                },
                &mut out,
            );
            out.clear();
            let f = h.feed(&late, &mut out);
            assert!(f.fatal);
            assert_eq!(goaway_code(&out), Some(STREAM_CLOSED));
        }
    }

    #[test]
    fn late_frames_after_our_own_reset_are_ignored_and_rst_window_updates_are_harmless() {
        let (mut h, mut out) = started();
        open_with_body_pending(&mut h, 1);
        // Oversized body → we reset with CANCEL; the peer's in-flight DATA must not provoke more errors.
        let mut small = H2Conn::new(4, &mut out);
        small.feed(PREFACE, &mut out);
        small.feed(&frame(HEADERS, END_HEADERS, 1, &post_headers()), &mut out);
        out.clear();
        small.feed(&frame(DATA, 0, 1, b"toolong"), &mut out);
        assert_eq!(rst_codes(&out), vec![(1, CANCEL)]);
        out.clear();
        let f = small.feed(&frame(DATA, END_STREAM, 1, b"more"), &mut out);
        assert!(!f.fatal);
        assert!(rst_codes(&out).is_empty());
        // Late WINDOW_UPDATE / RST_STREAM on closed streams are ignored.
        assert!(
            !small
                .feed(&frame(WINDOW_UPDATE, 0, 1, &1u32.to_be_bytes()), &mut out)
                .fatal
        );
        assert!(
            !small
                .feed(&frame(RST_STREAM, 0, 1, &CANCEL.to_be_bytes()), &mut out)
                .fatal
        );
    }

    #[test]
    fn priority_self_dependency_is_a_stream_error() {
        let (mut h, mut out) = started();
        out.clear();
        assert!(!h.feed(&priority(3, 3, 1, false), &mut out).fatal);
        assert_eq!(rst_codes(&out), vec![(3, PROTOCOL_ERROR)]);
        // HEADERS carrying a self-dependent priority field.
        let (mut h, mut out) = started();
        out.clear();
        let mut payload = Vec::new();
        payload.extend_from_slice(&1u32.to_be_bytes());
        payload.push(15);
        payload.extend_from_slice(&get_headers("/"));
        assert!(
            !h.feed(
                &frame(
                    HEADERS,
                    END_HEADERS | END_STREAM | PRIORITY_FLAG,
                    1,
                    &payload
                ),
                &mut out
            )
            .fatal
        );
        assert_eq!(rst_codes(&out), vec![(1, PROTOCOL_ERROR)]);
        assert!(h.take_ready().is_none());
    }

    fn malformed(headers: &[(&str, &str)]) -> Vec<(u32, u32)> {
        let (mut h, mut out) = started();
        out.clear();
        let f = h.feed(
            &frame(HEADERS, END_HEADERS | END_STREAM, 1, &encode_req(headers)),
            &mut out,
        );
        assert!(!f.fatal);
        assert!(h.take_ready().is_none());
        rst_codes(&out)
    }

    #[test]
    fn malformed_pseudo_headers_are_stream_errors() {
        let expect = vec![(1, PROTOCOL_ERROR)];
        assert_eq!(
            malformed(&[(":method", "GET"), (":path", "/")]),
            expect,
            "missing :scheme"
        );
        assert_eq!(
            malformed(&[
                (":method", "GET"),
                (":method", "GET"),
                (":scheme", "https"),
                (":path", "/")
            ]),
            expect,
            "duplicate :method"
        );
        assert_eq!(
            malformed(&[
                (":method", "GET"),
                (":scheme", "https"),
                (":scheme", "https"),
                (":path", "/")
            ]),
            expect,
            "duplicate :scheme"
        );
        assert_eq!(
            malformed(&[
                (":method", "GET"),
                (":scheme", "https"),
                (":path", "/"),
                (":path", "/")
            ]),
            expect,
            "duplicate :path"
        );
        assert_eq!(
            malformed(&[(":scheme", "https"), (":path", "/")]),
            expect,
            "missing :method"
        );
    }

    #[test]
    fn content_length_must_match_the_body() {
        // Declared 1, END_STREAM on HEADERS (no body).
        assert_eq!(
            malformed(&[
                (":method", "POST"),
                (":scheme", "https"),
                (":path", "/"),
                ("content-length", "1")
            ]),
            vec![(1, PROTOCOL_ERROR)]
        );
        // Declared 1, body of 2 across two DATA frames.
        let (mut h, mut out) = started();
        let blk = encode_req(&[
            (":method", "POST"),
            (":scheme", "https"),
            (":path", "/"),
            ("content-length", "1"),
        ]);
        h.feed(&frame(HEADERS, END_HEADERS, 1, &blk), &mut out);
        out.clear();
        h.feed(&frame(DATA, 0, 1, b"a"), &mut out);
        h.feed(&frame(DATA, END_STREAM, 1, b"b"), &mut out);
        assert_eq!(rst_codes(&out), vec![(1, PROTOCOL_ERROR)]);
        assert!(h.take_ready().is_none());
        // Declared 2, body of 1.
        let (mut h, mut out) = started();
        let blk = encode_req(&[
            (":method", "POST"),
            (":scheme", "https"),
            (":path", "/"),
            ("content-length", "2"),
        ]);
        h.feed(&frame(HEADERS, END_HEADERS, 1, &blk), &mut out);
        out.clear();
        h.feed(&frame(DATA, END_STREAM, 1, b"a"), &mut out);
        assert_eq!(rst_codes(&out), vec![(1, PROTOCOL_ERROR)]);
        // Matching length (padding does not count) is accepted.
        let (mut h, mut out) = started();
        let blk = encode_req(&[
            (":method", "POST"),
            (":scheme", "https"),
            (":path", "/"),
            ("content-length", "2"),
        ]);
        h.feed(&frame(HEADERS, END_HEADERS, 1, &blk), &mut out);
        out.clear();
        h.feed(
            &frame(DATA, END_STREAM | PADDED, 1, &[3, b'o', b'k', 0, 0, 0]),
            &mut out,
        );
        assert!(rst_codes(&out).is_empty());
        assert_eq!(h.take_ready().map(|r| r.body), Some(b"ok".to_vec()));
    }

    #[test]
    fn rst_stream_frame_errors() {
        // §6.4: idle stream → PROTOCOL_ERROR; wrong length → FRAME_SIZE_ERROR (connection error).
        assert_eq!(
            conn_error(&[frame(RST_STREAM, 0, 1, &[0, 0, 0])]),
            Some(FRAME_SIZE_ERROR)
        );
        assert_eq!(
            conn_error(&[frame(RST_STREAM, 0, 0, &CANCEL.to_be_bytes())]),
            Some(PROTOCOL_ERROR)
        );
    }

    #[test]
    fn several_initial_window_sizes_in_one_frame_take_effect_in_order() {
        let (mut h, mut out) = started();
        open_stream(&mut h, 1);
        h.respond(1, mem_response(100), &mut out);
        let mut s = Vec::new();
        push_setting(&mut s, 4, 1);
        push_setting(&mut s, 4, 200_000);
        out.clear();
        assert!(!h.feed(&frame(SETTINGS, 0, 0, &s), &mut out).fatal);
        assert_eq!(h.peer_init_window, 200_000);
        // A stream opened now gets the final value.
        open_stream(&mut h, 3);
        out.clear();
        h.respond(3, mem_response(100_000), &mut out);
        h.feed(
            &frame(WINDOW_UPDATE, 0, 0, &(1u32 << 20).to_be_bytes()),
            &mut out,
        );
        h.poll_output(&mut out);
        assert_eq!(
            data_bytes(&out),
            (100_100, true),
            "stream 1's 100 bytes + stream 3's 100000"
        );
    }

    #[test]
    fn reset_mid_response_then_next_request_works() {
        // Browser cancels a CSS download half-way; the connection must stay healthy.
        let (mut h, mut out) = started();
        open_stream(&mut h, 1);
        h.respond(1, mem_response(300_000), &mut out);
        h.poll_output(&mut out);
        out.clear();
        assert!(
            !h.feed(&frame(RST_STREAM, 0, 1, &CANCEL.to_be_bytes()), &mut out)
                .fatal
        );
        // The peer's WINDOW_UPDATE for the dead stream arrives afterwards: ignored.
        assert!(
            !h.feed(
                &frame(WINDOW_UPDATE, 0, 1, &65_535u32.to_be_bytes()),
                &mut out
            )
            .fatal
        );
        // Bytes already sent on the dead stream still count against the connection window.
        h.feed(
            &frame(WINDOW_UPDATE, 0, 0, &65_535u32.to_be_bytes()),
            &mut out,
        );
        open_stream(&mut h, 3);
        out.clear();
        h.respond(3, mem_response(10), &mut out);
        h.poll_output(&mut out);
        assert_eq!(data_bytes(&out), (10, true));
    }
}
