// Connection edge: async socket I/O over channels, plus the per-connection
// Descriptor that lives inside GameState. Commands never touch sockets —
// they append to Descriptor::outbuf, which the Game task flushes to the
// writer task after each command/pulse (mirrors C process_input/process_output).

use crate::types::{CharId, ConnId};
use anyhow::Result;
use std::net::SocketAddr;
use std::os::unix::io::{AsRawFd, RawFd};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

// --- Telnet protocol constants (RFC 854 / 1073 / 1091) ---
const IAC: u8 = 255; // Interpret As Command
const DONT: u8 = 254;
const DO: u8 = 253;
const WONT: u8 = 252;
const WILL: u8 = 251;
const SB: u8 = 250; // Subnegotiation begin
const SE: u8 = 240; // Subnegotiation end

// Out-of-band protocol options we DO support (everything else is still refused).
// GMCP (Generic Mud Communication Protocol, "Atcp2"/option 201) carries JSON
// out-of-band data for modern clients (Mudlet/Mudslinger gauges + auto-mapper).
// MSSP (Mud Server Status Protocol, option 70) lets crawlers read server status.
const TELOPT_GMCP: u8 = 201;
const TELOPT_MSSP: u8 = 70;

/// Byte-level telnet input filter. Strips IAC command sequences from a raw byte
/// stream so negotiation a client sends on connect (Mudlet's NAWS/TTYPE/GMCP
/// hello, plain `IAC DO/WILL` bursts) never corrupts the input line — notably
/// the first one (the name prompt). The C server does this in comm.c
/// (process_input's telnet scanner); we mirror it as a small state machine.
///
/// `feed()` consumes a byte slice, emits any completed input lines via the
/// `on_line` callback, and pushes refusal bytes (`IAC WONT/DONT <opt>`) into
/// `refuse` for options we don't support so clients don't block waiting for a
/// reply.
struct TelnetFilter {
    state: TelnetState,
    /// Accumulated printable bytes of the line in progress.
    line: Vec<u8>,
    /// Option byte of the subnegotiation currently being consumed (the byte
    /// right after IAC SB). We consume the whole payload regardless, but tracking
    /// it lets us recognize an incoming GMCP `IAC SB GMCP ... IAC SE` (e.g. the
    /// client's Core.Hello / Core.Supports.Set) without choking on it.
    subneg_opt: u8,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TelnetState {
    /// Normal data.
    Data,
    /// Saw IAC; next byte is a command.
    Iac,
    /// Saw IAC <WILL|WONT|DO|DONT>; next byte is the option.
    Negotiate(u8),
    /// Saw IAC SB; next byte is the subnegotiation option.
    SubnegOpt,
    /// Inside IAC SB <opt> ...; consuming subnegotiation data until IAC SE.
    Subneg,
    /// Inside subnegotiation and saw an IAC; next byte is SE (end) or escaped.
    SubnegIac,
}

/// A client capability we just learned about from telnet negotiation, surfaced
/// out of `TelnetFilter::feed` so the input loop can notify the Game (which owns
/// the live player/world data the client wants out-of-band).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientCap {
    /// Client sent `IAC DO GMCP`: it wants out-of-band JSON (gauges, mapper).
    EnableGmcp,
    /// Client sent `IAC DO MSSP`: it wants the one-shot server status block.
    RequestMssp,
}

impl TelnetFilter {
    fn new() -> Self {
        TelnetFilter {
            state: TelnetState::Data,
            line: Vec::with_capacity(256),
            subneg_opt: 0,
        }
    }

    /// Feed raw bytes. Completed lines (terminated by `\n`, with a trailing
    /// `\r` stripped) are passed to `on_line` as owned Strings (lossy UTF-8).
    /// Reply bytes to send back to the client (option refusals, plus the
    /// `IAC WILL <opt>` accepts for GMCP/MSSP) are appended to `reply`. Any
    /// capabilities the client just enabled are pushed into `caps` so the input
    /// loop can hand them to the Game.
    fn feed<F: FnMut(String)>(
        &mut self,
        data: &[u8],
        reply: &mut Vec<u8>,
        caps: &mut Vec<ClientCap>,
        mut on_line: F,
    ) {
        for &b in data {
            match self.state {
                TelnetState::Data => match b {
                    IAC => self.state = TelnetState::Iac,
                    b'\n' => {
                        // Strip a single trailing '\r' if present.
                        if self.line.last() == Some(&b'\r') {
                            self.line.pop();
                        }
                        let s = String::from_utf8_lossy(&self.line).into_owned();
                        self.line.clear();
                        on_line(s);
                    }
                    // Drop bare CR and NUL (telnet sends "\r\0" for a bare CR);
                    // they are not line content. '\r' before '\n' is handled
                    // above by the trailing-CR strip.
                    b'\0' => {}
                    _ => self.line.push(b),
                },
                TelnetState::Iac => match b {
                    IAC => {
                        // Escaped 0xFF -> literal data byte.
                        self.line.push(IAC);
                        self.state = TelnetState::Data;
                    }
                    WILL | WONT | DO | DONT => self.state = TelnetState::Negotiate(b),
                    SB => self.state = TelnetState::SubnegOpt,
                    // Any other 2-byte IAC command (NOP, GA, AYT, etc.): consume.
                    _ => self.state = TelnetState::Data,
                },
                TelnetState::Negotiate(verb) => {
                    // We support GMCP and MSSP; for those, ACCEPT (IAC WILL <opt>)
                    // when the client says DO, and notify the Game. All OTHER
                    // options are still refused so the client doesn't wait:
                    // answer DO/WILL with WONT/DONT respectively. We do not reply
                    // to WONT/DONT (no further response is required).
                    match (verb, b) {
                        (DO, TELOPT_GMCP) => {
                            reply.extend_from_slice(&[IAC, WILL, TELOPT_GMCP]);
                            caps.push(ClientCap::EnableGmcp);
                        }
                        (DO, TELOPT_MSSP) => {
                            reply.extend_from_slice(&[IAC, WILL, TELOPT_MSSP]);
                            caps.push(ClientCap::RequestMssp);
                        }
                        // Client agreeing to a WILL we never sent, or turning an
                        // option off: nothing more to do for GMCP/MSSP.
                        (WILL, TELOPT_GMCP) | (WILL, TELOPT_MSSP) => {}
                        (DONT, TELOPT_GMCP) | (DONT, TELOPT_MSSP)
                        | (WONT, TELOPT_GMCP) | (WONT, TELOPT_MSSP) => {}
                        (DO, _) => reply.extend_from_slice(&[IAC, WONT, b]),
                        (WILL, _) => reply.extend_from_slice(&[IAC, DONT, b]),
                        _ => {} // WONT / DONT for unsupported options: nothing to send.
                    }
                    self.state = TelnetState::Data;
                }
                TelnetState::SubnegOpt => {
                    // The byte right after IAC SB is the option (GMCP, MSSP, ...).
                    // We don't currently act on inbound subneg payloads (a GMCP
                    // Core.Hello / Core.Supports.Set is consumed and ignored), but
                    // record the option so the consume loop is explicit.
                    self.subneg_opt = b;
                    self.state = if b == IAC {
                        // Degenerate IAC SB IAC ...: treat as end-of-subneg scan.
                        TelnetState::SubnegIac
                    } else {
                        TelnetState::Subneg
                    };
                }
                TelnetState::Subneg => {
                    if b == IAC {
                        self.state = TelnetState::SubnegIac;
                    }
                    // else: still inside subneg payload, consume the byte. This
                    // swallows the whole GMCP `<package> <json>` blob safely.
                }
                TelnetState::SubnegIac => {
                    if b == SE {
                        // End of subnegotiation.
                        self.state = TelnetState::Data;
                    } else {
                        // IAC IAC inside SB is an escaped 0xFF (still payload),
                        // or a stray IAC <other>; either way stay in subneg.
                        self.state = TelnetState::Subneg;
                    }
                }
            }
        }
    }
}

/// Connection / login state machine. Editor/OLC states will extend this via
/// the nested-input stack (Batch 1 Pillar D groundwork: `editor`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConState {
    GetName,
    GetOldPassword,
    ConfirmName, // "Did I get that right (Y/N)?"
    GetNewPassword,
    ConfirmPassword,
    GetSex,
    GetClass,
    GetRace,
    ReadMotd,
    Menu,
    Playing,
    Close,
}

/// A nested input context (string editor, OLC editor). Tier-0 stub; the
/// stack lets a Playing descriptor push an editor without a giant enum.
#[derive(Debug, Clone)]
pub enum InputContext {
    StringEdit { buffer: String, max_len: usize },
}

pub struct Descriptor {
    pub id: ConnId,
    pub host: String,
    /// Raw OS file descriptor of the underlying TCP socket. Captured before the
    /// stream is split (connection.rs handle_client) so do_copyover can write a
    /// final flush, clear FD_CLOEXEC, and inherit the live socket across execv —
    /// this is the linchpin of C's seamless copyover (act.wizard.c do_copyover).
    pub raw_fd: RawFd,
    pub state: ConState,
    /// Stack of nested input contexts; empty == normal command/menu input.
    pub editors: Vec<InputContext>,
    /// True once the client negotiated GMCP (`IAC DO GMCP`). When set, the Game
    /// pushes Char.Vitals + Room.Info out-of-band after each command so Mudlet's
    /// gauges and the GMCP mapper stay live.
    pub gmcp: bool,
    pub character: Option<CharId>,
    pub original: Option<CharId>, // for `switch`
    /// Output accumulated this pulse; flushed by the Game task.
    pub outbuf: String,
    /// True when a fresh prompt should be sent after flushing.
    pub need_prompt: bool,
    /// Command-lag counter (C `d->wait`): the heartbeat decrements it each pulse
    /// and only pulls the next queued command when it reaches <= 0. WAIT_STATE
    /// sets it from combat skills/casting to impose command lag.
    pub wait: i32,
    /// Queued raw input lines awaiting the wait gate (C `d->input`).
    pub input_queue: std::collections::VecDeque<String>,
    // Scratch during login / char creation.
    pub temp_name: Option<String>,
    pub temp_password: Option<String>,
}

impl Descriptor {
    pub fn new(id: ConnId, host: String) -> Self {
        Self::with_fd(id, host, -1)
    }

    pub fn with_fd(id: ConnId, host: String, raw_fd: RawFd) -> Self {
        Descriptor {
            id,
            host,
            raw_fd,
            state: ConState::GetName,
            editors: Vec::new(),
            gmcp: false,
            character: None,
            original: None,
            outbuf: String::new(),
            need_prompt: true,
            wait: 1,
            input_queue: std::collections::VecDeque::new(),
            temp_name: None,
            temp_password: None,
        }
    }

    pub fn write(&mut self, msg: &str) {
        self.outbuf.push_str(msg);
    }
}

// ANSI color: DeltaMUD `&x` codes. (Render path; the strip path for
// color-off players is added with the act() engine.)
pub const COLOR_CODES: &[(&str, &str)] = &[
    ("&n", "\x1b[0m"),
    ("&r", "\x1b[0;31m"),
    ("&g", "\x1b[0;32m"),
    ("&y", "\x1b[0;33m"),
    ("&b", "\x1b[0;34m"),
    ("&m", "\x1b[0;35m"),
    ("&c", "\x1b[0;36m"),
    ("&w", "\x1b[0;37m"),
    ("&R", "\x1b[1;31m"),
    ("&G", "\x1b[1;32m"),
    ("&Y", "\x1b[1;33m"),
    ("&B", "\x1b[1;34m"),
    ("&M", "\x1b[1;35m"),
    ("&C", "\x1b[1;36m"),
    ("&W", "\x1b[1;37m"),
];

/// Map a color-code char (the byte after `&`) to its ANSI escape, or None if
/// it is not a recognized code. Derived from COLOR_CODES (every entry is a
/// two-byte `&x` sequence, so the code char is the second byte of `.0`).
fn color_ansi(c: char) -> Option<&'static str> {
    for (code, ansi) in COLOR_CODES {
        // code is "&x"; match on the second char.
        if code.as_bytes().len() == 2 && code.as_bytes()[1] == c as u8 {
            return Some(ansi);
        }
    }
    None
}

/// Render DeltaMUD `&x` color codes to ANSI in a single pass. A `&` followed by
/// a known code char emits the escape; a `&` followed by anything else (or a
/// trailing `&`) passes through unchanged.
pub fn render_color(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '&' {
            if let Some(&next) = chars.peek() {
                if let Some(ansi) = color_ansi(next) {
                    result.push_str(ansi);
                    chars.next(); // consume the code char
                    continue;
                }
            }
            // Lone '&' or unknown code char: pass the '&' through unchanged.
            result.push('&');
        } else {
            result.push(c);
        }
    }
    result
}

/// Strip color codes (for players with color off / for logs) in a single pass.
/// A `&` followed by a known code char is removed; any other `&` passes through.
pub fn strip_color(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '&' {
            if let Some(&next) = chars.peek() {
                if color_ansi(next).is_some() {
                    chars.next(); // drop the code char, emit nothing
                    continue;
                }
            }
            result.push('&');
        } else {
            result.push(c);
        }
    }
    result
}

// Messages from connection tasks to the single Game task.
#[derive(Debug)]
pub enum GameMessage {
    NewConnection {
        id: ConnId,
        host: String,
        raw_fd: RawFd,
        output_tx: mpsc::Sender<String>,
    },
    /// Re-attach a player whose live socket was inherited across a copyover
    /// execv (comm.c copyover_recover). The Game loads the named player straight
    /// into Playing state, skipping the login nanny.
    Recover {
        id: ConnId,
        host: String,
        raw_fd: RawFd,
        name: String,
        output_tx: mpsc::Sender<String>,
    },
    Input {
        conn_id: ConnId,
        input: String,
    },
    /// Client negotiated GMCP (`IAC DO GMCP`). The Game flips the descriptor's
    /// gmcp flag and starts pushing out-of-band Char.Vitals/Room.Info.
    EnableGmcp {
        conn_id: ConnId,
    },
    /// Client requested the MSSP status block (`IAC DO MSSP`). The Game builds it
    /// (it needs the live player count / uptime) and sends it once.
    SendMssp {
        conn_id: ConnId,
    },
    Disconnect {
        conn_id: ConnId,
    },
}

/// Per-connection task: split the stream, register with the Game, pump input
/// lines into the game channel, and run a writer task draining output.
pub async fn handle_client(
    stream: TcpStream,
    addr: SocketAddr,
    conn_id: ConnId,
    game_tx: mpsc::Sender<GameMessage>,
) -> Result<()> {
    // Capture the raw fd BEFORE into_split() consumes the stream. do_copyover
    // needs this to inherit the live socket across execv (FD_CLOEXEC dance).
    let fd = stream.as_raw_fd();
    let host = addr.ip().to_string();

    let (mut reader, mut writer) = stream.into_split();

    let (output_tx, mut output_rx) = mpsc::channel::<String>(256);

    game_tx
        .send(GameMessage::NewConnection {
            id: conn_id,
            host,
            raw_fd: fd,
            output_tx: output_tx.clone(),
        })
        .await?;

    let write_handle = tokio::spawn(async move {
        while let Some(msg) = output_rx.recv().await {
            if writer.write_all(msg.as_bytes()).await.is_err() {
                break;
            }
            if writer.flush().await.is_err() {
                break;
            }
        }
    });

    run_input_loop(&mut reader, conn_id, &game_tx, &output_tx).await;

    let _ = game_tx.send(GameMessage::Disconnect { conn_id }).await;
    write_handle.abort();
    Ok(())
}

/// Pump raw bytes through the telnet filter, forwarding completed lines to the
/// Game and pushing any negotiation refusals back through the output channel.
/// Returns on EOF or read error. Shared by fresh and recovered connections.
async fn run_input_loop<R: AsyncReadExt + Unpin>(
    reader: &mut R,
    conn_id: ConnId,
    game_tx: &mpsc::Sender<GameMessage>,
    output_tx: &mpsc::Sender<String>,
) {
    let mut filter = TelnetFilter::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = match reader.read(&mut buf).await {
            Ok(0) => break, // EOF
            Ok(n) => n,
            Err(_) => break,
        };

        let mut lines: Vec<String> = Vec::new();
        let mut reply: Vec<u8> = Vec::new();
        let mut caps: Vec<ClientCap> = Vec::new();
        filter.feed(&buf[..n], &mut reply, &mut caps, |line| lines.push(line));

        // Send negotiation replies (IAC WONT/DONT for refused options, IAC WILL
        // for the GMCP/MSSP we accept) back to the client so it doesn't block
        // waiting for a reply. The writer only ever calls `.as_bytes()`, so
        // wrapping the raw bytes in a String is lossless.
        if !reply.is_empty() {
            let msg = unsafe { String::from_utf8_unchecked(reply) };
            if output_tx.send(msg).await.is_err() {
                break;
            }
        }

        // Forward any newly-negotiated capabilities to the Game, which owns the
        // live player/world data the out-of-band channel needs.
        for cap in caps {
            let msg = match cap {
                ClientCap::EnableGmcp => GameMessage::EnableGmcp { conn_id },
                ClientCap::RequestMssp => GameMessage::SendMssp { conn_id },
            };
            if game_tx.send(msg).await.is_err() {
                return;
            }
        }

        for input in lines {
            if game_tx
                .send(GameMessage::Input { conn_id, input })
                .await
                .is_err()
            {
                return;
            }
        }
    }
}

/// Copyover recovery task (comm.c copyover_recover). The socket fd was inherited
/// across execv and the player named `name` was playing before the reboot. We
/// re-wrap the fd as a tokio TcpStream and drive the same writer/input loop as a
/// fresh connection, but tell the Game to RECOVER (load the player straight into
/// Playing, skip the nanny). `stream` was already rebuilt from the raw fd by the
/// boot path (which set the std socket non-blocking before from_std).
pub async fn handle_recovered(
    stream: TcpStream,
    conn_id: ConnId,
    raw_fd: RawFd,
    name: String,
    host: String,
    game_tx: mpsc::Sender<GameMessage>,
) -> Result<()> {
    let (mut reader, mut writer) = stream.into_split();

    let (output_tx, mut output_rx) = mpsc::channel::<String>(256);

    game_tx
        .send(GameMessage::Recover {
            id: conn_id,
            host,
            raw_fd,
            name,
            output_tx: output_tx.clone(),
        })
        .await?;

    let write_handle = tokio::spawn(async move {
        while let Some(msg) = output_rx.recv().await {
            if writer.write_all(msg.as_bytes()).await.is_err() {
                break;
            }
            if writer.flush().await.is_err() {
                break;
            }
        }
    });

    run_input_loop(&mut reader, conn_id, &game_tx, &output_tx).await;

    let _ = game_tx.send(GameMessage::Disconnect { conn_id }).await;
    write_handle.abort();
    Ok(())
}
