// modify.rs — the run-time string editor + pager (CircleMUD modify.c), ported
// to the single-owner GameState. This module owns:
//
//   * the line-oriented string editor (string_add / parse_action): the `/`-style
//     editor commands (/a /c /d /e /f /fi /i /l /n /r /ra /s /h), buffer
//     accumulation, and save/abort dispatch to the right consumer (note / mail /
//     board / OLC immortal field);
//   * format_text / replace_str (utils.c) used by /f and /r;
//   * the Michael-Buselli pager (next_page / count_pages / paginate_string /
//     page_string / show_string) with per-connection paging state;
//   * do_string  — the immortal field editor (string <field> <target>);
//   * do_skillset — the immortal "skillset <name> '<skill>' <value>" command.
//
// Integration contract (the bits game.rs must call):
//   * start_string_editing(g, conn, max_len) — push a blank generic editor;
//   * editor_input(g, conn, line) -> bool      — feed one input line to the
//     active StringEdit editor; returns `true` while still editing, `false`
//     once the editor saved or aborted (so the caller pops the context and
//     restores the normal prompt);
//   * page_active(conn) / page_input(g, conn, line) — pager hooks.
//
// Per-edit state the Descriptor / Character lack (the edit's *purpose*, the
// pager's page vector) lives in module statics keyed by ConnId, exactly as the
// brief prescribes (no GameState methods / Character fields added).

use crate::act::{act, ActArg, To};
use crate::connection::InputContext;
use crate::interpreter::{half_chop, one_argument};
use crate::spell_parser::{find_skill_num, skill_name, TOP_SPELL_DEFINE};
use crate::state::GameState;
use crate::types::*;
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::OnceLock;

// ---------------------------------------------------------------------------
// Constants mirrored from structs.h / interpreter.h.
// ---------------------------------------------------------------------------

/// FORMAT_INDENT (interpreter.h) — /fi indented format flag.
const FORMAT_INDENT: i32 = 1 << 0;

/// PLR_WRITING / PLR_MAILING (structs.h): cleared when the editor finishes.
const PLR_WRITING: i64 = 1 << 4;
const PLR_MAILING: i64 = 1 << 5;

/// Pager geometry (modify.c).
const PAGE_LENGTH: i32 = 22;
const PAGE_WIDTH: i32 = 80;

// ---------------------------------------------------------------------------
// Per-connection edit purpose. The C code stashes this in the descriptor
// (d->mail_to / d->note / OLC_MODE / STATE(d)); here it is a side table keyed
// by ConnId so the Character / Descriptor field sets stay untouched.
// ---------------------------------------------------------------------------

/// Which immortal-editable field a `do_string` edit targets (string_fields[]).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum StrField {
    Name,
    Short,
    Long,
    Description,
    Title,
    DeleteDescription,
}

/// What a finished editor should do with its gathered buffer.
#[derive(Clone)]
enum EditTarget {
    /// A plain player message (no save hook); just drop it on finish.
    Plain,
    /// `write` — install the note text into an object's action_description.
    Note(ObjId),
    /// `mail` compose — hand off to mail::finish_mail / abort_mail.
    Mail,
    /// board write — hand off to boards::board_finish_write.
    Board,
    /// `string` immortal field edit on a character.
    CharField { cid: CharId, field: StrField },
    /// `string` immortal field edit on an object.
    ObjField { oid: ObjId, field: StrField },
    /// `tedit` — CON_TEXTED: write the saved buffer to a text file (OLC_STORAGE).
    TextFile(std::path::PathBuf),
}

struct EditState {
    target: EditTarget,
    max_len: usize,
}

fn edits() -> &'static Mutex<HashMap<ConnId, EditState>> {
    static EDITS: OnceLock<Mutex<HashMap<ConnId, EditState>>> = OnceLock::new();
    EDITS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn set_edit(conn: ConnId, target: EditTarget, max_len: usize) {
    edits()
        .lock()
        .unwrap()
        .insert(conn, EditState { target, max_len });
}

fn take_edit(conn: ConnId) -> Option<EditState> {
    edits().lock().unwrap().remove(&conn)
}

fn edit_max_len(conn: ConnId) -> usize {
    edits()
        .lock()
        .unwrap()
        .get(&conn)
        .map(|e| e.max_len)
        .unwrap_or(0)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BufferEditorResult {
    Continue,
    Save,
    Abort,
}

// ---------------------------------------------------------------------------
// Per-connection pager state (showstr_*). Holds the already-paginated pages so
// successive RETURN/B/R/<n> requests can walk them.
// ---------------------------------------------------------------------------

struct Pager {
    pages: Vec<String>,
    page: usize,
}

fn pagers() -> &'static Mutex<HashMap<ConnId, Pager>> {
    static PAGERS: OnceLock<Mutex<HashMap<ConnId, Pager>>> = OnceLock::new();
    PAGERS.get_or_init(|| Mutex::new(HashMap::new()))
}

// ===========================================================================
// Buffer access helpers — the editor buffer lives in the descriptor's top
// StringEdit context (InputContext::StringEdit{buffer,max_len}). Mirrors C's
// `*d->str`.
// ===========================================================================

fn read_buffer(g: &GameState, conn: ConnId) -> Option<String> {
    let d = g.descriptors.get(&conn)?;
    match d.editors.last() {
        Some(InputContext::StringEdit { buffer, .. }) => Some(buffer.clone()),
        _ => None,
    }
}

fn write_buffer(g: &mut GameState, conn: ConnId, new: String) {
    if let Some(d) = g.descriptors.get_mut(&conn) {
        if let Some(InputContext::StringEdit { buffer, .. }) = d.editors.last_mut() {
            *buffer = new;
        }
    }
}

/// Append text to the connection's output buffer (SEND_TO_Q on d).
fn send_to_q(g: &mut GameState, conn: ConnId, msg: &str) {
    if let Some(d) = g.descriptors.get_mut(&conn) {
        d.outbuf.push_str(msg);
    }
}

fn conn_char(g: &GameState, conn: ConnId) -> Option<CharId> {
    g.descriptors.get(&conn).and_then(|d| d.character)
}

// ===========================================================================
// delete_doubledollar (interpreter.c): collapse "$$" -> "$".
// ===========================================================================
pub fn delete_doubledollar(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut bytes = s.chars().peekable();
    while let Some(c) = bytes.next() {
        if c == '$' && bytes.peek() == Some(&'$') {
            bytes.next();
        }
        out.push(c);
    }
    out
}

// ===========================================================================
// Public entry points used by game.rs / the editor openers.
// ===========================================================================

/// start_string_editing — open a blank generic editor (no save hook). The
/// caller is responsible for the "Write your message." prompt; this just pushes
/// the StringEdit context and registers the (Plain) edit purpose.
pub fn start_string_editing(g: &mut GameState, conn: ConnId, max_len: usize) {
    push_editor(g, conn, max_len, EditTarget::Plain);
}

/// Open a note editor whose save installs the body into `obj`'s
/// action_description (the do_write consumer).
pub fn start_note_editing(g: &mut GameState, conn: ConnId, obj: ObjId, max_len: usize) {
    push_editor(g, conn, max_len, EditTarget::Note(obj));
}

/// Open the mail-compose editor (boards/mail openers already register their own
/// pending state; this binds the StringEdit context to the Mail finisher).
pub fn start_mail_editing(g: &mut GameState, conn: ConnId, max_len: usize) {
    push_editor(g, conn, max_len, EditTarget::Mail);
}

/// Open the board-post editor (the board opener already reserved the slot).
pub fn start_board_editing(g: &mut GameState, conn: ConnId, max_len: usize) {
    push_editor(g, conn, max_len, EditTarget::Board);
}

/// Open the CON_TEXTED text-file editor (do_tedit). The current file contents
/// pre-fill the buffer (C echoes `fields[l].buffer` and seeds `backstr`); on
/// save the buffer is written back to `path`.
pub fn start_textfile_editing(
    g: &mut GameState,
    conn: ConnId,
    path: std::path::PathBuf,
    initial: &str,
    max_len: usize,
) {
    push_editor_with(
        g,
        conn,
        max_len,
        EditTarget::TextFile(path),
        initial.to_string(),
    );
}

fn push_editor(g: &mut GameState, conn: ConnId, max_len: usize, target: EditTarget) {
    push_editor_with(g, conn, max_len, target, String::new());
}

fn push_editor_with(
    g: &mut GameState,
    conn: ConnId,
    max_len: usize,
    target: EditTarget,
    initial: String,
) {
    set_edit(conn, target, max_len);
    if let Some(d) = g.descriptors.get_mut(&conn) {
        d.editors.push(InputContext::StringEdit {
            buffer: initial,
            max_len,
        });
    }
    // Mark the character PLR_WRITING (and PLR_MAILING for mail).
    let mailing = matches!(
        edits().lock().unwrap().get(&conn).map(|e| &e.target),
        Some(EditTarget::Mail)
    );
    if let Some(cid) = conn_char(g, conn) {
        if let Some(c) = g.get_char_mut(cid) {
            if !c.is_npc {
                c.act_flags |= PLR_WRITING;
                if mailing {
                    c.act_flags |= PLR_MAILING;
                }
            }
        }
    }
}

/// True while `conn` has an active string editor on top of its context stack.
pub fn editing(g: &GameState, conn: ConnId) -> bool {
    matches!(
        g.descriptors.get(&conn).and_then(|d| d.editors.last()),
        Some(InputContext::StringEdit { .. })
    )
}

/// editor_input — feed one input line into the active StringEdit editor. This is
/// the port of CircleMUD string_add(). Returns `true` while the editor remains
/// active (caller stays in edit mode) and `false` once the editor has saved or
/// aborted (caller pops the StringEdit context off the descriptor and resumes
/// normal input). The StringEdit context itself is NOT popped here — the caller
/// (game.rs) removes it when this returns false, mirroring the C state reset.
pub fn editor_input(g: &mut GameState, conn: ConnId, line: &str) -> bool {
    let max = edit_max_len(conn);
    let mut buf = read_buffer(g, conn).unwrap_or_default();
    let result = editor_buffer_input(g, conn, &mut buf, max, line);
    write_buffer(g, conn, buf);

    match result {
        BufferEditorResult::Continue => true,
        BufferEditorResult::Save => {
            finish_editor(g, conn, 1);
            false
        }
        BufferEditorResult::Abort => {
            finish_editor(g, conn, 2);
            false
        }
    }
}

/// Feed one input line into a raw editable buffer using CircleMUD's string
/// editor command set. OLC editors use this adapter for inline text fields so
/// their `/` commands stay identical to the generic runtime editor.
pub fn editor_buffer_input(
    g: &mut GameState,
    conn: ConnId,
    buf: &mut String,
    max_len: usize,
    line: &str,
) -> BufferEditorResult {
    // determine if this is the terminal string, and truncate if so
    // (C: '/<letter>' style editing commands; '@' handling removed in C).
    let str_in = delete_doubledollar(line);

    let mut terminator = BufferEditorResult::Continue;
    let mut action = false; // an editor command ran (don't append to buffer)
    let mut content = str_in.clone(); // text to append (cleared by commands)

    if str_in.starts_with('/') {
        action = true;
        // actions = str_in[2..] (everything after "/x").
        let actions: String = str_in.chars().skip(2).collect();
        content.clear();
        let cmd = str_in.chars().nth(1).unwrap_or('\0');
        match cmd {
            'a' => terminator = BufferEditorResult::Abort,
            'c' => {
                if !buf.is_empty() {
                    buf.clear();
                    send_to_q(g, conn, "Current buffer cleared.\r\n");
                } else {
                    send_to_q(g, conn, "Current buffer empty.\r\n");
                }
            }
            'd' => parse_action(g, conn, ParseCmd::Delete, &actions, buf, max_len),
            'e' => parse_action(g, conn, ParseCmd::Edit, &actions, buf, max_len),
            'f' => {
                if !buf.is_empty() {
                    parse_action(g, conn, ParseCmd::Format, &actions, buf, max_len);
                } else {
                    send_to_q(g, conn, "Current buffer empty.\r\n");
                }
            }
            'i' => {
                if !buf.is_empty() {
                    parse_action(g, conn, ParseCmd::Insert, &actions, buf, max_len);
                } else {
                    send_to_q(g, conn, "Current buffer empty.\r\n");
                }
            }
            'h' => parse_action(g, conn, ParseCmd::Help, &actions, buf, max_len),
            'l' => {
                if !buf.is_empty() {
                    parse_action(g, conn, ParseCmd::ListNorm, &actions, buf, max_len);
                } else {
                    send_to_q(g, conn, "Current buffer empty.\r\n");
                }
            }
            'n' => {
                if !buf.is_empty() {
                    parse_action(g, conn, ParseCmd::ListNum, &actions, buf, max_len);
                } else {
                    send_to_q(g, conn, "Current buffer empty.\r\n");
                }
            }
            'r' => parse_action(g, conn, ParseCmd::Replace, &actions, buf, max_len),
            's' => terminator = BufferEditorResult::Save,
            _ => send_to_q(g, conn, "Invalid option.\r\n"),
        }
    }

    // Append the line to the buffer (only if it was not a command), with the
    // max_str truncation / overflow rules of string_add.
    if !action {
        let mut append = content.clone();
        if buf.is_empty() {
            if append.len() > max_len {
                if let Some(cid) = conn_char(g, conn) {
                    g.send_to_char(cid, "String too long - Truncated.\r\n");
                }
                append.truncate(max_len);
            }
            *buf = append;
        } else if append.len() + buf.len() > max_len {
            if let Some(cid) = conn_char(g, conn) {
                g.send_to_char(
                    cid,
                    "String too long, limit reached on message. Last line ignored.\r\n",
                );
            }
            return BufferEditorResult::Continue;
        } else {
            buf.push_str(&append);
        }
    }

    if terminator == BufferEditorResult::Continue {
        // Not finished: a non-command line gets a trailing CRLF appended
        // (C: `else if (!action) strcat(*d->str, "\r\n");`).
        if !action {
            buf.push_str("\r\n");
        }
        return BufferEditorResult::Continue;
    }

    terminator
}

fn buffer_nonempty(g: &GameState, conn: ConnId) -> bool {
    read_buffer(g, conn).map(|b| !b.is_empty()).unwrap_or(false)
}

/// Dispatch the finished buffer (terminator 1 = save, 2 = abort) to its target,
/// clear PLR_WRITING/PLR_MAILING, and drop the edit-state side table entry.
/// The StringEdit context is removed by the caller after this returns.
fn finish_editor(g: &mut GameState, conn: ConnId, terminator: i32) {
    let buffer = read_buffer(g, conn).unwrap_or_default();
    let saved = terminator == 1;
    // An all-empty saved buffer becomes "no string" (C NULLs it out).
    let body = buffer.trim_end_matches(['\r', '\n']);

    let state = take_edit(conn);
    let target = state.map(|s| s.target).unwrap_or(EditTarget::Plain);
    let cid = conn_char(g, conn);

    match target {
        EditTarget::Note(oid) => {
            if saved {
                let text = if body.is_empty() {
                    None
                } else {
                    Some(buffer.clone())
                };
                if let Some(o) = g.get_obj_mut(oid) {
                    o.action_description = text;
                }
                send_to_q(g, conn, "Note saved.\r\n");
            } else {
                send_to_q(g, conn, "Note aborted.\r\n");
            }
            if let Some(cid) = cid {
                act(
                    g,
                    "$n stops writing a note.",
                    true,
                    cid,
                    None,
                    ActArg::None,
                    To::Room,
                );
            }
        }
        EditTarget::Mail => {
            if saved {
                if crate::mail::finish_mail(g, conn, body) {
                    send_to_q(g, conn, "Message sent!\r\n");
                }
            } else {
                crate::mail::abort_mail(conn);
                send_to_q(g, conn, "Mail aborted.\r\n");
            }
        }
        EditTarget::Board => {
            crate::boards::board_finish_write(g, conn, body, saved);
            if saved {
                send_to_q(g, conn, "Post not aborted, use REMOVE <post #>.\r\n");
            }
        }
        EditTarget::CharField { cid: tcid, field } => {
            apply_char_field(g, tcid, field, saved, body);
            if !saved {
                send_to_q(g, conn, "Edit aborted.\r\n");
            } else {
                send_to_q(g, conn, "Field updated.\r\n");
            }
        }
        EditTarget::ObjField { oid, field } => {
            apply_obj_field(g, oid, field, saved, body);
            if !saved {
                send_to_q(g, conn, "Edit aborted.\r\n");
            } else {
                send_to_q(g, conn, "Field updated.\r\n");
            }
        }
        EditTarget::TextFile(path) => {
            // CON_TEXTED save path (modify.c): fopen(OLC_STORAGE,"w") + fputs of
            // stripcr(*d->str). Mirror the SYSERR/OLC mudlog lines and the
            // "$n stops editing some scrolls." room act.
            if saved {
                let stripped: String = buffer.chars().filter(|&c| c != '\r').collect();
                match std::fs::write(&path, stripped.as_bytes()) {
                    Ok(_) => {
                        let name = cid
                            .and_then(|c| g.get_char(c))
                            .map(|c| c.player.name.clone())
                            .unwrap_or_default();
                        mudlog(
                            g,
                            &format!("OLC: {} saves '{}'.", name, path.display()),
                            crate::syslog::CMP,
                            LVL_GOD,
                        );
                        send_to_q(g, conn, "Saved.\r\n");
                    }
                    Err(_) => {
                        mudlog(
                            g,
                            &format!("SYSERR: Can't write file '{}'.", path.display()),
                            crate::syslog::CMP,
                            LVL_IMPL,
                        );
                    }
                }
            } else {
                send_to_q(g, conn, "Edit aborted.\r\n");
            }
            if let Some(cid) = cid {
                act(
                    g,
                    "$n stops editing some scrolls.",
                    true,
                    cid,
                    None,
                    ActArg::None,
                    To::Room,
                );
            }
        }
        EditTarget::Plain => {
            if !saved {
                send_to_q(g, conn, "Message aborted.\r\n");
            }
        }
    }

    // Clear PLR_WRITING | PLR_MAILING on the writer.
    if let Some(cid) = cid {
        if let Some(c) = g.get_char_mut(cid) {
            if !c.is_npc {
                c.act_flags &= !(PLR_WRITING | PLR_MAILING);
            }
        }
    }
}

/// Install a saved buffer into an immortal-edited character field.
fn apply_char_field(g: &mut GameState, cid: CharId, field: StrField, saved: bool, body: &str) {
    if !saved {
        return;
    }
    let text = body.to_string();
    if let Some(c) = g.get_char_mut(cid) {
        match field {
            StrField::Name => c.player.name = text,
            StrField::Short => c.short_desc = if text.is_empty() { None } else { Some(text) },
            StrField::Long => {
                c.long_desc = if text.is_empty() {
                    None
                } else {
                    Some(format!("{}\r\n", text))
                }
            }
            StrField::Description => c.player.description = text,
            StrField::Title => c.player.title = if text.is_empty() { None } else { Some(text) },
            StrField::DeleteDescription => c.player.description = String::new(),
        }
    }
}

/// Install a saved buffer into an immortal-edited object field.
fn apply_obj_field(g: &mut GameState, oid: ObjId, field: StrField, saved: bool, body: &str) {
    if !saved {
        return;
    }
    let text = body.to_string();
    if let Some(o) = g.get_obj_mut(oid) {
        match field {
            StrField::Name => o.name = text,
            StrField::Short => o.short_description = text,
            StrField::Long | StrField::Description => o.description = text,
            StrField::Title => o.short_description = text,
            StrField::DeleteDescription => o.action_description = None,
        }
    }
}

// ===========================================================================
// parse_action — the `/` editor command handlers (modify.c).
// ===========================================================================

#[derive(Clone, Copy)]
enum ParseCmd {
    Format,
    Replace,
    Help,
    Delete,
    Insert,
    ListNorm,
    ListNum,
    Edit,
}

fn parse_action(
    g: &mut GameState,
    conn: ConnId,
    command: ParseCmd,
    string: &str,
    buf: &mut String,
    max: usize,
) {
    match command {
        ParseCmd::Help => {
            let help = "Editor command formats: /<letter>\r\n\r\n\
/a         -  aborts editor\r\n\
/c         -  clears buffer\r\n\
/d#        -  deletes a line #\r\n\
/e# <text> -  changes the line at # with <text>\r\n\
/f         -  formats text\r\n\
/fi        -  indented formatting of text\r\n\
/h         -  list text editor commands\r\n\
/i# <text> -  inserts <text> before line #\r\n\
/l         -  lists buffer\r\n\
/n         -  lists buffer with line numbers\r\n\
/r 'a' 'b' -  replace 1st occurance of text <a> in buffer with text <b>\r\n\
/ra 'a' 'b'-  replace all occurances of text <a> within buffer with text <b>\r\n\
              usage: /r[a] 'pattern' 'replacement'\r\n\
/s         -  saves text\r\n";
            send_to_q(g, conn, help);
        }
        ParseCmd::Format => {
            // Parse the leading flag chars ('i' => indent), up to 2.
            let mut indent = false;
            let mut flags = 0;
            for ch in string.chars().take(2) {
                if !ch.is_ascii_alphabetic() {
                    break;
                }
                if ch == 'i' && !indent {
                    indent = true;
                    flags |= FORMAT_INDENT;
                }
            }
            let formatted = format_text(buf, flags, max);
            *buf = formatted;
            let msg = format!(
                "Text formatted with{} indent.\r\n",
                if indent { "" } else { "out" }
            );
            send_to_q(g, conn, &msg);
        }
        ParseCmd::Replace => {
            // Optional 'a' (replace-all) prefix before the first quote.
            let mut rep_all = false;
            for ch in string.chars().take(2) {
                if !ch.is_ascii_alphabetic() {
                    break;
                }
                if ch == 'a' {
                    rep_all = true;
                }
            }
            // strtok(string, "'") chain: tokens between single quotes.
            let mut toks = string.split('\'');
            let _flagseg = toks.next(); // text before the 1st quote (the flags)
            let s = match toks.next() {
                Some(t) if !t.is_empty() => t,
                Some(_) => {
                    send_to_q(
                        g,
                        conn,
                        "Target string must be enclosed in single quotes.\r\n",
                    );
                    return;
                }
                None => {
                    send_to_q(g, conn, "Invalid format.\r\n");
                    return;
                }
            };
            // skip the separator between pattern and replacement (the C
            // strtok(NULL,"'") that yields the inter-quote glue).
            let _between = toks.next();
            let t = match toks.next() {
                Some(t) => t,
                None => {
                    send_to_q(g, conn, "No replacement string.\r\n");
                    return;
                }
            };

            // total_len = (len(t) - len(s)) + len(buf) <= max
            let total_len = buf.len() as i64 - s.len() as i64 + t.len() as i64;
            if total_len > max as i64 {
                send_to_q(g, conn, "Not enough space left in buffer.\r\n");
                return;
            }
            match replace_str(buf, s, t, rep_all, max) {
                ReplaceResult::Replaced(n, new) => {
                    *buf = new;
                    let plural = if n != 1 { "s " } else { " " };
                    send_to_q(
                        g,
                        conn,
                        &format!(
                            "Replaced {} occurance{}of '{}' with '{}'.\r\n",
                            n, plural, s, t
                        ),
                    );
                }
                ReplaceResult::NotFound => {
                    send_to_q(g, conn, &format!("String '{}' not found.\r\n", s));
                }
                ReplaceResult::Overflow => {
                    send_to_q(
                        g,
                        conn,
                        "ERROR: Replacement string causes buffer overflow, aborted replace.\r\n",
                    );
                }
            }
        }
        ParseCmd::Delete => {
            let (low, high) = match scan_range(string) {
                Some(r) => r,
                None => {
                    send_to_q(
                        g,
                        conn,
                        "You must specify a line number or range to delete.\r\n",
                    );
                    return;
                }
            };
            if high < low {
                send_to_q(g, conn, "That range is invalid.\r\n");
                return;
            }
            if low <= 0 {
                send_to_q(
                    g,
                    conn,
                    "Invalid line numbers to delete must be higher than 0.\r\n",
                );
                return;
            }
            if buf.is_empty() {
                send_to_q(g, conn, "Buffer is empty.\r\n");
                return;
            }
            match delete_lines(buf, low, high) {
                Some((new, deleted)) => {
                    *buf = new;
                    let plural = if deleted != 1 { "s " } else { " " };
                    send_to_q(g, conn, &format!("{} line{}deleted.\r\n", deleted, plural));
                }
                None => send_to_q(g, conn, "Line(s) out of range; not deleting.\r\n"),
            }
        }
        ParseCmd::ListNorm => list_buffer(g, conn, string, false, buf),
        ParseCmd::ListNum => list_buffer(g, conn, string, true, buf),
        ParseCmd::Insert => {
            let (numstr, text) = half_chop(string);
            if numstr.is_empty() {
                send_to_q(
                    g,
                    conn,
                    "You must specify a line number before which to insert text.\r\n",
                );
                return;
            }
            let line_low: i32 = numstr.parse().unwrap_or(0);
            if buf.is_empty() {
                send_to_q(g, conn, "Buffer is empty, nowhere to insert.\r\n");
                return;
            }
            if line_low <= 0 {
                send_to_q(g, conn, "Line number must be higher than 0.\r\n");
                return;
            }
            match insert_line(buf, line_low, &text, max) {
                InsertResult::Ok(new) => {
                    *buf = new;
                    send_to_q(g, conn, "Line inserted.\r\n");
                }
                InsertResult::OutOfRange => {
                    send_to_q(g, conn, "Line number out of range; insert aborted.\r\n");
                }
                InsertResult::Overflow => {
                    send_to_q(
                        g,
                        conn,
                        "Insert text pushes buffer over maximum size, insert aborted.\r\n",
                    );
                }
            }
        }
        ParseCmd::Edit => {
            let (numstr, text) = half_chop(string);
            if numstr.is_empty() {
                send_to_q(
                    g,
                    conn,
                    "You must specify a line number at which to change text.\r\n",
                );
                return;
            }
            let line_low: i32 = numstr.parse().unwrap_or(0);
            if buf.is_empty() {
                send_to_q(g, conn, "Buffer is empty, nothing to change.\r\n");
                return;
            }
            if line_low <= 0 {
                send_to_q(g, conn, "Line number must be higher than 0.\r\n");
                return;
            }
            match edit_line(buf, line_low, &text, max) {
                EditLineResult::Ok(new) => {
                    *buf = new;
                    send_to_q(g, conn, "Line changed.\r\n");
                }
                EditLineResult::OutOfRange => {
                    send_to_q(g, conn, "Line number out of range; change aborted.\r\n");
                }
                EditLineResult::Overflow => {
                    send_to_q(
                        g,
                        conn,
                        "Change causes new length to exceed buffer maximum size, aborted.\r\n",
                    );
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Buffer line operations (pure on the buffer string). Lines are delimited by
// '\n' exactly as the C code counts them (CRLF leaves "\r" at the end of each
// listed line, matching the original byte-for-byte).
// ---------------------------------------------------------------------------

/// Split into '\n'-terminated segments preserving the terminators, like the C
/// pointer walk over '\n' boundaries. The final segment may have no '\n'.
fn line_segments(buf: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for ch in buf.chars() {
        cur.push(ch);
        if ch == '\n' {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// sscanf(" %d - %d ") emulation: None (no number), Some((n,n)) (one), or
/// Some((low,high)) (a range).
fn scan_range(s: &str) -> Option<(i32, i32)> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(idx) = trimmed.find('-') {
        let low = trimmed[..idx].trim().parse::<i32>().ok();
        let high = trimmed[idx + 1..].trim().parse::<i32>().ok();
        match (low, high) {
            (Some(l), Some(h)) => Some((l, h)),
            (Some(l), None) => Some((l, l)),
            _ => None,
        }
    } else {
        trimmed
            .split_whitespace()
            .next()
            .and_then(|w| w.parse::<i32>().ok())
            .map(|n| (n, n))
    }
}

/// Delete lines [low, high] (1-based). Returns (new_buffer, lines_deleted) or
/// None if the range starts past the end of the buffer.
fn delete_lines(buf: &str, low: i32, high: i32) -> Option<(String, i32)> {
    let segs = line_segments(buf);
    let total = segs.len() as i32;
    if low > total {
        return None;
    }
    let lo = (low - 1).max(0) as usize;
    let hi = (high.min(total) as usize).max(lo + 1);
    let hi = hi.min(segs.len());
    let deleted = (hi - lo) as i32;
    let mut out = String::new();
    for (i, seg) in segs.iter().enumerate() {
        if i < lo || i >= hi {
            out.push_str(seg);
        }
    }
    Some((out, deleted))
}

enum InsertResult {
    Ok(String),
    OutOfRange,
    Overflow,
}

/// Insert `text` (with a CRLF appended, as the C does) before 1-based line
/// `line_low`.
fn insert_line(buf: &str, line_low: i32, text: &str, max: usize) -> InsertResult {
    let segs = line_segments(buf);
    let idx = (line_low - 1) as usize;
    if idx >= segs.len() {
        return InsertResult::OutOfRange;
    }
    let insertion = format!("{}\r\n", text);
    let mut out = String::new();
    for (i, seg) in segs.iter().enumerate() {
        if i == idx {
            out.push_str(&insertion);
        }
        out.push_str(seg);
    }
    if out.len() + 3 > max {
        return InsertResult::Overflow;
    }
    InsertResult::Ok(out)
}

enum EditLineResult {
    Ok(String),
    OutOfRange,
    Overflow,
}

/// Replace 1-based line `line_low` with `text` (CRLF-terminated).
fn edit_line(buf: &str, line_low: i32, text: &str, max: usize) -> EditLineResult {
    let segs = line_segments(buf);
    let idx = (line_low - 1) as usize;
    if idx >= segs.len() {
        return EditLineResult::OutOfRange;
    }
    let replacement = format!("{}\r\n", text);
    let mut out = String::new();
    for (i, seg) in segs.iter().enumerate() {
        if i == idx {
            out.push_str(&replacement);
        } else {
            out.push_str(seg);
        }
    }
    if out.len() > max {
        return EditLineResult::Overflow;
    }
    EditLineResult::Ok(out)
}

/// List the buffer (/l plain, /n numbered) honoring an optional line range.
fn list_buffer(g: &mut GameState, conn: ConnId, string: &str, numbered: bool, buf: &str) {
    let (low, high) = if string.trim().is_empty() {
        (1, 999999)
    } else {
        match scan_range(string) {
            Some((l, h)) => (l, h),
            None => (1, 999999),
        }
    };
    if low < 1 {
        send_to_q(g, conn, "Line numbers must be greater than 0.\r\n");
        return;
    }
    if high < low {
        send_to_q(g, conn, "That range is invalid.\r\n");
        return;
    }
    let segs = line_segments(buf);
    let total = segs.len() as i32;
    if low > total {
        send_to_q(g, conn, "Line(s) out of range; no buffer listing.\r\n");
        return;
    }

    let mut out = String::new();
    if !numbered && (high < 999999 || low > 1) {
        out.push_str(&format!("Current buffer range [{} - {}]:\r\n", low, high));
    }
    let lo = (low - 1) as usize;
    let hi = high.min(total) as usize;
    for (i, seg) in segs.iter().enumerate().take(hi).skip(lo) {
        if numbered {
            out.push_str(&format!("{:4}:\r\n", i + 1));
        }
        out.push_str(seg);
    }
    page_string(g, conn, &out);
}

// ===========================================================================
// format_text / replace_str (utils.c).
// ===========================================================================

/// format_text (utils.c): word-wrap + sentence capitalisation, 79-col, optional
/// 3-space indent. Returns the formatted buffer (truncated to `maxlen`).
fn format_text(src: &str, mode: i32, maxlen: usize) -> String {
    let mut formatted = String::new();
    let mut total_chars: usize;
    if mode & FORMAT_INDENT != 0 {
        formatted.push_str("   ");
        total_chars = 3;
    } else {
        total_chars = 0;
    }

    let chars: Vec<char> = src.chars().collect();
    let n = chars.len();
    let mut i = 0;
    let mut cap_next = true;
    let mut cap_next_next = false;

    let is_ws = |c: char| matches!(c, '\n' | '\r' | '\u{0c}' | '\t' | '\u{0b}' | ' ');

    while i < n {
        // Skip leading whitespace.
        while i < n && is_ws(chars[i]) {
            i += 1;
        }
        if i < n {
            let start = i;
            i += 1;
            while i < n && !is_ws(chars[i]) && chars[i] != '.' && chars[i] != '?' && chars[i] != '!'
            {
                i += 1;
            }
            if cap_next_next {
                cap_next_next = false;
                cap_next = true;
            }
            // Move off any sentence delimiters that follow the word.
            while i < n && (chars[i] == '.' || chars[i] == '!' || chars[i] == '?') {
                cap_next_next = true;
                i += 1;
            }
            let mut word: String = chars[start..i].iter().collect();

            if total_chars + word.chars().count() + 1 > 79 {
                formatted.push_str("\r\n");
                total_chars = 0;
            }
            if !cap_next {
                if total_chars > 0 {
                    formatted.push(' ');
                    total_chars += 1;
                }
            } else {
                cap_next = false;
                // UPPER(*start): capitalise the word's first character.
                let mut wc = word.chars();
                if let Some(first) = wc.next() {
                    word = first.to_ascii_uppercase().to_string() + wc.as_str();
                }
            }
            total_chars += word.chars().count();
            formatted.push_str(&word);
        }

        if cap_next_next {
            if total_chars + 3 > 79 {
                formatted.push_str("\r\n");
                total_chars = 0;
            } else {
                formatted.push_str("  ");
                total_chars += 2;
            }
        }
    }
    formatted.push_str("\r\n");
    if formatted.len() > maxlen {
        formatted.truncate(maxlen);
    }
    formatted
}

enum ReplaceResult {
    Replaced(i32, String),
    NotFound,
    Overflow,
}

/// replace_str (utils.c): replace the first (or all) occurrence(s) of `pattern`
/// with `replacement`, bounded by `max_size`.
fn replace_str(
    buf: &str,
    pattern: &str,
    replacement: &str,
    rep_all: bool,
    max_size: usize,
) -> ReplaceResult {
    if pattern.is_empty() {
        return ReplaceResult::NotFound;
    }
    // (len(buf) - len(pattern)) + len(replacement) > max_size  ->  overflow.
    if buf.len().saturating_sub(pattern.len()) + replacement.len() > max_size {
        return ReplaceResult::Overflow;
    }

    if rep_all {
        let mut out = String::new();
        let mut count = 0;
        let mut rest = buf;
        while let Some(pos) = rest.find(pattern) {
            if out.len() + pos + replacement.len() > max_size {
                return ReplaceResult::Overflow;
            }
            out.push_str(&rest[..pos]);
            out.push_str(replacement);
            count += 1;
            rest = &rest[pos + pattern.len()..];
        }
        if count == 0 {
            return ReplaceResult::NotFound;
        }
        out.push_str(rest);
        ReplaceResult::Replaced(count, out)
    } else {
        match buf.find(pattern) {
            None => ReplaceResult::NotFound,
            Some(pos) => {
                let mut out = String::with_capacity(buf.len());
                out.push_str(&buf[..pos]);
                out.push_str(replacement);
                out.push_str(&buf[pos + pattern.len()..]);
                ReplaceResult::Replaced(1, out)
            }
        }
    }
}

// ===========================================================================
// Pager (modify.c): next_page / count_pages / paginate_string / page_string /
// show_string. ANSI-aware, 22-line / 80-col pages.
// ===========================================================================

/// next_page: byte offset of the start of the next page in `str`, or None if
/// this is the last page. Mirrors the C col/line/ANSI walk.
fn next_page(bytes: &[u8], start: usize) -> Option<usize> {
    let mut col = 1;
    let mut line = 1;
    let mut spec_code = false;
    let mut idx = start;
    loop {
        if idx >= bytes.len() {
            return None;
        }
        if line > PAGE_LENGTH {
            return Some(idx);
        }
        let c = bytes[idx];
        if c == 0x1B && !spec_code {
            spec_code = true;
        } else if c == b'm' && spec_code {
            spec_code = false;
        } else if !spec_code {
            if c == b'\r' {
                col = 1;
            } else if c == b'\n' {
                line += 1;
            } else if col > PAGE_WIDTH {
                col = 1;
                line += 1;
            } else {
                col += 1;
            }
        }
        idx += 1;
    }
}

/// Split `str` into page strings using next_page boundaries.
fn paginate(str: &str) -> Vec<String> {
    let bytes = str.as_bytes();
    let mut bounds = vec![0usize];
    let mut at = 0usize;
    while let Some(next) = next_page(bytes, at) {
        bounds.push(next);
        at = next;
    }
    bounds.push(bytes.len());
    let mut pages = Vec::new();
    for w in bounds.windows(2) {
        if w[0] < w[1] {
            // Slice on byte boundaries that are guaranteed valid: next_page only
            // ever stops at the start of a fresh column/line, never mid-UTF-8,
            // since multibyte bytes count as ordinary columns.
            pages.push(str[w[0]..w[1]].to_string());
        }
    }
    if pages.is_empty() {
        pages.push(String::new());
    }
    pages
}

/// page_string: the entry point — paginate `str`, store the pages on the
/// connection, and show the first page (CircleMUD page_string keep_internal=1).
pub fn page_string(g: &mut GameState, conn: ConnId, str: &str) {
    if str.is_empty() {
        return;
    }
    let pages = paginate(str);
    pagers()
        .lock()
        .unwrap()
        .insert(conn, Pager { pages, page: 0 });
    show_string(g, conn, "");
}

/// page_active: whether `conn` is mid-pagination (has pending pages).
pub fn page_active(conn: ConnId) -> bool {
    pagers().lock().unwrap().contains_key(&conn)
}

/// page_input: feed a pager command line (RETURN/Q/R/B/<n>) for an active
/// pager. Returns true if the connection was paging (input was consumed).
pub fn page_input(g: &mut GameState, conn: ConnId, line: &str) -> bool {
    if !page_active(conn) {
        return false;
    }
    show_string(g, conn, line);
    true
}

/// show_string (modify.c): display the next page (or honor Q/R/B/<n>).
fn show_string(g: &mut GameState, conn: ConnId, input: &str) {
    let (cmd, count, total) = {
        let guard = pagers().lock().unwrap();
        let p = match guard.get(&conn) {
            Some(p) => p,
            None => return,
        };
        let first = input
            .trim_start()
            .chars()
            .next()
            .map(|c| c.to_ascii_lowercase());
        (first, p.page, p.pages.len())
    };

    match cmd {
        Some('q') => {
            pagers().lock().unwrap().remove(&conn);
            return;
        }
        Some('r') => {
            let mut guard = pagers().lock().unwrap();
            if let Some(p) = guard.get_mut(&conn) {
                p.page = p.page.saturating_sub(1);
            }
        }
        Some('b') => {
            let mut guard = pagers().lock().unwrap();
            if let Some(p) = guard.get_mut(&conn) {
                p.page = p.page.saturating_sub(2);
            }
        }
        Some(c) if c.is_ascii_digit() => {
            let want = input.trim().parse::<i64>().unwrap_or(1);
            let new = (want - 1).clamp(0, total as i64 - 1) as usize;
            let mut guard = pagers().lock().unwrap();
            if let Some(p) = guard.get_mut(&conn) {
                p.page = new;
            }
        }
        Some(_) => {
            let _ = count;
            send_to_q(
                g,
                conn,
                "Valid commands while paging are RETURN, Q, R, B, or a numeric value.\r\n",
            );
            return;
        }
        None => {}
    }

    // Emit the current page; if it was the last one, drop the pager.
    let (text, last) = {
        let guard = pagers().lock().unwrap();
        let p = match guard.get(&conn) {
            Some(p) => p,
            None => return,
        };
        let page = p.page.min(p.pages.len().saturating_sub(1));
        (p.pages[page].clone(), page + 1 >= p.pages.len())
    };
    send_to_q(g, conn, &text);
    if last {
        pagers().lock().unwrap().remove(&conn);
    } else {
        let mut guard = pagers().lock().unwrap();
        if let Some(p) = guard.get_mut(&conn) {
            p.page += 1;
        }
    }
}

// ===========================================================================
// do_string (act.wizard.c style) — immortal field editor.
//   Syntax: string <field> <name> [text...]
// Where <field> is one of: name short long description title delete-description.
// With trailing text the field is set directly; with none, the string editor
// opens on that field (saved by /s). DeltaMUD's string_fields[] / length[].
// ===========================================================================

const STRING_FIELDS: &[(&str, StrField)] = &[
    ("name", StrField::Name),
    ("short", StrField::Short),
    ("long", StrField::Long),
    ("description", StrField::Description),
    ("title", StrField::Title),
    ("delete-description", StrField::DeleteDescription),
];

/// length[] — max field lengths (modify.c): name 15, short 60, long 256,
/// description 240, title 60. delete-description reuses the description bound.
fn field_max_len(field: StrField) -> usize {
    match field {
        StrField::Name => 15,
        StrField::Short => 60,
        StrField::Long => 256,
        StrField::Description => 240,
        StrField::Title => 60,
        StrField::DeleteDescription => 240,
    }
}

pub fn do_string(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let conn = match g.get_char(ch).and_then(|c| c.desc) {
        Some(c) => c,
        None => return,
    };
    let (field_name, rest) = half_chop(arg);
    if field_name.is_empty() {
        g.send_to_char(ch, "Syntax: string <field> <name> [<text>]\r\n");
        g.send_to_char(
            ch,
            "Fields: name short long description title delete-description\r\n",
        );
        return;
    }
    // Match the field by prefix (CircleMUD search_block).
    let field = match STRING_FIELDS
        .iter()
        .find(|(fname, _)| fname.starts_with(field_name.as_str()))
    {
        Some((_, f)) => *f,
        None => {
            g.send_to_char(ch, "That's not a valid field.\r\n");
            return;
        }
    };

    let (target_name, text) = half_chop(&rest);
    if target_name.is_empty() {
        g.send_to_char(ch, "Set the field on whom (or what)?\r\n");
        return;
    }

    // Find a target: a character in the room first, then an object (inv/room).
    let max_len = field_max_len(field);
    if let Some(tcid) = g.get_char_room_vis(ch, &target_name) {
        if g.get_char(tcid).map(|c| c.is_npc).unwrap_or(false)
            || field == StrField::DeleteDescription
        {
            if !text.is_empty() || field == StrField::DeleteDescription {
                // Inline set / delete.
                apply_char_field(g, tcid, field, true, &text);
                g.send_to_char(ch, "Field set.\r\n");
            } else {
                // Open the editor seeded with the current value.
                let seed = current_char_field(g, tcid, field);
                open_field_editor(
                    g,
                    conn,
                    EditTarget::CharField { cid: tcid, field },
                    max_len,
                    &seed,
                );
                g.send_to_char(ch, "Edit the field.  (/s saves /h for help)\r\n\r\n");
            }
        } else {
            g.send_to_char(
                ch,
                "You can only string NPCs and objects (not players' core fields).\r\n",
            );
        }
        return;
    }

    // Object lookup: inventory then room.
    let inv = g
        .get_char(ch)
        .map(|c| c.carrying.clone())
        .unwrap_or_default();
    let mut oid = g.get_obj_in_list_vis(ch, &target_name, &inv);
    if oid.is_none() {
        if let Some(rnum) = g.get_char(ch).and_then(|c| c.in_room) {
            let contents = g.room(rnum).contents.clone();
            oid = g.get_obj_in_list_vis(ch, &target_name, &contents);
        }
    }
    match oid {
        Some(o) => {
            if !text.is_empty() || field == StrField::DeleteDescription {
                apply_obj_field(g, o, field, true, &text);
                g.send_to_char(ch, "Field set.\r\n");
            } else {
                let seed = current_obj_field(g, o, field);
                open_field_editor(
                    g,
                    conn,
                    EditTarget::ObjField { oid: o, field },
                    max_len,
                    &seed,
                );
                g.send_to_char(ch, "Edit the field.  (/s saves /h for help)\r\n\r\n");
            }
        }
        None => g.send_to_char(ch, "No such thing around to set.\r\n"),
    }
}

/// Push a field editor seeded with the field's current text (so /l shows it).
fn open_field_editor(
    g: &mut GameState,
    conn: ConnId,
    target: EditTarget,
    max_len: usize,
    seed: &str,
) {
    set_edit(conn, target, max_len);
    if let Some(d) = g.descriptors.get_mut(&conn) {
        d.editors.push(InputContext::StringEdit {
            buffer: seed.to_string(),
            max_len,
        });
    }
    if let Some(cid) = conn_char(g, conn) {
        if let Some(c) = g.get_char_mut(cid) {
            if !c.is_npc {
                c.act_flags |= PLR_WRITING;
            }
        }
    }
}

fn current_char_field(g: &GameState, cid: CharId, field: StrField) -> String {
    let c = match g.get_char(cid) {
        Some(c) => c,
        None => return String::new(),
    };
    match field {
        StrField::Name => c.player.name.clone(),
        StrField::Short => c.short_desc.clone().unwrap_or_default(),
        StrField::Long => c.long_desc.clone().unwrap_or_default(),
        StrField::Description => c.player.description.clone(),
        StrField::Title => c.player.title.clone().unwrap_or_default(),
        StrField::DeleteDescription => String::new(),
    }
}

fn current_obj_field(g: &GameState, oid: ObjId, field: StrField) -> String {
    let o = match g.get_obj(oid) {
        Some(o) => o,
        None => return String::new(),
    };
    match field {
        StrField::Name => o.name.clone(),
        StrField::Short | StrField::Title => o.short_description.clone(),
        StrField::Long | StrField::Description => o.description.clone(),
        StrField::DeleteDescription => o.action_description.clone().unwrap_or_default(),
    }
}

// ===========================================================================
// do_skillset (modify.c) — immortal skill/spell proficiency setter.
//   Syntax: skillset <name> '<skill>' <value>
// ===========================================================================

pub fn do_skillset(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let (name, rest) = one_argument(arg);

    // No arguments: print the informative spell-list help, 4 per line.
    if name.is_empty() {
        g.send_to_char(ch, "Syntax: skillset <name> '<skill>' <value>\r\n");
        let mut help = String::from("Skill being one of the following:\r\n");
        let mut col = 0;
        for i in 1..=TOP_SPELL_DEFINE {
            let sn = skill_name(i);
            // Skip the reserved / unused / undefined fillers (C skips '!'-names).
            if sn.starts_with('!') || sn == "UNUSED" || sn == "UNDEFINED" {
                continue;
            }
            help.push_str(&format!("{:18}", sn));
            col += 1;
            if col % 4 == 0 {
                help.push_str("\r\n");
                g.send_to_char(ch, &help);
                help.clear();
            }
        }
        if !help.is_empty() {
            g.send_to_char(ch, &help);
        }
        g.send_to_char(ch, "\r\n");
        return;
    }

    // Locate the (visible) target. C uses get_char_vis; we lean on the
    // world-visible finder shared by the casting core.
    let vict = match crate::spell_parser::get_char_world_vis(g, ch, &name) {
        Some(v) => v,
        None => {
            g.send_to_char(ch, "No-one by that name here.\r\n");
            return;
        }
    };

    let rest = rest.trim_start();
    if rest.is_empty() {
        g.send_to_char(ch, "Skill name expected.\r\n");
        return;
    }
    if !rest.starts_with('\'') {
        g.send_to_char(ch, "Skill must be enclosed in: ''\r\n");
        return;
    }
    // Locate the matching close-quote; lowercase the contents (C LOWER loop).
    let after_open = &rest[1..];
    let close = match after_open.find('\'') {
        Some(idx) => idx,
        None => {
            g.send_to_char(ch, "Skill must be enclosed in: ''\r\n");
            return;
        }
    };
    let skillname = after_open[..close].to_lowercase();
    let value_str = after_open[close + 1..].trim_start();

    let skill = find_skill_num(&skillname);
    if skill <= 0 {
        g.send_to_char(ch, "Unrecognized skill.\r\n");
        return;
    }

    let (value_tok, _) = one_argument(value_str);
    if value_tok.is_empty() {
        g.send_to_char(ch, "Learned value expected.\r\n");
        return;
    }
    let value: i32 = value_tok.parse().unwrap_or(-1);
    if value < 0 {
        g.send_to_char(ch, "Minimum value for learned is 0.\r\n");
        return;
    }
    if value > 100 {
        g.send_to_char(ch, "Max value for learned is 100.\r\n");
        return;
    }
    if g.get_char(vict).map(|c| c.is_npc).unwrap_or(false) {
        g.send_to_char(ch, "You can't set NPC skills.\r\n");
        return;
    }
    if skill > TOP_SPELL_DEFINE {
        g.send_to_char(ch, "Unrecognized skill.\r\n");
        return;
    }

    let ch_name = g
        .get_char(ch)
        .map(|c| c.player.name.clone())
        .unwrap_or_default();
    let vict_name = g
        .get_char(vict)
        .map(|c| c.player.name.clone())
        .unwrap_or_default();
    // C: mudlog(buf2, BRF, -1, TRUE) — the -1 level suppresses the immortal
    // echo (file-only). syslog::mudlog takes an unsigned threshold and always
    // echoes; LVL_IMMORT keeps the echo gated to immortals.
    mudlog(
        g,
        &format!(
            "{} changed {}'s {} to {}.",
            ch_name,
            vict_name,
            skill_name(skill),
            value
        ),
        crate::syslog::BRF,
        LVL_IMMORT,
    );

    if let Some(c) = g.get_char_mut(vict) {
        c.set_skill(skill as u16, value as u8);
    }

    g.send_to_char(
        ch,
        &format!(
            "You change {}'s {} to {}.\r\n",
            vict_name,
            skill_name(skill),
            value
        ),
    );
}

/// mudlog — delegate to the shared `syslog::mudlog`, which writes the on-disk
/// `<lib>/syslog` line and echoes it to online immortals filtered by their
/// PRF_LOG syslog level (C utils.c mudlog). `log_type` is the C OFF/BRF/NRM/
/// PFT/CMP class for this message.
fn mudlog(g: &mut GameState, line: &str, log_type: u8, min_level: u8) {
    crate::syslog::mudlog(g, line, log_type, min_level);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::character::Character;
    use crate::config::Config;
    use crate::connection::{ConState, Descriptor};
    use crate::object::Object;
    use crate::types::{Class, ConnId, Race};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST_CONN: AtomicU64 = AtomicU64::new(1);

    fn editor_game() -> (GameState, ConnId, CharId, ObjId) {
        let mut g = GameState::new(Config::default());
        let conn = ConnId(NEXT_TEST_CONN.fetch_add(1, Ordering::SeqCst));

        let mut ch = Character::new_player("Writer".to_string(), Class::Warrior, Race::Human);
        ch.desc = Some(conn);
        let ch_id = g.create_char(ch);

        let mut d = Descriptor::new(conn, "test".to_string());
        d.state = ConState::Playing;
        d.character = Some(ch_id);
        g.descriptors.insert(conn, d);

        let obj = Object::new(1, "note paper".to_string(), "a note".to_string());
        let obj_id = g.create_obj(obj);
        (g, conn, ch_id, obj_id)
    }

    #[test]
    fn note_editor_save_installs_body_and_finishes() {
        let (mut g, conn, _ch, obj) = editor_game();
        start_note_editing(&mut g, conn, obj, 1000);

        assert!(editor_input(&mut g, conn, "hello"));
        assert!(!editor_input(&mut g, conn, "/s"));
        g.descriptors.get_mut(&conn).unwrap().editors.pop();

        assert!(g.descriptors.get(&conn).unwrap().editors.is_empty());
        assert_eq!(
            g.get_obj(obj).unwrap().action_description.as_deref(),
            Some("hello\r\n")
        );
        assert!(g
            .descriptors
            .get(&conn)
            .unwrap()
            .outbuf
            .contains("Note saved."));
    }

    #[test]
    fn note_editor_abort_clears_writer_flag_without_saving() {
        let (mut g, conn, ch, obj) = editor_game();
        start_note_editing(&mut g, conn, obj, 1000);
        assert_ne!(g.get_char(ch).unwrap().act_flags & PLR_WRITING, 0);

        assert!(editor_input(&mut g, conn, "discard me"));
        assert!(!editor_input(&mut g, conn, "/a"));
        g.descriptors.get_mut(&conn).unwrap().editors.pop();

        assert!(g.descriptors.get(&conn).unwrap().editors.is_empty());
        assert!(g.get_obj(obj).unwrap().action_description.is_none());
        assert_eq!(g.get_char(ch).unwrap().act_flags & PLR_WRITING, 0);
        assert!(g
            .descriptors
            .get(&conn)
            .unwrap()
            .outbuf
            .contains("Note aborted."));
    }

    #[test]
    fn buffer_editor_supports_replace_all_command() {
        let (mut g, conn, _ch, _obj) = editor_game();
        let mut buf = "alpha beta alpha\r\n".to_string();

        assert_eq!(
            editor_buffer_input(&mut g, conn, &mut buf, 1000, "/ra 'alpha' 'omega'"),
            BufferEditorResult::Continue
        );

        assert_eq!(buf, "omega beta omega\r\n");
        assert!(g
            .descriptors
            .get(&conn)
            .unwrap()
            .outbuf
            .contains("Replaced 2 occurances"));
    }

    #[test]
    fn buffer_editor_supports_indented_format_command() {
        let (mut g, conn, _ch, _obj) = editor_game();
        let mut buf = "one sentence. another sentence\r\n".to_string();

        assert_eq!(
            editor_buffer_input(&mut g, conn, &mut buf, 1000, "/fi"),
            BufferEditorResult::Continue
        );

        assert!(buf.starts_with("   One sentence."));
        assert!(buf.contains("  Another sentence"));
        assert!(g
            .descriptors
            .get(&conn)
            .unwrap()
            .outbuf
            .contains("Text formatted with indent."));
    }
}
