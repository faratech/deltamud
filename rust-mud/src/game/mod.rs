// Game: the async shell around the synchronous GameState. It owns the world,
// drains the input channel, runs commands/nanny to completion against
// &mut GameState, drives the heartbeat, and flushes each descriptor's output
// buffer to its writer task. This is the only place async meets the world.

mod db_bridge;
mod deferred;
mod heartbeat;
mod nanny;
mod presentation;
mod teardown;

use crate::DatabaseInterface;
use crate::character::Abilities;
use crate::combat;
use crate::connection::{ConState, Descriptor, GameMessage, OutputFrame, QueuedInput};
use crate::flags::{PRF_HOLYLIGHT, PRF_NOHASSLE};
use crate::interpreter::run_authenticated_command;
use crate::metrics::Metrics;
use crate::state::{
    GameState, OfflineOpAuthority, PLAYER_INSPECTION_DENIED, ProcessDisposition, ShutdownRequest,
};
use crate::types::*;
use anyhow::Result;
use futures_util::FutureExt;
use log::{error, info, warn};
use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::os::unix::io::RawFd;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::{Duration, interval};

// Telnet ECHO negotiation (RFC 857). The server WILL-ECHO before a password
// prompt so the client suppresses its own local echo (cleartext password no
// longer appears on the user's screen), and WONT-ECHO when leaving the password
// state so normal local echo resumes. These three-byte control sequences must
// reach the socket verbatim; they are NOT routed through the outbuf/render_color
// String path (render_color iterates `.chars()`, which would mangle the lone
// 0xFF IAC byte). Instead they go straight down the per-conn output channel,
// exactly like connection.rs's negotiation-refusal path, which wraps raw telnet
// bytes. The output channel carries Vec<u8>: telnet frames are not valid
// UTF-8, and the writer writes them verbatim.
const IAC_WILL_ECHO: [u8; 3] = [0xFF, 0xFB, 0x01]; // IAC WILL ECHO
const IAC_WONT_ECHO: [u8; 3] = [0xFF, 0xFC, 0x01]; // IAC WONT ECHO

// Telnet framing for out-of-band subnegotiations (GMCP/MSSP). A subneg is
// `IAC SB <opt> <payload> IAC SE`. These bytes, like the ECHO negotiation
// above, must reach the socket verbatim and so go down the raw-bytes channel,
// never through render_color (whose `.chars()` pass would mangle the lone 0xFF).
const IAC: u8 = 0xFF;
const SB: u8 = 0xFA; // Subnegotiation begin
const SE: u8 = 0xF0; // Subnegotiation end
const TELOPT_GMCP: u8 = 201; // Generic Mud Communication Protocol
const TELOPT_MSSP: u8 = 70; // Mud Server Status Protocol

// MSSP control bytes (Mud Server Status Protocol): each datum is
// `MSSP_VAR <name> MSSP_VAL <value>` inside the IAC SB MSSP ... IAC SE frame.
const MSSP_VAR: u8 = 1;
const MSSP_VAL: u8 = 2;

const PLR_SITEOK: i64 = 1 << 7;
const PRF_ROOMFLAGS: i64 = 1 << 21;

fn player_authority_state(character: &crate::character::Character) -> crate::PlayerAuthorityState {
    crate::PlayerAuthorityState {
        level: character.player.level,
        trust: character.trust,
        exp: character.points.exp,
        godcmds1: character.godcmds1,
        godcmds2: character.godcmds2,
        godcmds3: character.godcmds3,
        godcmds4: character.godcmds4,
    }
}

fn persisted_player_trust(character: &crate::character::Character) -> Option<i32> {
    (0..=i32::from(LVL_IMPL))
        .contains(&character.trust)
        .then_some(character.trust)
}

fn apply_player_authority_state(
    character: &mut crate::character::Character,
    authority: crate::PlayerAuthorityState,
) {
    character.player.level = authority.level;
    character.trust = authority.trust;
    character.points.exp = authority.exp;
    character.godcmds1 = authority.godcmds1;
    character.godcmds2 = authority.godcmds2;
    character.godcmds3 = authority.godcmds3;
    character.godcmds4 = authority.godcmds4;
    character.invis_level = character.invis_level.min(authority.trust.max(0));
    if authority.trust < i32::from(LVL_IMMORT) {
        character.prf_flags &= !(PRF_NOHASSLE | PRF_HOLYLIGHT | PRF_ROOMFLAGS);
    }
}

fn least_privileged_authority(
    first: crate::PlayerAuthorityState,
    second: crate::PlayerAuthorityState,
) -> crate::PlayerAuthorityState {
    let privilege_key = |state: crate::PlayerAuthorityState| {
        (
            state.trust,
            state.level,
            state.godcmds1.count_ones()
                + state.godcmds2.count_ones()
                + state.godcmds3.count_ones()
                + state.godcmds4.count_ones(),
        )
    };
    if privilege_key(first) <= privilege_key(second) {
        first
    } else {
        second
    }
}

fn lockout_unlock_is_current(
    state: &GameState,
    character: CharId,
    principal: CharId,
    descriptor: ConnId,
    idnum: i64,
    name: &str,
    expected_hash: &str,
) -> bool {
    let authority = state.principal_authority(character);
    let Some(live) = state.get_char(character) else {
        return false;
    };
    let session_hash = state
        .descriptors
        .get(&descriptor)
        .filter(|session| {
            session.state == ConState::Playing
                && session.character == Some(character)
                && session.original.is_none()
        })
        .and_then(|session| session.password_hash.as_deref());
    let effective_hash = live.pending_password_hash.as_deref().or(session_hash);
    authority.is_some_and(|authority| {
        authority.is_authenticated_player()
            && authority.principal == principal
            && principal == character
    }) && !live.is_npc
        && live.desc == Some(descriptor)
        && live.idnum == idnum
        && live.get_name() == name
        && live.prf2_flags & crate::flags::PRF2_LOCKOUT != 0
        && effective_hash == Some(expected_hash)
}

fn authority_update_request_is_current(
    state: &GameState,
    request: &crate::state::AuthorityUpdateRequest,
) -> bool {
    if !state.authenticated_command_request_is_current(
        request.authorization,
        i32::from(LVL_IMMORT),
        1,
        crate::gcmd::GCMD_ADVANCE,
    ) {
        return false;
    }
    let requester = state.principal_authority(request.authorization.requester_body);
    let target = state.principal_authority(request.victim);
    let live_is_exact = state.get_char(request.victim).is_some_and(|character| {
        !character.is_npc
            && character.idnum == request.idnum
            && character.get_name() == request.name
            && player_authority_state(character) == request.expected
            && state
                .players_by_name
                .get(&request.name.to_lowercase())
                .copied()
                == Some(request.victim)
    });
    let target_is_exact_player = target.is_some_and(|principal| {
        principal.principal_is_player && principal.principal == request.victim
    });
    let hierarchy_is_current = requester.zip(target).is_some_and(|(requester, target)| {
        requester.authority > target.authority && requester.authority >= request.replacement.trust
    });
    let canonical_grants =
        crate::gcmd::canonical_advance_grants(request.replacement.level, LVL_IMMORT, LVL_IMPL);
    let replacement_is_canonical = (1..=LVL_IMPL).contains(&request.replacement.level)
        && request.replacement.trust == i32::from(request.replacement.level)
        && request.replacement.exp
            == crate::limits::exp_to_level(i32::from(request.replacement.level) - 1)
        && canonical_grants
            == (
                request.replacement.godcmds1,
                request.replacement.godcmds2,
                request.replacement.godcmds3,
                request.replacement.godcmds4,
            );
    target_is_exact_player
        && live_is_exact
        && hierarchy_is_current
        && replacement_is_canonical
        && !state.authority_quarantine.contains(&request.idnum)
}

fn password_update_target_is_current(
    state: &GameState,
    request: &crate::state::PasswordUpdateRequest,
) -> Option<Option<CharId>> {
    let target_key = request.name.to_lowercase();
    if let Some(victim) = state.players_by_name.get(&target_key).copied() {
        let exact_principal = state.principal_authority(victim).is_some_and(|authority| {
            authority.principal_is_player && authority.principal == victim
        });
        let exact_character = victim == request.victim
            && state.get_char(victim).is_some_and(|character| {
                !character.is_npc
                    && character.idnum == request.idnum
                    && character.get_name().eq_ignore_ascii_case(&request.name)
                    && character.trust < i32::from(LVL_GRGOD)
            });
        return (exact_principal && exact_character).then_some(Some(victim));
    }

    // An offline replay extracts its temporary Character before this queue is
    // drained. Any still-live body or same-id player means the offline index is
    // no longer a sufficient identity predicate.
    if state.char_exists(request.victim)
        || state.char_ids().into_iter().any(|candidate| {
            state
                .get_char(candidate)
                .is_some_and(|character| character.idnum == request.idnum)
        })
    {
        return None;
    }
    state
        .player_table
        .iter()
        .any(|player| {
            player.idnum == request.idnum
                && player.name.eq_ignore_ascii_case(&request.name)
                && player.trust < i32::from(LVL_GRGOD)
        })
        .then_some(None)
}

fn password_update_request_is_current(
    state: &GameState,
    request: &crate::state::PasswordUpdateRequest,
) -> Option<Option<CharId>> {
    state
        .authenticated_command_request_is_current(
            request.authorization,
            i32::from(LVL_IMPL),
            1,
            crate::gcmd::GCMD_SET,
        )
        .then(|| password_update_target_is_current(state, request))
        .flatten()
}

fn player_rename_request_is_current(
    state: &GameState,
    request: &crate::state::PlayerRenameRequest,
) -> Option<String> {
    if !state.authenticated_command_request_is_current(
        request.authorization,
        i32::from(LVL_IMMORT),
        2,
        crate::gcmd::GCMD2_IMP,
    ) {
        return None;
    }
    let requester = state.principal_authority(request.authorization.requester_body)?;
    let target = state.principal_authority(request.victim)?;
    let victim = state.get_char(request.victim)?;
    let old_key = request.old_name.to_lowercase();
    let new_key = request.new_name.to_lowercase();
    let target_is_exact = target.principal_is_player
        && target.principal == request.victim
        && !victim.is_npc
        && victim.idnum == request.idnum
        && victim.get_name().eq_ignore_ascii_case(&request.old_name)
        && state.players_by_name.get(&old_key).copied() == Some(request.victim);
    let hierarchy_is_current = requester.authority > target.authority;
    let live_collision = state
        .players_by_name
        .get(&new_key)
        .is_some_and(|owner| *owner != request.victim);
    let index_collision = state.player_table.iter().any(|player| {
        player.name.eq_ignore_ascii_case(&request.new_name) && player.idnum != request.idnum
    });
    if !target_is_exact
        || !hierarchy_is_current
        || live_collision
        || index_collision
        || request.old_name.eq_ignore_ascii_case(&request.new_name)
    {
        return None;
    }
    state
        .get_char(request.authorization.requester_principal)
        .map(|principal| principal.get_name().to_string())
}
// ---- C config.c:256-295: the login/menu strings, verbatim (#198) ----
pub const ANSI_QUESTION: &str = "\u{1b}[0;31;1mRED\u{1b}[31;0m \u{1b}[0;34;1mBLUE\u{1b}[34;0m \u{1b}[0;32;1mGREEN\u{1b}[32;0m\r\nIs the above text shown in color? ";

pub const MENU: &str = "\r\n\
&GWelcome to the DeltaMUD Menu&n\r\n\
&B------------------------------&n\r\n\
&R[&n&C0&n&R]&n Exit from DeltaMUD.\r\n\
&R[&n&C1&n&R]&n Enter the game.\r\n\
&R[&n&C2&n&R]&n Enter description.\r\n\
&R[&n&C3&n&R]&n Read the background story.\r\n\
&R[&n&C4&n&R]&n Read the latest news.\r\n\
&R[&n&C5&n&R]&n Read the game policy.\r\n\
&R[&n&C6&n&R]&n See who is online.\r\n\
&R[&n&C7&n&R]&n Change password.\r\n\
&R[&n&C8&n&R]&n Delete this character.\r\n\
&B------------------------------&n\r\n\r\n   Make your choice: ";

pub const ASK_NAME: &str = "\r\nPlease enter a name&R:&n ";

pub const WELC_MESSG: &str = "\r\n\
Welcome to the ever changing world of Deltania..may your life here\r\n\
be full of adventure and intrigue...\r\n\
\r\n\r\n";

pub const START_MESSG: &str = "\r\n\
This is your new DeltaMUD character!  You can now earn &Ygold&n,\r\n\
gain &Cexperience&n, find &Rweapons&n and &Mequipment&n, and much more.\r\n\
\r\nThe first thing you should do is read the Newbie Guide. You do that\r\n\
by typing 'read guide' (without the quotes, of course)\r\n\
\r\n\r\n";

const NEWBIE_STAT_EXPLANATION: &str = "\r\nHere is a brief explanation of each ability:\r\n\
[&YStr&n] - Strength determines how hard you hit your opponents in a fight.\r\n\r\n\
[&YInt&n] - Intelligence determines how well you hit your opponents in a fight,\r\n\
        and also the amount of magic points for spells (clerics and mages).\r\n\r\n\
[&YWis&n] - Wisdom determines how well you hit your opponents in a fight,\r\n\
        and also the amount of magic spells you can learn (clerics and mages).\r\n\r\n\
[&YDex&n] - Dexterity determines how well you fight in a battle, and also\r\n\
        how cunning and sneaky you are.\r\n\r\n\
[&YCon&n] - Constitution determines how much health you have.\r\n\r\n\
[&YCha&n] - Charisma determines how good you are with people :)\r\n\r\n";

/// Wrap a payload in an `IAC SB <opt> ... IAC SE` telnet subnegotiation frame.
/// 0xFF (IAC) bytes inside JSON/MSSP payloads are vanishingly unlikely (ASCII
/// JSON, printable MSSP values), so no IAC-doubling is needed for our content;
/// we emit the frame verbatim.
fn telnet_subneg(opt: u8, payload: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(payload.len() + 5);
    v.push(IAC);
    v.push(SB);
    v.push(opt);
    v.extend_from_slice(payload);
    v.push(IAC);
    v.push(SE);
    v
}

/// Pre-escape GMCP payloads: drop control bytes (newlines, a lone IAC) and
/// strip the `&x` color-code introducers room/zone names carry, so the value
/// is clean text for the client's mapper. The JSON encoding itself is done by
/// serde_json (hostile names with quotes/backslashes/non-ASCII stay valid).
fn gmcp_clean(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '&' {
            // COLOURLIST codes are one letter/char after the & — drop both.
            chars.next();
            continue;
        }
        if (c as u32) < 0x20 || c as u32 == 0x7f {
            continue;
        }
        out.push(c);
    }
    out
}

/// Encode one GMCP message: "<name> {json}" with serde_json handling the
/// escaping of every string value.
fn gmcp_message(name: &str, value: &serde_json::Value) -> String {
    format!("{name} {value}")
}

/// True if `s` is one of the password-entry connection states (the only states
/// whose prompts must suppress client-side echo).
fn is_password_state(s: ConState) -> bool {
    matches!(
        s,
        ConState::GetOldPassword
            | ConState::GetNewPassword
            | ConState::ConfirmPassword
            | ConState::ChPwdGetOld
            | ConState::ChPwdGetNew
            | ConState::ChPwdVerify
            | ConState::DelCnf1
    )
}

/// C comm.c:894-903: every drained input command resets the idle timer and,
/// if the character was pulled into the void by check_idling, returns them to
/// their previous room with "$n has returned." (issue #217).
fn reset_idle_on_input(g: &mut GameState, ch: CharId) {
    if let Some(c) = g.get_char_mut(ch) {
        c.timer = 0;
    }
    let was_in = g.get_char(ch).and_then(|c| c.was_in_room);
    if let Some(room) = was_in {
        if g.get_char(ch).and_then(|c| c.in_room).is_some() {
            g.char_from_room(ch);
        }
        g.char_to_room(ch, room);
        if let Some(c) = g.get_char_mut(ch) {
            c.was_in_room = None;
        }
        crate::act::act(
            g,
            "$n has returned.",
            true,
            ch,
            None,
            crate::act::ActArg::None,
            crate::act::To::Room,
        );
    }
}

/// Run a player command through the interpreter inside catch_unwind so a panic
/// in any single command (bad index, arithmetic overflow in debug, a stray
/// unwrap deep in the world) is contained to that command instead of killing
/// the whole single-threaded Game task (which would disconnect every player).
///
/// AssertUnwindSafe is required because `&mut GameState` is not UnwindSafe; we
/// accept the bounded risk that a panic caught mid-mutation leaves minor state
/// inconsistency — vastly preferable to the server dying. The recovered payload
/// is logged with the offending player + input, which is the key diagnostic.
fn dispatch_command_isolated(
    state: &mut GameState,
    ch: CharId,
    input: &str,
    context: &str,
) -> bool {
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_authenticated_command(state, ch, input);
    }));
    match res {
        Ok(()) => true,
        Err(payload) => {
            let msg = panic_payload_str(&payload);
            let command = panic_command_verb(input);
            let pname = state
                .get_char(ch)
                .map(|c| c.get_name().to_string())
                .unwrap_or_else(|| "<unknown>".to_string());
            error!(
                "PANIC contained in command [{}] from player '{}' verb {:?}: {}",
                context, pname, command, msg
            );
            state.send_to_char(ch, "An error occurred processing that command.\r\n");
            false
        }
    }
}

/// Panic diagnostics must never serialize command arguments: they can contain
/// account credentials (`unlock`, `set ... passwd`) or other private text.
fn panic_command_verb(input: &str) -> String {
    input
        .split_whitespace()
        .next()
        .unwrap_or("<empty>")
        .chars()
        .take(32)
        .collect()
}

/// Extract a human-readable message from a catch_unwind payload.
fn panic_payload_str(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "non-string panic payload".to_string()
    }
}

/// Outcome of the shutdown save pass (W6): reported to the log and asserted
/// by the shutdown round-trip test.
#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub struct ShutdownReport {
    pub player_saves_attempted: u32,
    pub players_saved: u32,
    pub alias_writes_attempted: u32,
    pub aliases_written: u32,
    pub alias_errors: u32,
    pub database_errors: u32,
    pub crash_saves_attempted: u32,
    pub crash_saves_written: u32,
    pub crash_save_errors: u32,
    pub calendar_saved: bool,
    pub calendar_errors: u32,
    /// Aggregate persistence failures. Output-delivery failures are reported
    /// separately because they occur only after durability has committed.
    pub save_errors: u32,
    pub output_attempted: u32,
    pub output_acknowledged: u32,
    pub output_failed: u32,
    pub output_timed_out: u32,
    /// Backward-compatible aggregate of failed plus timed-out final flushes.
    pub output_failures: u32,
}

impl ShutdownReport {
    fn finish_persistence_counts(&mut self) {
        self.save_errors = self
            .alias_errors
            .saturating_add(self.database_errors)
            .saturating_add(self.crash_save_errors)
            .saturating_add(self.calendar_errors);
    }

    fn persistence_succeeded(&self) -> bool {
        self.save_errors == 0
    }
}

struct PendingPlayerSave {
    name: String,
    snapshot: crate::character::Character,
    task: tokio::task::JoinHandle<std::result::Result<(), String>>,
}

pub struct Game {
    state: GameState,
    db: Arc<dyn DatabaseInterface>,
    /// Async output channel per connection (the writer half lives in the
    /// connection task). The Descriptor (in GameState) only buffers text.
    outputs: HashMap<ConnId, mpsc::Sender<OutputFrame>>,
    /// Character-creation choices accumulated across nanny steps.
    pending: HashMap<ConnId, PendingChoices>,
    /// Player records loaded at password-verify time (gates + motd choice)
    /// and consumed by menu option 1, so login loads the row once.
    pending_load: HashMap<ConnId, crate::character::Character>,
    /// Connections whose character was just created (their first menu-enter
    /// also runs the C `do_start` branch: START_MESSG + do_newbie).
    just_created: std::collections::HashSet<ConnId>,
    /// At most one ordered save chain per persistent player id. Disconnect is
    /// non-blocking, while a later save waits behind the prior generation so an
    /// old snapshot can never finish last and overwrite a newer session.
    pending_player_saves: HashMap<i64, PendingPlayerSave>,
    player_save_failures: u32,
    /// The live input receiver is kept on the Game so a database wait can
    /// continue servicing established sessions. Messages which would start a
    /// second database operation are deferred until the current operation
    /// completes; gameplay input, heartbeats, and output remain live.
    game_rx: Option<mpsc::Receiver<GameMessage>>,
    deferred_messages: VecDeque<GameMessage>,
    lib_path: String,
    /// Who-list JSON snapshot (Deltania Breathes W5), shared with the metrics
    /// HTTP task's /api/who route. Written by the Game once a second; readers
    /// take a short read-lock. Empty string = nothing published yet.
    who_snapshot: Arc<std::sync::RwLock<String>>,
    /// Updated on the heartbeat hot path (atomics, no mutex).
    metrics: Arc<Metrics>,
    /// Unix timestamp the Game task started, for the MSSP UPTIME datum (which
    /// reports the server boot time per the MSSP spec).
    started_at: i64,
    /// C db.c zone_update state: the 60-second accumulator (a static counter
    /// in C) and the reset queue of zones past their lifespan.
    zone_minute_timer: u64,
    zone_reset_queue: Vec<i32>,
    /// C comm.c mins_since_crashsave: minutes since the last autosave sweep.
    mins_since_crashsave: u32,
    /// Auto-reboot warning latch (one warning per armed schedule).
    reboot_warned: bool,
    /// Present only for an OS-signal request forwarded by main. The result lets
    /// main distinguish a committed stop from an OLC-preserving refusal.
    system_shutdown_result:
        Option<tokio::sync::oneshot::Sender<crate::connection::SystemShutdownResult>>,
}

impl Game {
    pub fn new(state: GameState, db: Arc<dyn DatabaseInterface>) -> Self {
        Game {
            state,
            db,
            outputs: HashMap::new(),
            pending: HashMap::new(),
            pending_load: HashMap::new(),
            just_created: std::collections::HashSet::new(),
            pending_player_saves: HashMap::new(),
            player_save_failures: 0,
            game_rx: None,
            deferred_messages: VecDeque::new(),
            lib_path: "./lib".to_string(),
            metrics: Arc::new(Metrics::new()),
            who_snapshot: Arc::new(std::sync::RwLock::new(String::new())),
            started_at: chrono::Utc::now().timestamp(),
            zone_minute_timer: 0,
            zone_reset_queue: Vec::new(),
            mins_since_crashsave: 0,
            reboot_warned: false,
            system_shutdown_result: None,
        }
    }

    /// Install the shared metrics handle (main.rs creates one Arc and shares it
    /// with both the Game and the HTTP task). Defaults to a private Metrics so
    /// the Game is usable without one (e.g. in tests).
    /// Share the who-list snapshot with the metrics HTTP task (main.rs creates
    /// the Arc; /api/who reads it).
    pub fn set_who_snapshot(&mut self, snapshot: Arc<std::sync::RwLock<String>>) {
        self.who_snapshot = snapshot;
    }

    pub fn set_metrics(&mut self, metrics: Arc<Metrics>) {
        self.metrics = metrics;
    }

    pub fn state_mut(&mut self) -> &mut GameState {
        &mut self.state
    }

    pub async fn load_text_files(&mut self, lib_path: &str) {
        self.lib_path = lib_path.to_string();
        let text_dir = std::path::Path::new(lib_path).join("text");
        self.state.credits = tokio::fs::read_to_string(text_dir.join("credits"))
            .await
            .unwrap_or_default();
        self.state.news = tokio::fs::read_to_string(text_dir.join("news"))
            .await
            .unwrap_or_default();
        self.state.info = tokio::fs::read_to_string(text_dir.join("info"))
            .await
            .unwrap_or_default();
        self.state.handbook = tokio::fs::read_to_string(text_dir.join("handbook"))
            .await
            .unwrap_or_default();
        self.state.policies = tokio::fs::read_to_string(text_dir.join("policies"))
            .await
            .unwrap_or_default();
        self.state.motd = tokio::fs::read_to_string(text_dir.join("motd"))
            .await
            .unwrap_or_else(|_| "\r\nWelcome to DeltaMUD!\r\n".to_string());
        self.state.imotd = tokio::fs::read_to_string(text_dir.join("imotd"))
            .await
            .unwrap_or_default();
        self.state.circlemud = tokio::fs::read_to_string(text_dir.join("circlemud"))
            .await
            .unwrap_or_default();
        self.state.startup = tokio::fs::read_to_string(text_dir.join("startup"))
            .await
            .unwrap_or_default();
        self.state.background = tokio::fs::read_to_string(text_dir.join("background"))
            .await
            .unwrap_or_default();
    }

    pub fn prime_zones(&mut self) {
        // The initial zone reset moved to main (before House_boot, per
        // db.c boot order, #242); the Game task only primes live weather.
        let _ = &self.state;
        info!("Initial zone prime moved before house boot (db.c order)");
        // C boots the surface map (read_map) which calls init_weather, so the
        // world starts with MAX_WEATHER storms already on the map. Prime them
        // here so the weather map shows live storms from the first tick.
        crate::maputils::prime_weather(&mut self.state);
    }

    pub async fn run(
        &mut self,
        game_rx: mpsc::Receiver<GameMessage>,
    ) -> Result<ProcessDisposition> {
        info!("Game loop starting...");
        self.game_rx = Some(game_rx);
        let mut tick = interval(Duration::from_millis(100)); // 10 pulses/sec
        // A stall (blocked flush, slow DB) must not turn into a catch-up
        // burst of hundreds of back-to-back pulses on resume: Delay skips to
        // the next future deadline instead (tokio default is Burst).
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            if let Some(msg) = self.deferred_messages.pop_front() {
                self.handle_message_isolated(msg).await;
            } else {
                tokio::select! {
                    Some(msg) = self.game_rx.as_mut().expect("receiver installed").recv() => {
                        self.handle_message_isolated(msg).await;
                    }
                    _ = tick.tick() => self.heartbeat(),
                }
            }
            // Async bridge for OFFLINE immortal commands (set/stat/show on a
            // logged-off player): cmd_wizard queues an OfflineOp; here — between
            // awaits, where &mut self.state is free for the sync replay — we load
            // the player, replay the command, save, and extract.
            self.drain_offline_ops().await;
            self.drain_authority_update_requests().await;
            self.drain_lockout_unlock_requests().await;
            self.drain_password_update_requests().await;
            self.drain_player_rename_requests().await;
            self.drain_deferred_db_ops().await;
            self.drain_player_save_requests().await;
            self.drain_pfileclean().await;
            self.reap_completed_player_saves().await;
            self.flush_all().await;

            if let Some(requester) = self.take_authorized_copyover_request() {
                self.execute_copyover(requester).await;
            }

            // The `shutdown` immortal command sets this (C circle_shutdown=1);
            // halt via the same graceful path as a SIGTERM so the server stops.
            if let Some(disposition) = self.take_authorized_shutdown_request() {
                info!("shutdown requested by command; beginning graceful shutdown.");
                let committed = self.shutdown().await;
                if let Some(result_tx) = self.system_shutdown_result.take() {
                    let result = if committed {
                        crate::connection::SystemShutdownResult::Committed
                    } else {
                        crate::connection::SystemShutdownResult::Refused
                    };
                    let _ = result_tx.send(result);
                }
                if committed {
                    return Ok(disposition);
                }
            }
        }
    }

    fn take_authorized_copyover_request(&mut self) -> Option<CharId> {
        let request = self.state.copyover_requested.take()?;
        if self.state.authenticated_command_request_is_current(
            request,
            i32::from(LVL_IMMORT),
            3,
            crate::gcmd::GCMD3_COPYOVER,
        ) {
            return Some(request.requester_body);
        }
        warn!(
            "AUDIT: queued copyover canceled because its authenticated authority or grant changed"
        );
        if self.state.char_exists(request.requester_body) {
            self.state.send_to_char(
                request.requester_body,
                "Copyover canceled because your session authority changed.\n\r",
            );
        }
        None
    }

    fn take_authorized_shutdown_request(&mut self) -> Option<ProcessDisposition> {
        match self.state.shutdown_requested.take()? {
            ShutdownRequest::System(disposition) => Some(disposition),
            ShutdownRequest::Command {
                authorization,
                mode,
            } if self.state.authenticated_command_request_is_current(
                authorization,
                i32::from(LVL_IMMORT),
                1,
                crate::gcmd::GCMD_SHUTDOWN,
            ) =>
            {
                crate::cmd_wizard::publish_authorized_shutdown(
                    &mut self.state,
                    authorization.requester_body,
                    mode,
                );
                Some(mode.disposition())
            }
            ShutdownRequest::Command { authorization, .. } => {
                warn!(
                    "AUDIT: queued shutdown canceled because its authenticated authority or grant changed"
                );
                if self.state.char_exists(authorization.requester_body) {
                    self.state.send_to_char(
                        authorization.requester_body,
                        "Shutdown canceled because your session authority changed.\r\n",
                    );
                }
                None
            }
        }
    }

    /// Await one database operation without parking the single world task.
    ///
    /// Login/offline maintenance needs async SQL results before it can mutate
    /// the world. While that result is outstanding, continue ticking the world,
    /// accepting connections, draining input for already-playing descriptors,
    /// and flushing output. DB-dependent messages are kept in arrival order and
    /// replayed as soon as this operation finishes. Unit tests which call nanny
    /// helpers directly have no installed receiver and simply await the future.
    async fn handle_message_isolated(&mut self, msg: GameMessage) {
        let conn_id = msg.conn_id();
        let kind = msg.kind();
        let outcome = AssertUnwindSafe(self.handle_message(msg))
            .catch_unwind()
            .await;
        if let Err(payload) = outcome {
            error!(
                "PANIC while handling {} for connection {:?}: {}",
                kind,
                conn_id,
                panic_payload_str(&*payload)
            );
            if let Some(conn_id) = conn_id {
                crate::modify::abort_conn(&mut self.state, conn_id);
                if let Some(descriptor) = self.state.descriptors.get_mut(&conn_id) {
                    descriptor.state = ConState::Close;
                }
                self.disconnect(conn_id).await;
            }
        }
    }

    async fn handle_message(&mut self, msg: GameMessage) {
        match msg {
            GameMessage::SystemShutdown { result_tx } => {
                // Main is the sole OS-signal owner. Queueing the request here
                // makes service stops use the same OLC-preserving shutdown path
                // as an authorized in-game stop.
                if let Some(previous) = self.system_shutdown_result.replace(result_tx) {
                    let _ = previous.send(crate::connection::SystemShutdownResult::Refused);
                }
                self.state.shutdown_requested =
                    Some(ShutdownRequest::System(ProcessDisposition::Stop));
            }
            GameMessage::NewConnection {
                id,
                host,
                peer_ip,
                verified_hostname,
                raw_fd,
                output_tx,
            } => {
                info!("New connection from {}", host);
                self.metrics.inc_connections();
                let mut d = Descriptor::with_identity(id, host, peer_ip, verified_hostname, raw_fd);
                // C comm.c:1608: the colour question is the very first output;
                // the startup banner follows the answer (CON_QANSI) (#198).
                d.write(ANSI_QUESTION);
                self.state.descriptors.insert(id, d);
                self.outputs.insert(id, output_tx);
                self.write_prompt(id);
            }
            GameMessage::Recover {
                id,
                host,
                peer_ip,
                verified_hostname,
                raw_fd,
                name,
                output_tx,
            } => {
                self.recover_player(
                    id,
                    host,
                    peer_ip,
                    verified_hostname,
                    raw_fd,
                    name,
                    output_tx,
                )
                .await;
            }
            GameMessage::Input { conn_id, input } => {
                self.handle_input(conn_id, input).await;
            }
            GameMessage::Gmcp { conn_id, event } => {
                self.handle_gmcp_event(conn_id, event);
            }
            GameMessage::SendMssp { conn_id } => {
                self.send_mssp(conn_id);
            }
            GameMessage::Disconnect { conn_id } => {
                self.disconnect(conn_id).await;
            }
            #[cfg(test)]
            GameMessage::PanicForTest { .. } => {
                panic!("injected async message panic");
            }
        }
    }

    /// C comm.c perform_subst (1911-1960): "^telm^tell" replaces the first
    /// occurrence of the text between the carets in `orig` with the
    /// replacement. Returns None when the syntax is bad or the search text is
    /// absent (caller prints "Invalid substitution.").
    fn perform_subst(orig: &str, subst: &str) -> Option<String> {
        let rest = &subst[1..];
        let idx = rest.find('^')?;
        let first = &rest[..idx];
        let second = &rest[idx + 1..];
        let pos = orig.find(first)?;
        let mut new = String::with_capacity(orig.len() + second.len());
        new.push_str(&orig[..pos]);
        new.push_str(second);
        new.push_str(&orig[pos + first.len()..]);
        Some(crate::text::utf8_prefix(&new, crate::types::MAX_INPUT_LENGTH).to_string())
    }

    fn process_input_queues(&mut self) {
        let conn_ids: Vec<ConnId> = self.state.descriptors.keys().copied().collect();
        for cid in conn_ids {
            let ready = match self.state.descriptors.get_mut(&cid) {
                Some(d) if d.state == ConState::Playing => {
                    d.wait = (d.wait - 1).max(-1);
                    d.wait <= 0 && !d.input_queue.is_empty()
                }
                _ => false,
            };
            if !ready {
                continue;
            }
            let queued = match self.state.descriptors.get_mut(&cid) {
                Some(d) => {
                    d.wait = 1;
                    d.input_queue.pop_front()
                }
                None => None,
            };
            let queued = match queued {
                Some(i) => i,
                None => continue,
            };
            if let Some(ch) = self.state.descriptors.get(&cid).and_then(|d| d.character) {
                reset_idle_on_input(&mut self.state, ch);
                let mut input = queued.line;
                if !queued.aliased {
                    match crate::alias::alias_expand(&self.state, ch, &input) {
                        Some(crate::alias::AliasExpansion::Simple(line)) => {
                            input = line;
                        }
                        Some(crate::alias::AliasExpansion::Complex(lines)) => {
                            if let Some(d) = self.state.descriptors.get_mut(&cid) {
                                for line in lines.into_iter().rev() {
                                    d.input_queue.push_front(QueuedInput::aliased(line));
                                }
                                input = match d.input_queue.pop_front() {
                                    Some(q) => q.line,
                                    None => continue,
                                };
                            } else {
                                continue;
                            }
                        }
                        None => {}
                    }
                }
                self.metrics.inc_commands();
                dispatch_command_isolated(&mut self.state, ch, &input, "input-queue");
            }
            let st = self.state.descriptors.get(&cid).map(|d| d.state);
            if st.is_some() && st != Some(ConState::Close) {
                self.write_prompt(cid);
            }
        }
    }
}

// ---- Login / character creation (CircleMUD nanny) lives in game/nanny.rs ----

/// Pending character-creation choices held between nanny steps.
#[derive(Clone, Copy)]
struct PendingChoices {
    sex: Gender,
    class: Class,
    race: Race,
    race_index: i32,
    newbie: u8,
    deity: u8,
    hometown: RoomVnum,
    rolled: Abilities,
}
impl Default for PendingChoices {
    fn default() -> Self {
        PendingChoices {
            sex: Gender::Neutral,
            class: Class::Warrior,
            race: Race::Human,
            race_index: crate::races::RACE_HUMAN,
            newbie: 1,
            deity: crate::deity::DEITY_AETOS as u8,
            hometown: 1,
            rolled: Abilities::default(),
        }
    }
}

fn stat_roll_prompt(abils: Abilities) -> String {
    format!(
        "\r\nStr: {} Int: {} Wis: {} Dex: {} Con: {} Cha: {}\r\n\
Are these values acceptable? (Y/&YN&n): ",
        abils.str, abils.intel, abils.wis, abils.dex, abils.con, abils.cha
    )
}

fn normalize_name(s: &str) -> String {
    let mut c = s.trim().chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + &c.as_str().to_lowercase(),
        None => String::new(),
    }
}

fn valid_name(name: &str) -> bool {
    // C: MAX_NAME_LENGTH == 20 (structs.h) — the player-name field is 20+1, and
    // the nanny name-entry path caps names at MAX_NAME_LENGTH, not 16 (BUG #16).
    name.len() >= 2 && name.len() <= 20 && name.chars().all(|c| c.is_ascii_alphabetic())
}

/// C interpreter.c:694-718: fill words ("in from with the on at to") and the
/// reserved list ("a an self me all room someone something") are both refused
/// as player names (#223).
fn reserved_or_fill_word(name: &str) -> bool {
    const RESERVED: &[&str] = &[
        "a",
        "an",
        "self",
        "me",
        "all",
        "room",
        "someone",
        "something",
    ];
    crate::interpreter::FILL_WORDS
        .iter()
        .any(|f| f.eq_ignore_ascii_case(name))
        || RESERVED.iter().any(|r| r.eq_ignore_ascii_case(name))
}

#[cfg(test)]
// Tests in this module hold synchronous guards only to serialize process-global
// ban/arena fixtures on the current test process; production paths do not.
#[allow(clippy::await_holding_lock)]
mod tests {
    use super::*;
    use crate::DatabaseInterface;
    use crate::config::Config;
    use crate::mock_database::MockDatabase;
    use std::sync::Arc;
    use std::sync::{Mutex, OnceLock};
    use tokio::io::AsyncReadExt;

    #[test]
    fn panic_diagnostics_never_include_command_arguments() {
        assert_eq!(panic_command_verb("unlock swordfish"), "unlock");
        assert_eq!(panic_command_verb("set Victim passwd hunter2"), "set");
        assert!(!panic_command_verb("set Victim passwd hunter2").contains("hunter2"));
    }

    pub(super) fn test_game(db: Arc<MockDatabase>) -> Game {
        let db_trait: Arc<dyn DatabaseInterface> = db;
        let mut cfg = Config::default();
        // Keep the user_cntr USRCNT write (lib/../USRCNT) out of the repo.
        cfg.lib_path = std::env::temp_dir()
            .join(format!("deltamud-game-lib-{}", std::process::id()))
            .to_string_lossy()
            .to_string();
        let _ = std::fs::create_dir_all(&cfg.lib_path);
        Game::new(GameState::new(cfg), db_trait)
    }

    fn attach_descriptor(game: &mut Game, conn: ConnId) {
        attach_descriptor_host(game, conn, "example.test");
    }

    pub(super) fn attach_descriptor_host(game: &mut Game, conn: ConnId, host: &str) {
        game.state
            .descriptors
            .insert(conn, Descriptor::new(conn, host.to_string()));
    }

    pub(super) async fn persistent_connected_player(
        game: &mut Game,
        db: &MockDatabase,
        conn: ConnId,
        name: &str,
        level: Level,
    ) -> CharId {
        attach_descriptor(game, conn);
        let descriptor = game.state.descriptors.get_mut(&conn).unwrap();
        descriptor.state = ConState::Playing;

        let mut character =
            crate::character::Character::new_player(name.to_string(), Class::Warrior, Race::Human);
        character.desc = Some(conn);
        character.player.level = level;
        character.trust = i32::from(level);
        let grants = crate::gcmd::canonical_advance_grants(level, LVL_IMMORT, LVL_IMPL);
        character.godcmds1 = grants.0;
        character.godcmds2 = grants.1;
        character.godcmds3 = grants.2;
        character.godcmds4 = grants.3;
        character.idnum = db.create_player(&character, "test-password").await.unwrap();

        let id = game.state.create_char(character);
        game.state.descriptors.get_mut(&conn).unwrap().character = Some(id);
        game.state.players_by_name.insert(name.to_lowercase(), id);
        id
    }

    fn authenticated_request(
        game: &Game,
        body: CharId,
    ) -> crate::state::AuthenticatedCommandRequest {
        let authority = game.state.principal_authority(body).unwrap();
        let descriptor = authority.descriptor.unwrap();
        let principal = game.state.get_char(authority.principal).unwrap();
        crate::state::AuthenticatedCommandRequest {
            requester_body: body,
            requester_principal: authority.principal,
            descriptor,
            idnum: principal.idnum,
        }
    }

    /// Attach a descriptor and answer the CON_QANSI colour question so the
    /// connection sits at GetName (tests written against the pre-#198 flow).
    async fn attach_descriptor_at_name(game: &mut Game, conn: ConnId, host: &str) {
        attach_descriptor_host(game, conn, host);
        game.nanny(conn, "y".to_string()).await;
        assert_eq!(descriptor_state(game, conn), ConState::GetName);
    }

    async fn attach_descriptor_identity_at_name(
        game: &mut Game,
        conn: ConnId,
        peer_ip: &str,
        verified_hostname: &str,
    ) {
        game.state.descriptors.insert(
            conn,
            Descriptor::with_identity(
                conn,
                verified_hostname.to_string(),
                peer_ip.to_string(),
                Some(verified_hostname.to_string()),
                -1,
            ),
        );
        game.nanny(conn, "y".to_string()).await;
        assert_eq!(descriptor_state(game, conn), ConState::GetName);
    }

    fn descriptor_state(game: &Game, conn: ConnId) -> ConState {
        game.state.descriptors.get(&conn).unwrap().state
    }

    fn zone_with_builders(builders: &str) -> crate::world::Zone {
        crate::world::Zone {
            number: 30,
            name: "Builder ACL test zone".into(),
            builders: builders.into(),
            lifespan: 30,
            age: 0,
            top: 3099,
            reset_mode: 2,
            min_level: 0,
            max_level: LVL_IMPL,
            status_mode: 0,
            map_x: None,
            map_y: None,
            reset_commands: Vec::new(),
        }
    }

    #[tokio::test]
    async fn terminal_unlock_is_verified_async_before_lock_is_cleared() {
        let db = Arc::new(MockDatabase::new());
        let mut game = test_game(db.clone());
        let character =
            persistent_connected_player(&mut game, db.as_ref(), ConnId(8_901), "Terminalowner", 10)
                .await;
        game.state.get_char_mut(character).unwrap().prf2_flags |= crate::flags::PRF2_LOCKOUT;
        game.state
            .descriptors
            .get_mut(&ConnId(8_901))
            .unwrap()
            .password_hash = Some(crate::password::hash_password("unlock-me"));

        crate::cmd_other::do_lockout(&mut game.state, character, "unlock-me", 0);

        assert_ne!(
            game.state.get_char(character).unwrap().prf2_flags & crate::flags::PRF2_LOCKOUT,
            0
        );
        assert!(
            !game.state.descriptors[&ConnId(8_901)]
                .outbuf
                .contains("terminal is now unlocked")
        );
        assert_eq!(game.state.lockout_unlock_requests.len(), 1);

        game.drain_lockout_unlock_requests().await;

        assert_eq!(
            game.state.get_char(character).unwrap().prf2_flags & crate::flags::PRF2_LOCKOUT,
            0
        );
        assert!(
            game.state.descriptors[&ConnId(8_901)]
                .outbuf
                .contains("terminal is now unlocked")
        );
    }

    #[tokio::test]
    async fn terminal_unlock_wrong_password_or_changed_session_fails_closed() {
        let db = Arc::new(MockDatabase::new());
        let mut game = test_game(db.clone());
        let character =
            persistent_connected_player(&mut game, db.as_ref(), ConnId(8_902), "Lockedowner", 10)
                .await;
        game.state.get_char_mut(character).unwrap().prf2_flags |= crate::flags::PRF2_LOCKOUT;
        game.state
            .descriptors
            .get_mut(&ConnId(8_902))
            .unwrap()
            .password_hash = Some(crate::password::hash_password("right-password"));

        crate::cmd_other::do_lockout(&mut game.state, character, "wrong-password", 0);
        game.drain_lockout_unlock_requests().await;
        assert_ne!(
            game.state.get_char(character).unwrap().prf2_flags & crate::flags::PRF2_LOCKOUT,
            0
        );
        assert!(
            game.state.descriptors[&ConnId(8_902)]
                .outbuf
                .contains("Password mismatch")
        );

        game.state
            .descriptors
            .get_mut(&ConnId(8_902))
            .unwrap()
            .outbuf
            .clear();
        crate::cmd_other::do_lockout(&mut game.state, character, "right-password", 0);
        game.state
            .descriptors
            .get_mut(&ConnId(8_902))
            .unwrap()
            .password_hash = Some(crate::password::hash_password("rotated-password"));
        game.drain_lockout_unlock_requests().await;
        assert_ne!(
            game.state.get_char(character).unwrap().prf2_flags & crate::flags::PRF2_LOCKOUT,
            0
        );
        assert!(
            game.state.descriptors[&ConnId(8_902)]
                .outbuf
                .contains("verification expired")
        );
    }

    #[tokio::test]
    async fn advance_is_durable_before_live_publication_and_survives_reload() {
        let db = Arc::new(MockDatabase::new());
        let mut game = test_game(db.clone());
        let room = game.state.add_room(crate::room::Room::new(
            100,
            0,
            "Authority room".to_string(),
            "A room.".to_string(),
        ));
        let actor = persistent_connected_player(
            &mut game,
            db.as_ref(),
            ConnId(9_001),
            "Authorityactor",
            LVL_IMPL,
        )
        .await;
        let target = persistent_connected_player(
            &mut game,
            db.as_ref(),
            ConnId(9_002),
            "Authoritytarget",
            2,
        )
        .await;
        game.state.char_to_room(actor, room);
        game.state.char_to_room(target, room);

        run_authenticated_command(&mut game.state, actor, "advance Authoritytarget 101");

        assert_eq!(game.state.get_char(target).unwrap().player.level, 2);
        assert_eq!(
            db.load_player("Authoritytarget")
                .await
                .unwrap()
                .player
                .level,
            2
        );
        assert!(
            !game.state.descriptors[&ConnId(9_001)]
                .outbuf
                .contains("has advanced")
        );

        game.drain_authority_update_requests().await;

        let live = game.state.get_char(target).unwrap();
        assert_eq!(live.player.level, LVL_IMMORT);
        assert_eq!(live.trust, i32::from(LVL_IMMORT));
        assert_eq!(live.points.exp, crate::limits::exp_to_level(100));
        assert_eq!(live.godcmds1, crate::gcmd::GCMD_GEN);
        assert_eq!((live.godcmds2, live.godcmds3, live.godcmds4), (0, 0, 0));
        assert!(
            game.state.descriptors[&ConnId(9_001)]
                .outbuf
                .contains("has advanced Authoritytarget to level 101")
        );

        let reloaded = db.load_player("Authoritytarget").await.unwrap();
        assert_eq!(reloaded.player.level, LVL_IMMORT);
        assert_eq!(reloaded.trust, i32::from(LVL_IMMORT));
        assert_eq!(reloaded.points.exp, crate::limits::exp_to_level(100));
        assert_eq!(reloaded.godcmds1, crate::gcmd::GCMD_GEN);
        assert_eq!(
            (reloaded.godcmds2, reloaded.godcmds3, reloaded.godcmds4),
            (0, 0, 0)
        );
    }

    #[tokio::test]
    async fn advance_demotion_revokes_trust_and_every_capability_durably() {
        let db = Arc::new(MockDatabase::new());
        let mut game = test_game(db.clone());
        let room = game.state.add_room(crate::room::Room::new(
            101,
            0,
            "Authority room".to_string(),
            "A room.".to_string(),
        ));
        let actor =
            persistent_connected_player(&mut game, db.as_ref(), ConnId(9_011), "Demoter", LVL_IMPL)
                .await;
        let target = persistent_connected_player(
            &mut game,
            db.as_ref(),
            ConnId(9_012),
            "Demoted",
            LVL_GRGOD,
        )
        .await;
        game.state.char_to_room(actor, room);
        game.state.char_to_room(target, room);
        {
            let character = game.state.get_char_mut(target).unwrap();
            character.godcmds1 = !0;
            character.godcmds2 = !0;
            character.godcmds3 = !0;
            character.godcmds4 = !0;
        }
        db.save_player(game.state.get_char(target).unwrap())
            .await
            .unwrap();

        run_authenticated_command(&mut game.state, actor, "advance Demoted 1");
        game.drain_authority_update_requests().await;

        let reloaded = db.load_player("Demoted").await.unwrap();
        assert_eq!((reloaded.player.level, reloaded.trust), (1, 1));
        assert_eq!(reloaded.points.exp, crate::limits::exp_to_level(0));
        assert_eq!(
            (
                reloaded.godcmds1,
                reloaded.godcmds2,
                reloaded.godcmds3,
                reloaded.godcmds4,
            ),
            (0, 0, 0, 0)
        );
        let live = game.state.get_char(target).unwrap();
        assert_eq!((live.player.level, live.trust), (1, 1));
        assert_eq!(
            (live.godcmds1, live.godcmds2, live.godcmds3, live.godcmds4),
            (0, 0, 0, 0)
        );
    }

    #[tokio::test]
    async fn queued_authority_update_rechecks_the_exact_advance_grant() {
        let db = Arc::new(MockDatabase::new());
        let mut game = test_game(db.clone());
        let actor = persistent_connected_player(
            &mut game,
            db.as_ref(),
            ConnId(9_013),
            "Grantactor",
            LVL_IMPL,
        )
        .await;
        let target =
            persistent_connected_player(&mut game, db.as_ref(), ConnId(9_014), "Granttarget", 2)
                .await;

        run_authenticated_command(&mut game.state, actor, "advance Granttarget 101");
        assert_eq!(game.state.authority_update_requests.len(), 1);
        game.state.get_char_mut(actor).unwrap().godcmds1 &= !crate::gcmd::GCMD_ADVANCE;

        game.drain_authority_update_requests().await;

        assert_eq!(game.state.get_char(target).unwrap().trust, 2);
        assert_eq!(db.load_player("Granttarget").await.unwrap().trust, 2);
    }

    #[tokio::test]
    async fn authority_update_failure_keeps_live_and_durable_state_unchanged() {
        let db = Arc::new(MockDatabase::new());
        let mut game = test_game(db.clone());
        let room = game.state.add_room(crate::room::Room::new(
            102,
            0,
            "Authority room".to_string(),
            "A room.".to_string(),
        ));
        let actor = persistent_connected_player(
            &mut game,
            db.as_ref(),
            ConnId(9_021),
            "Failureactor",
            LVL_IMPL,
        )
        .await;
        let target =
            persistent_connected_player(&mut game, db.as_ref(), ConnId(9_022), "Failuretarget", 2)
                .await;
        game.state.char_to_room(actor, room);
        game.state.char_to_room(target, room);
        run_authenticated_command(&mut game.state, actor, "advance Failuretarget 101");
        db.fail_next_authority_update();

        game.drain_authority_update_requests().await;

        assert_eq!(game.state.get_char(target).unwrap().player.level, 2);
        assert_eq!(game.state.get_char(target).unwrap().trust, 2);
        let durable = db.load_player("Failuretarget").await.unwrap();
        assert_eq!((durable.player.level, durable.trust), (2, 2));
        assert!(!game.state.authority_quarantine.contains(&durable.idnum));
        let output = &game.state.descriptors[&ConnId(9_021)].outbuf;
        assert!(output.contains("rejected because durable state changed"));
        assert!(!output.contains("has advanced"));
    }

    #[tokio::test]
    async fn authority_postcommit_error_is_confirmed_by_exact_readback() {
        let db = Arc::new(MockDatabase::new());
        let mut game = test_game(db.clone());
        let room = game.state.add_room(crate::room::Room::new(
            104,
            0,
            "Authority room".to_string(),
            "A room.".to_string(),
        ));
        let actor = persistent_connected_player(
            &mut game,
            db.as_ref(),
            ConnId(9_041),
            "Readbackactor",
            LVL_IMPL,
        )
        .await;
        let target =
            persistent_connected_player(&mut game, db.as_ref(), ConnId(9_042), "Readbacktarget", 2)
                .await;
        game.state.char_to_room(actor, room);
        game.state.char_to_room(target, room);
        run_authenticated_command(&mut game.state, actor, "advance Readbacktarget 101");
        db.fail_next_authority_update_after_commit();

        game.drain_authority_update_requests().await;

        let live = game.state.get_char(target).unwrap();
        assert_eq!(
            (live.player.level, live.trust),
            (LVL_IMMORT, i32::from(LVL_IMMORT))
        );
        let durable = db.load_player("Readbacktarget").await.unwrap();
        assert_eq!(
            (durable.player.level, durable.trust),
            (LVL_IMMORT, i32::from(LVL_IMMORT))
        );
        assert!(!game.state.authority_quarantine.contains(&durable.idnum));
        assert!(
            game.state.descriptors[&ConnId(9_041)]
                .outbuf
                .contains("has advanced Readbacktarget to level 101")
        );
    }

    #[tokio::test]
    async fn indeterminate_authority_outcome_quarantines_the_account_fail_closed() {
        let db = Arc::new(MockDatabase::new());
        let mut game = test_game(db.clone());
        let room = game.state.add_room(crate::room::Room::new(
            105,
            0,
            "Authority room".to_string(),
            "A room.".to_string(),
        ));
        let actor = persistent_connected_player(
            &mut game,
            db.as_ref(),
            ConnId(9_051),
            "Quarantineactor",
            LVL_IMPL,
        )
        .await;
        let target = persistent_connected_player(
            &mut game,
            db.as_ref(),
            ConnId(9_052),
            "Quarantinetarget",
            LVL_GRGOD,
        )
        .await;
        game.state.char_to_room(actor, room);
        game.state.char_to_room(target, room);
        run_authenticated_command(&mut game.state, actor, "advance Quarantinetarget 1");
        db.fail_next_authority_update();
        db.fail_next_authority_read();

        game.drain_authority_update_requests().await;

        let idnum = game.state.get_char(target).unwrap().idnum;
        assert!(game.state.authority_quarantine.contains(&idnum));
        assert_eq!(
            (
                game.state.get_char(target).unwrap().player.level,
                game.state.get_char(target).unwrap().trust,
            ),
            (1, 1),
            "an ambiguous demotion must expose only the less-privileged tuple"
        );
        run_authenticated_command(&mut game.state, target, "shutdown die");
        assert!(game.state.shutdown_requested.is_none());
        assert!(
            game.state.descriptors[&ConnId(9_051)]
                .outbuf
                .contains("privilege-quarantined")
        );
        assert!(
            !game.state.descriptors[&ConnId(9_051)]
                .outbuf
                .contains("has advanced")
        );
        assert_eq!(
            db.load_player("Quarantinetarget").await.unwrap().trust,
            i32::from(LVL_GRGOD),
            "the injected pre-commit failure leaves durable state old while live authority is denied"
        );
    }

    #[tokio::test]
    async fn authority_cas_runs_after_an_older_broad_save() {
        let db = Arc::new(MockDatabase::new());
        let mut game = test_game(db.clone());
        let room = game.state.add_room(crate::room::Room::new(
            103,
            0,
            "Authority room".to_string(),
            "A room.".to_string(),
        ));
        let actor = persistent_connected_player(
            &mut game,
            db.as_ref(),
            ConnId(9_031),
            "Saveorderactor",
            LVL_IMPL,
        )
        .await;
        let target = persistent_connected_player(
            &mut game,
            db.as_ref(),
            ConnId(9_032),
            "Saveordertarget",
            2,
        )
        .await;
        game.state.char_to_room(actor, room);
        game.state.char_to_room(target, room);

        let stale_snapshot = game.state.get_char(target).unwrap().clone();
        db.set_save_delay(Some(std::time::Duration::from_millis(25)));
        game.queue_player_save(stale_snapshot, String::new());
        run_authenticated_command(&mut game.state, actor, "advance Saveordertarget 101");

        game.drain_authority_update_requests().await;
        db.set_save_delay(None);

        let durable = db.load_player("Saveordertarget").await.unwrap();
        assert_eq!(
            (durable.player.level, durable.trust),
            (LVL_IMMORT, i32::from(LVL_IMMORT))
        );
        assert_eq!(durable.godcmds1, crate::gcmd::GCMD_GEN);
        assert!(!game.pending_player_saves.contains_key(&durable.idnum));
    }

    #[test]
    fn online_save_snapshot_accumulates_played_time_and_resets_logon() {
        let db = Arc::new(MockDatabase::new());
        let mut game = test_game(db);
        let conn = ConnId(1);
        attach_descriptor(&mut game, conn);

        let mut ch = crate::character::Character::new_player(
            "Timer".to_string(),
            Class::Warrior,
            Race::Human,
        );
        ch.desc = Some(conn);
        ch.player.time_played = 40;
        ch.last_logon = chrono::Utc::now() - chrono::Duration::seconds(90);
        let cid = game.state.create_char(ch);
        game.state.descriptors.get_mut(&conn).unwrap().character = Some(cid);

        let snapshot = game.snapshot_online_player_for_save(cid).unwrap();

        assert!(snapshot.player.time_played >= 130);
        let live = game.state.get_char(cid).unwrap();
        assert_eq!(live.player.time_played, snapshot.player.time_played);
        assert_eq!(live.last_logon, snapshot.last_logon);
        assert!((chrono::Utc::now() - live.last_logon).num_seconds() <= 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn disconnect_persists_restored_arena_state_before_extracting() {
        let db = Arc::new(MockDatabase::new());
        let mut game = test_game(db.clone());
        let arena_room = game.state.add_room(crate::room::Room::new(
            4801,
            48,
            "Arena Prep".into(),
            String::new(),
        ));
        let conn = ConnId(91);
        attach_descriptor(&mut game, conn);

        let mut ch = crate::character::Character::new_player(
            "ArenaSaver".to_string(),
            Class::Warrior,
            Race::Human,
        );
        ch.wimp_level = 12;
        ch.recall_level = 34;
        ch.affect_flags = crate::flags::AFF_INVISIBLE;
        crate::gold::set(&mut ch, crate::gold::Account::Carried, 7_777);
        ch.desc = Some(conn);
        ch.idnum = db.create_player(&ch, "pw").await.unwrap();
        let cid = game.state.create_char(ch);
        game.state.char_to_room(cid, arena_room);
        game.state.descriptors.get_mut(&conn).unwrap().character = Some(cid);
        crate::arena::set_stat_for_test(&mut game.state, cid, crate::arena::ARENA_COMBATANT1);
        crate::arena::bup_affects(&mut game.state, cid);
        game.state.get_char_mut(cid).unwrap().affect_flags = crate::flags::AFF_BLIND;

        game.disconnect(conn).await;
        assert_eq!(game.await_all_player_saves().await, 0);

        let saved = db.load_player("ArenaSaver").await.unwrap();
        assert_eq!(saved.affect_flags, crate::flags::AFF_INVISIBLE);
        assert_eq!(saved.wimp_level, 12);
        assert_eq!(saved.recall_level, 34);
        assert_eq!(saved.points.gold, 7_777);
        assert_eq!(
            crate::arena::arena_stat(&game.state, cid),
            crate::arena::ARENA_NOT
        );
        assert!(!game.state.char_exists(cid));
    }

    fn ban_test_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        crate::lock_ok::lock(LOCK.get_or_init(|| Mutex::new(())))
    }

    #[test]
    fn zone_update_ages_once_per_minute_and_queues_resets() {
        // C db.c:1877-1952 (#231): six PULSE_ZONE ticks make one minute;
        // a zone reaching its lifespan is queued (age = ZO_DEAD). An OCCUPIED
        // zone (reset_mode 1) is not reset until a tick finds it empty.
        let db = Arc::new(MockDatabase::new());
        let mut game = test_game(db);
        game.state.zones.push(crate::world::Zone {
            number: 30,
            name: "Test Zone".into(),
            builders: String::new(),
            lifespan: 1,
            age: 0,
            top: 3099,
            reset_mode: 1,
            min_level: 0,
            max_level: 0,
            status_mode: 0,
            map_x: None,
            map_y: None,
            reset_commands: Vec::new(),
        });
        let rnum = game
            .state
            .add_room(crate::room::Room::new(3001, 30, "z".into(), "".into()));

        // An idle player inside the zone keeps zone_is_empty() false.
        let conn = ConnId(55);
        game.state
            .descriptors
            .insert(conn, Descriptor::new(conn, "example.test".to_string()));
        let mut occupant = crate::character::Character::new_player(
            "Zoner".to_string(),
            Class::Warrior,
            Race::Human,
        );
        occupant.desc = Some(conn);
        let oid = game.state.create_char(occupant);
        game.state.descriptors.get_mut(&conn).unwrap().character = Some(oid);
        game.state.descriptors.get_mut(&conn).unwrap().state = ConState::Playing;
        game.state.char_to_room(oid, rnum);

        for _ in 0..5 {
            game.zone_update();
        }
        assert_eq!(game.state.zones[0].age, 0, "no minute has fully passed");

        game.zone_update(); // 6th tick = 60 s
        assert_eq!(game.state.zones[0].age, crate::world::ZONE_DEAD);
        assert_eq!(game.zone_reset_queue, vec![30]);

        // Occupied: the queued reset must NOT fire.
        game.zone_update();
        assert_eq!(game.zone_reset_queue, vec![30], "occupied zone waits");

        // The occupant leaves: the next tick resets the zone.
        game.state.char_from_room(oid);
        game.zone_update();
        assert!(game.zone_reset_queue.is_empty(), "empty zone resets");
        assert_eq!(game.state.zones[0].age, 0);
    }

    #[test]
    fn drained_input_resets_idle_timer_and_returns_from_void() {
        // C comm.c:894-903 (#217): a drained command zeroes the idle timer
        // and returns a void-idled character to their previous room.
        let mut g = GameState::new(Config::default());
        g.add_room(crate::room::Room::new(3001, 30, "Home".into(), "".into()));
        g.add_room(crate::room::Room::new(
            3002,
            30,
            "Elsewhere".into(),
            "".into(),
        ));
        let mut ch = crate::character::Character::new_player(
            "Idler".to_string(),
            Class::Warrior,
            Race::Human,
        );
        ch.timer = 9; // past the >8 void threshold
        let cid = g.create_char(ch);

        let conn = ConnId(77);
        g.descriptors
            .insert(conn, Descriptor::new(conn, "example.test".to_string()));
        let mut observer = crate::character::Character::new_player(
            "Watcher".to_string(),
            Class::Warrior,
            Race::Human,
        );
        observer.desc = Some(conn);
        let obs = g.create_char(observer);
        g.descriptors.get_mut(&conn).unwrap().character = Some(obs);

        g.char_to_room(obs, 0);
        g.char_to_room(cid, 0);
        // Simulate the void pull (limits.rs check_idling): was_in saved, char
        // parked elsewhere.
        g.get_char_mut(cid).unwrap().was_in_room = Some(0);
        g.char_to_room(cid, 1);

        reset_idle_on_input(&mut g, cid);

        let c = g.get_char(cid).unwrap();
        assert_eq!(c.timer, 0, "drained command must reset the idle timer");
        assert_eq!(c.in_room, Some(0));
        assert_eq!(c.was_in_room, None);
        let out = &g.descriptors.get(&conn).unwrap().outbuf;
        assert!(out.contains("has returned"), "observer saw: {out:?}");
    }

    fn temp_ban_lib(name: &str, badsites: &str) -> String {
        let path =
            std::env::temp_dir().join(format!("deltamud-ban-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(path.join("etc")).unwrap();
        std::fs::create_dir_all(path.join("misc")).unwrap();
        std::fs::write(path.join("etc/badsites"), badsites).unwrap();
        std::fs::write(path.join("misc/xnames"), "").unwrap();
        path.to_string_lossy().to_string()
    }

    #[tokio::test]
    async fn creation_walks_c_nanny_choice_sequence() {
        let db = Arc::new(MockDatabase::new());
        let mut game = test_game(db);
        let conn = ConnId(1);
        attach_descriptor_at_name(&mut game, conn, "example.test").await;

        game.nanny(conn, "Alice".to_string()).await;
        assert_eq!(descriptor_state(&game, conn), ConState::ConfirmName);
        game.nanny(conn, "y".to_string()).await;
        assert_eq!(descriptor_state(&game, conn), ConState::GetNewPassword);
        game.nanny(conn, "secret".to_string()).await;
        assert_eq!(descriptor_state(&game, conn), ConState::ConfirmPassword);
        game.nanny(conn, "secret".to_string()).await;
        assert_eq!(descriptor_state(&game, conn), ConState::GetNewbie);
        game.nanny(conn, "n".to_string()).await;
        assert_eq!(descriptor_state(&game, conn), ConState::GetSex);
        game.nanny(conn, "f".to_string()).await;
        assert_eq!(descriptor_state(&game, conn), ConState::GetRace);
        game.nanny(conn, "a".to_string()).await;
        assert_eq!(descriptor_state(&game, conn), ConState::GetDeity);
        game.nanny(conn, "b".to_string()).await;
        assert_eq!(descriptor_state(&game, conn), ConState::GetClass);
        game.nanny(conn, "c".to_string()).await;
        assert_eq!(descriptor_state(&game, conn), ConState::GetHometown);
        game.nanny(conn, "b".to_string()).await;
        assert_eq!(descriptor_state(&game, conn), ConState::RollStats);

        let pending = game.pending.get(&conn).unwrap();
        assert_eq!(pending.newbie, 0);
        assert_eq!(pending.sex, Gender::Female);
        assert_eq!(pending.race_index, crate::races::RACE_HUMAN);
        assert_eq!(pending.deity, crate::deity::DEITY_CORGUS as u8);
        assert_eq!(pending.class, Class::Warrior);
        assert_eq!(pending.hometown, 2);
        assert!(pending.rolled.str > 0);
        assert!(pending.rolled.con > 0);
    }

    #[tokio::test]
    async fn deleted_builder_acl_name_cannot_be_reused_for_a_new_character() {
        let db = Arc::new(MockDatabase::new());
        let mut deleted = crate::character::Character::new_player(
            "Aclbuilder".into(),
            Class::Warrior,
            Race::Human,
        );
        deleted.act_flags |= crate::flags::PLR_DELETED;
        db.create_player(&deleted, "gone").await.unwrap();
        assert_eq!(db.delete_deleted_players().await.unwrap(), 1);
        assert!(!db.player_exists("Aclbuilder").await.unwrap());

        let mut game = test_game(db);
        game.state
            .zones
            .push(zone_with_builders("Michael Aclbuilder, Claude"));
        let conn = ConnId(101);
        attach_descriptor_at_name(&mut game, conn, "builder-name.test").await;

        game.nanny(conn, "Aclbuilder".into()).await;

        let descriptor = &game.state.descriptors[&conn];
        assert_eq!(descriptor.state, ConState::GetName);
        assert_eq!(descriptor.temp_name, None);
        assert!(descriptor.outbuf.contains("Invalid name"));
    }

    #[tokio::test]
    async fn current_builder_acl_change_is_rechecked_before_creation_confirmation() {
        let db = Arc::new(MockDatabase::new());
        let mut game = test_game(db);
        let conn = ConnId(102);
        attach_descriptor_at_name(&mut game, conn, "builder-race.test").await;

        game.nanny(conn, "Aclbuilder".into()).await;
        assert_eq!(descriptor_state(&game, conn), ConState::ConfirmName);
        game.state
            .zones
            .push(zone_with_builders("Michael ACLBUILDER, Claude"));

        game.nanny(conn, "y".into()).await;

        let descriptor = &game.state.descriptors[&conn];
        assert_eq!(descriptor.state, ConState::GetName);
        assert_eq!(descriptor.temp_name, None);
        assert!(descriptor.outbuf.contains("Invalid name"));
    }

    #[tokio::test]
    async fn existing_builder_account_still_reaches_the_password_prompt() {
        let db = Arc::new(MockDatabase::new());
        let builder = crate::character::Character::new_player(
            "Aclbuilder".into(),
            Class::Warrior,
            Race::Human,
        );
        db.create_player(&builder, "secret").await.unwrap();

        let mut game = test_game(db);
        game.state
            .zones
            .push(zone_with_builders("Michael Aclbuilder, Claude"));
        let conn = ConnId(103);
        attach_descriptor_at_name(&mut game, conn, "existing-builder.test").await;

        game.nanny(conn, "Aclbuilder".into()).await;

        assert_eq!(descriptor_state(&game, conn), ConState::GetOldPassword);
        assert_eq!(
            game.state.descriptors[&conn].temp_name.as_deref(),
            Some("Aclbuilder")
        );
    }

    #[tokio::test]
    async fn accepted_creation_stats_are_started_and_saved() {
        let db = Arc::new(MockDatabase::new());
        let seed = crate::character::Character::new_player(
            "Seed".to_string(),
            Class::Warrior,
            Race::Human,
        );
        db.create_player(&seed, "seedpass").await.unwrap();

        let mut game = test_game(db.clone());
        let configured_newbie_room = game.state.config.newbie_room;
        let configured_newbie_rnum = game.state.add_room(crate::room::Room::new(
            configured_newbie_room,
            2,
            "Configured newbie room".into(),
            String::new(),
        ));
        let obsolete_c_default_rnum = game.state.add_room(crate::room::Room::new(
            2200,
            22,
            "Obsolete C newbie room".into(),
            String::new(),
        ));
        let conn = ConnId(2);
        attach_descriptor_at_name(&mut game, conn, "example.test").await;

        game.nanny(conn, "Bob".to_string()).await;
        game.nanny(conn, "y".to_string()).await;
        game.nanny(conn, "secret".to_string()).await;
        game.nanny(conn, "secret".to_string()).await;
        assert!(game.state.descriptors[&conn].temp_password.is_none());
        assert!(
            game.state.descriptors[&conn]
                .password_hash
                .as_deref()
                .is_some_and(|hash| hash.starts_with("$argon2id$"))
        );
        game.nanny(conn, "y".to_string()).await;
        game.nanny(conn, "m".to_string()).await;
        game.nanny(conn, "d".to_string()).await;
        game.nanny(conn, "c".to_string()).await;
        game.nanny(conn, "b".to_string()).await;
        assert_eq!(descriptor_state(&game, conn), ConState::RollStats);
        let accepted = game.pending.get(&conn).unwrap().rolled;

        game.nanny(conn, "y".to_string()).await;
        // C start_player: creation ends at MOTD -> PRESS RETURN -> MENU (#198).
        assert_eq!(descriptor_state(&game, conn), ConState::ReadMotd);
        game.nanny(conn, String::new()).await;
        assert_eq!(descriptor_state(&game, conn), ConState::Menu);
        game.nanny(conn, "1".to_string()).await;
        assert_eq!(descriptor_state(&game, conn), ConState::Playing);
        let live_player = game.state.descriptors[&conn].character.unwrap();
        assert_eq!(
            game.state.get_char(live_player).unwrap().in_room,
            Some(configured_newbie_rnum),
            "new players must enter the configured newbie room"
        );
        assert_ne!(configured_newbie_rnum, obsolete_c_default_rnum);

        let saved = db.load_player("Bob").await.unwrap();
        assert_eq!(saved.idnum, 2);
        assert_eq!(saved.player.level, 1);
        assert_eq!(saved.trust, 1);
        assert_eq!(saved.points.exp, 1);
        assert_eq!(saved.player.sex, Gender::Male);
        assert_eq!(saved.race_index_for_test(), crate::races::RACE_DWARF);
        assert_eq!(saved.player.deity, crate::deity::DEITY_LYTHERN as u8);
        assert_eq!(saved.player.class, Class::Thief);
        assert_eq!(saved.player.hometown, 1);
        assert_eq!(saved.newbie, 1);
        assert_eq!(saved.clan, -1);
        assert_eq!(saved.clan_rank, -1);
        assert_eq!(saved.tloadroom, -1);
        assert_eq!(saved.real_abils.str, accepted.str);
        assert_eq!(saved.aff_abils.dex, accepted.dex);
        assert_eq!(saved.points.hit, saved.points.max_hit);
        assert_eq!(saved.points.mana, saved.points.max_mana);
        assert_eq!(saved.points.move_points, saved.points.max_move);
        assert_eq!(saved.conditions[THIRST], 24);
        assert_eq!(saved.conditions[FULL], 24);
        assert_eq!(saved.conditions[DRUNK], 0);
        assert!(saved.prf_flags & crate::flags::PRF_DISPHP != 0);
        assert!(saved.prf_flags & crate::flags::PRF_DISPMANA != 0);
        assert!(saved.prf_flags & crate::flags::PRF_DISPMOVE != 0);
        assert!(saved.prf_flags & crate::flags::PRF_DISPEXP != 0);
        assert!(saved.prf_flags & crate::flags::PRF_NOLOOKSTACK != 0);
        assert!(saved.prf2_flags & crate::flags::PRF2_DISPMOB != 0);
        assert!(db.verify_password("Bob", "secret").await.unwrap());
        assert!(!db.verify_password("Bob", "not-secret").await.unwrap());
        assert_eq!(
            db.get_password_hash("Bob").await.unwrap().as_deref(),
            game.state.descriptors[&conn].password_hash.as_deref(),
            "creation must persist the already-generated session hash without a second KDF"
        );
    }

    #[tokio::test]
    async fn first_created_character_is_not_implicitly_privileged() {
        let db = Arc::new(MockDatabase::new());
        let mut game = test_game(db.clone());
        let conn = ConnId(3);
        attach_descriptor_at_name(&mut game, conn, "example.test").await;

        game.nanny(conn, "First".to_string()).await;
        game.nanny(conn, "y".to_string()).await;
        game.nanny(conn, "secret".to_string()).await;
        game.nanny(conn, "secret".to_string()).await;
        game.nanny(conn, "y".to_string()).await;
        game.nanny(conn, "m".to_string()).await;
        game.nanny(conn, "a".to_string()).await;
        game.nanny(conn, "a".to_string()).await;
        game.nanny(conn, "c".to_string()).await;
        game.nanny(conn, "y".to_string()).await;
        game.nanny(conn, String::new()).await; // RMOTD -> MENU
        game.nanny(conn, "1".to_string()).await; // enter the game

        let saved = db.load_player("First").await.unwrap();
        assert_eq!(saved.idnum, 1);
        assert_eq!(saved.player.level, 1);
        assert_ne!(saved.player.title.as_deref(), Some("the Implementor"));
        assert_eq!(
            saved.godcmds1 | saved.godcmds2 | saved.godcmds3 | saved.godcmds4,
            0
        );
    }

    #[tokio::test]
    async fn ban_new_blocks_new_character_confirmation_by_ip() {
        let _guard = ban_test_lock();
        // The C build stores a zero-padded numeric host while slow DNS is
        // enabled (the production default). Rust stores SocketAddr's canonical
        // IP string, so boot_ban must migrate the persisted C-style mask.
        let lib = temp_ban_lib("new", "new 010.020.* 0 Root\n");
        let db = Arc::new(MockDatabase::new());
        let mut game = test_game(db);
        crate::ban::boot_ban(&mut game.state, &lib);
        let conn = ConnId(5);
        attach_descriptor_at_name(&mut game, conn, "10.20.30.40").await;

        game.nanny(conn, "Denied".to_string()).await;
        game.nanny(conn, "y".to_string()).await;

        assert_eq!(descriptor_state(&game, conn), ConState::Close);
        assert!(
            game.state
                .descriptors
                .get(&conn)
                .unwrap()
                .outbuf
                .contains("new characters are not allowed")
        );
        let empty = temp_ban_lib("empty-new", "");
        {
            let mut gtmp = crate::state::GameState::new(crate::config::Config::default());
            crate::ban::boot_ban(&mut gtmp, &empty);
        }
    }

    #[tokio::test]
    async fn ban_new_blocks_new_character_confirmation_by_verified_hostname() {
        let _guard = ban_test_lock();
        let lib = temp_ban_lib("new-hostname", "new *.blocked.example 0 Root\n");
        let db = Arc::new(MockDatabase::new());
        let mut game = test_game(db);
        crate::ban::boot_ban(&mut game.state, &lib);
        let conn = ConnId(51);
        attach_descriptor_identity_at_name(&mut game, conn, "192.0.2.51", "dialup.blocked.example")
            .await;

        game.nanny(conn, "HostDenied".to_string()).await;
        game.nanny(conn, "y".to_string()).await;

        assert_eq!(descriptor_state(&game, conn), ConState::Close);
        assert!(
            game.state
                .descriptors
                .get(&conn)
                .unwrap()
                .outbuf
                .contains("new characters are not allowed")
        );
        let empty = temp_ban_lib("empty-new-hostname", "");
        {
            let mut gtmp = crate::state::GameState::new(crate::config::Config::default());
            crate::ban::boot_ban(&mut gtmp, &empty);
        }
    }

    #[tokio::test]
    async fn ban_all_socket_gate_checks_verified_hostname_and_c_numeric_masks() {
        let _guard = ban_test_lock();
        let hostname_lib = temp_ban_lib("all-hostname", "all *.blocked.example 0 Root\n");
        let hostname_handle = {
            let mut gtmp = crate::state::GameState::new(crate::config::Config::default());
            crate::ban::boot_ban(&mut gtmp, &hostname_lib);
            gtmp.social.ban_handle.clone()
        };

        let peer_ip = "192.0.2.52".parse().unwrap();
        let (_client, mut unverified_stream) = tokio::io::duplex(128);
        assert!(
            !crate::connection::reject_ban_all(
                &mut unverified_stream,
                &crate::connection::PeerIdentity::numeric(peer_ip),
                &crate::ban::BanHandle::default(),
            )
            .await,
            "an unverified PTR name must never be trusted"
        );

        let identity = crate::connection::PeerIdentity {
            peer_ip,
            verified_hostname: Some("dialup.blocked.example".to_string()),
        };
        let (mut client, mut server) = tokio::io::duplex(128);
        assert!(crate::connection::reject_ban_all(&mut server, &identity, &hostname_handle).await);
        let mut rejection = Vec::new();
        client.read_to_end(&mut rejection).await.unwrap();
        assert_eq!(rejection, b"Your site is BANNED!\r\n");

        // C renders unresolved IPv4 peers as three-digit octets. Retaining and
        // checking that form is required for masks whose wildcard itself spans
        // a padded octet (it cannot be losslessly canonicalized to `10.*`).
        let numeric_lib = temp_ban_lib("all-c-numeric", "all 01?.020.* 0 Root\n");
        let numeric_handle = {
            let mut gtmp = crate::state::GameState::new(crate::config::Config::default());
            crate::ban::boot_ban(&mut gtmp, &numeric_lib);
            gtmp.social.ban_handle.clone()
        };
        let numeric_identity =
            crate::connection::PeerIdentity::numeric("10.20.30.40".parse().unwrap());
        let (mut client, mut server) = tokio::io::duplex(128);
        assert!(
            crate::connection::reject_ban_all(&mut server, &numeric_identity, &numeric_handle)
                .await
        );
        let mut rejection = Vec::new();
        client.read_to_end(&mut rejection).await.unwrap();
        assert_eq!(rejection, b"Your site is BANNED!\r\n");

        let empty = temp_ban_lib("empty-all-hostname", "");
        {
            let mut gtmp = crate::state::GameState::new(crate::config::Config::default());
            crate::ban::boot_ban(&mut gtmp, &empty);
        }
    }

    #[tokio::test]
    async fn ban_select_blocks_login_without_siteok_after_password_by_ip() {
        let _guard = ban_test_lock();
        let lib = temp_ban_lib("select", "select 192.0.2.44 0 Root\n");
        {
            let mut gtmp = crate::state::GameState::new(crate::config::Config::default());
            crate::ban::boot_ban(&mut gtmp, &lib);
        }

        let db = Arc::new(MockDatabase::new());
        let ch = crate::character::Character::new_player(
            "Blocked".to_string(),
            Class::Warrior,
            Race::Human,
        );
        db.create_player(&ch, "secret").await.unwrap();
        let mut game = test_game(db);
        crate::ban::boot_ban(&mut game.state, &lib);
        let conn = ConnId(6);
        attach_descriptor_at_name(&mut game, conn, "192.0.2.44").await;

        game.nanny(conn, "Blocked".to_string()).await;
        assert_eq!(descriptor_state(&game, conn), ConState::GetOldPassword);
        game.nanny(conn, "secret".to_string()).await;

        assert_eq!(descriptor_state(&game, conn), ConState::Close);
        assert!(
            game.state
                .descriptors
                .get(&conn)
                .unwrap()
                .outbuf
                .contains("has not been cleared for login")
        );
        let empty = temp_ban_lib("empty-select", "");
        {
            let mut gtmp = crate::state::GameState::new(crate::config::Config::default());
            crate::ban::boot_ban(&mut gtmp, &empty);
        }
    }

    #[tokio::test]
    async fn login_staff_exceptions_and_imotd_use_persisted_trust() {
        let db = Arc::new(MockDatabase::new());
        let mut display_high = crate::character::Character::new_player(
            "Displayhigh".to_string(),
            Class::Warrior,
            Race::Human,
        );
        display_high.player.level = LVL_IMPL;
        display_high.trust = 1;
        db.create_player(&display_high, "secret").await.unwrap();
        let mut game = test_game(db);
        game.state.motd = "MORTAL MOTD\r\n".into();
        game.state.imotd = "STAFF IMOTD\r\n".into();
        let conn = ConnId(61);
        attach_descriptor_at_name(&mut game, conn, "display-high.test").await;
        game.nanny(conn, "Displayhigh".into()).await;
        game.nanny(conn, "secret".into()).await;
        assert_eq!(descriptor_state(&game, conn), ConState::ReadMotd);
        let output = &game.state.descriptors[&conn].outbuf;
        assert!(output.contains("MORTAL MOTD"));
        assert!(!output.contains("STAFF IMOTD"));

        let db = Arc::new(MockDatabase::new());
        let mut trusted = crate::character::Character::new_player(
            "Trustedlow".to_string(),
            Class::Warrior,
            Race::Human,
        );
        trusted.player.level = 1;
        trusted.trust = i32::from(LVL_IMMORT);
        db.create_player(&trusted, "secret").await.unwrap();
        let mut game = test_game(db);
        game.state.motd = "MORTAL MOTD\r\n".into();
        game.state.imotd = "STAFF IMOTD\r\n".into();
        let conn = ConnId(62);
        attach_descriptor_at_name(&mut game, conn, "trusted-low.test").await;
        game.nanny(conn, "Trustedlow".into()).await;
        game.nanny(conn, "secret".into()).await;
        assert_eq!(descriptor_state(&game, conn), ConState::ReadMotd);
        let output = &game.state.descriptors[&conn].outbuf;
        assert!(output.contains("STAFF IMOTD"));
        assert!(!output.contains("MORTAL MOTD"));
    }

    #[test]
    fn complex_alias_expands_through_descriptor_queue_one_pulse_at_a_time() {
        let db = Arc::new(MockDatabase::new());
        let mut game = test_game(db);
        let conn = ConnId(4);
        let mut ch = crate::character::Character::new_player(
            "Aliaser".to_string(),
            Class::Warrior,
            Race::Human,
        );
        ch.idnum = 44;
        ch.desc = Some(conn);
        let cid = game.state.create_char(ch);

        crate::alias::set_aliases(
            &mut game.state,
            44,
            vec![crate::alias::AliasEntry {
                alias: "combo".to_string(),
                replacement: "bogus-one;bogus-two".to_string(),
                atype: 1,
            }],
        );

        let mut d = Descriptor::new(conn, "example.test".to_string());
        d.state = ConState::Playing;
        d.character = Some(cid);
        d.input_queue
            .push_back(QueuedInput::raw("combo".to_string()));
        game.state.descriptors.insert(conn, d);

        game.process_input_queues();
        let queued = &game.state.descriptors.get(&conn).unwrap().input_queue;
        assert_eq!(queued.len(), 1);
        assert_eq!(queued.front().unwrap().line, "bogus-two");
        assert!(queued.front().unwrap().aliased);

        game.process_input_queues();
        assert!(
            game.state
                .descriptors
                .get(&conn)
                .unwrap()
                .input_queue
                .is_empty()
        );
        crate::alias::clear_aliases(&mut game.state, 44);
    }

    trait RaceIndexForTest {
        fn race_index_for_test(&self) -> i32;
    }

    impl RaceIndexForTest for crate::character::Character {
        fn race_index_for_test(&self) -> i32 {
            self.player.race as u8 as i32
        }
    }

    #[tokio::test]
    async fn wrong_password_reprompts_then_disconnects_at_max_bad_pws() {
        let db = Arc::new(MockDatabase::new());
        let mut game = test_game(db.clone());
        let conn = ConnId(20);
        attach_descriptor_at_name(&mut game, conn, "example.test").await;

        let seed = crate::character::Character::new_player(
            "Pwtest".to_string(),
            Class::Warrior,
            Race::Human,
        );
        db.create_player(&seed, "right").await.unwrap();
        game.nanny(conn, "Pwtest".to_string()).await;
        assert_eq!(descriptor_state(&game, conn), ConState::GetOldPassword);

        // First failure: re-prompt (C max_bad_pws = 2) (#194).
        game.nanny(conn, "wrong".to_string()).await;
        assert_eq!(descriptor_state(&game, conn), ConState::GetOldPassword);
        assert!(
            game.state
                .descriptors
                .get(&conn)
                .unwrap()
                .outbuf
                .contains("Wrong password.")
        );
        assert_eq!(db.load_player("Pwtest").await.unwrap().bad_pws, 1);

        // Second failure: disconnect.
        game.nanny(conn, "wrong".to_string()).await;
        assert_eq!(descriptor_state(&game, conn), ConState::Close);
        assert!(
            game.state
                .descriptors
                .get(&conn)
                .unwrap()
                .outbuf
                .contains("Wrong password... disconnecting.")
        );
    }

    #[tokio::test]
    async fn duplicate_login_usurps_the_live_body() {
        let db = Arc::new(MockDatabase::new());
        let mut game = test_game(db.clone());
        let seed = crate::character::Character::new_player(
            "Dupe".to_string(),
            Class::Warrior,
            Race::Human,
        );
        db.create_player(&seed, "pw").await.unwrap();

        // First login walks all the way into the game.
        let c1 = ConnId(21);
        attach_descriptor_at_name(&mut game, c1, "a.test").await;
        game.nanny(c1, "Dupe".to_string()).await;
        game.nanny(c1, "pw".to_string()).await;
        game.nanny(c1, String::new()).await;
        game.nanny(c1, "1".to_string()).await;
        assert_eq!(descriptor_state(&game, c1), ConState::Playing);
        let body = game.state.descriptors.get(&c1).unwrap().character.unwrap();

        // Second login on another connection takes the body over (#218).
        let c2 = ConnId(22);
        attach_descriptor_at_name(&mut game, c2, "b.test").await;
        game.nanny(c2, "Dupe".to_string()).await;
        game.nanny(c2, "pw".to_string()).await;
        assert_eq!(descriptor_state(&game, c2), ConState::Playing);
        assert_eq!(
            game.state.descriptors.get(&c2).unwrap().character,
            Some(body)
        );
        // The old socket is detached and closing, with the usurp message.
        assert_eq!(game.state.descriptors.get(&c1).unwrap().character, None);
        assert_eq!(descriptor_state(&game, c1), ConState::Close);
        assert!(
            game.state
                .descriptors
                .get(&c1)
                .unwrap()
                .outbuf
                .contains("This body has been usurped!")
        );
        // Exactly one entity carries the idnum.
        let owners: Vec<CharId> = game
            .state
            .descriptors
            .values()
            .filter_map(|d| d.character)
            .collect();
        assert_eq!(owners, vec![body]);
    }

    #[tokio::test]
    async fn two_pending_menu_logins_materialize_one_body_and_one_rent_inventory() {
        let db = Arc::new(MockDatabase::new());
        let seed = crate::character::Character::new_player(
            "Pendingdupe".to_string(),
            Class::Warrior,
            Race::Human,
        );
        let idnum = db.create_player(&seed, "pw").await.unwrap();
        let record = db.load_player("Pendingdupe").await.unwrap();
        let mut game = test_game(db);
        game.lib_path = game.state.config.lib_path.clone();
        game.state.add_room(crate::room::Room::new(
            0,
            0,
            "The Void".into(),
            String::new(),
        ));

        let rent = crate::objsave::crash_filename(&game.lib_path, "Pendingdupe").unwrap();
        std::fs::create_dir_all(rent.parent().unwrap()).unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        std::fs::write(
            &rent,
            format!(
                "RENT 1 {now} 0 0 0\n\
                 OBJ 0 9001 9 1 0 1 1 1 -1 0 0 0 0 0 0 0 0 0 -1|token|a unique token|A unique token lies here.|\n"
            ),
        )
        .unwrap();

        let first = ConnId(221);
        let second = ConnId(222);
        for conn in [first, second] {
            attach_descriptor(&mut game, conn);
            let descriptor = game.state.descriptors.get_mut(&conn).unwrap();
            descriptor.temp_name = Some("Pendingdupe".into());
            descriptor.state = ConState::Menu;
            game.pending_load.insert(conn, record.clone());
        }

        // This is the original exploit ordering: both sessions already hold
        // the same loaded row at the menu, then both select enter-game.
        game.nanny(first, "1".to_string()).await;
        game.nanny(second, "1".to_string()).await;

        let bodies: Vec<_> = game
            .state
            .chars
            .iter()
            .filter(|(_, character)| !character.is_npc && character.idnum == idnum)
            .map(|(&cid, _)| cid)
            .collect();
        assert_eq!(bodies.len(), 1);
        let materialized: Vec<_> = game
            .state
            .objs
            .iter()
            .filter(|(_, object)| object.item_number == 9001)
            .map(|(&oid, _)| oid)
            .collect();
        assert_eq!(
            materialized.len(),
            1,
            "rent inventory loaded more than once"
        );
        assert_eq!(
            game.state.get_char(bodies[0]).unwrap().carrying,
            materialized
        );
        assert_eq!(descriptor_state(&game, first), ConState::Playing);
        assert_eq!(descriptor_state(&game, second), ConState::Close);
        assert!(game.pending_load.is_empty());
        assert!(
            game.state.descriptors[&second]
                .outbuf
                .contains("body was taken over")
        );
    }

    #[tokio::test]
    async fn duplicate_login_adopts_descriptorless_body_and_removes_duplicates() {
        let db = Arc::new(MockDatabase::new());
        let mut game = test_game(db);
        let conn = ConnId(24);
        attach_descriptor(&mut game, conn);
        game.state.descriptors.get_mut(&conn).unwrap().temp_name = Some("Orphan".into());

        let mut canonical = crate::character::Character::new_player(
            "Orphan".to_string(),
            Class::Warrior,
            Race::Human,
        );
        canonical.idnum = 4242;
        let canonical_id = game.state.create_char(canonical);
        game.state
            .players_by_name
            .insert("orphan".into(), canonical_id);

        let mut duplicate = crate::character::Character::new_player(
            "Orphan".to_string(),
            Class::Warrior,
            Race::Human,
        );
        duplicate.idnum = 4242;
        let duplicate_id = game.state.create_char(duplicate);

        assert!(game.perform_dupe_check(conn, 4242).await);
        assert_eq!(
            game.state.descriptors.get(&conn).unwrap().character,
            Some(canonical_id)
        );
        assert_eq!(descriptor_state(&game, conn), ConState::Playing);
        assert_eq!(game.state.get_char(canonical_id).unwrap().desc, Some(conn));
        assert!(!game.state.char_exists(duplicate_id));
        assert_eq!(
            game.state
                .chars
                .values()
                .filter(|ch| !ch.is_npc && ch.idnum == 4242)
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn duplicate_login_finds_a_switched_descriptors_original_body() {
        let db = Arc::new(MockDatabase::new());
        let mut game = test_game(db);
        let old_conn = ConnId(25);
        let new_conn = ConnId(26);
        attach_descriptor(&mut game, old_conn);
        attach_descriptor(&mut game, new_conn);
        game.state.descriptors.get_mut(&new_conn).unwrap().temp_name = Some("Switcher".into());

        let mut original = crate::character::Character::new_player(
            "Switcher".to_string(),
            Class::Warrior,
            Race::Human,
        );
        original.idnum = 4343;
        let original_id = game.state.create_char(original);
        game.state
            .players_by_name
            .insert("switcher".into(), original_id);
        let mut switched = crate::character::Character::new_player(
            "borrowed body".to_string(),
            Class::Warrior,
            Race::Human,
        );
        switched.is_npc = true;
        let switched_id = game.state.create_char(switched);
        game.state.get_char_mut(switched_id).unwrap().desc = Some(old_conn);
        {
            let descriptor = game.state.descriptors.get_mut(&old_conn).unwrap();
            descriptor.state = ConState::Playing;
            descriptor.character = Some(switched_id);
            descriptor.original = Some(original_id);
        }

        assert!(game.perform_dupe_check(new_conn, 4343).await);
        assert_eq!(
            game.state.descriptors.get(&new_conn).unwrap().character,
            Some(original_id)
        );
        let old = game.state.descriptors.get(&old_conn).unwrap();
        assert_eq!(old.state, ConState::Close);
        assert_eq!(old.character, None);
        assert_eq!(old.original, None);
        assert_eq!(game.state.get_char(switched_id).unwrap().desc, None);
    }

    #[tokio::test]
    async fn menu_option_zero_says_goodbye() {
        let db = Arc::new(MockDatabase::new());
        let mut game = test_game(db);
        let conn = ConnId(23);
        attach_descriptor_at_name(&mut game, conn, "example.test").await;
        game.nanny(conn, String::new()).await;
        // QANSI 'y' lands at GetName, not ReadMotd; walk: the ReadMotd arm
        // is reachable directly for this check.
        if let Some(d) = game.state.descriptors.get_mut(&conn) {
            d.state = ConState::ReadMotd;
        }
        game.nanny(conn, String::new()).await;
        assert_eq!(descriptor_state(&game, conn), ConState::Menu);
        game.nanny(conn, "9".to_string()).await;
        assert!(
            game.state
                .descriptors
                .get(&conn)
                .unwrap()
                .outbuf
                .contains("That's not a menu choice!")
        );
        assert_eq!(descriptor_state(&game, conn), ConState::Menu);
        game.nanny(conn, "0".to_string()).await;
        assert_eq!(descriptor_state(&game, conn), ConState::Close);
        assert!(
            game.state
                .descriptors
                .get(&conn)
                .unwrap()
                .outbuf
                .contains("land called reality")
        );
    }

    #[tokio::test]
    async fn input_doubles_dollars_and_supports_history() {
        let db = Arc::new(MockDatabase::new());
        let mut game = test_game(db);
        let conn = ConnId(30);
        attach_descriptor(&mut game, conn);
        let mut ch = crate::character::Character::new_player(
            "Hist".to_string(),
            Class::Warrior,
            Race::Human,
        );
        ch.desc = Some(conn);
        let cid = game.state.create_char(ch);
        game.state.descriptors.get_mut(&conn).unwrap().character = Some(cid);
        game.state.descriptors.get_mut(&conn).unwrap().state = ConState::Playing;

        // '$' is doubled on entry so act() renders one literal '$' (#222).
        game.handle_input(conn, "say Hi $n".to_string()).await;
        assert_eq!(
            game.state
                .descriptors
                .get(&conn)
                .unwrap()
                .input_queue
                .back()
                .map(|q| q.line.clone()),
            Some("say Hi $$n".to_string())
        );

        // '!' repeats the previous line, '^old^new' substitutes (#224).
        game.state
            .descriptors
            .get_mut(&conn)
            .unwrap()
            .input_queue
            .clear();
        game.handle_input(conn, "!".to_string()).await;
        assert_eq!(
            game.state
                .descriptors
                .get(&conn)
                .unwrap()
                .input_queue
                .back()
                .map(|q| q.line.clone()),
            Some("say Hi $$n".to_string())
        );
        game.handle_input(conn, "^Hi^Bye".to_string()).await;
        assert_eq!(
            game.state
                .descriptors
                .get(&conn)
                .unwrap()
                .input_queue
                .back()
                .map(|q| q.line.clone()),
            Some("say Bye $$n".to_string())
        );
        // Bad substitution refuses cleanly.
        game.handle_input(conn, "^zzz^qqq".to_string()).await;
        assert!(
            game.state
                .descriptors
                .get(&conn)
                .unwrap()
                .outbuf
                .contains("Invalid substitution.")
        );
    }

    #[tokio::test]
    async fn playing_input_queue_accepts_32_commands_and_closes_on_the_33rd() {
        let db = Arc::new(MockDatabase::new());
        let mut game = test_game(db);
        let conn = ConnId(129);
        attach_descriptor(&mut game, conn);
        let mut player = crate::character::Character::new_player(
            "Queuecap".to_string(),
            Class::Warrior,
            Race::Human,
        );
        player.desc = Some(conn);
        let player = game.state.create_char(player);
        let descriptor = game.state.descriptors.get_mut(&conn).unwrap();
        descriptor.character = Some(player);
        descriptor.state = ConState::Playing;

        for command in 0..32 {
            game.handle_input(conn, format!("look {command}")).await;
        }
        assert_eq!(game.state.descriptors[&conn].input_queue.len(), 32);
        assert_eq!(descriptor_state(&game, conn), ConState::Playing);

        game.handle_input(conn, "look overflow".to_string()).await;

        let descriptor = &game.state.descriptors[&conn];
        assert_eq!(descriptor.input_queue.len(), 32);
        assert_eq!(descriptor.state, ConState::Close);
        assert!(descriptor.outbuf.contains("Input queue full."));
        assert!(
            descriptor
                .input_queue
                .iter()
                .all(|queued| queued.line != "look overflow")
        );
    }

    #[tokio::test]
    async fn playing_input_truncates_multibyte_characters_at_the_byte_limit() {
        for (index, character) in ["é", "€", "🦀"].into_iter().enumerate() {
            let db = Arc::new(MockDatabase::new());
            let mut game = test_game(db);
            let conn = ConnId(130 + index as u64);
            attach_descriptor(&mut game, conn);
            let mut player = crate::character::Character::new_player(
                format!("Utf{index}"),
                Class::Warrior,
                Race::Human,
            );
            player.desc = Some(conn);
            let player = game.state.create_char(player);
            let descriptor = game.state.descriptors.get_mut(&conn).unwrap();
            descriptor.character = Some(player);
            descriptor.state = ConState::Playing;

            let input = format!("{}{}", "x".repeat(MAX_INPUT_LENGTH - 1), character);
            game.handle_input(conn, input).await;
            let queued = &game.state.descriptors[&conn].input_queue[0].line;
            assert!(queued.len() <= MAX_INPUT_LENGTH);
            assert!(std::str::from_utf8(queued.as_bytes()).is_ok());
            assert_eq!(queued, &"x".repeat(MAX_INPUT_LENGTH - 1));
            assert!(
                game.state.descriptors[&conn]
                    .outbuf
                    .contains("Line too long")
            );
        }
    }

    #[test]
    fn perform_subst_mirrors_the_c_semantics() {
        // C comm.c:1911-1960: '^telm^tell' repairs the typo in last_input.
        assert_eq!(
            Game::perform_subst("telm bob hello", "^telm^tell").as_deref(),
            Some("tell bob hello")
        );
        assert_eq!(
            Game::perform_subst("say Hi", "^Hi^Bye").as_deref(),
            Some("say Bye")
        );
        assert_eq!(Game::perform_subst("say Hi", "^zzz^qqq"), None);
        assert_eq!(Game::perform_subst("say Hi", "^Hi"), None);
        let replaced =
            Game::perform_subst(&format!("{}é", "x".repeat(MAX_INPUT_LENGTH - 1)), "^x^x").unwrap();
        assert!(replaced.len() <= MAX_INPUT_LENGTH);
        assert!(std::str::from_utf8(replaced.as_bytes()).is_ok());
    }

    #[test]
    fn scratch_name_debug() {
        let db = Arc::new(MockDatabase::new());
        let mut game = test_game(db);
        let name = game_normalize(&mut game, "Wanderer");
        println!("normalized: {:?}", name);
        println!("valid_name: {}", valid_name(&name));
        println!("reserved_or_fill: {}", reserved_or_fill_word(&name));
        println!(
            "ban::valid_name_in: {}",
            crate::ban::valid_name_in(&game.state, &name)
        );
    }

    fn game_normalize(game: &mut Game, s: &str) -> String {
        let _ = game;
        normalize_name(s)
    }

    #[test]
    fn reserved_and_fill_words_are_rejected_as_names() {
        // C interpreter.c:694-718 (#223).
        assert!(reserved_or_fill_word("me"));
        assert!(reserved_or_fill_word("all"));
        assert!(reserved_or_fill_word("something"));
        assert!(reserved_or_fill_word("the"));
        assert!(!reserved_or_fill_word("Thrall"));
    }

    fn test_mob_proto(vnum: MobVnum, name: &str) -> crate::world::MobileProto {
        crate::world::MobileProto {
            vnum,
            name: name.to_string(),
            short_desc: name.to_string(),
            long_desc: format!("{} is here.\r\n", name),
            description: String::new(),
            level: 1,
            hitpoints: 1,
            hit_dice: (0, 0, 1),
            experience: 0,
            gold: 0,
            position: Position::Standing,
            default_pos: Position::Standing,
            sex: Gender::Neutral,
            alignment: 0,
            act_flags: 0,
            affect_flags: 0,
            armor: 0,
            hitroll: 0,
            damroll: 0,
            damnodice: 1,
            damsizedice: 1,
            power: 0,
            mpower: 0,
            defense: 0,
            mdefense: 0,
            technique: 0,
            abilities: None,
            attack_type: 0,
        }
    }

    #[tokio::test]
    async fn mob_keyword_names_are_rejected() {
        let db = Arc::new(MockDatabase::new());
        let mut game = test_game(db);
        // A mob prototype whose keywords include "dragon".
        let proto = test_mob_proto(3001, "red dragon Dragon");
        game.state.mob_protos.insert(3001, proto);
        let conn = ConnId(40);
        attach_descriptor_at_name(&mut game, conn, "example.test").await;
        game.nanny(conn, "dragon".to_string()).await;
        // Still at GetName with the C refusal, not ConfirmName.
        assert_eq!(descriptor_state(&game, conn), ConState::GetName);
        assert!(
            game.state
                .descriptors
                .get(&conn)
                .unwrap()
                .outbuf
                .contains("Invalid name, please try another.")
        );
    }

    #[tokio::test]
    async fn quest_e2e_kill_quest_assigns_and_rewards() {
        // The shipped lib (sibling of the crate) carries the authored quest
        // content; skip on exotic checkouts without it.
        let lib = concat!(env!("CARGO_MANIFEST_DIR"), "/../lib");
        if !std::path::Path::new(&format!("{}/world/worldmap", lib)).exists() {
            return;
        }
        let mut g = crate::state::GameState::new(Config::default());
        g.config.lib_path = lib.to_string();
        crate::file_loader::FileLoader::load_world(&mut g, lib)
            .await
            .unwrap();
        g.prime_zones();

        let room100 = g.real_room(100).unwrap();
        let mut player =
            crate::character::Character::new_player("Rmeln".into(), Class::Warrior, Race::Human);
        player.player.level = 3;
        let pl = g.create_char(player);
        g.char_to_room(pl, room100);

        let qm = crate::quest::find_questmaster(&g, pl)
            .expect("questmaster must be present in room 100");

        // C denies probabilistically (qchance(15) + the 99-candidate lottery),
        // so retry until a target is assigned.
        // C rolls 50/50 between kill quests and object quests, denies
        // probabilistically, and locks out re-requests on deny — so retry,
        // clearing the deny lockout, until a KILL quest is assigned.
        let mut qmob = 0i32;
        for _ in 0..40 {
            crate::quest::do_autoquest(&mut g, pl, "request", 0);
            qmob = g.get_char(pl).unwrap().quest_mob;
            if qmob > 0 {
                break;
            }
            if let Some(c) = g.get_char_mut(pl) {
                c.next_quest = 0;
                c.quest_obj = 0; // drop an object-quest draw; we want a kill quest
                c.act_flags &= !(1 << 16); // PLR_QUESTOR
                c.quest_countdown = 0;
            }
        }
        assert!(
            qmob > 0,
            "a kill-target quest must be assigned, got {}",
            qmob
        );

        let victim = g
            .char_ids()
            .into_iter()
            .find(|c| g.get_char(*c).map(|c| c.nr == qmob).unwrap_or(false))
            .expect("target instance must exist");
        assert!(crate::quest::quest_on_kill(&mut g, pl, victim));
        assert!(g.get_char(pl).unwrap().quest_mob < 0);

        g.get_char_mut(pl).unwrap().quest_countdown = 5;
        crate::quest::do_autoquest(&mut g, pl, "complete", 0);
        let pts = g.get_char(pl).unwrap().quest_points;
        assert!(pts > 0, "reward quest points must be granted, got {}", pts);
        let _ = qm;
    }

    fn bare_sha256(password: &str) -> String {
        use sha2::{Digest, Sha256};
        Sha256::digest(password.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    #[tokio::test]
    async fn current_login_caches_the_exact_stored_hash_without_rehashing() {
        let db = Arc::new(MockDatabase::new());
        let character = crate::character::Character::new_player(
            "Currenthash".to_string(),
            Class::Warrior,
            Race::Human,
        );
        db.create_player(&character, "secret").await.unwrap();
        let stored = db.get_password_hash("Currenthash").await.unwrap().unwrap();
        let mut game = test_game(db);
        let conn = ConnId(501);
        attach_descriptor_at_name(&mut game, conn, "example.test").await;

        game.nanny(conn, "Currenthash".to_string()).await;
        game.nanny(conn, "secret".to_string()).await;

        assert_eq!(descriptor_state(&game, conn), ConState::ReadMotd);
        assert_eq!(
            game.state.descriptors[&conn].password_hash.as_deref(),
            Some(stored.as_str())
        );
        assert_eq!(
            game.db_get_password_hash("Currenthash").await.unwrap(),
            Some(stored)
        );
    }

    #[tokio::test]
    async fn legacy_login_upgrade_is_targeted_and_failure_does_not_block_login() {
        for fail_update in [false, true] {
            let db = Arc::new(MockDatabase::new());
            let character = crate::character::Character::new_player(
                "Legacyhash".to_string(),
                Class::Warrior,
                Race::Human,
            );
            db.create_player(&character, "secret").await.unwrap();
            let legacy = bare_sha256("secret");
            db.set_password_hash_for_test("Legacyhash", &legacy);
            if fail_update {
                db.fail_next_password_update();
            }
            let mut game = test_game(db.clone());
            let conn = ConnId(if fail_update { 503 } else { 502 });
            attach_descriptor_at_name(&mut game, conn, "example.test").await;

            game.nanny(conn, "Legacyhash".to_string()).await;
            game.nanny(conn, "secret".to_string()).await;

            assert_eq!(descriptor_state(&game, conn), ConState::ReadMotd);
            let durable = db.get_password_hash("Legacyhash").await.unwrap().unwrap();
            if fail_update {
                assert_eq!(durable, legacy);
            } else {
                assert_ne!(durable, legacy);
                assert!(durable.starts_with("$argon2id$"));
            }
            assert_eq!(
                game.state.descriptors[&conn].password_hash.as_deref(),
                Some(durable.as_str())
            );
        }
    }

    async fn password_change_menu_session(
        db: Arc<MockDatabase>,
        conn: ConnId,
    ) -> (Game, i64, String) {
        let character = crate::character::Character::new_player(
            "Menuaccount".to_string(),
            Class::Warrior,
            Race::Human,
        );
        let idnum = db.create_player(&character, "oldpass").await.unwrap();
        let old_hash = db.get_password_hash("Menuaccount").await.unwrap().unwrap();
        let mut game = test_game(db.clone());
        attach_descriptor_host(&mut game, conn, "example.test");
        let loaded = db.load_player("Menuaccount").await.unwrap();
        game.pending_load.insert(conn, loaded);
        let descriptor = game.state.descriptors.get_mut(&conn).unwrap();
        descriptor.temp_name = Some("Menuaccount".to_string());
        descriptor.temp_password = Some("newpass".to_string());
        descriptor.password_hash = Some(old_hash.clone());
        descriptor.password_change_expected_hash = Some(old_hash.clone());
        descriptor.state = ConState::ChPwdVerify;
        (game, idnum, old_hash)
    }

    #[tokio::test]
    async fn menu_password_change_publishes_success_only_after_targeted_update() {
        let db = Arc::new(MockDatabase::new());
        let conn = ConnId(504);
        let (mut game, _idnum, old_hash) = password_change_menu_session(db.clone(), conn).await;

        game.nanny(conn, "newpass".to_string()).await;

        assert_eq!(descriptor_state(&game, conn), ConState::Menu);
        assert!(game.state.descriptors[&conn].outbuf.contains("Done."));
        assert!(!db.verify_password("Menuaccount", "oldpass").await.unwrap());
        assert!(db.verify_password("Menuaccount", "newpass").await.unwrap());
        let durable = db.get_password_hash("Menuaccount").await.unwrap().unwrap();
        assert_ne!(durable, old_hash);
        assert_eq!(
            game.state.descriptors[&conn].password_hash.as_deref(),
            Some(durable.as_str())
        );
    }

    #[tokio::test]
    async fn menu_password_change_failure_keeps_old_credential_and_session_state() {
        let db = Arc::new(MockDatabase::new());
        let conn = ConnId(505);
        let (mut game, _idnum, old_hash) = password_change_menu_session(db.clone(), conn).await;
        db.fail_next_password_update();

        game.nanny(conn, "newpass".to_string()).await;

        assert_eq!(descriptor_state(&game, conn), ConState::Menu);
        assert!(!game.state.descriptors[&conn].outbuf.contains("Done."));
        assert!(
            game.state.descriptors[&conn]
                .outbuf
                .contains("requested password was not installed")
        );
        assert!(db.verify_password("Menuaccount", "oldpass").await.unwrap());
        assert!(!db.verify_password("Menuaccount", "newpass").await.unwrap());
        assert_eq!(
            game.state.descriptors[&conn].password_hash.as_deref(),
            Some(old_hash.as_str())
        );
    }

    #[tokio::test]
    async fn menu_password_change_cannot_overwrite_a_concurrent_security_reset() {
        let db = Arc::new(MockDatabase::new());
        let conn = ConnId(5_051);
        let (mut game, _idnum, old_hash) = password_change_menu_session(db.clone(), conn).await;
        let security_reset_hash = crate::password::hash_password("security-reset");
        db.set_password_hash_for_test("Menuaccount", &security_reset_hash);

        game.nanny(conn, "newpass".to_string()).await;

        assert_eq!(descriptor_state(&game, conn), ConState::Menu);
        assert!(!game.state.descriptors[&conn].outbuf.contains("Done."));
        assert!(
            game.state.descriptors[&conn]
                .outbuf
                .contains("changed during this operation")
        );
        assert_eq!(
            db.get_password_hash("Menuaccount")
                .await
                .unwrap()
                .as_deref(),
            Some(security_reset_hash.as_str())
        );
        assert!(!db.verify_password("Menuaccount", "newpass").await.unwrap());
        assert_eq!(
            game.state.descriptors[&conn].password_hash.as_deref(),
            Some(old_hash.as_str()),
            "the session cache must not claim the rejected credential is active"
        );
        assert!(
            game.state.descriptors[&conn]
                .password_change_expected_hash
                .is_none()
        );
    }

    fn attach_admin_password_target(
        game: &mut Game,
        admin_conn: ConnId,
        target_conn: ConnId,
        target: crate::character::Character,
    ) -> (CharId, CharId) {
        attach_descriptor_host(game, admin_conn, "admin.example.test");
        let mut admin = crate::character::Character::new_player(
            "Implementor".to_string(),
            Class::Warrior,
            Race::Human,
        );
        admin.idnum = 8001;
        admin.player.level = LVL_IMPL;
        admin.trust = i32::from(LVL_IMPL);
        (
            admin.godcmds1,
            admin.godcmds2,
            admin.godcmds3,
            admin.godcmds4,
        ) = crate::implementor_command_grants();
        admin.desc = Some(admin_conn);
        let admin_id = game.state.create_char(admin);
        game.state
            .players_by_name
            .insert("implementor".to_string(), admin_id);
        {
            let descriptor = game.state.descriptors.get_mut(&admin_conn).unwrap();
            descriptor.character = Some(admin_id);
            descriptor.state = ConState::Playing;
        }

        attach_descriptor_host(game, target_conn, "target.example.test");
        let mut target = target;
        let target_key = target.get_name().to_lowercase();
        target.desc = Some(target_conn);
        let target_id = game.state.create_char(target);
        game.state.players_by_name.insert(target_key, target_id);
        {
            let descriptor = game.state.descriptors.get_mut(&target_conn).unwrap();
            descriptor.character = Some(target_id);
            descriptor.state = ConState::Playing;
        }
        (admin_id, target_id)
    }

    #[tokio::test]
    async fn admin_password_update_reports_durable_success_and_updates_live_cache() {
        let db = Arc::new(MockDatabase::new());
        let target = crate::character::Character::new_player(
            "Admintarget".to_string(),
            Class::Warrior,
            Race::Human,
        );
        let idnum = db.create_player(&target, "oldpass").await.unwrap();
        let target = db.load_player("Admintarget").await.unwrap();
        assert_eq!(target.idnum, idnum);
        let mut game = test_game(db.clone());
        let (admin, target_id) =
            attach_admin_password_target(&mut game, ConnId(506), ConnId(507), target);

        run_authenticated_command(&mut game.state, admin, "set Admintarget passwd newpass");
        assert!(db.verify_password("Admintarget", "oldpass").await.unwrap());
        assert!(
            game.state
                .get_char(target_id)
                .unwrap()
                .pending_password_hash
                .is_none()
        );
        game.drain_password_update_requests().await;

        assert!(!db.verify_password("Admintarget", "oldpass").await.unwrap());
        assert!(db.verify_password("Admintarget", "newpass").await.unwrap());
        assert!(
            game.state.descriptors[&ConnId(506)]
                .outbuf
                .contains("Password changed for Admintarget.")
        );
        let live_hash = game.state.descriptors[&ConnId(507)]
            .password_hash
            .as_deref()
            .unwrap();
        assert!(crate::password::check_password(live_hash, "newpass"));
    }

    #[tokio::test]
    async fn admin_password_update_failure_keeps_old_credential_and_live_cache() {
        let db = Arc::new(MockDatabase::new());
        let target = crate::character::Character::new_player(
            "Admintarget".to_string(),
            Class::Warrior,
            Race::Human,
        );
        db.create_player(&target, "oldpass").await.unwrap();
        let target = db.load_player("Admintarget").await.unwrap();
        let old_hash = db.get_password_hash("Admintarget").await.unwrap().unwrap();
        let mut game = test_game(db.clone());
        let (admin, _) = attach_admin_password_target(&mut game, ConnId(508), ConnId(509), target);
        game.state
            .descriptors
            .get_mut(&ConnId(509))
            .unwrap()
            .password_hash = Some(old_hash.clone());
        db.fail_next_password_update();

        run_authenticated_command(&mut game.state, admin, "set Admintarget passwd newpass");
        game.drain_password_update_requests().await;

        assert!(db.verify_password("Admintarget", "oldpass").await.unwrap());
        assert!(!db.verify_password("Admintarget", "newpass").await.unwrap());
        assert_eq!(
            game.state.descriptors[&ConnId(509)]
                .password_hash
                .as_deref(),
            Some(old_hash.as_str())
        );
        assert!(
            !game.state.descriptors[&ConnId(508)]
                .outbuf
                .contains("Password changed for Admintarget.")
        );
        assert!(
            game.state.descriptors[&ConnId(508)]
                .outbuf
                .contains("requested credential was not active at durable readback")
        );
    }

    #[tokio::test]
    async fn password_drain_uses_target_trust_not_spoofable_display_level() {
        let db = Arc::new(MockDatabase::new());
        let mut target = crate::character::Character::new_player(
            "Admintarget".to_string(),
            Class::Warrior,
            Race::Human,
        );
        target.player.level = 1;
        target.trust = i32::from(LVL_GRGOD);
        let idnum = db.create_player(&target, "oldpass").await.unwrap();
        let target = db.load_player("Admintarget").await.unwrap();
        let mut game = test_game(db.clone());
        let (admin, target_id) =
            attach_admin_password_target(&mut game, ConnId(5_081), ConnId(5_082), target);
        let authorization = authenticated_request(&game, admin);
        game.state.queue_password_update(
            authorization,
            target_id,
            idnum,
            "Admintarget",
            "newpass".to_string(),
        );

        game.drain_password_update_requests().await;

        assert!(db.verify_password("Admintarget", "oldpass").await.unwrap());
        assert!(!db.verify_password("Admintarget", "newpass").await.unwrap());
        assert!(
            game.state.descriptors[&ConnId(5_081)]
                .outbuf
                .contains("authority or the player identity changed")
        );
    }

    #[tokio::test]
    async fn password_drain_rechecks_authenticated_requester_trust() {
        let db = Arc::new(MockDatabase::new());
        let target = crate::character::Character::new_player(
            "Admintarget".to_string(),
            Class::Warrior,
            Race::Human,
        );
        let idnum = db.create_player(&target, "oldpass").await.unwrap();
        let target = db.load_player("Admintarget").await.unwrap();
        let mut game = test_game(db.clone());
        let (admin, target_id) =
            attach_admin_password_target(&mut game, ConnId(5_083), ConnId(5_084), target);
        let authorization = authenticated_request(&game, admin);
        game.state.queue_password_update(
            authorization,
            target_id,
            idnum,
            "Admintarget",
            "newpass".to_string(),
        );
        let admin_record = game.state.get_char_mut(admin).unwrap();
        admin_record.player.level = LVL_IMPL;
        admin_record.trust = 1;

        game.drain_password_update_requests().await;

        assert!(db.verify_password("Admintarget", "oldpass").await.unwrap());
        assert!(!db.verify_password("Admintarget", "newpass").await.unwrap());
    }

    #[tokio::test]
    async fn password_update_rejects_a_descriptor_body_change_before_kdf() {
        let db = Arc::new(MockDatabase::new());
        let target = crate::character::Character::new_player(
            "Bodytarget".to_string(),
            Class::Warrior,
            Race::Human,
        );
        db.create_player(&target, "oldpass").await.unwrap();
        let target = db.load_player("Bodytarget").await.unwrap();
        let mut game = test_game(db.clone());
        let admin_conn = ConnId(5_085);
        let (admin, target_id) =
            attach_admin_password_target(&mut game, admin_conn, ConnId(5_086), target);

        run_authenticated_command(&mut game.state, admin, "set Bodytarget passwd newpass");
        assert_eq!(game.state.password_update_requests.len(), 1);
        game.state
            .descriptors
            .get_mut(&admin_conn)
            .unwrap()
            .character = Some(target_id);

        game.drain_password_update_requests().await;

        assert!(db.verify_password("Bodytarget", "oldpass").await.unwrap());
        assert!(!db.verify_password("Bodytarget", "newpass").await.unwrap());
    }

    #[tokio::test]
    async fn password_update_disconnect_during_kdf_cancels_the_durable_write() {
        let db = Arc::new(MockDatabase::new());
        let target = crate::character::Character::new_player(
            "Kdfvictim".to_string(),
            Class::Warrior,
            Race::Human,
        );
        db.create_player(&target, "oldpass").await.unwrap();
        let target = db.load_player("Kdfvictim").await.unwrap();
        let mut game = test_game(db.clone());
        let admin_conn = ConnId(5_087);
        let (admin, _) = attach_admin_password_target(&mut game, admin_conn, ConnId(5_088), target);

        run_authenticated_command(&mut game.state, admin, "set Kdfvictim passwd newpass");
        assert_eq!(game.state.password_update_requests.len(), 1);
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        game.game_rx = Some(rx);
        tx.send(GameMessage::Disconnect {
            conn_id: admin_conn,
        })
        .await
        .unwrap();

        game.drain_password_update_requests().await;

        assert!(!game.state.descriptors.contains_key(&admin_conn));
        assert!(db.verify_password("Kdfvictim", "oldpass").await.unwrap());
        assert!(!db.verify_password("Kdfvictim", "newpass").await.unwrap());
    }

    #[tokio::test]
    async fn offline_admin_password_update_skips_the_generic_character_save() {
        let db = Arc::new(MockDatabase::new());
        let target = crate::character::Character::new_player(
            "Offlinetarget".to_string(),
            Class::Warrior,
            Race::Human,
        );
        let idnum = db.create_player(&target, "oldpass").await.unwrap();
        let mut game = test_game(db.clone());
        attach_descriptor_host(&mut game, ConnId(510), "admin.example.test");
        let mut admin = crate::character::Character::new_player(
            "Implementor".to_string(),
            Class::Warrior,
            Race::Human,
        );
        admin.idnum = 8002;
        admin.player.level = LVL_IMPL;
        admin.trust = i32::from(LVL_IMPL);
        (
            admin.godcmds1,
            admin.godcmds2,
            admin.godcmds3,
            admin.godcmds4,
        ) = crate::implementor_command_grants();
        admin.desc = Some(ConnId(510));
        let admin = game.state.create_char(admin);
        game.state
            .players_by_name
            .insert("implementor".to_string(), admin);
        {
            let descriptor = game.state.descriptors.get_mut(&ConnId(510)).unwrap();
            descriptor.character = Some(admin);
            descriptor.state = ConState::Playing;
        }
        game.state
            .update_player_index(idnum, "Offlinetarget", 1, 0, "offline");
        // If the offline replay accidentally queues its historical broad save,
        // this injected failure is consumed. The typed password path must leave
        // it untouched.
        db.fail_next_save();

        run_authenticated_command(
            &mut game.state,
            admin,
            "set file Offlinetarget passwd newpass",
        );
        game.drain_offline_ops().await;
        assert_eq!(game.state.password_update_requests.len(), 1);
        game.drain_password_update_requests().await;

        assert!(
            !db.verify_password("Offlinetarget", "oldpass")
                .await
                .unwrap()
        );
        assert!(
            db.verify_password("Offlinetarget", "newpass")
                .await
                .unwrap()
        );
        assert!(game.state.find_player_by_name("Offlinetarget").is_none());
        let loaded = db.load_player("Offlinetarget").await.unwrap();
        assert!(
            db.save_player(&loaded).await.is_err(),
            "password-only replay must not consume the generic save failure"
        );
    }

    #[tokio::test]
    async fn creation_password_guards_match_c() {
        let db = Arc::new(MockDatabase::new());
        let mut game = test_game(db);
        let conn = ConnId(50);
        attach_descriptor_at_name(&mut game, conn, "example.test").await;

        game.nanny(conn, "Guard".to_string()).await;
        game.nanny(conn, "y".to_string()).await;
        // "New character." banner precedes the password prompt (C 1774).
        assert!(
            game.state
                .descriptors
                .get(&conn)
                .unwrap()
                .outbuf
                .contains("New character.")
        );

        // C interpreter.c:2043-2045: >64 chars, name-equality, and <3 all
        // refuse with 'Illegal password.' (#319).
        for bad in ["a", &"x".repeat(65), "Guard"] {
            game.nanny(conn, bad.to_string()).await;
            assert_eq!(descriptor_state(&game, conn), ConState::GetNewPassword);
            assert!(
                game.state
                    .descriptors
                    .get(&conn)
                    .unwrap()
                    .outbuf
                    .contains("Illegal password.")
            );
        }

        // A legal password proceeds; mismatch shows C's 'start over.' text.
        game.nanny(conn, "goodpw".to_string()).await;
        game.nanny(conn, "otherpw".to_string()).await;
        assert!(
            game.state
                .descriptors
                .get(&conn)
                .unwrap()
                .outbuf
                .contains("Passwords don't match... start over.")
        );
        assert_eq!(descriptor_state(&game, conn), ConState::GetNewPassword);
    }

    #[tokio::test]
    async fn name_lookup_error_never_enters_the_character_creation_flow() {
        let db = Arc::new(MockDatabase::new());
        let mut game = test_game(db.clone());
        let conn = ConnId(512);
        attach_descriptor_at_name(&mut game, conn, "example.test").await;
        db.fail_next_exists();

        game.nanny(conn, "Uncertain".to_string()).await;

        assert_eq!(descriptor_state(&game, conn), ConState::GetName);
        assert!(game.state.descriptors[&conn].temp_name.is_none());
        assert!(
            game.state.descriptors[&conn]
                .outbuf
                .contains("Unable to check that name right now; please try again.")
        );
        assert!(!db.player_exists("Uncertain").await.unwrap());
    }

    #[tokio::test]
    async fn sex_retry_uses_c_inline_prompt() {
        let db = Arc::new(MockDatabase::new());
        let mut game = test_game(db);
        let conn = ConnId(51);
        attach_descriptor_at_name(&mut game, conn, "example.test").await;
        game.nanny(conn, "Sexer".to_string()).await;
        game.nanny(conn, "y".to_string()).await;
        game.nanny(conn, "pw12345".to_string()).await;
        game.nanny(conn, "pw12345".to_string()).await;
        game.nanny(conn, "y".to_string()).await; // newbie
        game.nanny(conn, "q".to_string()).await; // invalid sex
        assert!(
            game.state
                .descriptors
                .get(&conn)
                .unwrap()
                .outbuf
                .contains("That is not a sex..\r\nWhat IS your sex? ")
        );
        assert_eq!(descriptor_state(&game, conn), ConState::GetSex);
    }
}

#[cfg(test)]
mod offline_inspection_tests {
    use super::tests::{attach_descriptor_host, test_game};
    use super::*;
    use crate::DatabaseInterface;
    use crate::character::Character;
    use crate::mock_database::MockDatabase;
    use crate::types::{Class, Race};
    use std::sync::Arc;

    const ROUTES: [(&str, &str); 3] = [
        ("stat-player", "IDNum:"),
        ("stat-file", "IDNum:"),
        ("show-player", "Player:"),
    ];

    fn attach_requester(game: &mut Game, conn: ConnId, level: u8) -> CharId {
        attach_descriptor_host(game, conn, "authority.example.test");
        let mut requester =
            Character::new_player("Requester".to_string(), Class::Warrior, Race::Human);
        requester.desc = Some(conn);
        requester.player.level = level;
        requester.trust = i32::from(level);
        requester.godcmds1 = !0;
        requester.godcmds2 = !0;
        requester.godcmds3 = !0;
        requester.godcmds4 = !0;
        let requester = game.state.create_char(requester);
        game.state
            .players_by_name
            .insert("requester".to_string(), requester);
        let descriptor = game.state.descriptors.get_mut(&conn).unwrap();
        descriptor.character = Some(requester);
        descriptor.state = ConState::Playing;
        requester
    }

    async fn seed_target(db: &MockDatabase, name: &str, level: u8) -> i64 {
        let mut target = Character::new_player(name.to_string(), Class::Warrior, Race::Human);
        target.player.level = level;
        target.trust = i32::from(level);
        db.create_player(&target, "secret").await.unwrap()
    }

    fn queue_route(game: &mut Game, requester: CharId, route: &str, target: &str) {
        match route {
            "stat-player" => crate::cmd_wizard::do_stat(
                &mut game.state,
                requester,
                &format!("player {target}"),
                0,
            ),
            "stat-file" => {
                crate::cmd_wizard::do_stat(&mut game.state, requester, &format!("file {target}"), 0)
            }
            "show-player" => crate::cmd_wizard::do_show(
                &mut game.state,
                requester,
                &format!("player {target}"),
                0,
            ),
            _ => panic!("unknown route {route}"),
        }
    }

    async fn queued_game(
        route: &str,
        indexed_level: u8,
        database_level: u8,
    ) -> (Game, Arc<MockDatabase>, CharId, i64) {
        let db = Arc::new(MockDatabase::new());
        let mut game = test_game(db.clone());
        game.lib_path = game.state.config.lib_path.clone();
        let requester = attach_requester(&mut game, ConnId(240), LVL_GOD);
        let idnum = seed_target(&db, "Target", database_level).await;
        game.state
            .update_player_index(idnum, "Target", indexed_level, 0, "offline");
        queue_route(&mut game, requester, route, "Target");
        (game, db, requester, idnum)
    }

    #[tokio::test]
    async fn replay_matrix_renders_lower_and_equal_targets_for_every_route() {
        for (route, record_marker) in ROUTES {
            for target_level in [LVL_GOD - 1, LVL_GOD] {
                let (mut game, _db, _requester, _idnum) =
                    queued_game(route, target_level, target_level).await;
                assert_eq!(game.state.offline_ops.len(), 1, "route={route}");

                game.drain_offline_ops().await;

                let output = &game.state.descriptors[&ConnId(240)].outbuf;
                assert!(
                    output.contains(record_marker),
                    "route={route} level={target_level} output={output:?}"
                );
                assert!(
                    !output.contains(PLAYER_INSPECTION_DENIED.trim()),
                    "route={route} level={target_level} output={output:?}"
                );
                assert!(game.state.find_player_by_name("Target").is_none());
            }
        }
    }

    #[tokio::test]
    async fn db_trust_change_between_queue_and_replay_is_denied_for_every_route() {
        for (route, record_marker) in ROUTES {
            let (mut game, db, _requester, _idnum) =
                queued_game(route, LVL_GOD - 1, LVL_GOD - 1).await;
            let mut changed = db.load_player("Target").await.unwrap();
            changed.trust = i32::from(LVL_GOD + 1);
            db.save_player(&changed).await.unwrap();

            game.drain_offline_ops().await;

            let output = &game.state.descriptors[&ConnId(240)].outbuf;
            assert!(
                output.contains(PLAYER_INSPECTION_DENIED.trim()),
                "route={route} output={output:?}"
            );
            assert!(
                !output.contains(record_marker),
                "route={route} leaked fields: {output:?}"
            );
            assert!(game.state.find_player_by_name("Target").is_none());
        }
    }

    #[tokio::test]
    async fn target_racing_online_at_higher_trust_is_denied_for_every_route() {
        for (route, record_marker) in ROUTES {
            let (mut game, _db, requester, idnum) =
                queued_game(route, LVL_GOD - 1, LVL_GOD - 1).await;
            let mut target =
                Character::new_player("Target".to_string(), Class::Warrior, Race::Human);
            target.idnum = idnum;
            target.player.level = 1;
            target.trust = i32::from(LVL_GOD + 1);
            let live_target = game.state.create_char(target);
            game.state
                .players_by_name
                .insert("target".to_string(), live_target);

            game.drain_offline_ops().await;

            let output = &game.state.descriptors[&ConnId(240)].outbuf;
            assert!(output.contains(PLAYER_INSPECTION_DENIED.trim()));
            assert!(
                !output.contains(record_marker),
                "route={route} output={output:?}"
            );
            assert!(game.state.char_exists(requester));
            assert!(game.state.char_exists(live_target));
        }
    }

    #[tokio::test]
    async fn disconnected_requester_cancels_every_inspection_without_loading_target() {
        for (route, record_marker) in ROUTES {
            let (mut game, _db, requester, _idnum) =
                queued_game(route, LVL_GOD - 1, LVL_GOD - 1).await;
            game.state.extract_char(requester);

            game.drain_offline_ops().await;

            let output = &game.state.descriptors[&ConnId(240)].outbuf;
            assert!(
                !output.contains(record_marker),
                "route={route} output={output:?}"
            );
            assert!(!output.contains(PLAYER_INSPECTION_DENIED.trim()));
            assert!(game.state.find_player_by_name("Target").is_none());
            assert!(game.state.offline_ops.is_empty());
        }
    }
}

#[cfg(test)]
mod self_delete_tests {
    use super::tests::{attach_descriptor_host, persistent_connected_player, test_game};
    use super::*;
    use crate::DatabaseInterface;
    use crate::alias::AliasEntry;
    use crate::character::Character;
    use crate::mock_database::MockDatabase;
    use crate::types::{Class, Race};
    use std::path::PathBuf;
    use std::sync::Arc;

    async fn deletion_session(
        conn: ConnId,
        name: &str,
        act_flags: i64,
    ) -> (Game, Arc<MockDatabase>, i64) {
        let db = Arc::new(MockDatabase::new());
        let mut game = test_game(db.clone());
        game.lib_path = game.state.config.lib_path.clone();
        let mut character = Character::new_player(name.to_string(), Class::Warrior, Race::Human);
        character.act_flags = act_flags;
        db.create_player(&character, "secret").await.unwrap();
        let idnum = 9_413_000 + conn.0 as i64;
        character.idnum = idnum;
        db.save_player(&character).await.unwrap();

        attach_descriptor_host(&mut game, conn, "delete.example.test");
        let descriptor = game.state.descriptors.get_mut(&conn).unwrap();
        descriptor.temp_name = Some(name.to_string());
        descriptor.state = ConState::DelCnf2;
        game.pending_load.insert(conn, character);
        (game, db, idnum)
    }

    fn seed_sidecars(game: &mut Game, name: &str, idnum: i64) -> (PathBuf, PathBuf) {
        let rent = crate::objsave::crash_filename(&game.lib_path, name).unwrap();
        std::fs::create_dir_all(rent.parent().unwrap()).unwrap();
        std::fs::write(&rent, b"rent evidence").unwrap();

        crate::alias::set_aliases(
            &mut game.state,
            idnum,
            vec![AliasEntry {
                alias: "stale".to_string(),
                replacement: "say private text".to_string(),
                atype: 0,
            }],
        );
        crate::alias::write_aliases(&game.state, &game.lib_path, name, idnum).unwrap();
        let alias = crate::alias::alias_filename(&game.lib_path, name).unwrap();
        (rent, alias)
    }

    #[tokio::test]
    async fn confirmed_delete_removes_mixed_case_sidecars_and_name_reuse_is_clean() {
        let conn = ConnId(210);
        let name = "MiXeDcase";
        let (mut game, db, idnum) = deletion_session(conn, name, 0).await;
        let (rent, alias) = seed_sidecars(&mut game, name, idnum);

        game.nanny(conn, "yes".to_string()).await;

        assert!(!rent.exists());
        assert!(!alias.exists());
        assert!(crate::alias::get_aliases(&game.state, idnum).is_empty());
        assert_ne!(
            db.load_player(name).await.unwrap().act_flags & crate::flags::PLR_DELETED,
            0
        );
        assert!(
            game.state.descriptors[&conn]
                .outbuf
                .contains("Character 'MiXeDcase' deleted!")
        );

        let reused_idnum = idnum + 10_000;
        crate::alias::read_aliases(&mut game.state, &game.lib_path, "mixedCASE", reused_idnum)
            .unwrap();
        assert!(crate::alias::get_aliases(&game.state, reused_idnum).is_empty());
    }

    #[tokio::test]
    async fn confirmed_delete_treats_missing_sidecars_as_already_clean() {
        let conn = ConnId(211);
        let name = "Nosidecars";
        let (mut game, db, _) = deletion_session(conn, name, 0).await;

        game.nanny(conn, "YES".to_string()).await;

        assert_ne!(
            db.load_player(name).await.unwrap().act_flags & crate::flags::PLR_DELETED,
            0
        );
        assert!(game.state.descriptors[&conn].outbuf.contains("deleted!"));
    }

    #[tokio::test]
    async fn sidecar_cleanup_failure_is_audited_without_false_success() {
        let conn = ConnId(212);
        let name = "Blockedfiles";
        let (mut game, db, idnum) = deletion_session(conn, name, 0).await;
        let rent = crate::objsave::crash_filename(&game.lib_path, name).unwrap();
        let alias = crate::alias::alias_filename(&game.lib_path, name).unwrap();
        std::fs::create_dir_all(&rent).unwrap();
        std::fs::create_dir_all(&alias).unwrap();
        crate::alias::set_aliases(
            &mut game.state,
            idnum,
            vec![AliasEntry {
                alias: "cached".to_string(),
                replacement: "say stale".to_string(),
                atype: 0,
            }],
        );

        game.nanny(conn, "yes".to_string()).await;

        assert_ne!(
            db.load_player(name).await.unwrap().act_flags & crate::flags::PLR_DELETED,
            0,
            "the durable tombstone is authoritative"
        );
        let output = &game.state.descriptors[&conn].outbuf;
        assert!(output.contains("cleanup is incomplete"));
        assert!(!output.contains("Character 'Blockedfiles' deleted!"));
        assert!(rent.is_dir());
        assert!(alias.is_dir());
        assert!(crate::alias::get_aliases(&game.state, idnum).is_empty());

        std::fs::remove_dir(alias).unwrap();
        std::fs::remove_dir(rent).unwrap();
    }

    #[tokio::test]
    async fn frozen_aborted_and_failed_database_deletes_preserve_sidecars() {
        let frozen_conn = ConnId(213);
        let (mut frozen, frozen_db, frozen_id) =
            deletion_session(frozen_conn, "Frozenone", crate::flags::PLR_FROZEN).await;
        let (frozen_rent, frozen_alias) = seed_sidecars(&mut frozen, "Frozenone", frozen_id);
        frozen.nanny(frozen_conn, "yes".to_string()).await;
        assert!(frozen_rent.exists() && frozen_alias.exists());
        assert_eq!(
            frozen_db.load_player("Frozenone").await.unwrap().act_flags & crate::flags::PLR_DELETED,
            0
        );

        let aborted_conn = ConnId(214);
        let (mut aborted, aborted_db, aborted_id) =
            deletion_session(aborted_conn, "Abortone", 0).await;
        let (aborted_rent, aborted_alias) = seed_sidecars(&mut aborted, "Abortone", aborted_id);
        aborted.nanny(aborted_conn, "no".to_string()).await;
        assert!(aborted_rent.exists() && aborted_alias.exists());
        assert_eq!(
            aborted_db.load_player("Abortone").await.unwrap().act_flags & crate::flags::PLR_DELETED,
            0
        );
        assert_eq!(
            aborted.state.descriptors[&aborted_conn].state,
            ConState::Menu
        );

        let failed_conn = ConnId(215);
        let (mut failed, failed_db, failed_id) =
            deletion_session(failed_conn, "Savefailure", 0).await;
        let (failed_rent, failed_alias) = seed_sidecars(&mut failed, "Savefailure", failed_id);
        failed_db.fail_next_save();
        failed.nanny(failed_conn, "yes".to_string()).await;
        assert!(failed_rent.exists() && failed_alias.exists());
        assert_eq!(
            failed_db
                .load_player("Savefailure")
                .await
                .unwrap()
                .act_flags
                & crate::flags::PLR_DELETED,
            0
        );
        assert!(
            failed.state.descriptors[&failed_conn]
                .outbuf
                .contains("no files were removed")
        );

        for path in [
            frozen_rent,
            frozen_alias,
            aborted_rent,
            aborted_alias,
            failed_rent,
            failed_alias,
        ] {
            std::fs::remove_file(path).unwrap();
        }
    }

    #[tokio::test]
    async fn self_delete_protects_persisted_staff_trust_before_sidecar_cleanup() {
        let conn = ConnId(216);
        let name = "Trustedstaff";
        let (mut game, db, idnum) = deletion_session(conn, name, 0).await;
        let (rent, alias) = seed_sidecars(&mut game, name, idnum);
        let mut durable = db.load_player(name).await.unwrap();
        durable.player.level = 1;
        durable.trust = i32::from(LVL_GRGOD);
        db.save_player(&durable).await.unwrap();

        game.nanny(conn, "yes".to_string()).await;

        assert_eq!(
            db.load_player(name).await.unwrap().act_flags & crate::flags::PLR_DELETED,
            0
        );
        assert!(rent.exists() && alias.exists());
        assert!(
            game.state.descriptors[&conn]
                .outbuf
                .contains("Privileged characters cannot self-delete")
        );

        std::fs::remove_file(rent).unwrap();
        std::fs::remove_file(alias).unwrap();
    }

    #[tokio::test]
    async fn administrative_pfileclean_removes_sidecars_before_deleting_the_db_row() {
        let db = Arc::new(MockDatabase::new());
        let mut character =
            Character::new_player("AdminGone".to_string(), Class::Warrior, Race::Human);
        character.act_flags |= crate::flags::PLR_DELETED;
        db.create_player(&character, "secret").await.unwrap();
        let idnum = 9_413_301;
        character.idnum = idnum;
        db.save_player(&character).await.unwrap();
        let mut game = test_game(db.clone());
        game.lib_path = game.state.config.lib_path.clone();
        let (rent, alias) = seed_sidecars(&mut game, "AdminGone", idnum);
        let cleaner = persistent_connected_player(
            &mut game,
            db.as_ref(),
            ConnId(217),
            "Cleanerone",
            LVL_IMPL,
        )
        .await;

        run_authenticated_command(&mut game.state, cleaner, "pfileclean OptimisePfile");
        game.drain_pfileclean().await;

        assert!(db.load_player("AdminGone").await.is_err());
        assert!(!rent.exists() && !alias.exists());
        assert!(crate::alias::get_aliases(&game.state, idnum).is_empty());
    }

    #[tokio::test]
    async fn pfileclean_disconnect_during_discovery_preserves_rows_and_sidecars() {
        let db = Arc::new(MockDatabase::new());
        let mut deleted =
            Character::new_player("CleanRace".to_string(), Class::Warrior, Race::Human);
        deleted.act_flags |= crate::flags::PLR_DELETED;
        let idnum = db.create_player(&deleted, "secret").await.unwrap();
        let mut game = test_game(db.clone());
        game.lib_path = game.state.config.lib_path.clone();
        let (rent, alias) = seed_sidecars(&mut game, "CleanRace", idnum);
        let cleaner_conn = ConnId(9_413_303);
        let cleaner = persistent_connected_player(
            &mut game,
            db.as_ref(),
            cleaner_conn,
            "Cleanerrace",
            LVL_IMPL,
        )
        .await;

        run_authenticated_command(&mut game.state, cleaner, "pfileclean OptimisePfile");
        assert!(game.state.pfileclean_requested.is_some());
        db.set_list_delay(Some(Duration::from_millis(50)));
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        game.game_rx = Some(rx);
        tx.send(GameMessage::Disconnect {
            conn_id: cleaner_conn,
        })
        .await
        .unwrap();

        game.drain_pfileclean().await;
        db.set_list_delay(None);

        assert!(db.load_player("CleanRace").await.is_ok());
        assert!(rent.is_file() && alias.is_file());
        crate::alias::clear_aliases(&mut game.state, idnum);
        std::fs::remove_file(rent).unwrap();
        std::fs::remove_file(alias).unwrap();
    }

    #[tokio::test]
    async fn administrative_pfileclean_retains_tombstone_until_sidecar_failure_is_fixed() {
        let db = Arc::new(MockDatabase::new());
        let mut character =
            Character::new_player("AdminRetry".to_string(), Class::Warrior, Race::Human);
        character.act_flags |= crate::flags::PLR_DELETED;
        db.create_player(&character, "secret").await.unwrap();
        let idnum = 9_413_302;
        character.idnum = idnum;
        db.save_player(&character).await.unwrap();
        let mut game = test_game(db.clone());
        game.lib_path = game.state.config.lib_path.clone();
        let rent = crate::objsave::crash_filename(&game.lib_path, "AdminRetry").unwrap();
        std::fs::create_dir_all(&rent).unwrap();
        crate::alias::set_aliases(
            &mut game.state,
            idnum,
            vec![AliasEntry {
                alias: "private".into(),
                replacement: "say retained until audited".into(),
                atype: 0,
            }],
        );
        crate::alias::write_aliases(&game.state, &game.lib_path, "AdminRetry", idnum).unwrap();
        let alias = crate::alias::alias_filename(&game.lib_path, "AdminRetry").unwrap();
        let cleaner = persistent_connected_player(
            &mut game,
            db.as_ref(),
            ConnId(218),
            "Cleanertwo",
            LVL_IMPL,
        )
        .await;

        run_authenticated_command(&mut game.state, cleaner, "pfileclean OptimisePfile");
        game.drain_pfileclean().await;

        let retained = db.load_player("AdminRetry").await.unwrap();
        assert_ne!(retained.act_flags & crate::flags::PLR_DELETED, 0);
        assert!(rent.is_dir());
        assert!(!alias.exists(), "successful cleanup steps still converge");
        assert!(crate::alias::get_aliases(&game.state, idnum).is_empty());

        std::fs::remove_dir(&rent).unwrap();
        run_authenticated_command(&mut game.state, cleaner, "pfileclean OptimisePfile");
        game.drain_pfileclean().await;
        assert!(db.load_player("AdminRetry").await.is_err());
    }
}

#[cfg(test)]
mod queued_admin_request_tests {
    use super::tests::{persistent_connected_player, test_game};
    use super::*;
    use crate::DatabaseInterface;
    use crate::character::Character;
    use crate::mock_database::MockDatabase;
    use std::sync::Arc;

    #[tokio::test]
    async fn queued_destructive_actions_revalidate_session_trust_and_grants() {
        let db = Arc::new(MockDatabase::new());
        let mut game = test_game(db.clone());
        let root = std::env::temp_dir().join(format!(
            "deltamud-admin-request-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("lib")).unwrap();
        game.lib_path = root.join("lib").to_string_lossy().into_owned();
        game.state.config.lib_path = game.lib_path.clone();

        let staff = persistent_connected_player(
            &mut game,
            db.as_ref(),
            ConnId(8_801),
            "Queuedstaff",
            LVL_IMPL,
        )
        .await;

        run_authenticated_command(&mut game.state, staff, "copyover");
        assert!(game.state.copyover_requested.is_some());
        game.state.get_char_mut(staff).unwrap().godcmds3 &= !crate::gcmd::GCMD3_COPYOVER;
        assert_eq!(game.take_authorized_copyover_request(), None);
        assert!(
            game.state.descriptors[&ConnId(8_801)]
                .outbuf
                .contains("Copyover canceled")
        );

        game.state.get_char_mut(staff).unwrap().godcmds3 |= crate::gcmd::GCMD3_COPYOVER;
        run_authenticated_command(&mut game.state, staff, "shutdown die");
        assert!(game.state.shutdown_requested.is_some());
        assert!(!root.join(".killscript").exists());
        game.state.get_char_mut(staff).unwrap().trust = 1;
        assert_eq!(game.take_authorized_shutdown_request(), None);
        assert!(!root.join(".killscript").exists());

        let mut deleted =
            Character::new_player("Queuedgone".to_string(), Class::Warrior, Race::Human);
        deleted.act_flags |= crate::flags::PLR_DELETED;
        db.create_player(&deleted, "secret").await.unwrap();
        {
            let staff = game.state.get_char_mut(staff).unwrap();
            staff.trust = i32::from(LVL_IMPL);
            staff.godcmds3 |= crate::gcmd::GCMD3_PFILECLEAN;
        }
        run_authenticated_command(&mut game.state, staff, "pfileclean OptimisePfile");
        assert!(game.state.pfileclean_requested.is_some());
        game.state.get_char_mut(staff).unwrap().godcmds3 &= !crate::gcmd::GCMD3_PFILECLEAN;
        game.drain_pfileclean().await;
        assert!(db.load_player("Queuedgone").await.is_ok());

        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod gmcp_tests {
    use super::tests::test_game;
    use super::*;
    use crate::character::Character;
    use crate::mock_database::MockDatabase;
    use crate::room::{Exit, Room};
    use crate::types::{Class, Race};
    use std::sync::Arc;

    #[test]
    fn movement_marks_gmcp_dirty_and_heartbeat_drains_it() {
        let mut game = test_game(Arc::new(MockDatabase::new()));
        let a = game
            .state
            .add_room(Room::new(100, 1, "A".into(), String::new()));
        let b = game
            .state
            .add_room(Room::new(101, 1, "B".into(), String::new()));
        let conn = ConnId(60);
        game.state
            .descriptors
            .insert(conn, Descriptor::new(conn, "t".into()));
        let ch = playing_char(&mut game, conn, "Gmcp", a);

        // A room transfer marks the mover and the bystanders stale.
        game.state.char_from_room(ch);
        game.state.char_to_room(ch, b);
        assert!(
            game.state.gmcp_dirty.contains(&conn),
            "transfer must mark the mover's connection dirty"
        );

        // The heartbeat drain pushes a snapshot and empties the set.
        game.heartbeat_inner();
        assert!(game.state.gmcp_dirty.is_empty(), "drain must clear the set");
    }

    #[test]
    fn gmcp_room_info_carries_doors_and_valid_json_names() {
        let mut game = test_game(Arc::new(MockDatabase::new()));
        let a = game.state.add_room(Room::new(
            100,
            1,
            "The \"Quoted\" &RRoom".into(),
            String::new(),
        ));
        let b = game
            .state
            .add_room(Room::new(101, 1, "B".into(), String::new()));
        game.state.rooms[a].exits[EAST] = Some(Exit {
            description: None,
            keyword: None,
            exit_info: crate::room::EX_CLOSED | crate::room::EX_LOCKED,
            key: -1,
            to_room: 101,
        });
        game.state.rooms[b].exits[WEST] = Some(Exit {
            description: None,
            keyword: None,
            exit_info: 0,
            key: -1,
            to_room: 100,
        });
        let conn = ConnId(61);
        game.state
            .descriptors
            .insert(conn, Descriptor::new(conn, "t".into()));
        playing_char(&mut game, conn, "Doors", a);

        let messages = game.gmcp_snapshots(conn);
        let room_info = messages
            .iter()
            .find(|m| m.starts_with("Room.Info "))
            .expect("Room.Info must be part of the snapshot");
        let json = room_info.split_once(' ').unwrap().1;
        let value: serde_json::Value =
            serde_json::from_str(json).expect("Room.Info must be valid JSON");
        assert_eq!(value["num"], 100);
        assert_eq!(
            value["name"], "The \"Quoted\" Room",
            "&R color code stripped"
        );
        assert_eq!(value["exits"]["e"], 101);
        assert_eq!(value["doors"][0], "e");
        assert_eq!(value["locked"][0], "e");
    }

    #[test]
    fn combat_damage_marks_both_sides_dirty() {
        let mut game = test_game(Arc::new(MockDatabase::new()));
        let a = game
            .state
            .add_room(Room::new(100, 1, "A".into(), String::new()));
        let conn = ConnId(63);
        game.state
            .descriptors
            .insert(conn, Descriptor::new(conn, "t".into()));
        let ch = playing_char(&mut game, conn, "Punched", a);
        let mut npc = Character::new_npc(500);
        npc.position = crate::types::Position::Standing;
        let npc = game.state.create_char(npc);
        game.state.char_to_room(npc, a);

        game.state.gmcp_dirty.clear();
        crate::combat::damage(&mut game.state, npc, ch, 5);
        assert!(
            game.state.gmcp_dirty.contains(&conn),
            "damage must stale the victim's vitals"
        );

        // Snapshot contains fresh vitals.
        let messages = game.gmcp_snapshots(conn);
        assert!(messages.iter().any(|m| m.starts_with("Char.Vitals ")));
    }

    #[test]
    fn non_gmcp_descriptors_get_no_snapshots() {
        let mut game = test_game(Arc::new(MockDatabase::new()));
        let a = game
            .state
            .add_room(Room::new(100, 1, "A".into(), String::new()));
        let conn = ConnId(64);
        game.state
            .descriptors
            .insert(conn, Descriptor::new(conn, "t".into()));
        playing_char(&mut game, conn, "Plain", a);
        game.state.descriptors.get_mut(&conn).unwrap().gmcp = false;

        // Core metadata without a negotiated DO must not enable GMCP or retain
        // attacker-controlled client state.
        game.handle_gmcp_event(
            conn,
            crate::connection::GmcpClientEvent::Hello {
                client_name: "Unnegotiated".into(),
                client_version: "1".into(),
            },
        );

        assert!(game.gmcp_snapshots(conn).is_empty());
        assert_eq!(
            game.state.descriptors.get(&conn).unwrap().gmcp_client,
            crate::connection::GmcpClientState::default()
        );
        let (output_tx, mut output_rx) = mpsc::channel(4);
        game.outputs.insert(conn, output_tx);
        game.push_gmcp_update(conn);
        assert!(matches!(
            output_rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        // Marking still happens (cheap) but the drain filters by d.gmcp.
        game.state.note_gmcp_room(a);
        game.heartbeat_inner();
        assert!(game.state.gmcp_dirty.is_empty(), "drain clears everything");
    }

    #[test]
    fn disabling_gmcp_clears_capabilities_and_stops_all_snapshots() {
        let mut game = test_game(Arc::new(MockDatabase::new()));
        let room = game
            .state
            .add_room(Room::new(100, 1, "A".into(), String::new()));
        let conn = ConnId(65);
        game.state
            .descriptors
            .insert(conn, Descriptor::new(conn, "t".into()));
        playing_char(&mut game, conn, "FormerGmcp", room);

        game.handle_gmcp_event(
            conn,
            crate::connection::GmcpClientEvent::Hello {
                client_name: "Mudlet".into(),
                client_version: "4.18.5".into(),
            },
        );
        game.handle_gmcp_event(
            conn,
            crate::connection::GmcpClientEvent::SupportsSet(
                [("char".to_string(), 1)].into_iter().collect(),
            ),
        );
        assert!(!game.gmcp_snapshots(conn).is_empty());

        game.handle_gmcp_event(conn, crate::connection::GmcpClientEvent::Disabled);
        let descriptor = game.state.descriptors.get(&conn).unwrap();
        assert!(!descriptor.gmcp);
        assert_eq!(
            descriptor.gmcp_client,
            crate::connection::GmcpClientState::default()
        );
        assert!(game.gmcp_snapshots(conn).is_empty());
    }

    fn playing_char(game: &mut Game, conn: ConnId, name: &str, room: usize) -> CharId {
        let mut ch = Character::new_player(name.to_string(), Class::Warrior, Race::Human);
        ch.player.level = 10;
        let cid = game.state.create_char(ch);
        game.state.char_to_room(cid, room);
        let d = game.state.descriptors.get_mut(&conn).unwrap();
        d.gmcp = true;
        d.state = ConState::Playing;
        d.character = Some(cid);
        if let Some(c) = game.state.get_char_mut(cid) {
            c.desc = Some(conn);
        }
        cid
    }
}

#[cfg(test)]
mod bounded_output_tests {
    use super::tests::test_game;
    use super::*;
    use crate::mock_database::MockDatabase;
    use std::sync::Arc;
    use std::sync::atomic::Ordering;

    #[tokio::test]
    async fn color_expansion_is_capped_and_counted_before_writer_enqueue() {
        let mut game = test_game(Arc::new(MockDatabase::new()));
        let conn = ConnId(160);
        game.state
            .descriptors
            .insert(conn, Descriptor::new(conn, "color.example.test".into()));
        game.state
            .descriptors
            .get_mut(&conn)
            .unwrap()
            .write(&"&r".repeat(crate::connection::DESCRIPTOR_OUTPUT_LIMIT / 2));
        let (tx, mut rx) = mpsc::channel(1);
        game.outputs.insert(conn, tx);

        game.flush_all().await;

        let frame = rx.recv().await.unwrap();
        assert!(frame.bytes.len() <= crate::connection::DESCRIPTOR_OUTPUT_LIMIT);
        assert!(
            frame
                .bytes
                .ends_with(crate::connection::OUTPUT_OVERFLOW_MARKER.as_bytes())
        );
        assert_eq!(
            game.metrics.output_overflows_total.load(Ordering::Relaxed),
            1
        );
    }

    #[tokio::test]
    async fn stalled_writer_channel_closes_only_that_client_and_increments_metric() {
        let mut game = test_game(Arc::new(MockDatabase::new()));
        let stalled = ConnId(161);
        let healthy = ConnId(162);
        game.state.descriptors.insert(
            stalled,
            Descriptor::new(stalled, "stalled.example.test".into()),
        );
        game.state.descriptors.insert(
            healthy,
            Descriptor::new(healthy, "healthy.example.test".into()),
        );
        game.state
            .descriptors
            .get_mut(&stalled)
            .unwrap()
            .write("stalled output");
        game.state
            .descriptors
            .get_mut(&healthy)
            .unwrap()
            .write("healthy output");

        let (stalled_tx, _stalled_rx) = mpsc::channel(1);
        stalled_tx
            .try_send(OutputFrame::data(b"queue full".to_vec()))
            .unwrap();
        game.outputs.insert(stalled, stalled_tx);
        let (healthy_tx, mut healthy_rx) = mpsc::channel(1);
        game.outputs.insert(healthy, healthy_tx);

        game.flush_all().await;

        assert!(!game.state.descriptors.contains_key(&stalled));
        assert!(game.state.descriptors.contains_key(&healthy));
        assert_eq!(healthy_rx.recv().await.unwrap().bytes, b"healthy output");
        assert_eq!(
            game.metrics
                .output_closed_clients_total
                .load(Ordering::Relaxed),
            1
        );
    }
}

#[cfg(test)]
#[allow(clippy::await_holding_lock)]
mod shutdown_tests {
    use super::tests::{persistent_connected_player, test_game};
    use super::*;
    use crate::alias::AliasEntry;
    use crate::character::Character;
    use crate::flags::{AFF_BLIND, AFF_INVISIBLE};
    use crate::mock_database::MockDatabase;
    use crate::room::Room;
    use crate::types::{Class, Race};
    use std::sync::Arc;

    fn unique_shutdown_lib(label: &str) -> std::path::PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "deltamud-shutdown-{label}-{}-{unique}",
            std::process::id()
        ))
    }

    /// W6: the extracted shutdown_save must persist a playing character
    /// (SQL row + alias sidecar + rent file) and report what it did, so a
    /// real SIGTERM shutdown is a verified path, not a hope.
    #[tokio::test]
    async fn shutdown_save_persists_player_inventory_and_reports() {
        let db = Arc::new(MockDatabase::new());
        let mut game = test_game(db.clone());
        // test_game points lib_path at a fresh temp dir; plrobjs lives under it.
        let plrobjs = format!("{}/plrobjs", game.state.config.lib_path);
        std::fs::create_dir_all(&plrobjs).unwrap();

        let room = game
            .state
            .add_room(Room::new(3001, 30, "Save Room".into(), String::new()));
        let conn = ConnId(70);
        game.state
            .descriptors
            .insert(conn, Descriptor::new(conn, "saver.example.test".into()));

        let mut ch = Character::new_player("Shutdownee".to_string(), Class::Warrior, Race::Human);
        ch.player.level = 22;
        crate::gold::set(&mut ch, crate::gold::Account::Carried, 4321);
        ch.player.title = Some("the Persisted".to_string());
        // A playing character carries a persistent idnum (create_player row);
        // save_player_with_host UPDATEs by it.
        ch.idnum = db.create_player(&ch, "pw").await.expect("create row");
        // enter_game sets PLR_CRASH on login: it is the crash_save trigger.
        ch.act_flags |= crate::objsave::PLR_CRASH;
        let cid = game.state.create_char(ch);
        game.state.char_to_room(cid, room);

        // Inventory: a real loaded object so crash_save has something to write.
        game.state.obj_protos.insert(
            9010,
            crate::world::ObjectProto {
                vnum: 9010,
                name: "brick gold".into(),
                short_desc: "a gold brick".into(),
                description: "A gold brick sits here.".into(),
                obj_type: crate::object::ObjectType::Armor,
                wear_flags: crate::object::WearFlags::TAKE,
                extra_flags: crate::object::ExtraFlags::empty(),
                weight: 20,
                cost: 50000,
                rent: 5000,
                values: [0; 4],
                curr_slots: 0,
                total_slots: 0,
                obj_class: 0,
                min_level: 0,
                bitvector: 0,
                action_description: String::new(),
                affects: Vec::new(),
                ex_descriptions: Vec::new(),
            },
        );
        let obj = game.state.load_object(9010).expect("brick loads");
        game.state.obj_to_char(obj, cid);

        // Attach as Playing.
        {
            let d = game.state.descriptors.get_mut(&conn).unwrap();
            d.state = ConState::Playing;
            d.character = Some(cid);
        }
        if let Some(c) = game.state.get_char_mut(cid) {
            c.desc = Some(conn);
        }
        // Register a writer-like task that acknowledges ordered flush barriers.
        let (tx, mut rx) = mpsc::channel(256);
        game.outputs.insert(conn, tx);
        let writer = tokio::spawn(async move {
            let mut bytes = Vec::new();
            while let Some(frame) = rx.recv().await {
                bytes.extend_from_slice(&frame.bytes);
                if let Some(ack) = frame.ack {
                    let _ = ack.send(true);
                    return bytes;
                }
            }
            bytes
        });

        let report = game.shutdown_save().await.unwrap();

        assert_eq!(report.players_saved, 1);
        assert_eq!(report.save_errors, 0);
        assert_eq!(report.output_failures, 0);
        assert_eq!(report.output_attempted, 1);
        assert_eq!(report.output_acknowledged, 1);
        assert_eq!(report.output_failed, 0);
        assert_eq!(report.output_timed_out, 0);
        // The shutdown notice + prompt went through the output channel.
        let drained = writer.await.expect("writer task completed");
        assert!(
            String::from_utf8_lossy(&drained).contains("shutting down"),
            "notice must be flushed"
        );

        // SQL row: reload through the db and check the core fields survived.
        let loaded = db
            .load_player("Shutdownee")
            .await
            .expect("player persisted");
        assert_eq!(loaded.player.level, 22);
        assert_eq!(loaded.points.gold, 4321);
        assert_eq!(loaded.player.title.as_deref(), Some("the Persisted"));

        // Rent file for the inventory: plrobjs/<bucket>/<name>.objs (the
        // bucket is the name's first-letter range, e.g. A-E / U-Z).
        let mut found = false;
        for entry in std::fs::read_dir(&plrobjs).unwrap().filter_map(|e| e.ok()) {
            let p = entry.path();
            if p.is_dir() {
                if let Ok(files) = std::fs::read_dir(&p) {
                    for f in files.filter_map(|f| f.ok()) {
                        if f.file_name()
                            .to_string_lossy()
                            .to_ascii_lowercase()
                            .contains("shutdownee")
                        {
                            found = true;
                        }
                    }
                }
            }
        }
        fn walk(dir: &std::path::Path, depth: usize) {
            if depth > 3 {
                return;
            }
            for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                let p = e.path();
                eprintln!("TREE: {}", p.display());
                if p.is_dir() {
                    walk(&p, depth + 1);
                }
            }
        }
        if !found {
            walk(std::path::Path::new(&plrobjs), 0);
        }
        assert!(found, "rent file must exist for the saved player");
    }

    #[tokio::test]
    async fn shutdown_save_reports_every_persistence_failure_before_teardown() {
        let db = Arc::new(MockDatabase::new());
        let mut game = test_game(db.clone());
        let conn = ConnId(1760);
        let player =
            persistent_connected_player(&mut game, db.as_ref(), conn, "Durabilityfail", 20).await;
        let idnum = game.state.get_char(player).unwrap().idnum;
        game.state.get_char_mut(player).unwrap().act_flags |= crate::objsave::PLR_CRASH;
        crate::alias::set_aliases(
            &mut game.state,
            idnum,
            vec![AliasEntry {
                alias: "greet".into(),
                replacement: "say hello".into(),
                atype: 0,
            }],
        );

        // A file where the configured library directory should be forces the
        // crash-file, calendar, and non-empty alias writers to fail
        // independently. SQL is failed by the mock on the same pass.
        let lib = unique_shutdown_lib("all-persistence-failures");
        std::fs::write(&lib, b"not a directory").unwrap();
        game.lib_path = lib.to_string_lossy().into_owned();
        game.state.config.lib_path = game.lib_path.clone();
        db.fail_next_save();

        let live_before = game.state.get_char(player).unwrap().clone();
        let (tx, mut rx) = mpsc::channel(8);
        game.outputs.insert(conn, tx);

        let report = game.shutdown_save().await.unwrap();

        assert_eq!(report.player_saves_attempted, 1);
        assert_eq!(report.players_saved, 0);
        assert_eq!(report.database_errors, 1);
        assert_eq!(report.alias_writes_attempted, 1);
        assert_eq!(report.aliases_written, 0);
        assert_eq!(report.alias_errors, 1);
        assert_eq!(report.crash_saves_attempted, 1);
        assert_eq!(report.crash_saves_written, 0);
        assert_eq!(report.crash_save_errors, 1);
        assert!(!report.calendar_saved);
        assert_eq!(report.calendar_errors, 1);
        assert_eq!(report.save_errors, 4);
        assert_eq!(report.output_attempted, 0);
        assert!(game.outputs.contains_key(&conn));
        assert!(game.state.descriptors.contains_key(&conn));
        assert!(rx.try_recv().is_err(), "no shutdown notice is published");

        let live_after = game.state.get_char(player).unwrap();
        assert_eq!(live_after.last_logon, live_before.last_logon);
        assert_eq!(
            live_after.player.time_played,
            live_before.player.time_played
        );
        assert_ne!(live_after.act_flags & crate::objsave::PLR_CRASH, 0);

        crate::alias::clear_aliases(&mut game.state, idnum);
        std::fs::remove_file(lib).unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn failed_shutdown_preserves_switched_connection_and_arena_then_retry_commits() {
        let db = Arc::new(MockDatabase::new());
        let mut game = test_game(db.clone());
        let lib = unique_shutdown_lib("retry");
        std::fs::create_dir_all(&lib).unwrap();
        game.lib_path = lib.to_string_lossy().into_owned();
        game.state.config.lib_path = game.lib_path.clone();

        let conn = ConnId(1761);
        let player =
            persistent_connected_player(&mut game, db.as_ref(), conn, "Shutdownretry", 20).await;
        {
            let character = game.state.get_char_mut(player).unwrap();
            character.wimp_level = 12;
            character.recall_level = 34;
            character.affect_flags = AFF_INVISIBLE;
            character.affected.push(crate::character::Affect {
                spell_type: 7,
                duration: 8,
                modifier: 9,
                location: 10,
                bitvector: AFF_INVISIBLE,
                caster: None,
            });
            character.act_flags |= crate::objsave::PLR_CRASH;
        }
        crate::arena::set_stat_for_test(&mut game.state, player, crate::arena::ARENA_COMBATANT1);
        crate::arena::bup_affects(&mut game.state, player);
        {
            let character = game.state.get_char_mut(player).unwrap();
            character.affect_flags = AFF_BLIND;
            character.affected.push(crate::character::Affect {
                spell_type: 11,
                duration: 12,
                modifier: 13,
                location: 14,
                bitvector: AFF_BLIND,
                caster: None,
            });
        }
        // A switched immortal's durable PC is reachable through `original`;
        // the descriptor's current body is an NPC. Both persistence and abort
        // notification must follow the connection rather than trusting only
        // `descriptor.character` or Character::desc.
        let mut npc = Character::new_npc(9901);
        npc.desc = Some(conn);
        let npc = game.state.create_char(npc);
        game.state.get_char_mut(player).unwrap().desc = None;
        {
            let descriptor = game.state.descriptors.get_mut(&conn).unwrap();
            descriptor.original = Some(player);
            descriptor.character = Some(npc);
        }
        let live_time_before = game.state.get_char(player).unwrap().last_logon;

        let (tx, mut rx) = mpsc::channel(16);
        game.outputs.insert(conn, tx);
        game.state.shutdown_requested = Some(ShutdownRequest::System(ProcessDisposition::Stop));
        db.fail_next_save();

        assert!(!game.shutdown().await);
        assert_eq!(game.state.shutdown_requested, None);
        assert!(game.outputs.contains_key(&conn));
        assert!(game.state.descriptors.contains_key(&conn));
        assert_eq!(game.state.descriptors[&conn].original, Some(player));
        assert_eq!(game.state.descriptors[&conn].character, Some(npc));
        assert_eq!(
            crate::arena::arena_stat(&game.state, player),
            crate::arena::ARENA_COMBATANT1
        );
        let live = game.state.get_char(player).unwrap();
        assert_eq!(live.last_logon, live_time_before);
        assert_eq!(live.affect_flags, AFF_BLIND);
        assert_eq!(live.affected[0].spell_type, 11);
        assert_ne!(live.act_flags & crate::objsave::PLR_CRASH, 0);
        let abort = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("shutdown-aborted notice timeout")
            .expect("shutdown-aborted notice");
        let abort = String::from_utf8_lossy(&abort.bytes);
        assert!(abort.contains("Shutdown aborted"));
        assert!(!abort.contains("server is shutting down"));

        game.state.shutdown_requested = Some(ShutdownRequest::System(ProcessDisposition::Stop));
        let writer = tokio::spawn(async move {
            let mut bytes = Vec::new();
            while let Some(frame) = rx.recv().await {
                bytes.extend_from_slice(&frame.bytes);
                if let Some(ack) = frame.ack {
                    let _ = ack.send(true);
                    break;
                }
            }
            bytes
        });

        assert!(game.shutdown().await);
        assert!(!game.outputs.contains_key(&conn));
        assert_eq!(
            crate::arena::arena_stat(&game.state, player),
            crate::arena::ARENA_NOT
        );
        let restored = game.state.get_char(player).unwrap();
        assert_eq!(restored.wimp_level, 12);
        assert_eq!(restored.recall_level, 34);
        assert_eq!(restored.affect_flags, AFF_INVISIBLE);
        assert_eq!(restored.affected[0].spell_type, 7);
        let final_output = writer.await.unwrap();
        assert!(String::from_utf8_lossy(&final_output).contains("server is shutting down"));

        let durable = db.load_player("Shutdownretry").await.unwrap();
        assert_eq!(durable.wimp_level, 12);
        assert_eq!(durable.recall_level, 34);
        assert_eq!(durable.affect_flags, AFF_INVISIBLE);
        assert_eq!(durable.affected[0].spell_type, 7);

        std::fs::remove_dir_all(lib).unwrap();
    }

    #[tokio::test]
    async fn system_shutdown_acknowledges_refusal_then_a_committed_retry() {
        const MISSING_ZONE: i32 = 29_994;

        let db = Arc::new(MockDatabase::new());
        let mut game = test_game(db);
        crate::olc::olc_add_to_save_list(&mut game.state, MISSING_ZONE, crate::olc::OLC_SAVE_ZONE);
        let lib = unique_shutdown_lib("system-ack");
        std::fs::create_dir_all(&lib).unwrap();
        game.lib_path = lib.to_string_lossy().into_owned();
        game.state.config.lib_path = game.lib_path.clone();

        let (game_tx, game_rx) = mpsc::channel(4);
        let game_task = tokio::spawn(async move { game.run(game_rx).await });

        let (first_tx, first_rx) = tokio::sync::oneshot::channel();
        game_tx
            .send(GameMessage::SystemShutdown {
                result_tx: first_tx,
            })
            .await
            .unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), first_rx)
                .await
                .expect("shutdown-refusal acknowledgement timeout")
                .expect("shutdown-refusal acknowledgement sender"),
            crate::connection::SystemShutdownResult::Refused
        );
        assert!(
            !game_task.is_finished(),
            "a durability refusal must keep the Game task alive"
        );

        // The commit phase runs on a fresh Game whose journal is clean: with the
        // save list owned by the Game, a test can no longer reach across the
        // task boundary to clear the failed entry the way the statics-era
        // version did. A clean-journal Game acknowledges the shutdown and stops.
        drop(game_tx);

        let db2 = Arc::new(MockDatabase::new());
        let mut game2 = test_game(db2);
        game2.lib_path = lib.to_string_lossy().into_owned();
        game2.state.config.lib_path = game2.lib_path.clone();
        let (game2_tx, game2_rx) = mpsc::channel(4);
        let game2_task = tokio::spawn(async move { game2.run(game2_rx).await });

        let (retry_tx, retry_rx) = tokio::sync::oneshot::channel();
        game2_tx
            .send(GameMessage::SystemShutdown {
                result_tx: retry_tx,
            })
            .await
            .unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), retry_rx)
                .await
                .expect("shutdown-commit acknowledgement timeout")
                .expect("shutdown-commit acknowledgement sender"),
            crate::connection::SystemShutdownResult::Committed
        );
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), game2_task)
                .await
                .expect("committed Game shutdown timeout")
                .expect("Game task join")
                .expect("Game task result"),
            ProcessDisposition::Stop
        );
        game_task.abort();

        std::fs::remove_dir_all(lib).unwrap();
    }

    #[tokio::test]
    async fn shutdown_reports_closed_and_full_writer_channels_as_failures() {
        let db = Arc::new(MockDatabase::new());
        let mut game = test_game(db);

        let closed = ConnId(171);
        game.state.descriptors.insert(
            closed,
            Descriptor::new(closed, "closed.example.test".into()),
        );
        let (closed_tx, closed_rx) = mpsc::channel(1);
        drop(closed_rx);
        game.outputs.insert(closed, closed_tx);

        let full = ConnId(172);
        game.state
            .descriptors
            .insert(full, Descriptor::new(full, "full.example.test".into()));
        let (full_tx, _full_rx) = mpsc::channel(1);
        full_tx
            .try_send(OutputFrame::data(b"already queued".to_vec()))
            .unwrap();
        game.outputs.insert(full, full_tx);

        let report = game.shutdown_save().await.unwrap();
        assert_eq!(report.output_attempted, 2);
        assert_eq!(report.output_acknowledged, 0);
        assert_eq!(report.output_failed, 2);
        assert_eq!(report.output_timed_out, 0);
        assert_eq!(report.output_failures, 2);
    }

    #[tokio::test]
    async fn one_timed_out_writer_does_not_hide_a_healthy_acknowledgement() {
        let db = Arc::new(MockDatabase::new());
        let mut game = test_game(db);

        let healthy = ConnId(173);
        game.state.descriptors.insert(
            healthy,
            Descriptor::new(healthy, "healthy.example.test".into()),
        );
        let (healthy_tx, mut healthy_rx) = mpsc::channel(4);
        game.outputs.insert(healthy, healthy_tx);
        let healthy_writer = tokio::spawn(async move {
            while let Some(frame) = healthy_rx.recv().await {
                if let Some(ack) = frame.ack {
                    let _ = ack.send(true);
                    break;
                }
            }
        });

        let stalled = ConnId(174);
        game.state.descriptors.insert(
            stalled,
            Descriptor::new(stalled, "stalled.example.test".into()),
        );
        let (stalled_tx, _stalled_rx) = mpsc::channel(4);
        game.outputs.insert(stalled, stalled_tx);

        let report = game.shutdown_save().await.unwrap();
        healthy_writer.await.unwrap();
        assert_eq!(report.output_attempted, 2);
        assert_eq!(report.output_acknowledged, 1);
        assert_eq!(report.output_failed, 0);
        assert_eq!(report.output_timed_out, 1);
        assert_eq!(report.output_failures, 1);
    }

    #[tokio::test]
    async fn shutdown_is_aborted_and_dirty_state_retained_when_olc_flush_fails() {
        const MISSING_ZONE: i32 = 29_991;

        let db = Arc::new(MockDatabase::new());
        let mut game = test_game(db);
        crate::olc::olc_add_to_save_list(&mut game.state, MISSING_ZONE, crate::olc::OLC_SAVE_ZONE);
        game.state.shutdown_requested = Some(ShutdownRequest::System(ProcessDisposition::Stop));
        let conn = ConnId(175);
        let mut builder = Character::new_player("Builder".into(), Class::Warrior, Race::Human);
        builder.desc = Some(conn);
        let builder = game.state.create_char(builder);
        game.state.players_by_name.insert("builder".into(), builder);
        let mut descriptor = Descriptor::new(conn, "builder.example.test".into());
        descriptor.state = ConState::Playing;
        descriptor.character = Some(builder);
        game.state.descriptors.insert(conn, descriptor);
        let (tx, mut rx) = mpsc::channel(4);
        game.outputs.insert(conn, tx);

        assert!(!game.shutdown().await);
        assert_eq!(game.state.shutdown_requested, None);
        assert!(
            crate::olc::flush_save_list_to_disk(&mut game.state).is_err(),
            "the failed target must remain pending for a later retry"
        );
        let frame = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("shutdown-aborted notice timeout")
            .expect("shutdown-aborted notice");
        assert!(String::from_utf8_lossy(&frame.bytes).contains("Shutdown aborted"));

        crate::olc::olc_remove_from_save_list(
            &mut game.state,
            MISSING_ZONE,
            crate::olc::OLC_SAVE_ZONE,
        );
    }
}

#[cfg(test)]
mod autoreboot_tests {
    use super::tests::test_game;
    use super::*;
    use crate::character::Character;
    use crate::mock_database::MockDatabase;
    use std::sync::Arc;

    #[test]
    fn scheduled_autoreboot_sets_shutdown_only_after_olc_flush_succeeds() {
        let mut game = test_game(Arc::new(MockDatabase::new()));

        game.autoreboot_check_at((4, 20, 4, 10), 4, 20);

        assert_eq!(
            game.state.shutdown_requested,
            Some(ShutdownRequest::System(ProcessDisposition::Restart))
        );
    }

    #[test]
    fn scheduled_autoreboot_stays_online_and_retains_dirty_olc_on_failure() {
        const MISSING_ZONE: i32 = 29_993;

        let mut game = test_game(Arc::new(MockDatabase::new()));
        crate::olc::olc_add_to_save_list(&mut game.state, MISSING_ZONE, crate::olc::OLC_SAVE_ZONE);

        let conn = ConnId(176);
        let mut player = Character::new_player("Clockwatcher".into(), Class::Warrior, Race::Human);
        player.desc = Some(conn);
        let player = game.state.create_char(player);
        game.state
            .players_by_name
            .insert("clockwatcher".into(), player);
        let mut descriptor = Descriptor::new(conn, "builder.example.test".into());
        descriptor.state = ConState::Playing;
        descriptor.character = Some(player);
        game.state.descriptors.insert(conn, descriptor);

        game.autoreboot_check_at((4, 20, 4, 10), 4, 20);

        assert_eq!(game.state.shutdown_requested, None);
        assert!(
            game.state.descriptors[&conn]
                .outbuf
                .contains("Automatic reboot aborted")
        );
        assert!(crate::olc::flush_save_list_to_disk(&mut game.state).is_err());

        crate::olc::olc_remove_from_save_list(
            &mut game.state,
            MISSING_ZONE,
            crate::olc::OLC_SAVE_ZONE,
        );
    }
}

#[cfg(test)]
mod async_message_isolation_tests {
    use super::tests::{attach_descriptor_host, test_game};
    use super::*;
    use crate::mock_database::MockDatabase;
    use std::sync::Arc;
    use std::sync::atomic::Ordering;

    #[tokio::test]
    async fn delayed_login_database_call_keeps_pulses_input_and_output_live() {
        let db = Arc::new(MockDatabase::new());
        db.set_exists_delay(Some(Duration::from_millis(500)));
        let mut game = test_game(db);
        let metrics = Arc::new(Metrics::new());
        game.set_metrics(metrics.clone());

        let login = ConnId(191);
        attach_descriptor_host(&mut game, login, "slow-login.example.test");
        game.state.descriptors.get_mut(&login).unwrap().state = ConState::GetName;
        let (login_output, _login_rx) = mpsc::channel(8);
        game.outputs.insert(login, login_output);

        let playing = ConnId(192);
        attach_descriptor_host(&mut game, playing, "active.example.test");
        let player = game
            .state
            .create_char(crate::character::Character::new_player(
                "Active".to_string(),
                Class::Warrior,
                Race::Human,
            ));
        {
            let descriptor = game.state.descriptors.get_mut(&playing).unwrap();
            descriptor.state = ConState::Playing;
            descriptor.character = Some(player);
            descriptor.write("queued output during SQL wait\r\n");
        }
        game.state.get_char_mut(player).unwrap().desc = Some(playing);
        let (playing_output, mut playing_rx) = mpsc::channel(8);
        game.outputs.insert(playing, playing_output);

        let (game_tx, game_rx) = mpsc::channel(8);
        let game_task = tokio::spawn(async move { game.run(game_rx).await });
        game_tx
            .send(GameMessage::Input {
                conn_id: login,
                input: "Neverstored".to_string(),
            })
            .await
            .unwrap();
        game_tx
            .send(GameMessage::Input {
                conn_id: playing,
                input: "score".to_string(),
            })
            .await
            .unwrap();

        let frame = tokio::time::timeout(Duration::from_millis(350), playing_rx.recv())
            .await
            .expect("output must flush before the delayed lookup completes")
            .expect("playing output channel remains open");
        assert!(String::from_utf8_lossy(&frame.bytes).contains("queued output"));

        tokio::time::timeout(Duration::from_millis(350), async {
            loop {
                if metrics.commands_total.load(Ordering::Relaxed) >= 1
                    && metrics.pulse.load(Ordering::Relaxed) >= 1
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("heartbeat must dispatch unrelated gameplay while SQL is delayed");

        game_task.abort();
        let _ = game_task.await;
    }

    #[tokio::test]
    async fn async_message_panic_disconnects_only_the_offending_connection() {
        let db = Arc::new(MockDatabase::new());
        let mut game = test_game(db);
        let bad = ConnId(91);
        let good = ConnId(92);
        attach_descriptor_host(&mut game, bad, "bad.example.test");
        attach_descriptor_host(&mut game, good, "good.example.test");

        game.handle_message_isolated(GameMessage::PanicForTest { conn_id: bad })
            .await;

        assert!(!game.state.descriptors.contains_key(&bad));
        assert!(game.state.descriptors.contains_key(&good));

        game.handle_message_isolated(GameMessage::Gmcp {
            conn_id: good,
            event: crate::connection::GmcpClientEvent::Enabled,
        })
        .await;
        assert!(game.state.descriptors.get(&good).unwrap().gmcp);
    }
}

#[cfg(test)]
#[allow(clippy::await_holding_lock)]
mod ordered_player_save_tests {
    use super::tests::test_game;
    use super::*;
    use crate::character::{Affect, Character};
    use crate::flags::{AFF_BLIND, AFF_INVISIBLE};
    use crate::mock_database::MockDatabase;
    use std::sync::Arc;

    #[tokio::test]
    async fn ordered_saves_keep_game_output_live_and_newest_snapshot_wins() {
        let db = Arc::new(MockDatabase::new());
        let seed = Character::new_player("Savechain".into(), Class::Warrior, Race::Human);
        let idnum = db.create_player(&seed, "pw").await.unwrap();
        let mut game = test_game(db.clone());
        db.set_save_delay(Some(Duration::from_millis(75)));

        let mut old = db.load_player("Savechain").await.unwrap();
        old.idnum = idnum;
        crate::gold::set(&mut old, crate::gold::Account::Carried, 10);
        game.queue_player_save(old, "old.example.test".into());

        let mut newest = db.load_player("Savechain").await.unwrap();
        newest.idnum = idnum;
        crate::gold::set(&mut newest, crate::gold::Account::Carried, 20);
        game.queue_player_save(newest, "new.example.test".into());

        let conn = ConnId(93);
        game.state
            .descriptors
            .insert(conn, Descriptor::new(conn, "viewer.example.test".into()));
        game.state
            .descriptors
            .get_mut(&conn)
            .unwrap()
            .write("still responsive");
        let (tx, mut rx) = mpsc::channel(1);
        game.outputs.insert(conn, tx);
        game.flush_all().await;
        let frame = tokio::time::timeout(Duration::from_millis(25), rx.recv())
            .await
            .expect("output must not wait for a delayed player save")
            .expect("output frame");
        assert_eq!(frame.bytes, b"still responsive");
        assert_eq!(
            game.pending_player_snapshot("Savechain")
                .unwrap()
                .points
                .gold,
            20
        );
        assert_eq!(
            game.load_player_latest("Savechain")
                .await
                .unwrap()
                .points
                .gold,
            20,
            "a fast reconnect must see the newest in-memory save generation"
        );

        assert_eq!(game.await_all_player_saves().await, 0);
        db.set_save_delay(None);
        assert_eq!(db.load_player("Savechain").await.unwrap().points.gold, 20);
    }

    #[tokio::test]
    async fn ordered_save_failures_are_counted_and_reported() {
        let db = Arc::new(MockDatabase::new());
        let seed = Character::new_player("Saveerror".into(), Class::Warrior, Race::Human);
        let idnum = db.create_player(&seed, "pw").await.unwrap();
        let mut game = test_game(db.clone());
        let mut snapshot = db.load_player("Saveerror").await.unwrap();
        snapshot.idnum = idnum;
        db.fail_next_save();

        game.queue_player_save(snapshot, String::new());

        assert_eq!(game.await_all_player_saves().await, 1);
        assert_eq!(game.player_save_failures, 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn copyover_database_failure_preserves_live_arena_and_session_state() {
        let db = Arc::new(MockDatabase::new());
        let seed = Character::new_player("Copyfail".into(), Class::Warrior, Race::Human);
        let idnum = db.create_player(&seed, "pw").await.unwrap();
        let mut game = test_game(db.clone());
        let room = game.state.add_room(crate::room::Room::new(
            4242,
            42,
            "Copy Room".into(),
            String::new(),
        ));
        let conn = ConnId(94);
        game.state
            .descriptors
            .insert(conn, Descriptor::new(conn, "copy.example.test".into()));
        let mut player = db.load_player("Copyfail").await.unwrap();
        player.idnum = idnum;
        player.desc = Some(conn);
        player.wimp_level = 12;
        player.recall_level = 34;
        player.affect_flags = AFF_INVISIBLE;
        player.affected.push(Affect {
            spell_type: 7,
            duration: 8,
            modifier: 9,
            location: 10,
            bitvector: AFF_INVISIBLE,
            caster: None,
        });
        player.tloadroom = 777;
        let player_id = game.state.create_char(player);
        game.state.char_to_room(player_id, room);
        {
            let descriptor = game.state.descriptors.get_mut(&conn).unwrap();
            descriptor.state = ConState::Playing;
            descriptor.character = Some(player_id);
        }
        crate::arena::set_stat_for_test(&mut game.state, player_id, crate::arena::ARENA_COMBATANT1);
        crate::arena::bup_affects(&mut game.state, player_id);
        {
            let player = game.state.get_char_mut(player_id).unwrap();
            player.affect_flags = AFF_BLIND;
            player.affected.push(Affect {
                spell_type: 11,
                duration: 12,
                modifier: 13,
                location: 14,
                bitvector: AFF_BLIND,
                caster: None,
            });
        }
        let live_before = game.state.get_char(player_id).unwrap().clone();
        db.fail_next_save();

        game.execute_copyover(player_id).await;

        assert!(game.pending_player_saves.is_empty());
        assert!(game.state.descriptors.contains_key(&conn));
        assert_eq!(game.state.descriptors[&conn].state, ConState::Playing);
        assert_eq!(game.state.descriptors[&conn].character, Some(player_id));
        assert_eq!(
            crate::arena::arena_stat(&game.state, player_id),
            crate::arena::ARENA_COMBATANT1
        );
        let live_after = game.state.get_char(player_id).unwrap();
        assert_eq!(live_after.last_logon, live_before.last_logon);
        assert_eq!(
            live_after.player.time_played,
            live_before.player.time_played
        );
        assert_eq!(live_after.tloadroom, 777);
        assert_eq!(live_after.affect_flags, AFF_BLIND);
        assert_eq!(live_after.affected[0].spell_type, 11);
        assert!(
            game.state.descriptors[&conn]
                .outbuf
                .contains("Copyover database save failed")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn copyover_output_failure_keeps_sessions_and_persists_exit_safe_clone() {
        let db = Arc::new(MockDatabase::new());
        let mut seed = Character::new_player("Copyflush".into(), Class::Warrior, Race::Human);
        seed.wimp_level = 12;
        seed.recall_level = 34;
        seed.affect_flags = AFF_INVISIBLE;
        seed.affected.push(Affect {
            spell_type: 7,
            duration: 8,
            modifier: 9,
            location: 10,
            bitvector: AFF_INVISIBLE,
            caster: None,
        });
        db.create_player(&seed, "pw").await.unwrap();
        let mut game = test_game(db.clone());
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let lib = std::env::temp_dir().join(format!(
            "deltamud-copyover-output-failure-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&lib).unwrap();
        game.lib_path = lib.to_string_lossy().into_owned();
        game.state.config.lib_path = game.lib_path.clone();

        let room = game.state.add_room(crate::room::Room::new(
            4343,
            43,
            "Flush Room".into(),
            String::new(),
        ));
        let conn = ConnId(941);
        let mut player = db.load_player("Copyflush").await.unwrap();
        player.desc = Some(conn);
        player.tloadroom = 888;
        let player_id = game.state.create_char(player);
        game.state.char_to_room(player_id, room);
        let mut descriptor = Descriptor::new(conn, "copy.example.test".into());
        descriptor.state = ConState::Playing;
        descriptor.character = Some(player_id);
        game.state.descriptors.insert(conn, descriptor);
        crate::arena::set_stat_for_test(&mut game.state, player_id, crate::arena::ARENA_COMBATANT1);
        crate::arena::bup_affects(&mut game.state, player_id);
        {
            let player = game.state.get_char_mut(player_id).unwrap();
            player.affect_flags = AFF_BLIND;
            player.affected.push(Affect {
                spell_type: 11,
                duration: 12,
                modifier: 13,
                location: 14,
                bitvector: AFF_BLIND,
                caster: None,
            });
        }
        let mut npc = Character::new_npc(9902);
        npc.desc = Some(conn);
        let npc = game.state.create_char(npc);
        game.state.get_char_mut(player_id).unwrap().desc = None;
        {
            let descriptor = game.state.descriptors.get_mut(&conn).unwrap();
            descriptor.original = Some(player_id);
            descriptor.character = Some(npc);
        }
        let live_before = game.state.get_char(player_id).unwrap().clone();
        let (closed_tx, closed_rx) = mpsc::channel(1);
        drop(closed_rx);
        game.outputs.insert(conn, closed_tx);

        game.execute_copyover(npc).await;

        assert!(game.outputs.contains_key(&conn));
        assert!(game.state.descriptors.contains_key(&conn));
        assert_eq!(game.state.descriptors[&conn].state, ConState::Playing);
        assert_eq!(game.state.descriptors[&conn].original, Some(player_id));
        assert_eq!(game.state.descriptors[&conn].character, Some(npc));
        assert_eq!(
            crate::arena::arena_stat(&game.state, player_id),
            crate::arena::ARENA_COMBATANT1
        );
        let live_after = game.state.get_char(player_id).unwrap();
        assert_eq!(live_after.last_logon, live_before.last_logon);
        assert_eq!(
            live_after.player.time_played,
            live_before.player.time_played
        );
        assert_eq!(live_after.tloadroom, 888);
        assert_eq!(live_after.affect_flags, AFF_BLIND);
        assert_eq!(live_after.affected[0].spell_type, 11);
        assert!(
            game.state.descriptors[&conn]
                .outbuf
                .contains("Copyover socket flush failed")
        );

        let durable = db.load_player("Copyflush").await.unwrap();
        assert_eq!(durable.tloadroom, 4343);
        assert_eq!(durable.wimp_level, 12);
        assert_eq!(durable.recall_level, 34);
        assert_eq!(durable.affect_flags, AFF_INVISIBLE);
        assert_eq!(durable.affected[0].spell_type, 7);
        assert!(!lib.join("copyover.dat").exists());

        std::fs::remove_dir_all(lib).unwrap();
    }

    #[tokio::test]
    async fn copyover_aborts_when_the_configured_mud_date_cannot_be_saved() {
        let db = Arc::new(MockDatabase::new());
        let mut game = test_game(db);
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let lib = std::env::temp_dir().join(format!(
            "deltamud-copyover-date-failure-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(lib.join("etc/date_record")).unwrap();
        game.lib_path = lib.to_string_lossy().into_owned();
        game.state.config.lib_path = game.lib_path.clone();

        let conn = ConnId(95);
        let mut requester = Character::new_player("Datekeeper".into(), Class::Warrior, Race::Human);
        requester.desc = Some(conn);
        let requester = game.state.create_char(requester);
        let mut descriptor = Descriptor::new(conn, "copy.example.test".into());
        descriptor.state = ConState::Menu;
        descriptor.character = Some(requester);
        game.state.descriptors.insert(conn, descriptor);

        game.execute_copyover(requester).await;

        let output = game
            .state
            .descriptors
            .get(&conn)
            .map(|descriptor| descriptor.outbuf.as_str())
            .unwrap_or_default();
        assert!(output.contains("Copyover calendar save failed"));
        assert!(!output.contains("Copyover unavailable"));
        assert!(!lib.join("copyover.dat").exists());

        let _ = std::fs::remove_dir_all(lib);
    }

    #[tokio::test]
    async fn copyover_aborts_before_other_exit_work_when_olc_flush_fails() {
        const MISSING_ZONE: i32 = 29_992;

        let db = Arc::new(MockDatabase::new());
        let mut game = test_game(db);
        crate::olc::olc_add_to_save_list(&mut game.state, MISSING_ZONE, crate::olc::OLC_SAVE_ZONE);
        let conn = ConnId(96);
        let mut player = Character::new_player("Olcbuilder".into(), Class::Warrior, Race::Human);
        player.desc = Some(conn);
        let requester = game.state.create_char(player);
        let mut descriptor = Descriptor::new(conn, "copy.example.test".into());
        descriptor.state = ConState::Menu;
        descriptor.character = Some(requester);
        game.state.descriptors.insert(conn, descriptor);

        game.execute_copyover(requester).await;

        let output = &game.state.descriptors[&conn].outbuf;
        assert!(output.contains("Copyover OLC save failed"));
        assert!(!output.contains("Copyover calendar save failed"));
        assert!(!output.contains("Copyover unavailable"));
        assert!(crate::olc::flush_save_list_to_disk(&mut game.state).is_err());

        crate::olc::olc_remove_from_save_list(
            &mut game.state,
            MISSING_ZONE,
            crate::olc::OLC_SAVE_ZONE,
        );
    }
}

#[cfg(test)]
mod durable_player_rename_tests {
    use super::*;
    use crate::DatabaseInterface;
    use crate::character::Character;
    use crate::config::Config;
    use crate::database_timeout::TimedDatabase;
    use crate::mock_database::MockDatabase;
    use std::path::PathBuf;

    struct RenameFixture {
        game: Game,
        db: Arc<MockDatabase>,
        lib: PathBuf,
        admin: CharId,
        victim: CharId,
        idnum: i64,
        old_rent: PathBuf,
        old_alias: PathBuf,
        new_rent: PathBuf,
        new_alias: PathBuf,
    }

    async fn fixture(
        label: &str,
        db: Arc<MockDatabase>,
        game_db: Arc<dyn DatabaseInterface>,
    ) -> RenameFixture {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let lib = std::env::temp_dir().join(format!(
            "deltamud-durable-rename-{label}-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&lib).unwrap();
        let mut config = Config::default();
        config.lib_path = lib.to_string_lossy().into_owned();
        let mut game = Game::new(GameState::new(config.clone()), game_db);
        game.lib_path = config.lib_path.clone();

        let mut stored = Character::new_player("Oldname".into(), Class::Warrior, Race::Human);
        stored.player.level = 20;
        stored.trust = 20;
        let idnum = db.create_player(&stored, "password").await.unwrap();
        let mut victim_record = db.load_player("Oldname").await.unwrap();
        victim_record.desc = Some(ConnId(202));
        let victim = game.state.create_char(victim_record);
        let mut victim_descriptor = Descriptor::new(ConnId(202), "victim.example.test".into());
        victim_descriptor.state = ConState::Playing;
        victim_descriptor.character = Some(victim);
        game.state
            .descriptors
            .insert(ConnId(202), victim_descriptor);
        game.state.players_by_name.insert("oldname".into(), victim);

        let mut admin_record = Character::new_player("Admin".into(), Class::Warrior, Race::Human);
        admin_record.player.level = LVL_IMPL;
        admin_record.trust = i32::from(LVL_IMPL);
        let grants = crate::gcmd::canonical_advance_grants(LVL_IMPL, LVL_IMMORT, LVL_IMPL);
        admin_record.godcmds1 = grants.0;
        admin_record.godcmds2 = grants.1;
        admin_record.godcmds3 = grants.2;
        admin_record.godcmds4 = grants.3;
        admin_record.idnum = 9_413_900;
        admin_record.desc = Some(ConnId(201));
        let admin = game.state.create_char(admin_record);
        let mut admin_descriptor = Descriptor::new(ConnId(201), "admin.example.test".into());
        admin_descriptor.state = ConState::Playing;
        admin_descriptor.character = Some(admin);
        game.state.descriptors.insert(ConnId(201), admin_descriptor);
        game.state.players_by_name.insert("admin".into(), admin);
        game.state.player_table = db.list_players().await.unwrap();

        let old_rent = crate::objsave::crash_filename(&config.lib_path, "Oldname").unwrap();
        std::fs::create_dir_all(old_rent.parent().unwrap()).unwrap();
        std::fs::write(&old_rent, b"Oldname rent").unwrap();
        crate::alias::set_aliases(
            &mut game.state,
            idnum,
            vec![crate::alias::AliasEntry {
                alias: "greet".into(),
                replacement: "say hello".into(),
                atype: 0,
            }],
        );
        crate::alias::write_aliases(&game.state, &config.lib_path, "Oldname", idnum).unwrap();
        let old_alias = crate::alias::alias_filename(&config.lib_path, "Oldname").unwrap();
        let new_rent = crate::objsave::crash_filename(&config.lib_path, "Newname").unwrap();
        let new_alias = crate::alias::alias_filename(&config.lib_path, "Newname").unwrap();

        RenameFixture {
            game,
            db,
            lib,
            admin,
            victim,
            idnum,
            old_rent,
            old_alias,
            new_rent,
            new_alias,
        }
    }

    fn cleanup(fixture: &mut RenameFixture) {
        crate::alias::clear_aliases(&mut fixture.game.state, fixture.idnum);
        let _ = std::fs::remove_dir_all(&fixture.lib);
    }

    fn queue_rename(fixture: &mut RenameFixture) {
        run_authenticated_command(
            &mut fixture.game.state,
            fixture.admin,
            "rename Oldname Newname",
        );
    }

    fn assert_old_live_identity(fixture: &RenameFixture) {
        assert_eq!(
            fixture
                .game
                .state
                .get_char(fixture.victim)
                .unwrap()
                .get_name(),
            "Oldname"
        );
        assert_eq!(
            fixture.game.state.find_player_by_name("Oldname"),
            Some(fixture.victim)
        );
        assert!(fixture.game.state.find_player_by_name("Newname").is_none());
        assert_eq!(
            fixture.game.state.get_name_by_id(fixture.idnum).as_deref(),
            Some("Oldname")
        );
    }

    #[tokio::test]
    async fn success_is_published_only_after_sql_and_both_sidecars_are_durable() {
        let db = Arc::new(MockDatabase::new());
        let game_db: Arc<dyn DatabaseInterface> = db.clone();
        let mut fixture = fixture("success", db, game_db).await;

        queue_rename(&mut fixture);

        assert_old_live_identity(&fixture);
        assert!(fixture.old_rent.is_file() && fixture.old_alias.is_file());
        assert!(!fixture.new_rent.exists() && !fixture.new_alias.exists());
        assert!(fixture.db.load_player("Oldname").await.is_ok());
        assert!(
            !fixture.game.state.descriptors[&ConnId(201)]
                .outbuf
                .contains("You have renamed")
        );

        fixture.game.drain_player_rename_requests().await;

        assert_eq!(
            fixture
                .game
                .state
                .get_char(fixture.victim)
                .unwrap()
                .get_name(),
            "Newname"
        );
        assert!(fixture.game.state.find_player_by_name("Oldname").is_none());
        assert_eq!(
            fixture.game.state.find_player_by_name("Newname"),
            Some(fixture.victim)
        );
        assert_eq!(
            fixture.game.state.get_name_by_id(fixture.idnum).as_deref(),
            Some("Newname")
        );
        assert!(fixture.db.load_player("Oldname").await.is_err());
        assert_eq!(
            fixture.db.load_player("Newname").await.unwrap().get_name(),
            "Newname"
        );
        assert!(!fixture.old_rent.exists() && !fixture.old_alias.exists());
        assert!(fixture.new_rent.is_file() && fixture.new_alias.is_file());
        assert!(
            fixture.game.state.descriptors[&ConnId(201)]
                .outbuf
                .contains("You have renamed Oldname to Newname")
        );
        assert!(
            fixture.game.state.descriptors[&ConnId(202)]
                .outbuf
                .contains("You have been renamed to Newname")
        );
        assert!(fixture.game.state.player_save_requests.is_empty());

        cleanup(&mut fixture);
    }

    #[tokio::test]
    async fn rename_hierarchy_uses_trust_not_either_display_level() {
        let db = Arc::new(MockDatabase::new());
        let game_db: Arc<dyn DatabaseInterface> = db.clone();
        let mut fixture = fixture("trust-hierarchy", db, game_db).await;
        {
            let admin = fixture.game.state.get_char_mut(fixture.admin).unwrap();
            admin.player.level = 1;
            admin.trust = i32::from(LVL_IMPL);
        }
        {
            let victim = fixture.game.state.get_char_mut(fixture.victim).unwrap();
            victim.player.level = LVL_IMPL;
            victim.trust = 20;
        }

        queue_rename(&mut fixture);
        assert_eq!(fixture.game.state.player_rename_requests.len(), 1);
        fixture.game.drain_player_rename_requests().await;

        assert!(fixture.db.load_player("Newname").await.is_ok());
        assert!(fixture.db.load_player("Oldname").await.is_err());
        assert!(
            fixture.game.state.descriptors[&ConnId(201)]
                .outbuf
                .contains("You have renamed Oldname to Newname")
        );
        cleanup(&mut fixture);
    }

    #[tokio::test]
    async fn database_error_keeps_old_sql_live_index_and_sidecars_without_false_success() {
        let db = Arc::new(MockDatabase::new());
        let game_db: Arc<dyn DatabaseInterface> = db.clone();
        let mut fixture = fixture("db-error", db, game_db).await;
        fixture.db.fail_next_rename();
        queue_rename(&mut fixture);

        fixture.game.drain_player_rename_requests().await;

        assert_old_live_identity(&fixture);
        assert_eq!(
            fixture.db.load_player("Oldname").await.unwrap().get_name(),
            "Oldname"
        );
        assert!(fixture.db.load_player("Newname").await.is_err());
        assert!(fixture.old_rent.is_file() && fixture.old_alias.is_file());
        assert!(!fixture.new_rent.exists() && !fixture.new_alias.exists());
        let output = &fixture.game.state.descriptors[&ConnId(201)].outbuf;
        assert!(output.contains("Rename failed"), "output={output:?}");
        assert!(!output.contains("You have renamed"), "output={output:?}");

        cleanup(&mut fixture);
    }

    #[tokio::test]
    async fn database_timeout_is_cancelled_before_mutation_and_leaves_sidecars_unpublished() {
        let db = Arc::new(MockDatabase::new());
        let inner: Arc<dyn DatabaseInterface> = db.clone();
        let game_db: Arc<dyn DatabaseInterface> =
            Arc::new(TimedDatabase::new(inner, Duration::from_millis(10)));
        let mut fixture = fixture("db-timeout", db, game_db).await;
        fixture
            .db
            .set_rename_delay(Some(Duration::from_millis(100)));
        queue_rename(&mut fixture);

        fixture.game.drain_player_rename_requests().await;
        tokio::time::sleep(Duration::from_millis(120)).await;

        assert_old_live_identity(&fixture);
        assert!(fixture.db.load_player("Oldname").await.is_ok());
        assert!(fixture.db.load_player("Newname").await.is_err());
        assert!(fixture.old_rent.is_file() && fixture.old_alias.is_file());
        assert!(!fixture.new_rent.exists() && !fixture.new_alias.exists());
        let output = &fixture.game.state.descriptors[&ConnId(201)].outbuf;
        assert!(output.contains("Rename failed"), "output={output:?}");
        assert!(!output.contains("You have renamed"), "output={output:?}");

        cleanup(&mut fixture);
    }

    #[tokio::test]
    async fn sidecar_collision_rolls_the_committed_sql_name_back_to_old_identity() {
        let db = Arc::new(MockDatabase::new());
        let game_db: Arc<dyn DatabaseInterface> = db.clone();
        let mut fixture = fixture("sidecar-rollback", db, game_db).await;
        std::fs::create_dir_all(fixture.new_alias.parent().unwrap()).unwrap();
        std::fs::write(&fixture.new_alias, b"belongs to another identity").unwrap();
        queue_rename(&mut fixture);

        fixture.game.drain_player_rename_requests().await;

        assert_old_live_identity(&fixture);
        assert_eq!(
            fixture.db.load_player("Oldname").await.unwrap().get_name(),
            "Oldname"
        );
        assert!(fixture.db.load_player("Newname").await.is_err());
        assert!(fixture.old_rent.is_file() && fixture.old_alias.is_file());
        assert!(!fixture.new_rent.exists());
        assert_eq!(
            std::fs::read(&fixture.new_alias).unwrap(),
            b"belongs to another identity"
        );
        let output = &fixture.game.state.descriptors[&ConnId(201)].outbuf;
        assert!(output.contains("Rename failed"), "output={output:?}");
        assert!(!output.contains("You have renamed"), "output={output:?}");

        cleanup(&mut fixture);
    }

    #[tokio::test]
    async fn rollback_failure_is_reported_as_critical_without_false_durable_state_claim() {
        let db = Arc::new(MockDatabase::new());
        let game_db: Arc<dyn DatabaseInterface> = db.clone();
        let mut fixture = fixture("critical-rollback", db, game_db).await;
        std::fs::create_dir_all(fixture.new_alias.parent().unwrap()).unwrap();
        std::fs::write(&fixture.new_alias, b"blocks sidecar publication").unwrap();
        // First call commits Oldname -> Newname; the second call is the
        // compensating SQL rollback and is deliberately failed.
        fixture.db.fail_rename_on_call(2);
        queue_rename(&mut fixture);

        fixture.game.drain_player_rename_requests().await;

        assert_old_live_identity(&fixture);
        assert!(fixture.db.load_player("Oldname").await.is_err());
        assert_eq!(
            fixture.db.load_player("Newname").await.unwrap().get_name(),
            "Newname"
        );
        assert!(fixture.old_rent.is_file() && fixture.old_alias.is_file());
        let output = &fixture.game.state.descriptors[&ConnId(201)].outbuf;
        assert!(output.contains("CRITICAL"), "output={output:?}");
        assert!(output.contains("inconsistent"), "output={output:?}");
        assert!(
            !output.contains("old name was restored"),
            "output={output:?}"
        );
        assert!(!output.contains("You have renamed"), "output={output:?}");

        cleanup(&mut fixture);
    }

    #[tokio::test]
    async fn drain_rechecks_authority_before_touching_sql_or_files() {
        let db = Arc::new(MockDatabase::new());
        let game_db: Arc<dyn DatabaseInterface> = db.clone();
        let mut fixture = fixture("authority", db, game_db).await;
        queue_rename(&mut fixture);
        fixture
            .game
            .state
            .get_char_mut(fixture.admin)
            .unwrap()
            .trust = 20;

        fixture.game.drain_player_rename_requests().await;

        assert_old_live_identity(&fixture);
        assert!(fixture.db.load_player("Oldname").await.is_ok());
        assert!(fixture.old_rent.is_file() && fixture.old_alias.is_file());
        assert!(!fixture.new_rent.exists() && !fixture.new_alias.exists());
        assert!(
            !fixture.game.state.descriptors[&ConnId(201)]
                .outbuf
                .contains("You have renamed")
        );

        cleanup(&mut fixture);
    }

    #[tokio::test]
    async fn drain_rejects_a_quarantined_rename_requester_before_storage_mutation() {
        let db = Arc::new(MockDatabase::new());
        let game_db: Arc<dyn DatabaseInterface> = db.clone();
        let mut fixture = fixture("quarantined-requester", db, game_db).await;
        queue_rename(&mut fixture);
        assert_eq!(fixture.game.state.player_rename_requests.len(), 1);
        let requester_idnum = fixture.game.state.get_char(fixture.admin).unwrap().idnum;
        fixture
            .game
            .state
            .authority_quarantine
            .insert(requester_idnum);

        fixture.game.drain_player_rename_requests().await;

        assert_old_live_identity(&fixture);
        assert!(fixture.db.load_player("Oldname").await.is_ok());
        assert!(fixture.old_rent.is_file() && fixture.old_alias.is_file());
        assert!(!fixture.new_rent.exists() && !fixture.new_alias.exists());
        cleanup(&mut fixture);
    }

    #[tokio::test]
    async fn durable_collision_appearing_after_queue_is_rejected_without_sidecar_publication() {
        let db = Arc::new(MockDatabase::new());
        let game_db: Arc<dyn DatabaseInterface> = db.clone();
        let mut fixture = fixture("durable-collision", db, game_db).await;
        queue_rename(&mut fixture);
        let collision = Character::new_player("Newname".into(), Class::MagicUser, Race::Elf);
        fixture
            .db
            .create_player(&collision, "password")
            .await
            .unwrap();

        fixture.game.drain_player_rename_requests().await;

        assert_old_live_identity(&fixture);
        assert!(fixture.db.load_player("Oldname").await.is_ok());
        assert_eq!(
            fixture
                .db
                .load_player("Newname")
                .await
                .unwrap()
                .player
                .class,
            Class::MagicUser
        );
        assert!(fixture.old_rent.is_file() && fixture.old_alias.is_file());
        assert!(!fixture.new_rent.exists() && !fixture.new_alias.exists());
        assert!(
            !fixture.game.state.descriptors[&ConnId(201)]
                .outbuf
                .contains("You have renamed")
        );

        cleanup(&mut fixture);
    }

    #[tokio::test]
    async fn prior_old_name_save_finishes_before_rename_and_cannot_recreate_the_old_key() {
        let db = Arc::new(MockDatabase::new());
        let game_db: Arc<dyn DatabaseInterface> = db.clone();
        let mut fixture = fixture("prior-save", db, game_db).await;
        fixture.db.set_save_delay(Some(Duration::from_millis(40)));
        let mut old_snapshot = fixture.db.load_player("Oldname").await.unwrap();
        crate::gold::set(&mut old_snapshot, crate::gold::Account::Carried, 1234);
        fixture
            .game
            .queue_player_save(old_snapshot, "saved.example.test".into());
        queue_rename(&mut fixture);

        fixture.game.drain_player_rename_requests().await;
        tokio::time::sleep(Duration::from_millis(60)).await;

        assert!(fixture.game.pending_player_saves.is_empty());
        assert!(fixture.db.load_player("Oldname").await.is_err());
        let stored = fixture.db.load_player("Newname").await.unwrap();
        assert_eq!(stored.get_name(), "Newname");
        assert_eq!(stored.points.gold, 1234);

        cleanup(&mut fixture);
    }
}
