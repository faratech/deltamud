// Connection edge: async socket I/O over channels, plus the per-connection
// Descriptor that lives inside GameState. Commands never touch sockets —
// they append to Descriptor::outbuf, which the Game task flushes to the
// writer task after each command/pulse (mirrors C process_input/process_output).

use crate::types::{CharId, ConnId};
use anyhow::Result;
use std::collections::BTreeMap;
use std::ffi::CStr;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::os::unix::io::{AsRawFd, RawFd};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{Semaphore, mpsc, oneshot};

/// C `LARGE_BUFSIZE` after protocol/header slack. A descriptor can never grow
/// beyond this many queued text bytes within one game pulse.
pub const DESCRIPTOR_OUTPUT_LIMIT: usize = 12_056;
pub(crate) const OUTPUT_OVERFLOW_MARKER: &str = "\r\n**OVERFLOW**\r\n";

/// C `HOST_LENGTH`. Keep the operator/player-facing descriptor host compatible
/// while retaining the complete verified hostname separately for ban matching.
const C_HOST_LENGTH: usize = 30;

/// Runtime reverse-DNS policy. The whole PTR + forward-confirmation operation
/// has one deadline, and libc resolver concurrency is bounded by the semaphore
/// supplied to `handle_client`.
#[derive(Debug, Clone, Copy)]
pub struct ReverseDnsConfig {
    pub enabled: bool,
    pub timeout: Duration,
}

impl ReverseDnsConfig {
    #[cfg(test)]
    fn disabled() -> Self {
        Self {
            enabled: false,
            timeout: Duration::from_millis(1),
        }
    }
}

/// Trusted connection identity. `peer_ip` always comes directly from the
/// accepted socket. `verified_hostname` is populated only when PTR lookup is
/// followed by an A/AAAA lookup containing that exact peer IP (FCrDNS).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PeerIdentity {
    pub peer_ip: IpAddr,
    pub verified_hostname: Option<String>,
}

impl PeerIdentity {
    pub(crate) fn numeric(peer_ip: IpAddr) -> Self {
        Self {
            peer_ip,
            verified_hostname: None,
        }
    }

    fn descriptor_host(&self) -> String {
        let mut host = self
            .verified_hostname
            .clone()
            .unwrap_or_else(|| self.peer_ip.to_string());
        crate::text::truncate_utf8_bytes(&mut host, C_HOST_LENGTH);
        host
    }
}

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

/// Hard bounds for client-supplied GMCP metadata. A malformed client can keep a
/// subnegotiation open across arbitrarily many TCP reads, so neither the parser
/// nor the Descriptor state may grow with untrusted input.
const MAX_GMCP_SUBNEGOTIATION: usize = 8 * 1024;
const MAX_GMCP_CLIENT_NAME: usize = 128;
const MAX_GMCP_CLIENT_VERSION: usize = 64;
const MAX_GMCP_PACKAGE_NAME: usize = 128;
const MAX_GMCP_PACKAGES: usize = 256;

/// Client identity and package versions learned through Core.Hello and
/// Core.Supports.{Set,Add,Remove}. Package names are normalized to lowercase
/// because GMCP package names are case-insensitive.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GmcpClientState {
    pub client_name: Option<String>,
    pub client_version: Option<String>,
    pub packages: BTreeMap<String, u32>,
}

/// Parsed GMCP state change emitted by the socket-edge parser and applied to the
/// Descriptor by the single-owner Game task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GmcpClientEvent {
    Enabled,
    Disabled,
    Hello {
        client_name: String,
        client_version: String,
    },
    SupportsSet(BTreeMap<String, u32>),
    SupportsAdd(BTreeMap<String, u32>),
    SupportsRemove(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ClientEvent {
    Gmcp(GmcpClientEvent),
    RequestMssp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GmcpNegotiationState {
    /// The connection task queued `IAC WILL GMCP` before starting the reader.
    Offered,
    Enabled,
    Refused,
}

/// Initial server-side negotiation. Recovered sockets may still have GMCP
/// enabled in the client from the old process, so reset that option before
/// offering it again. Fresh sockets need only the standards-required WILL.
fn initial_telnet_negotiation(recovered: bool) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(if recovered { 6 } else { 3 });
    if recovered {
        bytes.extend_from_slice(&[IAC, WONT, TELOPT_GMCP]);
    }
    bytes.extend_from_slice(&[IAC, WILL, TELOPT_GMCP]);
    bytes
}

fn safe_gmcp_text(value: &str, max_len: usize) -> bool {
    !value.is_empty() && value.len() <= max_len && !value.chars().any(char::is_control)
}

fn normalize_gmcp_package(value: &str) -> Option<String> {
    if value.is_empty()
        || value.len() > MAX_GMCP_PACKAGE_NAME
        || value.starts_with('.')
        || value.ends_with('.')
        || value.contains("..")
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return None;
    }
    Some(value.to_ascii_lowercase())
}

fn parse_gmcp_supports(data: &str) -> Option<BTreeMap<String, u32>> {
    let entries: Vec<String> = serde_json::from_str(data).ok()?;
    if entries.len() > MAX_GMCP_PACKAGES {
        return None;
    }

    let mut packages = BTreeMap::new();
    for entry in entries {
        let mut parts = entry.split_whitespace();
        let name = normalize_gmcp_package(parts.next()?)?;
        let version = parts.next()?.parse::<u32>().ok()?;
        if version == 0 || parts.next().is_some() {
            return None;
        }
        packages.insert(name, version);
    }
    Some(packages)
}

fn parse_gmcp_removals(data: &str) -> Option<Vec<String>> {
    let entries: Vec<String> = serde_json::from_str(data).ok()?;
    if entries.len() > MAX_GMCP_PACKAGES {
        return None;
    }

    let mut removals = Vec::with_capacity(entries.len());
    for entry in entries {
        if entry.split_whitespace().count() != 1 {
            return None;
        }
        removals.push(normalize_gmcp_package(&entry)?);
    }
    removals.sort();
    removals.dedup();
    Some(removals)
}

fn object_string_case_insensitive(
    object: &serde_json::Map<String, serde_json::Value>,
    wanted: &str,
) -> Option<String> {
    object
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(wanted))
        .and_then(|(_, value)| value.as_str())
        .map(str::to_string)
}

fn parse_gmcp_message(payload: &[u8]) -> Option<GmcpClientEvent> {
    let message = std::str::from_utf8(payload).ok()?.trim();
    let split = message.find(char::is_whitespace).unwrap_or(message.len());
    let package = &message[..split];
    let data = message[split..].trim_start();

    if package.eq_ignore_ascii_case("Core.Hello") {
        let value: serde_json::Value = serde_json::from_str(data).ok()?;
        let object = value.as_object()?;
        // Real clients exist with both the documented Client/Version spelling
        // and lowercase keys, so key matching is intentionally case-insensitive.
        let client_name = object_string_case_insensitive(object, "Client")?;
        let client_version = object_string_case_insensitive(object, "Version")?;
        if !safe_gmcp_text(&client_name, MAX_GMCP_CLIENT_NAME)
            || !safe_gmcp_text(&client_version, MAX_GMCP_CLIENT_VERSION)
        {
            return None;
        }
        return Some(GmcpClientEvent::Hello {
            client_name,
            client_version,
        });
    }
    if package.eq_ignore_ascii_case("Core.Supports.Set") {
        return Some(GmcpClientEvent::SupportsSet(parse_gmcp_supports(data)?));
    }
    if package.eq_ignore_ascii_case("Core.Supports.Add") {
        return Some(GmcpClientEvent::SupportsAdd(parse_gmcp_supports(data)?));
    }
    if package.eq_ignore_ascii_case("Core.Supports.Remove") {
        return Some(GmcpClientEvent::SupportsRemove(parse_gmcp_removals(data)?));
    }
    None
}

/// Byte-level telnet input filter. Strips IAC command sequences from a raw byte
/// stream so negotiation a client sends on connect (Mudlet's NAWS/TTYPE/GMCP
/// hello, plain `IAC DO/WILL` bursts) never corrupts the input line — notably
/// the first one (the name prompt). The C server does this in comm.c
/// (process_input's telnet scanner); we mirror it as a small state machine.
///
/// `feed()` consumes a byte slice, emits any completed input lines via the
/// `on_line` callback, pushes protocol events for the Game, and appends refusal
/// bytes (`IAC WONT/DONT <opt>`) to the caller's reply buffer for unsupported
/// options.
struct TelnetFilter {
    state: TelnetState,
    /// Accumulated printable bytes of the line in progress.
    line: Vec<u8>,
    /// Option byte of the subnegotiation currently being consumed (the byte
    /// right after IAC SB). We consume the whole payload regardless, but tracking
    /// it lets us recognize an incoming GMCP `IAC SB GMCP ... IAC SE` (e.g. the
    /// client's Core.Hello / Core.Supports.Set) without choking on it.
    subneg_opt: u8,
    /// Bounded payload buffer used only for inbound GMCP. Unknown options are
    /// still consumed without allocation.
    subneg_data: Vec<u8>,
    subneg_overflowed: bool,
    gmcp_state: GmcpNegotiationState,
    /// True while we are inside a contiguous run of line-terminator bytes
    /// (`\r`/`\n`). C `process_input` collapses ANY such run into ONE line break
    /// via `while (ISNEWL(*nl_pos)) nl_pos++;` (both CR and LF are ISNEWL), so
    /// `\r\n`, `\r\r`, `\n\n`, and `\r\n\r\n` (a double-Enter) each yield a
    /// single line, not one per char (BUG #27). We emit the line on the FIRST
    /// terminator and swallow the rest of the run WITHIN one feed() call.
    /// C's run-skip is per process_input pass (comm.c:1955-1960: the skip
    /// starts from the emitted line's nl_pos), so a STANDALONE `\r\n` in a
    /// later read is a fresh (empty) line — resetting the flag per feed()
    /// reproduces that; a genuine `\r\n` pair always arrives in one read.
    in_newline_run: bool,
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

impl TelnetFilter {
    fn new() -> Self {
        TelnetFilter {
            state: TelnetState::Data,
            line: Vec::with_capacity(256),
            subneg_opt: 0,
            subneg_data: Vec::with_capacity(256),
            subneg_overflowed: false,
            gmcp_state: GmcpNegotiationState::Offered,
            in_newline_run: false,
        }
    }

    fn push_subneg_byte(&mut self, byte: u8) {
        if self.subneg_opt != TELOPT_GMCP || self.subneg_overflowed {
            return;
        }
        if self.subneg_data.len() == MAX_GMCP_SUBNEGOTIATION {
            self.subneg_data.clear();
            self.subneg_overflowed = true;
            return;
        }
        self.subneg_data.push(byte);
    }

    fn finish_subnegotiation(&mut self, events: &mut Vec<ClientEvent>) {
        if self.subneg_opt == TELOPT_GMCP
            && self.gmcp_state == GmcpNegotiationState::Enabled
            && !self.subneg_overflowed
            && let Some(event) = parse_gmcp_message(&self.subneg_data)
        {
            events.push(ClientEvent::Gmcp(event));
        }
        self.subneg_opt = 0;
        self.subneg_data.clear();
        self.subneg_overflowed = false;
    }

    /// Feed raw bytes. Completed lines (terminated by CR or LF, with a CRLF /
    /// LFCR pair collapsed to one break — comm.c ISNEWL semantics) are passed to
    /// `on_line` as owned Strings (lossy UTF-8). Control bytes are dropped and
    /// backspace/DEL erase the last char, matching comm.c process_input.
    /// Reply bytes to send back to the client (option refusals and MSSP's
    /// client-initiated acceptance) are appended to `reply`. GMCP negotiation
    /// and parsed Core messages are pushed into `events` for the Game. GMCP's
    /// initial `IAC WILL` is queued by the connection task before this reader
    /// starts, so the accepting `IAC DO` is deliberately not echoed back.
    fn feed<F: FnMut(String)>(
        &mut self,
        data: &[u8],
        reply: &mut Vec<u8>,
        events: &mut Vec<ClientEvent>,
        mut on_line: F,
    ) {
        // C's newline-run skip is scoped to one process_input pass over the
        // socket buffer (comm.c:1863-1877); it never spans two reads. Reset
        // per feed so a standalone CR/LF in a later read is a fresh empty line.
        self.in_newline_run = false;
        for &b in data {
            match self.state {
                TelnetState::Data => match b {
                    IAC => {
                        // IAC (0xFF) is not a newline, so it ends any newline run.
                        self.in_newline_run = false;
                        self.state = TelnetState::Iac;
                    }
                    // ISNEWL: BOTH CR and LF terminate a line (comm.c
                    // process_input, BUG #27). C then skips the ENTIRE contiguous
                    // run of newline bytes (`while (ISNEWL(*nl_pos)) nl_pos++;`),
                    // so any run of \r/\n — `\r\n`, `\r\r`, `\n\n`, `\r\n\r\n` —
                    // collapses to ONE line break. Emit the line on the first
                    // terminator and swallow the rest of the run.
                    b'\r' | b'\n' => {
                        if self.in_newline_run {
                            // Mid-run: swallow this terminator, emit nothing.
                            continue;
                        }
                        let s = String::from_utf8_lossy(&self.line).into_owned();
                        self.line.clear();
                        self.in_newline_run = true;
                        on_line(s);
                    }
                    // Backspace (0x08) and DEL (0x7f): erase the last buffered
                    // char (comm.c process_input `if (*ptr == '\b')`). DEL is
                    // folded in here because raw terminals send it for the
                    // Backspace key.
                    0x08 | 0x7f => {
                        self.in_newline_run = false;
                        self.line.pop();
                    }
                    // Keep only printable ASCII (C: `isascii(*ptr) && isprint`).
                    // 0x20..=0x7e is the printable range; everything else (NUL,
                    // other control bytes, high-bit/8-bit bytes) is dropped — this
                    // also covers the old bare-CR `\r\0` and stray control noise.
                    0x20..=0x7e => {
                        self.in_newline_run = false;
                        // C comm.c:1749-1752 drops the connection when the raw
                        // buffer overflows MAX_RAW_INPUT_LENGTH (2k); here the
                        // excess bytes are dropped instead, which keeps the
                        // per-connection buffer bounded the same way.
                        if self.line.len() < crate::MAX_RAW_INPUT_LENGTH {
                            self.line.push(b);
                        }
                    }
                    _ => {
                        // Non-printable control / high-bit byte: drop it. A
                        // non-newline byte ends any newline run.
                        self.in_newline_run = false;
                    }
                },
                TelnetState::Iac => match b {
                    IAC => {
                        // Escaped 0xFF -> literal data byte (same cap as the
                        // printable branch: an IAC-escape flood must not grow
                        // the line buffer without bound).
                        if self.line.len() < crate::MAX_RAW_INPUT_LENGTH {
                            self.line.push(IAC);
                        }
                        self.state = TelnetState::Data;
                    }
                    WILL | WONT | DO | DONT => self.state = TelnetState::Negotiate(b),
                    SB => self.state = TelnetState::SubnegOpt,
                    // Any other 2-byte IAC command (NOP, GA, AYT, etc.): consume.
                    _ => self.state = TelnetState::Data,
                },
                TelnetState::Negotiate(verb) => {
                    // GMCP is server-initiated: the connection task already sent
                    // WILL, so DO is an acknowledgement and MUST NOT provoke a
                    // second WILL (which can cause negotiation loops). We still
                    // honor a later client-initiated DO as a state change. MSSP
                    // retains its existing request/response behavior. All other
                    // options are refused so clients do not wait indefinitely.
                    match (verb, b) {
                        (DO, TELOPT_GMCP) => {
                            if self.gmcp_state != GmcpNegotiationState::Enabled {
                                self.gmcp_state = GmcpNegotiationState::Enabled;
                                events.push(ClientEvent::Gmcp(GmcpClientEvent::Enabled));
                            }
                        }
                        (DO, TELOPT_MSSP) => {
                            reply.extend_from_slice(&[IAC, WILL, TELOPT_MSSP]);
                            events.push(ClientEvent::RequestMssp);
                        }
                        (DONT, TELOPT_GMCP) => {
                            if self.gmcp_state == GmcpNegotiationState::Enabled {
                                events.push(ClientEvent::Gmcp(GmcpClientEvent::Disabled));
                            }
                            self.gmcp_state = GmcpNegotiationState::Refused;
                        }
                        // A client should answer our GMCP WILL with DO/DONT, not
                        // WILL/WONT. Ignore that wrong-direction pair rather than
                        // creating a negotiation loop. MSSP disable remains a
                        // no-op because its payload is one-shot.
                        (WILL, TELOPT_GMCP) | (WILL, TELOPT_MSSP) => {}
                        (DONT, TELOPT_MSSP) | (WONT, TELOPT_GMCP) | (WONT, TELOPT_MSSP) => {}
                        (DO, _) => reply.extend_from_slice(&[IAC, WONT, b]),
                        (WILL, _) => reply.extend_from_slice(&[IAC, DONT, b]),
                        _ => {} // WONT / DONT for unsupported options: nothing to send.
                    }
                    self.state = TelnetState::Data;
                }
                TelnetState::SubnegOpt => {
                    // The byte right after IAC SB is the option (GMCP, MSSP, ...).
                    self.subneg_opt = b;
                    self.subneg_data.clear();
                    self.subneg_overflowed = false;
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
                    } else {
                        self.push_subneg_byte(b);
                    }
                }
                TelnetState::SubnegIac => {
                    if b == SE {
                        self.finish_subnegotiation(events);
                        self.state = TelnetState::Data;
                    } else if b == IAC {
                        // Escaped IAC inside the payload. It will make ordinary
                        // GMCP JSON invalid UTF-8, but retaining it here preserves
                        // correct telnet framing and keeps the parser stateful.
                        self.push_subneg_byte(IAC);
                        self.state = TelnetState::Subneg;
                    } else {
                        // A stray IAC command inside SB is invalid. Consume it and
                        // continue scanning for the real IAC SE terminator.
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
    /// CON_QANSI: the boot-time colour question (comm.c sends `ANSI`; the
    /// answer sets PRF_COLOR_1|2 before the name prompt) (#198).
    QAnsi,
    GetName,
    GetOldPassword,
    ConfirmName, // "Did I get that right (Y/N)?"
    GetNewPassword,
    ConfirmPassword,
    GetNewbie,
    GetSex,
    GetRace,
    GetDeity,
    GetClass,
    GetHometown,
    RollStats,
    /// CON_RMOTD: "PRESS RETURN" after the MOTD / a menu sub-page (#198).
    ReadMotd,
    /// CON_MENU: the DeltaMUD main menu, options 0-8 (#198).
    Menu,
    /// CON_EXDESC: menu option 2, the multi-line description editor.
    ExDesc,
    /// CON_CHPWD_GETOLD / GETNEW / VRFY: menu option 7 password change.
    ChPwdGetOld,
    ChPwdGetNew,
    ChPwdVerify,
    /// CON_DELCNF1 / DELCNF2: menu option 8 self-delete confirmation.
    DelCnf1,
    DelCnf2,
    Playing,
    Close,
}

/// A nested input context (string editor, OLC editor). Tier-0 stub; the
/// stack lets a Playing descriptor push an editor without a giant enum.
#[derive(Debug, Clone)]
pub enum InputContext {
    StringEdit { buffer: String, max_len: usize },
}

/// One queued gameplay line plus the C `aliased` marker. Complex aliases push
/// their expanded commands back onto the descriptor queue with `aliased=true`
/// so the heartbeat dispatches them in normal order without recursively
/// expanding aliases again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedInput {
    pub line: String,
    pub aliased: bool,
}

impl QueuedInput {
    pub fn raw(line: String) -> Self {
        QueuedInput {
            line,
            aliased: false,
        }
    }

    pub fn aliased(line: String) -> Self {
        QueuedInput {
            line,
            aliased: true,
        }
    }
}

pub struct Descriptor {
    pub id: ConnId,
    /// Operator-facing C-compatible host: the verified hostname (capped to C's
    /// HOST_LENGTH) when available, otherwise the canonical socket peer IP.
    pub host: String,
    /// Canonical address captured directly from the accepted socket. Live
    /// descriptors always retain this even when reverse DNS succeeds, so an IP
    /// ban can never be bypassed by a PTR record.
    pub peer_ip: String,
    /// Complete forward-confirmed PTR hostname used for hostname ban matching.
    /// This is separate from `host` because the C display/persistence field is
    /// only HOST_LENGTH bytes.
    pub verified_hostname: Option<String>,
    /// Raw OS file descriptor of the underlying TCP socket. Captured before the
    /// stream is split (connection.rs handle_client) so do_copyover can write a
    /// final flush, clear FD_CLOEXEC, and inherit the live socket across execv —
    /// this is the linchpin of C's seamless copyover (act.wizard.c do_copyover).
    pub raw_fd: RawFd,
    pub state: ConState,
    /// Stack of nested input contexts; empty == normal command/menu input.
    pub editors: Vec<InputContext>,
    /// True only while the client has accepted the server's GMCP offer. When
    /// set, the Game pushes Char.Vitals + Room.Info out-of-band after state
    /// changes so Mudlet's gauges and mapper stay live.
    pub gmcp: bool,
    /// Client identity and supported-package versions advertised over GMCP.
    /// This state is cleared whenever GMCP is disabled and starts empty on
    /// every fresh connection/copyover recovery.
    pub gmcp_client: GmcpClientState,
    pub character: Option<CharId>,
    pub original: Option<CharId>, // for `switch`
    /// Output accumulated this pulse; flushed by the Game task.
    pub outbuf: String,
    /// At least one append was dropped after `outbuf` reached its hard limit.
    pub output_overflowed: bool,
    /// True when a fresh prompt should be sent after flushing.
    pub need_prompt: bool,
    /// C comm.c d->idle_tics: 15-second heartbeat ticks spent sitting at a
    /// login prompt (name/password). Two ticks disconnect (issue #192).
    pub idle_tics: u8,
    /// Command-lag counter (C `d->wait`): the heartbeat decrements it each pulse
    /// and only pulls the next queued command when it reaches <= 0. WAIT_STATE
    /// sets it from combat skills/casting to impose command lag.
    pub wait: i32,
    /// Queued input lines awaiting the wait gate (C `d->input`), with the
    /// per-line `aliased` bit used to prevent recursive alias expansion.
    pub input_queue: std::collections::VecDeque<QueuedInput>,
    // Scratch during login / char creation.
    pub temp_name: Option<String>,
    pub temp_password: Option<String>,
    /// Exact durable hash authenticated at the start of a menu password
    /// change. The final update uses it as a compare-and-swap guard so a
    /// concurrent administrator/security reset cannot be overwritten.
    pub password_change_expected_hash: Option<String>,
    /// C d->bad_pws: consecutive password failures THIS connection; at
    /// max_bad_pws (2) the connection is dropped (#194).
    pub bad_pws: u32,
    /// The CON_QANSI answer, applied to the character at creation/login
    /// (C sets PRF_COLOR_1|2 on d->character right away) (#198).
    pub wants_colour: Option<bool>,
    /// Menu option 2's finished description, applied to the player at
    /// enter-game (C edits d->character->player.description directly) (#198).
    pub temp_description: Option<String>,
    /// C d->last_input: the previous completed input line, for the '!' and
    /// '^a^b' history substitution (comm.c:1861-1868) (#224).
    pub last_input: String,
    /// Set by a nanny arm that printed its own inline prompt (C messages such
    /// as 'Illegal password.\r\nPassword: ' carry the prompt); the normal
    /// post-nanny prompt is skipped once, mirroring C's SEND_TO_Q-and-return.
    pub suppress_prompt: bool,
    /// Hash of the password that opened this session, cached at login so the
    /// synchronous `unlock` handler (act.other.c do_lockout) can verify a
    /// `unlock <password>` against the real account password. C keeps
    /// GET_PASSWD(ch) on the character itself; the Rust Character never carries
    /// it, so the session hash lives on the descriptor. Empty after a copyover
    /// recovery (which skips the nanny), where C would still have it (#313).
    pub password_hash: Option<String>,
}

impl Descriptor {
    pub fn new(id: ConnId, host: String) -> Self {
        Self::with_fd(id, host, -1)
    }

    pub fn with_fd(id: ConnId, host: String, raw_fd: RawFd) -> Self {
        let parsed_ip = host.parse::<IpAddr>().ok();
        let peer_ip = parsed_ip.map(|ip| ip.to_string()).unwrap_or_default();
        let verified_hostname = parsed_ip.is_none().then(|| host.to_ascii_lowercase());
        Self::with_identity(id, host, peer_ip, verified_hostname, raw_fd)
    }

    pub fn with_identity(
        id: ConnId,
        host: String,
        peer_ip: String,
        verified_hostname: Option<String>,
        raw_fd: RawFd,
    ) -> Self {
        Descriptor {
            id,
            host,
            peer_ip,
            verified_hostname,
            raw_fd,
            state: ConState::QAnsi,
            editors: Vec::new(),
            gmcp: false,
            gmcp_client: GmcpClientState::default(),
            character: None,
            original: None,
            outbuf: String::new(),
            output_overflowed: false,
            idle_tics: 0,
            need_prompt: true,
            wait: 1,
            input_queue: std::collections::VecDeque::new(),
            temp_name: None,
            temp_password: None,
            password_change_expected_hash: None,
            bad_pws: 0,
            wants_colour: None,
            temp_description: None,
            last_input: String::new(),
            suppress_prompt: false,
            password_hash: None,
        }
    }

    /// Apply one parsed client-side GMCP event. Returns true only when this
    /// event transitions the descriptor from disabled to enabled; the Game uses
    /// that edge to send one initial snapshot to an already-playing client.
    pub fn apply_gmcp_event(&mut self, event: GmcpClientEvent) -> bool {
        let was_enabled = self.gmcp;
        match event {
            GmcpClientEvent::Enabled => {
                if !self.gmcp {
                    self.gmcp_client = GmcpClientState::default();
                }
                self.gmcp = true;
            }
            GmcpClientEvent::Disabled => {
                self.gmcp = false;
                self.gmcp_client = GmcpClientState::default();
            }
            GmcpClientEvent::Hello {
                client_name,
                client_version,
            } if self.gmcp
                && safe_gmcp_text(&client_name, MAX_GMCP_CLIENT_NAME)
                && safe_gmcp_text(&client_version, MAX_GMCP_CLIENT_VERSION) =>
            {
                self.gmcp_client.client_name = Some(client_name);
                self.gmcp_client.client_version = Some(client_version);
            }
            GmcpClientEvent::SupportsSet(packages) if self.gmcp => {
                self.gmcp_client.packages.clear();
                for (name, version) in packages {
                    if self.gmcp_client.packages.len() == MAX_GMCP_PACKAGES {
                        break;
                    }
                    if version > 0
                        && let Some(name) = normalize_gmcp_package(&name)
                    {
                        self.gmcp_client.packages.insert(name, version);
                    }
                }
            }
            GmcpClientEvent::SupportsAdd(packages) if self.gmcp => {
                for (name, version) in packages {
                    let Some(name) = normalize_gmcp_package(&name) else {
                        continue;
                    };
                    if version > 0
                        && (self.gmcp_client.packages.contains_key(&name)
                            || self.gmcp_client.packages.len() < MAX_GMCP_PACKAGES)
                    {
                        self.gmcp_client.packages.insert(name, version);
                    }
                }
            }
            GmcpClientEvent::SupportsRemove(packages) if self.gmcp => {
                for name in packages {
                    if let Some(name) = normalize_gmcp_package(&name) {
                        self.gmcp_client.packages.remove(&name);
                    }
                }
            }
            // Core data arriving before DO GMCP is not negotiated protocol data.
            _ => {}
        }
        !was_enabled && self.gmcp
    }

    pub fn write(&mut self, msg: &str) {
        if msg.is_empty() || self.output_overflowed {
            return;
        }
        if self.outbuf.len().saturating_add(msg.len()) <= DESCRIPTOR_OUTPUT_LIMIT {
            self.outbuf.push_str(msg);
        } else {
            self.output_overflowed = true;
        }
    }

    /// Take one bounded pulse of output and reset the overflow state. The
    /// marker itself is kept inside the same byte ceiling.
    pub fn take_output_status(&mut self) -> (String, bool) {
        let overflowed = self.output_overflowed;
        let mut output = std::mem::take(&mut self.outbuf);
        if overflowed {
            crate::text::truncate_utf8_bytes(
                &mut output,
                DESCRIPTOR_OUTPUT_LIMIT.saturating_sub(OUTPUT_OVERFLOW_MARKER.len()),
            );
            output.push_str(OUTPUT_OVERFLOW_MARKER);
        }
        self.output_overflowed = false;
        (output, overflowed)
    }

    pub fn take_output(&mut self) -> String {
        self.take_output_status().0
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

/// One ordered writer-queue item. A barrier has empty bytes and an ack sender;
/// the writer acknowledges it only after every preceding frame was written and
/// flushed to the socket.
#[derive(Debug)]
pub struct OutputFrame {
    pub bytes: Vec<u8>,
    pub ack: Option<oneshot::Sender<bool>>,
    /// Flush, half-close the socket, acknowledge, and terminate the writer.
    pub close_after: bool,
}

impl OutputFrame {
    pub fn data(bytes: Vec<u8>) -> Self {
        Self {
            bytes,
            ack: None,
            close_after: false,
        }
    }

    pub fn shutdown_barrier(ack: oneshot::Sender<bool>) -> Self {
        Self {
            bytes: Vec::new(),
            ack: Some(ack),
            close_after: true,
        }
    }

    /// Ordered non-closing flush used immediately before copyover hands the
    /// socket fd to synchronous exec preparation.
    pub fn flush_barrier(ack: oneshot::Sender<bool>) -> Self {
        Self {
            bytes: Vec::new(),
            ack: Some(ack),
            close_after: false,
        }
    }
}

async fn run_output_writer<W>(
    mut writer: W,
    mut output_rx: mpsc::Receiver<OutputFrame>,
) -> WriterEnd
where
    W: AsyncWrite + Unpin,
{
    while let Some(frame) = output_rx.recv().await {
        let mut ok = (frame.bytes.is_empty() || writer.write_all(&frame.bytes).await.is_ok())
            && writer.flush().await.is_ok();
        if ok && frame.close_after {
            ok = writer.shutdown().await.is_ok();
        }
        if let Some(ack) = frame.ack {
            let _ = ack.send(ok);
        }
        if !ok {
            return WriterEnd::IoFailure;
        }
        if frame.close_after {
            return WriterEnd::ShutdownBarrier;
        }
    }
    WriterEnd::OutputChannelClosed
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WriterEnd {
    IoFailure,
    ShutdownBarrier,
    OutputChannelClosed,
}

// Messages from connection tasks to the single Game task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemShutdownResult {
    Committed,
    Refused,
}

#[derive(Debug)]
pub enum GameMessage {
    /// Process-local stop request emitted only by the main signal owner. Socket
    /// tasks cannot construct this from client input.
    SystemShutdown {
        result_tx: tokio::sync::oneshot::Sender<SystemShutdownResult>,
    },
    NewConnection {
        id: ConnId,
        host: String,
        peer_ip: String,
        verified_hostname: Option<String>,
        raw_fd: RawFd,
        output_tx: mpsc::Sender<OutputFrame>,
    },
    /// Re-attach a player whose live socket was inherited across a copyover
    /// execv (comm.c copyover_recover). The Game loads the named player straight
    /// into Playing state, skipping the login nanny.
    Recover {
        id: ConnId,
        host: String,
        peer_ip: String,
        verified_hostname: Option<String>,
        raw_fd: RawFd,
        name: String,
        output_tx: mpsc::Sender<OutputFrame>,
    },
    Input {
        conn_id: ConnId,
        input: String,
    },
    /// Negotiated GMCP state or parsed Core metadata. The socket edge validates
    /// and bounds the payload; the Game applies it to the owned Descriptor.
    Gmcp {
        conn_id: ConnId,
        event: GmcpClientEvent,
    },
    /// Client requested the MSSP status block (`IAC DO MSSP`). The Game builds it
    /// (it needs the live player count / uptime) and sends it once.
    SendMssp {
        conn_id: ConnId,
    },
    Disconnect {
        conn_id: ConnId,
    },
    #[cfg(test)]
    PanicForTest {
        conn_id: ConnId,
    },
}

impl GameMessage {
    pub fn conn_id(&self) -> Option<ConnId> {
        match self {
            GameMessage::SystemShutdown { .. } => None,
            GameMessage::NewConnection { id, .. } | GameMessage::Recover { id, .. } => Some(*id),
            GameMessage::Input { conn_id, .. }
            | GameMessage::Gmcp { conn_id, .. }
            | GameMessage::SendMssp { conn_id }
            | GameMessage::Disconnect { conn_id } => Some(*conn_id),
            #[cfg(test)]
            GameMessage::PanicForTest { conn_id } => Some(*conn_id),
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            GameMessage::SystemShutdown { .. } => "system-shutdown",
            GameMessage::NewConnection { .. } => "new-connection",
            GameMessage::Recover { .. } => "recover",
            GameMessage::Input { .. } => "input",
            GameMessage::Gmcp { .. } => "gmcp",
            GameMessage::SendMssp { .. } => "send-mssp",
            GameMessage::Disconnect { .. } => "disconnect",
            #[cfg(test)]
            GameMessage::PanicForTest { .. } => "panic-test",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConnectionEnd {
    Reader,
    Writer(WriterEnd),
}

/// Drive both socket halves in the owning connection task. Keeping both loops
/// in this `select!` means the losing half is dropped before this function
/// returns; there is no nested writer task whose `JoinHandle` can be detached.
/// Writer errors and shutdown barriers therefore terminate a blocked reader in
/// exactly the same way reader EOF terminates a blocked writer.
async fn supervise_connection<R, W>(
    reader: &mut R,
    writer: W,
    conn_id: ConnId,
    game_tx: &mpsc::Sender<GameMessage>,
    output_tx: &mpsc::Sender<OutputFrame>,
    output_rx: mpsc::Receiver<OutputFrame>,
) -> ConnectionEnd
where
    R: AsyncReadExt + Unpin,
    W: AsyncWrite + Unpin,
{
    let ended = tokio::select! {
        biased;
        // A queued shutdown barrier must win a simultaneous read EOF so the
        // final output can be acknowledged instead of being cancelled.
        writer_end = run_output_writer(writer, output_rx) => ConnectionEnd::Writer(writer_end),
        _ = run_input_loop(reader, conn_id, game_tx, output_tx) => ConnectionEnd::Reader,
    };

    let disconnect = GameMessage::Disconnect { conn_id };
    match ended {
        // During graceful shutdown the Game has already closed the socket and
        // may no longer be draining its input queue. Do not let this redundant
        // cleanup notice extend the global shutdown bound.
        ConnectionEnd::Writer(WriterEnd::ShutdownBarrier | WriterEnd::OutputChannelClosed) => {
            let _ = game_tx.try_send(disconnect);
        }
        ConnectionEnd::Reader | ConnectionEnd::Writer(WriterEnd::IoFailure) => {
            let _ = game_tx.send(disconnect).await;
        }
    }
    ended
}

fn normalize_ptr_hostname(raw: &str) -> Option<String> {
    let hostname = raw.trim_end_matches('.').to_ascii_lowercase();
    if hostname.is_empty() || hostname.len() > 253 || !hostname.is_ascii() {
        return None;
    }
    let valid = hostname.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    });
    valid.then_some(hostname)
}

fn gai_error(code: libc::c_int) -> String {
    // SAFETY: gai_strerror returns either a process-lifetime C string or null
    // for the supplied getnameinfo status code.
    let ptr = unsafe { libc::gai_strerror(code) };
    if ptr.is_null() {
        return format!("resolver error {code}");
    }
    // SAFETY: non-null gai_strerror results are NUL-terminated C strings.
    unsafe { CStr::from_ptr(ptr) }
        .to_string_lossy()
        .into_owned()
}

fn getnameinfo_hostname(
    address: *const libc::sockaddr,
    address_len: libc::socklen_t,
) -> std::result::Result<String, String> {
    let mut hostname = [0 as libc::c_char; 1_025];
    // SAFETY: `address` points at a fully initialized sockaddr of
    // `address_len`; `hostname` is a writable buffer whose exact length is
    // supplied. No service buffer is requested.
    let status = unsafe {
        libc::getnameinfo(
            address,
            address_len,
            hostname.as_mut_ptr(),
            hostname.len() as libc::socklen_t,
            std::ptr::null_mut(),
            0,
            libc::NI_NAMEREQD,
        )
    };
    if status != 0 {
        return Err(gai_error(status));
    }
    // SAFETY: successful getnameinfo NUL-terminates the host buffer.
    let raw = unsafe { CStr::from_ptr(hostname.as_ptr()) }
        .to_str()
        .map_err(|_| "resolver returned a non-UTF-8 hostname".to_string())?;
    normalize_ptr_hostname(raw).ok_or_else(|| "resolver returned an invalid hostname".to_string())
}

fn reverse_lookup_system(peer: SocketAddr) -> std::result::Result<String, String> {
    match peer {
        SocketAddr::V4(peer) => {
            let address = libc::sockaddr_in {
                sin_family: libc::AF_INET as libc::sa_family_t,
                sin_port: 0,
                // s_addr is stored in network byte order; from_ne_bytes makes
                // the in-memory bytes exactly the IPv4 octets on every target.
                sin_addr: libc::in_addr {
                    s_addr: u32::from_ne_bytes(peer.ip().octets()),
                },
                sin_zero: [0; 8],
            };
            getnameinfo_hostname(
                (&raw const address).cast::<libc::sockaddr>(),
                std::mem::size_of_val(&address) as libc::socklen_t,
            )
        }
        SocketAddr::V6(peer) => {
            let address = libc::sockaddr_in6 {
                sin6_family: libc::AF_INET6 as libc::sa_family_t,
                sin6_port: 0,
                sin6_flowinfo: peer.flowinfo(),
                sin6_addr: libc::in6_addr {
                    s6_addr: peer.ip().octets(),
                },
                sin6_scope_id: peer.scope_id(),
            };
            getnameinfo_hostname(
                (&raw const address).cast::<libc::sockaddr>(),
                std::mem::size_of_val(&address) as libc::socklen_t,
            )
        }
    }
}

fn forward_addresses_confirm_peer(
    peer_ip: IpAddr,
    addresses: impl IntoIterator<Item = SocketAddr>,
) -> bool {
    addresses.into_iter().any(|address| address.ip() == peer_ip)
}

fn resolve_and_forward_confirm_system(
    peer: SocketAddr,
) -> std::result::Result<Option<String>, String> {
    let hostname = reverse_lookup_system(peer)?;
    let addresses = (hostname.as_str(), 0)
        .to_socket_addrs()
        .map_err(|error| format!("forward lookup failed: {error}"))?;
    Ok(forward_addresses_confirm_peer(peer.ip(), addresses).then_some(hostname))
}

/// Perform reverse DNS entirely at the connection edge, then forward-confirm
/// the PTR result against the socket's original address. Any disabled lookup,
/// queue closure, resolver error, unconfirmed PTR, or deadline expiry falls
/// back to the canonical peer IP.
pub(crate) async fn resolve_peer_identity(
    peer: SocketAddr,
    config: ReverseDnsConfig,
    resolver_slots: Arc<Semaphore>,
) -> PeerIdentity {
    let numeric = PeerIdentity::numeric(peer.ip());
    if !config.enabled {
        return numeric;
    }

    let lookup = async {
        let permit = resolver_slots.acquire_owned().await.ok()?;
        let resolved = tokio::task::spawn_blocking(move || {
            // Moving the permit into the blocking closure is intentional: if
            // the async caller times out, uncancellable libc reverse OR forward
            // calls still occupy one bounded resolver slot until both return.
            let result = resolve_and_forward_confirm_system(peer);
            (permit, result)
        })
        .await
        .ok()?;
        let (_permit, resolved) = resolved;
        resolved.ok().flatten()
    };

    match tokio::time::timeout(config.timeout, lookup).await {
        Ok(Some(hostname)) => PeerIdentity {
            peer_ip: peer.ip(),
            verified_hostname: Some(hostname),
        },
        Ok(None) => {
            log::debug!(
                "reverse DNS for {} failed or was not forward-confirmed",
                peer.ip()
            );
            numeric
        }
        Err(_) => {
            log::debug!(
                "reverse DNS for {} exceeded {}ms; using canonical IP",
                peer.ip(),
                config.timeout.as_millis()
            );
            numeric
        }
    }
}

/// The actual BAN_ALL socket gate, shared by the immediate numeric-IP check and
/// the post-FCrDNS check. Returns true after writing the C rejection and closing
/// the stream.
pub(crate) async fn reject_ban_all<W>(stream: &mut W, identity: &PeerIdentity) -> bool
where
    W: AsyncWrite + Unpin,
{
    let ban_type = crate::ban::isbanned_connection(
        &identity.peer_ip.to_string(),
        identity.verified_hostname.as_deref(),
    );
    if ban_type != crate::ban::BanType::All {
        return false;
    }

    match identity.verified_hostname.as_deref() {
        Some(hostname) => log::warn!(
            "Rejected banned peer {} (verified hostname {})",
            identity.peer_ip,
            hostname
        ),
        None => log::warn!("Rejected banned peer {}", identity.peer_ip),
    }
    let _ = stream.write_all(b"Your site is BANNED!\r\n").await;
    let _ = stream.shutdown().await;
    true
}

/// Per-connection task: split the stream, register with the Game, then
/// supervise the input and output loops together.
pub async fn handle_client(
    mut stream: TcpStream,
    addr: SocketAddr,
    conn_id: ConnId,
    game_tx: mpsc::Sender<GameMessage>,
    reverse_dns: ReverseDnsConfig,
    resolver_slots: Arc<Semaphore>,
) -> Result<()> {
    let identity = resolve_peer_identity(addr, reverse_dns, resolver_slots).await;
    if reject_ban_all(&mut stream, &identity).await {
        return Ok(());
    }

    // Capture the raw fd BEFORE into_split() consumes the stream. do_copyover
    // needs this to inherit the live socket across execv (FD_CLOEXEC dance).
    let fd = stream.as_raw_fd();
    let host = identity.descriptor_host();

    let (mut reader, writer) = stream.into_split();

    let (output_tx, output_rx) = mpsc::channel::<OutputFrame>(256);

    // Queue the standards-required server offer before registration can enqueue
    // banners/prompts, so GMCP negotiation is the first socket output.
    output_tx
        .send(OutputFrame::data(initial_telnet_negotiation(false)))
        .await?;

    game_tx
        .send(GameMessage::NewConnection {
            id: conn_id,
            host,
            peer_ip: identity.peer_ip.to_string(),
            verified_hostname: identity.verified_hostname,
            raw_fd: fd,
            output_tx: output_tx.clone(),
        })
        .await?;

    supervise_connection(
        &mut reader,
        writer,
        conn_id,
        &game_tx,
        &output_tx,
        output_rx,
    )
    .await;
    Ok(())
}

/// Pump raw bytes through the telnet filter, forwarding completed lines to the
/// Game and pushing any negotiation refusals back through the output channel.
/// Returns on EOF or read error. Shared by fresh and recovered connections.
async fn run_input_loop<R: AsyncReadExt + Unpin>(
    reader: &mut R,
    conn_id: ConnId,
    game_tx: &mpsc::Sender<GameMessage>,
    output_tx: &mpsc::Sender<OutputFrame>,
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
        let mut events: Vec<ClientEvent> = Vec::new();
        filter.feed(&buf[..n], &mut reply, &mut events, |line| lines.push(line));

        // Send negotiation replies (IAC WONT/DONT for refused options and the
        // existing client-initiated MSSP acceptance) so the client does not
        // block. GMCP's initial WILL was already queued before registration.
        // The channel carries raw bytes because IAC is not valid UTF-8.
        if !reply.is_empty() {
            if output_tx
                .send(OutputFrame::data(std::mem::take(&mut reply)))
                .await
                .is_err()
            {
                break;
            }
        }

        // Forward negotiated protocol state to the Game, which owns the live
        // Descriptor and all world data used by out-of-band snapshots.
        for event in events {
            let msg = match event {
                ClientEvent::Gmcp(event) => GameMessage::Gmcp { conn_id, event },
                ClientEvent::RequestMssp => GameMessage::SendMssp { conn_id },
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
    let peer_ip = stream
        .peer_addr()
        .map(|peer| peer.ip().to_string())
        .unwrap_or_default();
    // The hostname in a durable copyover snapshot came from the already-live,
    // previously FCrDNS-confirmed descriptor. Numeric hosts remain numeric.
    let verified_hostname = host
        .parse::<IpAddr>()
        .is_err()
        .then(|| host.to_ascii_lowercase());
    let (mut reader, writer) = stream.into_split();

    let (output_tx, output_rx) = mpsc::channel::<OutputFrame>(256);

    // The inherited client may remember GMCP as enabled from the old process.
    // Reset then re-offer it while the new Descriptor starts from empty state.
    output_tx
        .send(OutputFrame::data(initial_telnet_negotiation(true)))
        .await?;

    game_tx
        .send(GameMessage::Recover {
            id: conn_id,
            host,
            peer_ip,
            verified_hostname,
            raw_fd,
            name,
            output_tx: output_tx.clone(),
        })
        .await?;

    supervise_connection(
        &mut reader,
        writer,
        conn_id,
        &game_tx,
        &output_tx,
        output_rx,
    )
    .await;
    Ok(())
}

#[cfg(test)]
mod reverse_dns_tests {
    use super::*;

    #[test]
    fn ptr_hostname_validation_is_log_safe_and_canonical() {
        assert_eq!(
            normalize_ptr_hostname("Dialup.Example.TEST.").as_deref(),
            Some("dialup.example.test")
        );
        assert_eq!(normalize_ptr_hostname("bad\nname.example"), None);
        assert_eq!(normalize_ptr_hostname("empty..label.example"), None);
        assert_eq!(
            normalize_ptr_hostname(&format!("{}.test", "x".repeat(64))),
            None
        );
    }

    #[test]
    fn forward_confirmation_requires_the_original_peer_address() {
        let peer: IpAddr = "192.0.2.10".parse().unwrap();
        let matching = vec!["192.0.2.10:0".parse().unwrap()];
        let rebound = vec!["198.51.100.20:0".parse().unwrap()];
        assert!(forward_addresses_confirm_peer(peer, matching));
        assert!(!forward_addresses_confirm_peer(peer, rebound));
    }

    #[tokio::test]
    async fn disabled_and_timed_out_resolution_retain_canonical_peer_ip() {
        let peer: SocketAddr = "192.0.2.25:4000".parse().unwrap();
        let disabled = resolve_peer_identity(
            peer,
            ReverseDnsConfig::disabled(),
            Arc::new(Semaphore::new(1)),
        )
        .await;
        assert_eq!(disabled, PeerIdentity::numeric(peer.ip()));

        // No resolver slot can become available; the whole-operation deadline
        // must fire and return the same canonical identity.
        let started = tokio::time::Instant::now();
        let timed_out = resolve_peer_identity(
            peer,
            ReverseDnsConfig {
                enabled: true,
                timeout: Duration::from_millis(10),
            },
            Arc::new(Semaphore::new(0)),
        )
        .await;
        assert_eq!(timed_out, PeerIdentity::numeric(peer.ip()));
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}

#[cfg(test)]
mod telnet_tests {
    use super::*;

    fn drive_filter(chunks: &[&[u8]]) -> (Vec<String>, Vec<u8>, Vec<ClientEvent>) {
        let mut filter = TelnetFilter::new();
        let mut lines = Vec::new();
        let mut reply = Vec::new();
        let mut events = Vec::new();
        for chunk in chunks {
            filter.feed(chunk, &mut reply, &mut events, |line| lines.push(line));
        }
        (lines, reply, events)
    }

    /// Drive the filter over `chunks` (each chunk simulates one TCP read) and
    /// return the completed input lines.
    fn lines_of(chunks: &[&[u8]]) -> Vec<String> {
        drive_filter(chunks).0
    }

    fn gmcp_frame(message: &str) -> Vec<u8> {
        let mut frame = vec![IAC, SB, TELOPT_GMCP];
        frame.extend_from_slice(message.as_bytes());
        frame.extend_from_slice(&[IAC, SE]);
        frame
    }

    #[test]
    fn server_initiates_gmcp_and_copyover_resets_before_reoffering() {
        assert_eq!(
            initial_telnet_negotiation(false),
            vec![IAC, WILL, TELOPT_GMCP]
        );
        assert_eq!(
            initial_telnet_negotiation(true),
            vec![IAC, WONT, TELOPT_GMCP, IAC, WILL, TELOPT_GMCP]
        );
    }

    #[test]
    fn fragmented_do_acknowledges_offer_without_negotiation_loop() {
        let (_, reply, events) =
            drive_filter(&[&[IAC], &[DO], &[TELOPT_GMCP], &[IAC, DO, TELOPT_GMCP]]);
        assert!(reply.is_empty(), "DO must not provoke a duplicate WILL");
        assert_eq!(events, vec![ClientEvent::Gmcp(GmcpClientEvent::Enabled)]);
    }

    #[test]
    fn fragmented_gmcp_subnegotiation_parses_hello() {
        let mut bytes = vec![IAC, DO, TELOPT_GMCP];
        bytes.extend(gmcp_frame(
            r#"Core.Hello {"client":"Mudlet","version":"4.18.5"}"#,
        ));
        // A bare CR keeps this test focused on byte-fragmented telnet/GMCP;
        // CR and LF arriving in separate reads intentionally form two lines in
        // the inherited CircleMUD newline semantics tested below.
        bytes.extend_from_slice(b"look\r");

        let mut filter = TelnetFilter::new();
        let mut lines = Vec::new();
        let mut reply = Vec::new();
        let mut events = Vec::new();
        for byte in &bytes {
            filter.feed(
                std::slice::from_ref(byte),
                &mut reply,
                &mut events,
                |line| lines.push(line),
            );
        }

        assert!(reply.is_empty());
        assert_eq!(lines, vec!["look"]);
        assert_eq!(
            events,
            vec![
                ClientEvent::Gmcp(GmcpClientEvent::Enabled),
                ClientEvent::Gmcp(GmcpClientEvent::Hello {
                    client_name: "Mudlet".to_string(),
                    client_version: "4.18.5".to_string(),
                }),
            ]
        );
    }

    #[test]
    fn supports_set_add_remove_update_bounded_descriptor_state() {
        let set = gmcp_frame(r#"Core.Supports.Set ["Char 1","Room 1"]"#);
        let add = gmcp_frame(r#"core.supports.add ["CHAR 2","Comm.Channel 1"]"#);
        let remove = gmcp_frame(r#"Core.Supports.Remove ["ROOM"]"#);
        let chunks: [&[u8]; 4] = [&[IAC, DO, TELOPT_GMCP], &set, &add, &remove];
        let (_, reply, events) = drive_filter(&chunks);
        assert!(reply.is_empty());

        let mut descriptor = Descriptor::new(ConnId(900), "gmcp.test".to_string());
        for event in events {
            let ClientEvent::Gmcp(event) = event else {
                panic!("unexpected non-GMCP event");
            };
            descriptor.apply_gmcp_event(event);
        }
        assert!(descriptor.gmcp);
        assert_eq!(descriptor.gmcp_client.packages.get("char"), Some(&2));
        assert_eq!(
            descriptor.gmcp_client.packages.get("comm.channel"),
            Some(&1)
        );
        assert!(!descriptor.gmcp_client.packages.contains_key("room"));

        let oversized_add = (0..MAX_GMCP_PACKAGES + 32)
            .map(|index| (format!("Package{index}"), 1))
            .collect();
        descriptor.apply_gmcp_event(GmcpClientEvent::SupportsAdd(oversized_add));
        assert_eq!(descriptor.gmcp_client.packages.len(), MAX_GMCP_PACKAGES);
    }

    #[test]
    fn dont_disables_gmcp_and_clears_client_capabilities() {
        let set = gmcp_frame(r#"Core.Supports.Set ["Char 1"]"#);
        let chunks: [&[u8]; 3] = [&[IAC, DO, TELOPT_GMCP], &set, &[IAC, DONT, TELOPT_GMCP]];
        let (_, reply, events) = drive_filter(&chunks);
        assert!(reply.is_empty());

        let mut descriptor = Descriptor::new(ConnId(901), "gmcp.test".to_string());
        for event in events {
            if let ClientEvent::Gmcp(event) = event {
                descriptor.apply_gmcp_event(event);
            }
        }
        assert!(!descriptor.gmcp);
        assert_eq!(descriptor.gmcp_client, GmcpClientState::default());
    }

    #[test]
    fn oversized_gmcp_is_discarded_and_parser_recovers() {
        let mut filter = TelnetFilter::new();
        let mut reply = Vec::new();
        let mut events = Vec::new();
        let mut lines = Vec::new();
        filter.feed(
            &[IAC, DO, TELOPT_GMCP, IAC, SB, TELOPT_GMCP],
            &mut reply,
            &mut events,
            |line| lines.push(line),
        );
        let oversized = vec![b'x'; MAX_GMCP_SUBNEGOTIATION + 1];
        filter.feed(&oversized, &mut reply, &mut events, |line| lines.push(line));
        assert!(filter.subneg_overflowed);
        assert!(filter.subneg_data.is_empty());

        filter.feed(&[IAC, SE], &mut reply, &mut events, |line| lines.push(line));
        let valid = gmcp_frame(r#"Core.Supports.Set ["Char 1"]"#);
        filter.feed(&valid, &mut reply, &mut events, |line| lines.push(line));

        assert!(!filter.subneg_overflowed);
        assert_eq!(
            events,
            vec![
                ClientEvent::Gmcp(GmcpClientEvent::Enabled),
                ClientEvent::Gmcp(GmcpClientEvent::SupportsSet(BTreeMap::from([(
                    "char".to_string(),
                    1,
                )]))),
            ]
        );
    }

    #[test]
    fn unsupported_options_are_refused_and_mssp_behavior_is_preserved() {
        const UNKNOWN_CLIENT_OPTION: u8 = 42;
        const UNKNOWN_SERVER_OPTION: u8 = 43;
        let (_, reply, events) = drive_filter(&[
            &[IAC],
            &[DO, UNKNOWN_CLIENT_OPTION],
            &[IAC, WILL],
            &[UNKNOWN_SERVER_OPTION],
            &[IAC, DO, TELOPT_MSSP],
        ]);
        assert_eq!(
            reply,
            vec![
                IAC,
                WONT,
                UNKNOWN_CLIENT_OPTION,
                IAC,
                DONT,
                UNKNOWN_SERVER_OPTION,
                IAC,
                WILL,
                TELOPT_MSSP,
            ]
        );
        assert_eq!(events, vec![ClientEvent::RequestMssp]);
    }

    #[test]
    fn crlf_collapses_to_one_line() {
        assert_eq!(lines_of(&[b"hello\r\nworld\r\n"]), vec!["hello", "world"]);
    }

    #[test]
    fn newline_run_collapses_like_isnewl_skip() {
        // C process_input's `while (ISNEWL(*nl_pos)) nl_pos++;` collapses ANY
        // contiguous \r/\n run into ONE break — a double-Enter is one blank line.
        assert_eq!(lines_of(&[b"\r\n\r\n"]), vec![""]);
        assert_eq!(lines_of(&[b"\n\n"]), vec![""]);
        assert_eq!(lines_of(&[b"\r\r"]), vec![""]);
        // The blank "line" between `a` and `b` is part of the single contiguous
        // \r\n\r\n run, so C's `while (ISNEWL)` skip collapses it away entirely:
        // `a\r\n\r\nb` yields ["a", "b"], NOT ["a", "", "b"].
        assert_eq!(lines_of(&[b"a\r\n\r\nb\r\n"]), vec!["a", "b"]);
    }

    #[test]
    fn standalone_newline_across_reads_is_an_empty_line() {
        // C scopes the newline-run skip to one process_input pass
        // (comm.c:1955-1960): the run from a previous read does NOT extend,
        // so a standalone \n in the next read is a fresh EMPTY line — the
        // input the parity battery and RETURN-style prompts rely on.
        assert_eq!(
            lines_of(&[b"hi\r", b"\nthere\r\n"]),
            vec!["hi", "", "there"]
        );
        // A genuine \r\n pair within one read still collapses to one break.
        assert_eq!(lines_of(&[b"hi\r\nthere\r\n"]), vec!["hi", "there"]);
    }

    #[test]
    fn bare_cr_terminates_line() {
        assert_eq!(lines_of(&[b"foo\rbar\r\n"]), vec!["foo", "bar"]);
    }

    #[test]
    fn backspace_and_del_erase_last_char() {
        assert_eq!(lines_of(&[b"abc\x08\r\n"]), vec!["ab"]);
        assert_eq!(lines_of(&[b"abc\x7f\r\n"]), vec!["ab"]);
    }

    #[test]
    fn control_and_high_bytes_are_dropped() {
        // NUL, a stray control byte, and a non-IAC high-bit byte are stripped
        // (C isascii && isprint), leaving only the printable text. (0xFF is not
        // used here — it is IAC, handled by the telnet state machine, not data.)
        assert_eq!(lines_of(&[b"a\x00b\x01c\x80d\r\n"]), vec!["abcd"]);
    }

    #[test]
    fn iac_ends_newline_run() {
        // An IAC sequence between newline runs is not a line terminator, so the
        // following \r\n starts a fresh (empty) line — matching C treating 0xFF
        // as a non-ISNEWL byte.
        let seq: &[u8] = &[b'x', b'\r', b'\n', IAC, WILL, TELOPT_GMCP, b'\r', b'\n'];
        assert_eq!(lines_of(&[seq]), vec!["x", ""]);
    }

    #[test]
    fn descriptor_output_is_bounded_and_resets_after_flush() {
        let mut descriptor = Descriptor::new(ConnId(1), "example.test".to_string());
        descriptor.write(&"x".repeat(DESCRIPTOR_OUTPUT_LIMIT));
        let (exact, overflowed) = descriptor.take_output_status();
        assert_eq!(exact.len(), DESCRIPTOR_OUTPUT_LIMIT);
        assert!(!overflowed);

        descriptor.write(&"x".repeat(DESCRIPTOR_OUTPUT_LIMIT));
        descriptor.write("x");

        let (output, overflowed) = descriptor.take_output_status();
        assert!(output.len() <= DESCRIPTOR_OUTPUT_LIMIT);
        assert!(output.ends_with(OUTPUT_OVERFLOW_MARKER));
        assert!(overflowed);

        descriptor.write("next pulse");
        assert_eq!(descriptor.take_output(), "next pulse");
    }

    #[test]
    fn descriptor_overflow_truncation_preserves_utf8_boundaries() {
        for (index, character) in ["é", "€", "🦀"].into_iter().enumerate() {
            let mut descriptor =
                Descriptor::new(ConnId(2 + index as u64), "example.test".to_string());
            let prefix = "x".repeat(DESCRIPTOR_OUTPUT_LIMIT - 1);
            descriptor.write(&prefix);
            descriptor.write(character);

            let output = descriptor.take_output();
            assert!(output.len() <= DESCRIPTOR_OUTPUT_LIMIT);
            assert!(output.ends_with(OUTPUT_OVERFLOW_MARKER));
            assert!(std::str::from_utf8(output.as_bytes()).is_ok());
        }
    }

    #[test]
    fn one_huge_write_and_many_small_writes_emit_only_one_marker() {
        let mut huge = Descriptor::new(ConnId(10), "example.test".to_string());
        huge.write(&"x".repeat(DESCRIPTOR_OUTPUT_LIMIT * 8));
        huge.write("ignored after overflow");
        let output = huge.take_output();
        assert_eq!(output, OUTPUT_OVERFLOW_MARKER);

        let mut small = Descriptor::new(ConnId(11), "example.test".to_string());
        for _ in 0..DESCRIPTOR_OUTPUT_LIMIT {
            small.write("x");
        }
        small.write("x");
        small.write("x");
        let output = small.take_output();
        assert_eq!(output.matches(OUTPUT_OVERFLOW_MARKER).count(), 1);
        assert!(output.len() <= DESCRIPTOR_OUTPUT_LIMIT);
    }
}

#[cfg(test)]
mod output_writer_tests {
    use super::*;
    use std::io;
    use std::pin::Pin;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use std::task::{Context, Poll};
    use std::time::Duration;
    use tokio::io::{AsyncRead, AsyncReadExt, ReadBuf};
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn shutdown_barrier_acknowledges_only_after_flush_and_socket_shutdown() {
        let (mut client, server) = tokio::io::duplex(64);
        let (tx, rx) = mpsc::channel(4);
        let writer = tokio::spawn(run_output_writer(server, rx));
        tx.send(OutputFrame::data(b"final notice".to_vec()))
            .await
            .unwrap();
        let (ack_tx, ack_rx) = oneshot::channel();
        tx.send(OutputFrame::shutdown_barrier(ack_tx))
            .await
            .unwrap();

        assert!(ack_rx.await.unwrap());
        writer.await.unwrap();
        let mut received = Vec::new();
        client.read_to_end(&mut received).await.unwrap();
        assert_eq!(received, b"final notice");
    }

    struct PartialThenError {
        wrote_once: bool,
    }

    impl AsyncWrite for PartialThenError {
        fn poll_write(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            bytes: &[u8],
        ) -> Poll<io::Result<usize>> {
            if self.wrote_once {
                Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "injected partial-write failure",
                )))
            } else {
                self.wrote_once = true;
                Poll::Ready(Ok(bytes.len().min(2)))
            }
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    struct PendingReader;

    impl AsyncRead for PendingReader {
        fn poll_read(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            Poll::Pending
        }
    }

    struct EofReader;

    impl AsyncRead for EofReader {
        fn poll_read(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    struct DropAwareWriter {
        dropped: Arc<AtomicBool>,
    }

    impl Drop for DropAwareWriter {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::SeqCst);
        }
    }

    impl AsyncWrite for DropAwareWriter {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _bytes: &[u8],
        ) -> Poll<io::Result<usize>> {
            Poll::Pending
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Pending
        }

        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Pending
        }
    }

    struct ImmediateWriter {
        shutdown: Arc<AtomicBool>,
    }

    impl AsyncWrite for ImmediateWriter {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            bytes: &[u8],
        ) -> Poll<io::Result<usize>> {
            Poll::Ready(Ok(bytes.len()))
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            self.shutdown.store(true, Ordering::SeqCst);
            Poll::Ready(Ok(()))
        }
    }

    async fn tcp_pair() -> (TcpStream, TcpStream, SocketAddr) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (client, accepted) = tokio::join!(TcpStream::connect(address), listener.accept());
        let (server, peer) = accepted.unwrap();
        (client.unwrap(), server, peer)
    }

    async fn close_registered_writer(output_tx: mpsc::Sender<OutputFrame>) {
        let (ack_tx, ack_rx) = oneshot::channel();
        output_tx
            .send(OutputFrame::shutdown_barrier(ack_tx))
            .await
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(1), ack_rx)
                .await
                .expect("writer did not process shutdown barrier")
                .expect("writer dropped shutdown acknowledgement")
        );
    }

    #[tokio::test]
    async fn partial_write_error_drops_the_later_barrier_without_false_ack() {
        let (tx, rx) = mpsc::channel(4);
        let writer = tokio::spawn(run_output_writer(
            PartialThenError { wrote_once: false },
            rx,
        ));
        tx.send(OutputFrame::data(b"cannot finish".to_vec()))
            .await
            .unwrap();
        let (ack_tx, ack_rx) = oneshot::channel();
        tx.send(OutputFrame::shutdown_barrier(ack_tx))
            .await
            .unwrap();

        writer.await.unwrap();
        assert!(ack_rx.await.is_err());
    }

    #[tokio::test]
    async fn non_reading_peer_cannot_produce_a_premature_acknowledgement() {
        let (_client, server) = tokio::io::duplex(1);
        let (tx, rx) = mpsc::channel(4);
        let writer = tokio::spawn(run_output_writer(server, rx));
        tx.send(OutputFrame::data(vec![b'x'; 1024])).await.unwrap();
        let (ack_tx, ack_rx) = oneshot::channel();
        tx.send(OutputFrame::shutdown_barrier(ack_tx))
            .await
            .unwrap();

        assert!(
            tokio::time::timeout(Duration::from_millis(25), ack_rx)
                .await
                .is_err()
        );
        writer.abort();
        let _ = writer.await;
    }

    #[tokio::test]
    async fn writer_failure_terminates_a_blocked_reader_and_notifies_game() {
        let conn_id = ConnId(41);
        let mut reader = PendingReader;
        let (game_tx, mut game_rx) = mpsc::channel(4);
        let (output_tx, output_rx) = mpsc::channel(4);
        output_tx
            .send(OutputFrame::data(b"writer fails".to_vec()))
            .await
            .unwrap();

        let ended = tokio::time::timeout(
            Duration::from_secs(1),
            supervise_connection(
                &mut reader,
                PartialThenError { wrote_once: false },
                conn_id,
                &game_tx,
                &output_tx,
                output_rx,
            ),
        )
        .await
        .expect("writer failure left the connection reader running");

        assert_eq!(ended, ConnectionEnd::Writer(WriterEnd::IoFailure));
        assert!(matches!(
            game_rx.recv().await,
            Some(GameMessage::Disconnect { conn_id: id }) if id == conn_id
        ));
    }

    #[tokio::test]
    async fn reader_eof_drops_the_writer_before_supervisor_returns() {
        let conn_id = ConnId(42);
        let mut reader = EofReader;
        let dropped = Arc::new(AtomicBool::new(false));
        let writer = DropAwareWriter {
            dropped: Arc::clone(&dropped),
        };
        let (game_tx, mut game_rx) = mpsc::channel(4);
        let (output_tx, output_rx) = mpsc::channel(4);

        let ended = supervise_connection(
            &mut reader,
            writer,
            conn_id,
            &game_tx,
            &output_tx,
            output_rx,
        )
        .await;

        assert_eq!(ended, ConnectionEnd::Reader);
        assert!(
            dropped.load(Ordering::SeqCst),
            "the writer outlived its owning connection supervisor"
        );
        assert!(matches!(
            game_rx.recv().await,
            Some(GameMessage::Disconnect { conn_id: id }) if id == conn_id
        ));
    }

    #[tokio::test]
    async fn shutdown_barrier_wins_a_simultaneous_reader_eof() {
        let conn_id = ConnId(43);
        let mut reader = EofReader;
        let shutdown = Arc::new(AtomicBool::new(false));
        let writer = ImmediateWriter {
            shutdown: Arc::clone(&shutdown),
        };
        let (game_tx, mut game_rx) = mpsc::channel(4);
        let (output_tx, output_rx) = mpsc::channel(4);
        output_tx
            .send(OutputFrame::data(b"final output".to_vec()))
            .await
            .unwrap();
        let (ack_tx, ack_rx) = oneshot::channel();
        output_tx
            .send(OutputFrame::shutdown_barrier(ack_tx))
            .await
            .unwrap();

        let ended = supervise_connection(
            &mut reader,
            writer,
            conn_id,
            &game_tx,
            &output_tx,
            output_rx,
        )
        .await;

        assert_eq!(ended, ConnectionEnd::Writer(WriterEnd::ShutdownBarrier));
        assert!(ack_rx.await.unwrap());
        assert!(shutdown.load(Ordering::SeqCst));
        assert!(matches!(
            game_rx.recv().await,
            Some(GameMessage::Disconnect { conn_id: id }) if id == conn_id
        ));
    }

    #[tokio::test]
    async fn fresh_connection_shutdown_ends_the_outer_task() {
        let (mut client, server, peer) = tcp_pair().await;
        let conn_id = ConnId(44);
        let (game_tx, mut game_rx) = mpsc::channel(4);
        let task = tokio::spawn(handle_client(
            server,
            peer,
            conn_id,
            game_tx,
            ReverseDnsConfig::disabled(),
            Arc::new(Semaphore::new(1)),
        ));

        let output_tx = match game_rx.recv().await {
            Some(GameMessage::NewConnection { id, output_tx, .. }) if id == conn_id => output_tx,
            other => panic!("unexpected registration: {other:?}"),
        };
        let mut negotiation = [0; 3];
        tokio::time::timeout(Duration::from_secs(1), client.read_exact(&mut negotiation))
            .await
            .expect("fresh connection never offered GMCP")
            .unwrap();
        assert_eq!(negotiation, [IAC, WILL, TELOPT_GMCP]);
        close_registered_writer(output_tx).await;

        assert!(matches!(
            game_rx.recv().await,
            Some(GameMessage::Disconnect { conn_id: id }) if id == conn_id
        ));
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("fresh connection task outlived its writer")
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn recovered_connection_shutdown_ends_the_outer_task() {
        let (mut client, server, _peer) = tcp_pair().await;
        let raw_fd = server.as_raw_fd();
        let conn_id = ConnId(45);
        let (game_tx, mut game_rx) = mpsc::channel(4);
        let task = tokio::spawn(handle_recovered(
            server,
            conn_id,
            raw_fd,
            "Recovered".to_string(),
            "127.0.0.1".to_string(),
            game_tx,
        ));

        let output_tx = match game_rx.recv().await {
            Some(GameMessage::Recover { id, output_tx, .. }) if id == conn_id => output_tx,
            other => panic!("unexpected registration: {other:?}"),
        };
        let mut negotiation = [0; 6];
        tokio::time::timeout(Duration::from_secs(1), client.read_exact(&mut negotiation))
            .await
            .expect("recovered connection never reset/re-offered GMCP")
            .unwrap();
        assert_eq!(
            negotiation,
            [IAC, WONT, TELOPT_GMCP, IAC, WILL, TELOPT_GMCP]
        );
        close_registered_writer(output_tx).await;

        assert!(matches!(
            game_rx.recv().await,
            Some(GameMessage::Disconnect { conn_id: id }) if id == conn_id
        ));
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("recovered connection task outlived its writer")
            .unwrap()
            .unwrap();
    }
}

// ---------------------------------------------------------------------------
// colour.c port: COLOURLIST / is_colour / proc_color (#221). C's table is
// indexed by is_colour() code values; `&`-codes expand when the viewer's
// colour level >= C_NRM, are stripped below it, and are randomly scrambled
// (mode -1) for mortals standing in magic fog.
// ---------------------------------------------------------------------------

const COLOURLIST: [&str; 30] = [
    "\x1B[0;0m",  // 0 CNRM
    "\x1B[0;31m", // 1 CRED
    "\x1B[0;32m", // 2 CGRN
    "\x1B[0;33m", // 3 CYEL
    "\x1B[0;34m", // 4 CBLU
    "\x1B[0;35m", // 5 CMAG
    "\x1B[0;36m", // 6 CCYN
    "\x1B[0;37m", // 7 CWHT
    "\x1B[1;31m", // 8 BRED
    "\x1B[1;32m", // 9 BGRN
    "\x1B[1;33m", // 10 BYEL
    "\x1B[1;34m", // 11 BBLU
    "\x1B[1;35m", // 12 BMAG
    "\x1B[1;36m", // 13 BCYN
    "\x1B[1;37m", // 14 BWHT
    "\x1B[41m",   // 15 BKRED
    "\x1B[42m",   // 16 BKGRN
    "\x1B[43m",   // 17 BKYEL
    "\x1B[44m",   // 18 BKBLU
    "\x1B[45m",   // 19 BKMAG
    "\x1B[46m",   // 20 BKCYN
    "\x1B[47m",   // 21 BKWHT
    "&",          // 22 CAMP
    "\\",         // 23 CSLH
    "\x1B[40m",   // 24 BKBLK
    "\x1B[0;30m", // 25 CBLK (&k)
    "\x1B[5m",    // 26 CFSH (&f)
    "\x1B[7m",    // 27 CRVS (&v)
    "\x1B[4m",    // 28 CUDL (&u)
    "\x1B[1;30m", // 29 BBLK (&K)
];

const MAX_COLORS: i32 = 28;

/// C color.c is_colour: the `&x` code char -> COLOURLIST index, or -1.
fn is_colour(code: char) -> i32 {
    match code {
        'k' => 25,
        'r' => 1,
        'g' => 2,
        'y' => 3,
        'b' => 4,
        'm' => 5,
        'c' => 6,
        'w' => 7,
        'K' => 29,
        'R' => 8,
        'G' => 9,
        'Y' => 10,
        'B' => 11,
        'M' => 12,
        'C' => 13,
        'W' => 14,
        // Backgrounds: '0' (black) is BKBLK at index 24; '1'..'7' are
        // BKRED..BKWHT at 15..21.
        '0' => 24,
        '1' | '2' | '3' | '4' | '5' | '6' | '7' => 14 + (code as u8 - b'0') as i32,
        '&' => 22,
        '\\' => 23,
        'n' => 0,
        'f' => 26,
        'v' => 27,
        'u' => 28,
        _ => -1,
    }
}

/// C color.c proc_color. `colour`: >0 renders codes, 0 strips them, -1
/// scrambles every code to a random 1..=14 colour (magic fog). `rng` supplies
/// the scramble draws (C number(1,14)); pass the game RNG.
pub fn proc_color<R: FnMut(i32) -> i32>(text: &str, colour: i32, mut rand: R) -> String {
    if text.is_empty() {
        return String::new();
    }
    let bytes: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len() + 16);
    let mut j = 0usize;
    let mut lastcol = 0i32;
    let mut lastcol2 = 0i32;
    let is_num = |c: char| c.is_ascii_digit();
    while j < bytes.len() {
        let c;
        if j + 3 < bytes.len()
            && bytes[j] == '\\'
            && bytes[j + 1] == 'c'
            && is_num(bytes[j + 2])
            && is_num(bytes[j + 3])
        {
            c = (bytes[j + 2] as u8 - b'0') as i32 * 10 + (bytes[j + 3] as u8 - b'0') as i32;
            j += 4;
        } else if j + 1 < bytes.len()
            && bytes[j] == '&'
            && (is_colour(bytes[j + 1]) != -1 || bytes[j + 1].to_ascii_lowercase() == 'l')
        {
            if bytes[j + 1].to_ascii_lowercase() == 'l' {
                c = if colour != -1 { lastcol2 } else { rand(14) };
            } else {
                c = if colour != -1 {
                    is_colour(bytes[j + 1])
                } else {
                    rand(14)
                };
            }
            lastcol2 = lastcol;
            lastcol = c;
            j += 2;
        } else {
            out.push(bytes[j]);
            j += 1;
            continue;
        }
        let c = if c > MAX_COLORS + 1 { 0 } else { c };
        let slot = c.clamp(0, COLOURLIST.len() as i32 - 1) as usize;
        let expansion = COLOURLIST[slot];
        // C: emit only when colour is on OR the expansion is empty (CNUL).
        if colour != 0 || expansion.len() == 1 {
            out.push_str(expansion);
        }
    }
    out
}
