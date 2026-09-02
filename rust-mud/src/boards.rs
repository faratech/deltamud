// boards.rs — multiple bulletin boards (CircleMUD/DeltaMUD boards.c), ported
// 1:1 to the id-indexed GameState.
//
// The C subsystem is a *special procedure* (`gen_board`) attached to a board
// object sitting in a room. When a player standing in that room types one of
// the intercepted commands (look / examine / read / write / remove) the spec
// proc fires BEFORE the normal command handler, figures out which board object
// is present, and dispatches to one of the Board_* routines. This port keeps
// that exact shape:
//
//   * `board(g, ch, cmd, argument)` — the spec proc (`gen_board`). The
//     integrator's future special-procedure dispatcher calls this for every
//     command typed in a room that holds a board object; it returns `true`
//     when it consumed the command (the normal handler must then be skipped).
//   * `board_show`  — Board_show_board   (look / examine <board>)
//   * `board_write` — Board_write_message (write <header>)
//   * `board_remove`— Board_remove_msg    (remove <#>)
//   * plus board_display (read <#>), all taking a `board_type` index.
//   * `boot_boards(lib_path)` — init_boards(): allocate the message tables and
//     load every board's save file from disk.
//
// PERSISTENCE / ON-DISK FORMAT
// ----------------------------
// The C `Board_save_board`/`Board_load_board` dump a raw `int num_of_msgs`
// followed by, per message, a `struct board_msginfo` (including a dead pointer
// ignored on load), then NUL-terminated heading/body bytes. The port now
// auto-detects that exact x86-64 LP64 layout and its existing `DBRD` format,
// retains raw message bytes, and preserves the detected format on atomic
// rewrites. Files live under `<lib>/etc/board/...` exactly as in C.
//
// THE WRITE EDITOR
// ----------------
// C `Board_write_message` reserves a slot, stores the heading immediately,
// bumps `num_of_msgs`, then points `ch->desc->str` at the slot so the player's
// next lines flow into the string editor; saving the editor fills the body and
// `Board_save_board` runs on the eventual look/quit. We mirror that: the
// heading is committed at once, a body-pending record is keyed by the writer's
// ConnId, and a `StringEdit` context is pushed onto the descriptor. When the
// integrator's editor flush completes it must call `board_finish_write`
// (documented as a boot/heartbeat hook) to install the body and save. Until
// that hook is wired the heading still shows on the board with an empty body —
// graceful degradation identical to a player who never finishes typing in C.

use crate::act::{ActArg, To, act};
use crate::handler::isname;
use crate::interpreter::one_argument;
use crate::state::GameState;
use crate::types::*;

use std::borrow::Cow;
use std::collections::HashMap;
use std::io::Read;

// ---------------------------------------------------------------------------
// Constants (boards.h)
// ---------------------------------------------------------------------------

const NUM_OF_BOARDS: usize = 13;
const MAX_BOARD_MESSAGES: usize = 60;
const MAX_MESSAGE_LENGTH: usize = 4096;

// Level constants used by the board table (structs.h). `Level` is u8 so the
// immortal tiers fit (LVL_IMMORT == 101, LVL_IMPL == 105).
const B_LVL_GOD: Level = LVL_GOD; // 103
const B_LVL_GRGOD: Level = LVL_GRGOD; // 104
const B_LVL_IMPL: Level = LVL_IMPL; // 105
const B_LVL_IMMORT: Level = LVL_IMMORT; // 101
const B_LVL_FREEZE: Level = LVL_FREEZE; // == LVL_GRGOD

// PLR_WRITING (structs.h) — set while a player is composing a board/note
// message so the heartbeat skips them. Not yet in flags.rs; defined locally.
const PLR_WRITING: i64 = 1 << 4;

/// Magic header for our portable board save files.
const BOARD_FILE_MAGIC: &[u8; 4] = b"DBRD";

// ---------------------------------------------------------------------------
// Static board definition table (boards.c board_info[]).
// format: vnum, read_lvl, write_lvl, remove_lvl, filename
// ---------------------------------------------------------------------------

struct BoardInfo {
    vnum: ObjVnum,
    read_lvl: Level,
    write_lvl: Level,
    remove_lvl: Level,
    filename: &'static str,
}

const BOARD_INFO: [BoardInfo; NUM_OF_BOARDS] = [
    // Mortal boards.
    BoardInfo {
        vnum: 199,
        read_lvl: 0,
        write_lvl: 0,
        remove_lvl: B_LVL_GOD,
        filename: "etc/board/mortal/general",
    },
    BoardInfo {
        vnum: 198,
        read_lvl: 0,
        write_lvl: 0,
        remove_lvl: B_LVL_GOD,
        filename: "etc/board/mortal/social",
    },
    BoardInfo {
        vnum: 197,
        read_lvl: 0,
        write_lvl: 0,
        remove_lvl: B_LVL_GOD,
        filename: "etc/board/mortal/auction",
    },
    // Immortal boards.
    BoardInfo {
        vnum: 1200,
        read_lvl: B_LVL_GRGOD,
        write_lvl: B_LVL_GRGOD,
        remove_lvl: B_LVL_IMPL,
        filename: "etc/board/god/imps",
    },
    BoardInfo {
        vnum: 1201,
        read_lvl: B_LVL_IMMORT,
        write_lvl: B_LVL_IMMORT,
        remove_lvl: B_LVL_GRGOD,
        filename: "etc/board/god/general",
    },
    BoardInfo {
        vnum: 1202,
        read_lvl: B_LVL_IMMORT,
        write_lvl: B_LVL_IMMORT,
        remove_lvl: B_LVL_GRGOD,
        filename: "etc/board/god/ideas",
    },
    BoardInfo {
        vnum: 1203,
        read_lvl: B_LVL_IMMORT,
        write_lvl: B_LVL_IMMORT,
        remove_lvl: B_LVL_GRGOD,
        filename: "etc/board/god/quest",
    },
    BoardInfo {
        vnum: 1204,
        read_lvl: B_LVL_IMMORT,
        write_lvl: B_LVL_IMMORT,
        remove_lvl: B_LVL_GRGOD,
        filename: "etc/board/god/public",
    },
    BoardInfo {
        vnum: 1205,
        read_lvl: B_LVL_IMMORT,
        write_lvl: B_LVL_FREEZE,
        remove_lvl: B_LVL_IMPL,
        filename: "etc/board/god/freeze",
    },
    BoardInfo {
        vnum: 1206,
        read_lvl: B_LVL_IMMORT,
        write_lvl: B_LVL_IMMORT,
        remove_lvl: B_LVL_GRGOD,
        filename: "etc/board/god/typos",
    },
    BoardInfo {
        vnum: 1207,
        read_lvl: B_LVL_IMMORT,
        write_lvl: B_LVL_IMMORT,
        remove_lvl: B_LVL_GRGOD,
        filename: "etc/board/god/bugs",
    },
    BoardInfo {
        vnum: 1208,
        read_lvl: B_LVL_IMMORT,
        write_lvl: B_LVL_IMMORT,
        remove_lvl: B_LVL_GRGOD,
        filename: "etc/board/god/imburse",
    },
    BoardInfo {
        vnum: 1209,
        read_lvl: B_LVL_IMMORT,
        write_lvl: B_LVL_IMMORT,
        remove_lvl: B_LVL_GRGOD,
        filename: "etc/board/god/build",
    },
];

// ---------------------------------------------------------------------------
// Runtime board state (module static).
// ---------------------------------------------------------------------------

/// One stored message (C: board_msginfo.heading + msg_storage[slot]).
#[derive(Clone)]
pub(crate) struct BoardMsg {
    /// Bytes excluding the C file's required trailing NUL. Keeping bytes here
    /// prevents a load/save cycle from normalising non-UTF-8 legacy text.
    heading: Vec<u8>,
    message: Vec<u8>,
    level: i32,
}

impl BoardMsg {
    fn heading_text(&self) -> Cow<'_, str> {
        String::from_utf8_lossy(&self.heading)
    }

    fn message_text(&self) -> Cow<'_, str> {
        String::from_utf8_lossy(&self.message)
    }
}

pub struct BoardRuntime {
    /// Per-board message lists (index 0..NUM_OF_BOARDS). Order matters: message
    /// number shown to players is `index + 1`, newest appended last — exactly
    /// like C's `msg_index[board][0..num_of_msgs]`.
    pub(crate) boards: Vec<Vec<BoardMsg>>,
    formats: Vec<crate::cformat::PersistenceFormat>,
    /// Boards whose on-disk file failed to parse at boot. Unlike C — which
    /// "resets" a corrupt board and lets the next save destroy it — a
    /// quarantined board refuses every save (including the empty-board unlink)
    /// so the unreadable file is preserved for operator recovery. New posts
    /// stay in memory only and are lost at reboot; the error log says so.
    quarantined: Vec<bool>,
    /// Pending bodies being composed, keyed by the writer's ConnId. Maps to
    /// (board_type, message_index) so the editor-completion hook knows where
    /// to drop the finished text.
    pending: HashMap<u64, PendingBoardWrite>,
    /// The lib path captured at boot (for save-on-write/remove).
    lib_path: String,
}

#[derive(Clone, Copy)]
struct PendingBoardWrite {
    board_type: usize,
    msg_index: usize,
    authorization: crate::state::AuthenticatedCommandRequest,
}

/// Owned-runtime accessor (the old Mutex-returning boards() is gone).
fn boards(g: &mut GameState) -> &mut BoardRuntime {
    &mut g.social.boards
}

/// Read-only accessor.
fn boards_ref(g: &GameState) -> &BoardRuntime {
    &g.social.boards
}

// The board runtime lives on GameState as `social.boards` (phase-1 statics
// migration). A fresh GameState starts with empty boards and the default
// persistence format, matching the old never-booted static.
impl Default for BoardRuntime {
    fn default() -> Self {
        BoardRuntime {
            boards: vec![Vec::new(); NUM_OF_BOARDS],
            formats: vec![crate::cformat::default_persistence_format(); NUM_OF_BOARDS],
            quarantined: vec![false; NUM_OF_BOARDS],
            pending: HashMap::new(),
            lib_path: "./lib".to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// Boot (init_boards / Board_load_board)
// ---------------------------------------------------------------------------

/// init_boards(): wipe the runtime tables and load every board's save file.
/// Call once at boot (the C `gen_board` lazy-loads on first use; we hoist it
/// to an explicit boot hook). `lib_path` is the lib root (Config.lib_path);
/// board files live at `<lib_path>/etc/board/...`.
pub fn boot_boards(g: &mut GameState, lib_path: &str) {
    let guard = boards(g);
    guard.lib_path = lib_path.trim_end_matches('/').to_string();
    guard.pending.clear();
    for b in 0..NUM_OF_BOARDS {
        guard.boards[b].clear();
        guard.formats[b] = crate::cformat::default_persistence_format();
        guard.quarantined[b] = false;
    }
    // Load each board file from disk.
    for b in 0..NUM_OF_BOARDS {
        let path = board_file_path(&guard.lib_path, b);
        match load_board_file(&path) {
            Ok(BoardLoad::Loaded(msgs, format)) => {
                guard.boards[b] = msgs;
                guard.formats[b] = format;
            }
            Ok(BoardLoad::Missing) => {} // missing file == empty board (C: ENOENT is silent)
            Ok(BoardLoad::Corrupt) => {
                // Fail closed: the unreadable file must survive until an
                // operator recovers it, so refuse all saves for this board.
                guard.quarantined[b] = true;
                log::error!(
                    "SYSERR: Board file '{}' is corrupt; board quarantined — saves are \
                     refused to preserve the file for recovery. Posts made now are lost \
                     at reboot. Move or repair the file, then reboot.",
                    path
                );
            }
            Err(e) => {
                log::error!(
                    "SYSERR: Error reading board '{}': {}",
                    BOARD_INFO[b].filename,
                    e
                );
            }
        }
    }
}

fn board_file_path(lib_path: &str, board_type: usize) -> String {
    format!("{}/{}", lib_path, BOARD_INFO[board_type].filename)
}

// ---------------------------------------------------------------------------
// The special procedure (gen_board).
// ---------------------------------------------------------------------------

/// gen_board(): the board special procedure. The integrator's special-proc
/// dispatcher should call this for every command issued by a connected player
/// standing in a room that contains a board object, passing the typed command
/// word (lowercased) and the raw argument. Returns `true` if the board
/// consumed the command (the normal command handler MUST then be skipped),
/// `false` to fall through to normal handling — matching C's SPECIAL() return.
///
/// `cmd` is the command word as the player typed it (e.g. "look", "read",
/// "write", "remove", "examine"); abbreviations resolve the same way the
/// command interpreter does, so we match against the canonical names.
pub fn board(g: &mut GameState, ch: CharId, cmd: &str, argument: &str) -> bool {
    // C: `if (!ch->desc) return 0;` — NPCs/link-dead never trigger boards.
    if g.get_char(ch).and_then(|c| c.desc).is_none() {
        return false;
    }

    let cmd = cmd.to_lowercase();
    let is_write = is_cmd(&cmd, "write");
    let is_look = is_cmd(&cmd, "look");
    let is_examine = is_cmd(&cmd, "examine");
    let is_read = is_cmd(&cmd, "read");
    let is_remove = is_cmd(&cmd, "remove");

    if !is_write && !is_look && !is_examine && !is_read && !is_remove {
        return false;
    }

    // find_board(): which board object is in the room?
    let board_type = match find_board(g, ch) {
        Some(b) => b,
        None => {
            eprintln!("SYSERR:  degenerate board!  (what the hell...)");
            return false;
        }
    };

    if is_write {
        board_write(g, board_type, ch, argument);
        true
    } else if is_look || is_examine {
        board_show(g, board_type, ch, argument)
    } else if is_read {
        board_display(g, board_type, ch, argument)
    } else if is_remove {
        board_remove(g, board_type, ch, argument)
    } else {
        false
    }
}

/// Abbreviation match like the command interpreter (prefix of the canonical
/// name). The C spec proc compares against pre-resolved command numbers; the
/// interpreter resolved them by abbreviation, so we do the same here.
fn is_cmd(typed: &str, full: &str) -> bool {
    !typed.is_empty() && full.starts_with(typed)
}

/// find_board(): scan the objects in ch's room for one whose vnum matches a
/// board's vnum; return that board's index. (C compares rnums; we compare the
/// prototype vnum stored on the live object, which is equivalent.)
fn find_board(g: &GameState, ch: CharId) -> Option<usize> {
    let rnum = g.get_char(ch)?.in_room?;
    let contents = g.room_opt(rnum)?.contents.clone();
    for oid in contents {
        let vnum = match g.get_obj(oid) {
            Some(o) => o.item_number,
            None => continue,
        };
        for (i, info) in BOARD_INFO.iter().enumerate() {
            if info.vnum == vnum {
                return Some(i);
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Board_write_message
// ---------------------------------------------------------------------------

/// Board_write_message(): validate, commit the heading immediately, reserve
/// the body slot, push the string editor, and record the pending write so the
/// editor-completion hook can install the body.
pub fn board_write(g: &mut GameState, board_type: usize, ch: CharId, arg: &str) {
    let Some(authorization) = capture_board_authorization(g, ch) else {
        g.send_to_char(ch, "You are not holy enough to write on this board.\r\n");
        return;
    };
    let Some(level) = char_authority_i32(g, ch) else {
        g.send_to_char(ch, "You are not holy enough to write on this board.\r\n");
        return;
    };

    if board_type >= BOARD_INFO.len()
        || !g.authenticated_session_request_is_current(
            authorization,
            i32::from(WRITE_LVL(board_type)),
        )
    {
        g.send_to_char(ch, "You are not holy enough to write on this board.\r\n");
        return;
    }

    let guard = boards(g);
    {
        if guard.boards[board_type].len() >= MAX_BOARD_MESSAGES {
            g.send_to_char(ch, "The board is full.\r\n");
            return;
        }
    }

    // skip_spaces + delete_doubledollar, then truncate the headline at 80 chars
    // ("arg[81] = '\0'" in C — keep at most 81 bytes).
    let mut headline = arg.trim_start().to_string();
    headline = delete_doubledollar(&headline);
    crate::text::truncate_utf8_bytes(&mut headline, 81);

    if headline.is_empty() {
        g.send_to_char(ch, "We must have a headline!\r\n");
        return;
    }

    // "%6.10s %-12s :: %s" — date (first 10 chars of asctime, fixed width),
    // "(name)" left-justified to 12, then the headline.
    let name = g
        .get_char(ch)
        .map(|c| c.player.name.clone())
        .unwrap_or_default();
    let tmstr = board_timestamp();
    let date10: String = tmstr.chars().take(10).collect();
    let namebuf = format!("({})", name);
    let heading = format!("{:>6.10} {:<12} :: {}", date10, namebuf, headline);

    // Commit the heading + reserve the (empty) body slot, recording its index.
    let msg_index;
    let guard = boards(g);
    {
        guard.boards[board_type].push(BoardMsg {
            heading: heading.into_bytes(),
            message: Vec::new(),
            level,
        });
        msg_index = guard.boards[board_type].len() - 1;
    }
    let writer_conn = g.get_char(ch).and_then(|c| c.desc);
    {
        let guard = boards(g);
        if let Some(conn) = writer_conn {
            guard.pending.insert(
                conn.0,
                PendingBoardWrite {
                    board_type,
                    msg_index,
                    authorization,
                },
            );
        }
    }

    g.send_to_char(ch, "Write your message.  (/s saves /h for help)\r\n\r\n");
    act(
        g,
        "$n starts to write a message.",
        true,
        ch,
        None,
        ActArg::None,
        To::Room,
    );

    // Open the string editor on the descriptor (C: ch->desc->str = &slot).
    if let Some(conn) = g.get_char(ch).and_then(|c| c.desc) {
        crate::modify::start_board_editing(g, conn, MAX_MESSAGE_LENGTH);
    }
}

/// board_finish_write(): the editor-completion hook. The integrator calls this
/// when a player's `StringEdit` context (opened by `board_write`) finishes.
/// `save` mirrors whether the editor was saved (`/s`) or aborted; on abort the
/// reserved message is rolled back (C frees the slot when the player quits
/// without saving — we drop the just-pushed message). On save, the body is
/// installed, PLR_WRITING is cleared, and the board is flushed to disk.
///
/// Reports whether the edit belonged to a board and whether publication was
/// refused because the opening session no longer owns sufficient authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoardFinishOutcome {
    NotBoard,
    Finished,
    AuthorizationDenied,
}

pub fn board_finish_write(
    g: &mut GameState,
    conn: ConnId,
    body: &str,
    save: bool,
) -> BoardFinishOutcome {
    let pending = {
        let guard = boards(g);
        guard.pending.remove(&conn.0)
    };
    let pending = match pending {
        Some(p) => p,
        None => return BoardFinishOutcome::NotBoard,
    };
    let board_type = pending.board_type;
    let msg_index = pending.msg_index;

    // Clear PLR_WRITING on the owning character, if any.
    let cid = g.descriptors.get(&conn).and_then(|d| d.character);
    if let Some(cid) = cid {
        if let Some(c) = g.get_char_mut(cid) {
            c.act_flags &= !PLR_WRITING;
        }
    }

    if board_type >= g.social.boards.boards.len() {
        return BoardFinishOutcome::Finished;
    }
    if save
        && !g.authenticated_session_request_is_current(
            pending.authorization,
            i32::from(WRITE_LVL(board_type)),
        )
    {
        let guard = boards(g);
        if msg_index < guard.boards[board_type].len()
            && guard.boards[board_type][msg_index].message.is_empty()
        {
            guard.boards[board_type].remove(msg_index);
            for pending in guard.pending.values_mut() {
                if pending.board_type == board_type && pending.msg_index > msg_index {
                    pending.msg_index -= 1;
                }
            }
        }
        return BoardFinishOutcome::AuthorizationDenied;
    }
    let (path, msgs, format, quarantined);
    {
        let guard = boards(g);
        if save {
            if let Some(msg) = guard.boards[board_type].get_mut(msg_index) {
                msg.message = body.as_bytes().to_vec();
            }
        } else {
            // C boards.c:270-271 keeps the committed heading + slot on abort:
            // the board shows the ghost entry and `read` answers 'That message
            // seems to be empty.' The port deleted the entry instead (#171).
            if msg_index < guard.boards[board_type].len()
                && guard.boards[board_type][msg_index].message.is_empty()
            {
                // keep the reserved slot, as C does
            }
        }
        path = board_file_path(&guard.lib_path, board_type);
        msgs = guard.boards[board_type].clone();
        format = guard.formats[board_type];
        quarantined = guard.quarantined[board_type];
    }
    if quarantined {
        log::error!(
            "SYSERR: Board '{}' is quarantined (corrupt save file); write refused to \
             preserve the file for recovery.",
            BOARD_INFO[board_type].filename
        );
        return BoardFinishOutcome::Finished;
    }
    if let Err(e) = save_board_file(&path, &msgs, format) {
        eprintln!(
            "SYSERR: Error writing board '{}': {}",
            BOARD_INFO[board_type].filename, e
        );
    }
    BoardFinishOutcome::Finished
}

#[cfg(test)]
/// Serialize tests that re-boot the process-global board runtime: parallel
/// `boot_boards` callers interleave mid-reset and see each other's boards.
#[cfg(test)]
pub(crate) fn seed_pending_write_for_test(g: &mut GameState, conn: ConnId) {
    boards(g).pending.insert(
        conn.0,
        PendingBoardWrite {
            board_type: usize::MAX,
            msg_index: usize::MAX,
            authorization: crate::state::AuthenticatedCommandRequest {
                requester_body: CharId(u64::MAX),
                requester_principal: CharId(u64::MAX),
                descriptor: conn,
                idnum: -1,
            },
        },
    );
}

#[cfg(test)]
pub(crate) fn has_pending_write_for_test(g: &GameState, conn: ConnId) -> bool {
    boards_ref(g).pending.contains_key(&conn.0)
}

// ---------------------------------------------------------------------------
// Board_show_board (look / examine)
// ---------------------------------------------------------------------------

/// Board_show_board(): list every message heading on the board.
/// Returns `true` when the command was consumed.
pub fn board_show(g: &mut GameState, board_type: usize, ch: CharId, arg: &str) -> bool {
    if g.get_char(ch).and_then(|c| c.desc).is_none() {
        return false;
    }

    let (tmp, _) = one_argument(arg);
    if tmp.is_empty() || !isname(&tmp, "board bulletin") {
        return false;
    }

    if char_authority_i32(g, ch).is_none_or(|level| level < READ_LVL(board_type) as i32) {
        g.send_to_char(ch, "You try but fail to understand the holy words.\r\n");
        return true;
    }
    act(
        g,
        "$n studies the board.",
        true,
        ch,
        None,
        ActArg::None,
        To::Room,
    );

    let msgs = board_msgs(g, board_type);
    let mut buf = String::from(
        "This is a bulletin board.  Usage: READ/REMOVE <messg #>, WRITE <header>.\r\n\
         You will need to look at the board to save your message.\r\n",
    );
    if msgs.is_empty() {
        buf.push_str("The board is empty.\r\n");
    } else {
        buf.push_str(&format!(
            "There are {} messages on the board.\r\n",
            msgs.len()
        ));
        for (i, m) in msgs.iter().enumerate() {
            if m.heading.is_empty() {
                // C: "SYSERR: The board is fubar'd."
                eprintln!("SYSERR: The board is fubar'd.");
                g.send_to_char(ch, "Sorry, the board isn't working.\r\n");
                return true;
            }
            buf.push_str(&format!("{:<2} : {}\r\n", i + 1, m.heading_text()));
        }
    }
    // C boards.c:324: page_string(ch->desc, buf, 1) (#168).
    if let Some(conn) = g.get_char(ch).and_then(|c| c.desc) {
        crate::modify::page_string(g, conn, &buf);
    } else {
        g.send_to_char(ch, &buf);
    }
    true
}

// ---------------------------------------------------------------------------
// Board_display_msg (read <#>)
// ---------------------------------------------------------------------------

/// Board_display_msg(): show one message body by number. Returns `true` when
/// consumed (and `false` to let the normal `read`/`look` handler run, e.g. for
/// a non-numeric, non-"board" argument).
pub fn board_display(g: &mut GameState, board_type: usize, ch: CharId, arg: &str) -> bool {
    let (number, _) = one_argument(arg);
    if number.is_empty() {
        return false;
    }
    // "read board" -> show the board listing.
    if isname(&number, "board bulletin") {
        return board_show(g, board_type, ch, arg);
    }
    // Must start with a digit and parse to a non-zero number.
    if !number
        .chars()
        .next()
        .map(|c| c.is_ascii_digit())
        .unwrap_or(false)
    {
        return false;
    }
    let msg = match crate::text::parse_i32_atoi(&number) {
        Ok(msg) => msg,
        Err(crate::text::ParseIntError::Overflow) => {
            g.send_to_char(
                ch,
                "That message number is outside the supported range.\r\n",
            );
            return true;
        }
        Err(_) => return false,
    };
    if msg == 0 {
        return false;
    }

    if char_authority_i32(g, ch).is_none_or(|level| level < READ_LVL(board_type) as i32) {
        g.send_to_char(ch, "You try but fail to understand the holy words.\r\n");
        return true;
    }

    let msgs = board_msgs(g, board_type);
    if msgs.is_empty() {
        g.send_to_char(ch, "The board is empty!\r\n");
        return true;
    }
    if msg < 1 || msg > msgs.len() as i32 {
        g.send_to_char(ch, "That message exists only in your imagination.\r\n");
        return true;
    }

    let ind = (msg - 1) as usize;
    let m = &msgs[ind];
    if m.heading.is_empty() {
        g.send_to_char(ch, "That message appears to be screwed up.\r\n");
        return true;
    }
    if m.message.is_empty() {
        g.send_to_char(ch, "That message seems to be empty.\r\n");
        return true;
    }

    let buffer = format!(
        "Message {} : {}\r\n\r\n{}\r\n",
        msg,
        m.heading_text(),
        m.message_text()
    );
    // C boards.c:382: page_string(ch->desc, buffer, 1) (#168).
    if let Some(conn) = g.get_char(ch).and_then(|c| c.desc) {
        crate::modify::page_string(g, conn, &buffer);
    } else {
        g.send_to_char(ch, &buffer);
    }
    true
}

// ---------------------------------------------------------------------------
// Board_remove_msg (remove <#>)
// ---------------------------------------------------------------------------

/// Board_remove_msg(): delete a message by number, subject to level checks.
/// Returns `true` when consumed (false to fall through, matching C when the
/// argument is non-numeric so the real `remove` command handles eq removal).
pub fn board_remove(g: &mut GameState, board_type: usize, ch: CharId, arg: &str) -> bool {
    let (number, _) = one_argument(arg);

    if number.is_empty()
        || !number
            .chars()
            .next()
            .map(|c| c.is_ascii_digit())
            .unwrap_or(false)
    {
        return false;
    }
    let msg = match crate::text::parse_i32_atoi(&number) {
        Ok(msg) => msg,
        Err(crate::text::ParseIntError::Overflow) => {
            g.send_to_char(
                ch,
                "That message number is outside the supported range.\r\n",
            );
            return true;
        }
        Err(_) => return false,
    };
    if msg == 0 {
        return false;
    }

    let msgs = board_msgs(g, board_type);
    if msgs.is_empty() {
        g.send_to_char(ch, "The board is empty!\r\n");
        return true;
    }
    if msg < 1 || msg > msgs.len() as i32 {
        g.send_to_char(ch, "That message exists only in your imagination.\r\n");
        return true;
    }

    let ind = (msg - 1) as usize;
    let m = &msgs[ind];
    if m.heading.is_empty() {
        g.send_to_char(ch, "That message appears to be screwed up.\r\n");
        return true;
    }

    let name = g
        .get_char(ch)
        .map(|c| c.player.name.clone())
        .unwrap_or_default();
    let namebuf = format!("({})", name);
    let Some(level) = char_authority_i32(g, ch) else {
        g.send_to_char(
            ch,
            "You are not holy enough to remove other people's messages.\r\n",
        );
        return true;
    };

    // Permission: high enough to remove others' messages, OR it's your own.
    // Author match runs against the FIXED author column (bytes 11..23) only:
    // the old whole-heading substring match let a player's name inside a
    // player-typed headline ("I saw (Bob) cheating") bypass the level gate.
    let heading = m.heading_text();
    let author_col = heading.get(11..23).unwrap_or("").trim().to_string();
    let own_message = author_col == namebuf || author_col.starts_with(&namebuf);
    if level < REMOVE_LVL(board_type) as i32 && !own_message {
        g.send_to_char(
            ch,
            "You are not holy enough to remove other people's messages.\r\n",
        );
        return true;
    }
    if level < m.level {
        g.send_to_char(ch, "You can't remove a message holier than yourself.\r\n");
        return true;
    }

    // C guards against removing a message whose body is still being authored.
    // We block removal if any connection has a pending write to this exact
    // (board, index).
    let guard = boards(g);
    {
        let authoring = guard
            .pending
            .values()
            .any(|pending| pending.board_type == board_type && pending.msg_index == ind);
        if authoring {
            // guard dropped here (end of owned borrow)
            g.send_to_char(
                ch,
                "At least wait until the author is finished before removing it!\r\n",
            );
            return true;
        }
    }

    // Remove the message and shift indices (Vec::remove does the C shuffle).
    // Any pending writes at a higher index must shift down by one to stay
    // pointing at the right message.
    let guard = boards(g);
    {
        if ind < guard.boards[board_type].len() {
            guard.boards[board_type].remove(ind);
        }
        for pending in guard.pending.values_mut() {
            if pending.board_type == board_type && pending.msg_index > ind {
                pending.msg_index -= 1;
            }
        }
        let path = board_file_path(&guard.lib_path, board_type);
        let snapshot = guard.boards[board_type].clone();
        let format = guard.formats[board_type];
        let quarantined = guard.quarantined[board_type];
        // guard dropped here (end of owned borrow)
        if quarantined {
            log::error!(
                "SYSERR: Board '{}' is quarantined (corrupt save file); write refused to \
                 preserve the file for recovery.",
                BOARD_INFO[board_type].filename
            );
        } else if let Err(e) = save_board_file(&path, &snapshot, format) {
            eprintln!(
                "SYSERR: Error writing board '{}': {}",
                BOARD_INFO[board_type].filename, e
            );
        }
    }

    g.send_to_char(ch, "Message removed.\r\n");
    let line = format!("$n just removed message {}.", msg);
    act(g, &line, false, ch, None, ActArg::None, To::Room);
    true
}

// ---------------------------------------------------------------------------
// Save / load (Board_save_board / Board_load_board).
// ---------------------------------------------------------------------------

/// Board_save_board(): write the board's messages to disk, or remove the file
/// when the board is empty (C unlinks the file in that case).
fn save_board_file(
    path: &str,
    msgs: &[BoardMsg],
    format: crate::cformat::PersistenceFormat,
) -> std::io::Result<()> {
    if msgs.is_empty() {
        // unlink(FILENAME) — ignore "not found".
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        return Ok(());
    }

    let out = match format {
        crate::cformat::PersistenceFormat::C => {
            let records: Vec<_> = msgs
                .iter()
                .enumerate()
                .map(|(index, msg)| {
                    let mut heading = msg.heading.clone();
                    heading.push(0);
                    let message = if msg.message.is_empty() {
                        Vec::new()
                    } else {
                        let mut bytes = msg.message.clone();
                        bytes.push(0);
                        bytes
                    };
                    crate::cformat::CBoardMsg {
                        slot_num: index as i32,
                        level: msg.level,
                        heading,
                        message,
                    }
                })
                .collect();
            crate::cformat::encode_board_file(&records)
        }
        crate::cformat::PersistenceFormat::Rust => {
            let mut out = Vec::new();
            out.extend_from_slice(BOARD_FILE_MAGIC);
            out.extend_from_slice(&(msgs.len() as u32).to_le_bytes());
            for msg in msgs {
                write_blob(&mut out, &msg.level.to_le_bytes());
                write_bytes(&mut out, &msg.heading);
                write_bytes(&mut out, &msg.message);
            }
            out
        }
    };
    crate::cformat::atomic_write(std::path::Path::new(path), &out)
}

fn write_blob(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(bytes);
}

fn write_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(bytes);
}

/// Outcome of reading one board file.
enum BoardLoad {
    /// File absent: an empty board (C: silent ENOENT).
    Missing,
    /// Parsed cleanly.
    Loaded(Vec<BoardMsg>, crate::cformat::PersistenceFormat),
    /// Present but unparseable (bad magic, truncated, absurd counts). The
    /// caller must quarantine the board instead of fabricating an empty one.
    Corrupt,
}

/// Board_load_board(): read a board file. Unlike C — which resets a corrupt
/// board and lets the next save destroy it — corruption is reported to the
/// caller so the file can be preserved (fail closed).
fn load_board_file(path: &str) -> std::io::Result<BoardLoad> {
    let mut f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BoardLoad::Missing),
        Err(e) => return Err(e),
    };
    let mut data = Vec::new();
    f.read_to_end(&mut data)?;

    let decoded = if data.starts_with(BOARD_FILE_MAGIC) {
        parse_board_blob(&data).map(|msgs| (msgs, crate::cformat::PersistenceFormat::Rust))
    } else {
        crate::cformat::decode_board_file(&data).map(|records| {
            let msgs = records
                .into_iter()
                .map(|record| {
                    let mut heading = record.heading;
                    heading.pop(); // strict decoder proved the trailing NUL
                    let mut message = record.message;
                    if !message.is_empty() {
                        message.pop();
                    }
                    BoardMsg {
                        heading,
                        message,
                        level: record.level,
                    }
                })
                .collect();
            (msgs, crate::cformat::PersistenceFormat::C)
        })
    };
    Ok(match decoded {
        Some(result) => BoardLoad::Loaded(result.0, result.1),
        None => BoardLoad::Corrupt,
    })
}

fn parse_board_blob(data: &[u8]) -> Option<Vec<BoardMsg>> {
    let mut pos = 0usize;
    let magic = take(data, &mut pos, 4)?;
    if magic != BOARD_FILE_MAGIC {
        return None;
    }
    let count = read_u32(data, &mut pos)? as usize;
    if count > MAX_BOARD_MESSAGES {
        return None;
    }
    let mut msgs = Vec::with_capacity(count);
    for _ in 0..count {
        let level = read_i32(data, &mut pos)?;
        let heading = read_bytes(data, &mut pos)?;
        let message = read_bytes(data, &mut pos)?;
        // C resets the board if a heading_len is zero (heading required).
        if heading.is_empty() {
            return None;
        }
        msgs.push(BoardMsg {
            heading,
            message,
            level,
        });
    }
    (pos == data.len()).then_some(msgs)
}

fn take<'a>(data: &'a [u8], pos: &mut usize, n: usize) -> Option<&'a [u8]> {
    if *pos + n > data.len() {
        return None;
    }
    let slice = &data[*pos..*pos + n];
    *pos += n;
    Some(slice)
}

fn read_u32(data: &[u8], pos: &mut usize) -> Option<u32> {
    let b = take(data, pos, 4)?;
    Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn read_i32(data: &[u8], pos: &mut usize) -> Option<i32> {
    read_u32(data, pos).map(|v| v as i32)
}

fn read_bytes(data: &[u8], pos: &mut usize) -> Option<Vec<u8>> {
    let len = read_u32(data, pos)? as usize;
    if len > MAX_MESSAGE_LENGTH * 2 {
        return None;
    }
    let bytes = take(data, pos, len)?;
    Some(bytes.to_vec())
}

// ---------------------------------------------------------------------------
// Small helpers (utils.c equivalents) + field accessors.
// ---------------------------------------------------------------------------

#[allow(non_snake_case)]
fn READ_LVL(b: usize) -> Level {
    BOARD_INFO[b].read_lvl
}
#[allow(non_snake_case)]
fn WRITE_LVL(b: usize) -> Level {
    BOARD_INFO[b].write_lvl
}
#[allow(non_snake_case)]
fn REMOVE_LVL(b: usize) -> Level {
    BOARD_INFO[b].remove_lvl
}

/// A snapshot copy of one board's messages (so we hold no static lock while
/// touching GameState for I/O).
fn board_msgs(g: &GameState, board_type: usize) -> Vec<BoardMsg> {
    boards_ref(g)
        .boards
        .get(board_type)
        .cloned()
        .unwrap_or_default()
}

fn char_authority_i32(g: &GameState, ch: CharId) -> Option<i32> {
    // Invalid descriptor aliases and corrupt persisted trust return no
    // authority instead of inheriting the body's display level. Callers deny
    // explicitly, including the remove-your-own-message exception.
    g.principal_authority(ch)
        .map(|principal| principal.authority)
}

fn capture_board_authorization(
    g: &GameState,
    ch: CharId,
) -> Option<crate::state::AuthenticatedCommandRequest> {
    if let Some(request) = crate::interpreter::authenticated_command_request(g, ch) {
        return Some(request);
    }

    #[cfg(test)]
    {
        let authority = g
            .principal_authority(ch)
            .filter(|authority| authority.is_authenticated_player())?;
        let descriptor = authority.descriptor?;
        let principal = g.get_char(authority.principal)?;
        return Some(crate::state::AuthenticatedCommandRequest {
            requester_body: ch,
            requester_principal: authority.principal,
            descriptor,
            idnum: principal.idnum,
        });
    }

    #[cfg(not(test))]
    None
}

/// delete_doubledollar(): collapse "$$" to "$" (interpreter.c). Boards run this
/// on the headline before storing so escaped dollars round-trip.
fn delete_doubledollar(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        out.push(c);
        if c == '$' && chars.peek() == Some(&'$') {
            chars.next();
        }
    }
    out
}

/// Build the C `asctime(localtime())` style timestamp ("Tue Jun 15 ..."), of
/// which board_write keeps the first 10 chars ("Day Mon DD"). We avoid a hard
/// chrono format dependency by composing it from chrono::Local.
fn board_timestamp() -> String {
    use chrono::{Datelike, Local, Timelike};
    let now = Local::now();
    const WD: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    const MON: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let wd = now.weekday().num_days_from_monday() as usize;
    let mon = (now.month0()) as usize;
    // asctime: "Www Mmm dd hh:mm:ss yyyy" (day-of-month space-padded to 2).
    format!(
        "{} {} {:2} {:02}:{:02}:{:02} {}",
        WD[wd.min(6)],
        MON[mon.min(11)],
        now.day(),
        now.hour(),
        now.minute(),
        now.second(),
        now.year()
    )
}

#[cfg(test)]
mod persistence_tests {
    use super::*;

    fn connected_player(
        g: &mut GameState,
        conn: ConnId,
        name: &str,
        level: Level,
        trust: i32,
    ) -> CharId {
        let mut character =
            crate::character::Character::new_player(name.to_string(), Class::Warrior, Race::Human);
        character.player.level = level;
        character.trust = trust;
        character.desc = Some(conn);
        let character = g.create_char(character);
        let mut descriptor =
            crate::connection::Descriptor::new(conn, "board-authority.test".to_string());
        descriptor.state = crate::connection::ConState::Playing;
        descriptor.character = Some(character);
        g.descriptors.insert(conn, descriptor);
        character
    }

    fn temp_file(name: &str) -> String {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir()
            .join(format!(
                "deltamud-board-{}-{}-{}",
                std::process::id(),
                name,
                stamp
            ))
            .join("board")
            .to_string_lossy()
            .into_owned()
    }

    #[test]
    fn c_board_load_and_rewrite_preserve_format_and_raw_text_bytes() {
        let path = temp_file("c-format");
        let records = [crate::cformat::CBoardMsg {
            slot_num: 41,
            level: 55,
            heading: vec![b'(', 0xff, b')', 0],
            message: vec![b'A', 0xfe, b'B', 0],
        }];
        crate::cformat::atomic_write(
            std::path::Path::new(&path),
            &crate::cformat::encode_board_file(&records),
        )
        .unwrap();

        let BoardLoad::Loaded(messages, format) = load_board_file(&path).unwrap() else {
            panic!("valid C board file failed to load");
        };
        assert_eq!(format, crate::cformat::PersistenceFormat::C);
        assert_eq!(messages[0].heading, vec![b'(', 0xff, b')']);
        assert_eq!(messages[0].message, vec![b'A', 0xfe, b'B']);

        save_board_file(&path, &messages, format).unwrap();
        let rewritten = std::fs::read(&path).unwrap();
        assert!(!rewritten.starts_with(BOARD_FILE_MAGIC));
        let decoded = crate::cformat::decode_board_file(&rewritten).unwrap();
        assert_eq!(decoded[0].heading, records[0].heading);
        assert_eq!(decoded[0].message, records[0].message);
        let _ = std::fs::remove_dir_all(std::path::Path::new(&path).parent().unwrap());
    }

    #[test]
    fn existing_rust_board_format_remains_rust_on_rewrite() {
        let path = temp_file("rust-format");
        let messages = [BoardMsg {
            heading: b"Rust heading".to_vec(),
            message: b"Rust body".to_vec(),
            level: 12,
        }];
        save_board_file(&path, &messages, crate::cformat::PersistenceFormat::Rust).unwrap();

        let BoardLoad::Loaded(loaded, format) = load_board_file(&path).unwrap() else {
            panic!("valid board file failed to load");
        };
        assert_eq!(format, crate::cformat::PersistenceFormat::Rust);
        save_board_file(&path, &loaded, format).unwrap();
        assert!(std::fs::read(&path).unwrap().starts_with(BOARD_FILE_MAGIC));
        let _ = std::fs::remove_dir_all(std::path::Path::new(&path).parent().unwrap());
    }

    /// A corrupt board file must be quarantined, never silently replaced:
    /// boot reports it, leaves the bytes untouched, and the runtime refuses
    /// saves for that board (C "Resetting." would destroy it on next save).
    #[test]
    fn corrupt_board_file_is_quarantined_and_preserved() {
        let lib = std::path::PathBuf::from(temp_file("corrupt-quarantine"))
            .parent()
            .unwrap()
            .to_path_buf();
        let path = lib.join("etc/board/mortal/general");
        let corrupt: &[u8] = b"DBRD\xff\xff\xff\xffnot really a board";
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, corrupt).unwrap();

        let mut g = GameState::new(crate::config::Config::default());
        boot_boards(&mut g, lib.to_str().unwrap());

        assert_eq!(
            std::fs::read(&path).unwrap(),
            corrupt,
            "boot must not modify a corrupt board file"
        );
        {
            let guard = boards(&mut g);
            assert!(guard.quarantined[0], "corrupt board must be quarantined");
            assert!(guard.boards[0].is_empty());
        }
        let _ = std::fs::remove_dir_all(lib);
    }

    /// A truncated-but-magic-tagged Rust board (crash mid-write) is corrupt
    /// input for the same purpose: quarantined, bytes preserved.
    #[test]
    fn truncated_rust_board_file_is_quarantined_and_preserved() {
        let lib = std::path::PathBuf::from(temp_file("truncated-quarantine"))
            .parent()
            .unwrap()
            .to_path_buf();
        let path = lib.join("etc/board/mortal/general");
        let truncated: &[u8] = b"DBRD\x03\x00\x00\x00head";
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, truncated).unwrap();

        let mut g = GameState::new(crate::config::Config::default());
        boot_boards(&mut g, lib.to_str().unwrap());

        assert_eq!(std::fs::read(&path).unwrap(), truncated);
        {
            let guard = boards_ref(&g);
            assert!(guard.quarantined[0]);
        }
        let _ = std::fs::remove_dir_all(lib);
    }

    #[test]
    fn board_read_write_and_remove_levels_use_persisted_trust() {
        const IMPLEMENTOR_BOARD: usize = 3;
        let root = std::path::PathBuf::from(temp_file("authority"))
            .parent()
            .unwrap()
            .join("lib");
        let mut g = GameState::new(crate::config::Config::default());
        boot_boards(&mut g, root.to_str().unwrap());

        let display = connected_player(&mut g, ConnId(821), "Display", LVL_IMPL, 1);
        let trusted = connected_player(&mut g, ConnId(822), "Trusted", 1, i32::from(LVL_IMPL));

        assert!(board_show(&mut g, IMPLEMENTOR_BOARD, display, "board"));
        assert!(
            g.descriptors[&ConnId(821)]
                .outbuf
                .contains("fail to understand the holy words")
        );
        g.descriptors.get_mut(&ConnId(821)).unwrap().outbuf.clear();

        assert!(board_show(&mut g, IMPLEMENTOR_BOARD, trusted, "board"));
        assert!(
            g.descriptors[&ConnId(822)]
                .outbuf
                .contains("The board is empty")
        );
        g.descriptors.get_mut(&ConnId(822)).unwrap().outbuf.clear();

        board_write(&mut g, IMPLEMENTOR_BOARD, display, "forged authority");
        assert!(
            g.descriptors[&ConnId(821)]
                .outbuf
                .contains("not holy enough to write")
        );
        assert!(boards(&mut g).boards[IMPLEMENTOR_BOARD].is_empty());

        board_write(&mut g, IMPLEMENTOR_BOARD, trusted, "trusted authority");
        assert!(
            g.descriptors[&ConnId(822)]
                .outbuf
                .contains("Write your message")
        );
        assert_eq!(
            boards(&mut g).boards[IMPLEMENTOR_BOARD][0].level,
            i32::from(LVL_IMPL)
        );

        {
            let runtime = boards(&mut g);
            runtime.pending.clear();
            runtime.boards[IMPLEMENTOR_BOARD] = vec![BoardMsg {
                heading: b"           (Other)      :: protected".to_vec(),
                message: b"body".to_vec(),
                level: 1,
            }];
        }
        g.descriptors.get_mut(&ConnId(821)).unwrap().outbuf.clear();
        g.descriptors.get_mut(&ConnId(822)).unwrap().outbuf.clear();

        assert!(board_remove(&mut g, IMPLEMENTOR_BOARD, display, "1"));
        assert!(
            g.descriptors[&ConnId(821)]
                .outbuf
                .contains("not holy enough to remove other people's messages")
        );
        assert_eq!(boards(&mut g).boards[IMPLEMENTOR_BOARD].len(), 1);

        assert!(board_remove(&mut g, IMPLEMENTOR_BOARD, trusted, "1"));
        assert!(
            g.descriptors[&ConnId(822)]
                .outbuf
                .contains("Message removed")
        );
        assert!(boards(&mut g).boards[IMPLEMENTOR_BOARD].is_empty());

        boot_boards(&mut g, root.join("empty-reset").to_str().unwrap());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn private_board_save_revalidates_the_opening_session_and_write_level() {
        const IMPLEMENTOR_BOARD: usize = 3;
        #[derive(Clone, Copy, Debug)]
        enum Revocation {
            Trust,
            Quarantine,
            DescriptorBody,
        }

        for (index, revocation) in [
            Revocation::Trust,
            Revocation::Quarantine,
            Revocation::DescriptorBody,
        ]
        .into_iter()
        .enumerate()
        {
            let root = std::path::PathBuf::from(temp_file(&format!("write-revalidate-{index}")))
                .parent()
                .unwrap()
                .join("lib");
            let mut g = GameState::new(crate::config::Config::default());
            boot_boards(&mut g, root.to_str().unwrap());
            let conn = ConnId(850 + index as u64);
            let writer = connected_player(&mut g, conn, "Writer", LVL_IMPL, i32::from(LVL_IMPL));
            board_write(&mut g, IMPLEMENTOR_BOARD, writer, "private draft");
            assert!(has_pending_write_for_test(&g, conn), "case={revocation:?}");

            match revocation {
                Revocation::Trust => g.get_char_mut(writer).unwrap().trust = 1,
                Revocation::Quarantine => {
                    let idnum = g.get_char(writer).unwrap().idnum;
                    g.authority_quarantine.insert(idnum);
                }
                Revocation::DescriptorBody => {
                    let mut replacement = crate::character::Character::new_player(
                        "Replacement".into(),
                        Class::Warrior,
                        Race::Human,
                    );
                    replacement.desc = Some(conn);
                    let replacement = g.create_char(replacement);
                    g.descriptors.get_mut(&conn).unwrap().character = Some(replacement);
                }
            }

            assert_eq!(
                board_finish_write(&mut g, conn, "secret body", true),
                BoardFinishOutcome::AuthorizationDenied,
                "case={revocation:?}"
            );
            assert!(
                boards(&mut g).boards[IMPLEMENTOR_BOARD].is_empty(),
                "case={revocation:?}"
            );
            assert!(!has_pending_write_for_test(&g, conn), "case={revocation:?}");
            let _ = std::fs::remove_dir_all(root);
        }
    }
}
