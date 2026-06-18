// alias.rs — full port of the command-alias subsystem.
//
// Authority: C `src/alias.c` (persistence shape) + the aliasing routines that
// live in `src/interpreter.c` (find_alias / do_alias / perform_complex_alias /
// perform_alias). The C struct alias is a singly-linked list hung off
// player_special_data (GET_ALIASES); the Character struct in this port has no
// such field, so per-player alias storage lives in a module static keyed by
// Character.idnum (the persistent player id), exactly as the contract requires.
//
// House style (see cmd_informative.rs): read needed values into locals first,
// then mutate / send. do_alias talks only to the actor via send_to_char (the C
// uses no act() broadcasts here, so neither do we).
//
// Two entry points are exposed for the command path:
//   * do_alias(g, ch, arg, subcmd) — the `alias` command (list / add / remove).
//   * alias_expand(g, ch, input) -> Option<AliasExpansion> — the expansion hook
//     (perform_alias). SIMPLE aliases replace the current command; COMPLEX
//     aliases return the commands to push onto the descriptor input queue with
//     the C `aliased` marker.

use crate::state::GameState;
use crate::types::*;
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

// C: ALIAS_SIMPLE / ALIAS_COMPLEX (interpreter.h).
const ALIAS_SIMPLE: i32 = 0;
const ALIAS_COMPLEX: i32 = 1;

// C: ALIAS_SEP_CHAR / ALIAS_VAR_CHAR / ALIAS_GLOB_CHAR (interpreter.h).
const ALIAS_SEP_CHAR: char = ';';
const ALIAS_VAR_CHAR: char = '$';
const ALIAS_GLOB_CHAR: char = '*';

// C: NUM_TOKENS (perform_complex_alias) — only $1..$9 are substituted.
const NUM_TOKENS: usize = 9;

// C: MAX_INPUT_LENGTH — each expanded command is truncated to this length.
const MAX_INPUT_LENGTH: usize = 256;

/// One alias record (C `struct alias`). `type` is preserved verbatim
/// (ALIAS_SIMPLE / ALIAS_COMPLEX) so the on-disk format round-trips.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AliasEntry {
    pub alias: String,
    pub replacement: String,
    pub atype: i32,
}

/// Result of C perform_alias().
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AliasExpansion {
    /// ALIAS_SIMPLE: replace the current command line and dispatch it now.
    Simple(String),
    /// ALIAS_COMPLEX: push these commands to the front of the descriptor queue.
    Complex(Vec<String>),
}

/// Per-player alias lists, keyed by Character.idnum (the persistent player id;
/// -1 for mobs, which never get here because do_alias/alias_replace short out
/// on NPCs). The Vec is ordered newest-first, mirroring the C linked list which
/// prepends new aliases at the head (a->next = GET_ALIASES; GET_ALIASES = a).
static ALIASES: OnceLock<Mutex<HashMap<i64, Vec<AliasEntry>>>> = OnceLock::new();

fn table() -> &'static Mutex<HashMap<i64, Vec<AliasEntry>>> {
    ALIASES.get_or_init(|| Mutex::new(HashMap::new()))
}

// ---------------------------------------------------------------------------
// Storage API (also usable by the player-file layer for read/write_aliases).
// ---------------------------------------------------------------------------

/// Replace a player's entire alias list (C read_aliases load path).
pub fn set_aliases(idnum: i64, list: Vec<AliasEntry>) {
    table().lock().unwrap().insert(idnum, list);
}

/// Snapshot a player's alias list for saving (C write_aliases) — newest-first,
/// matching the in-memory order the C linked list iterates for output.
pub fn get_aliases(idnum: i64) -> Vec<AliasEntry> {
    table()
        .lock()
        .unwrap()
        .get(&idnum)
        .cloned()
        .unwrap_or_default()
}

/// Drop a player's aliases from the live table (e.g. on extract). Idempotent.
pub fn clear_aliases(idnum: i64) {
    table().lock().unwrap().remove(&idnum);
}

fn alias_bucket(name: &str) -> &'static str {
    match name.chars().next().unwrap_or('z').to_ascii_lowercase() {
        'a'..='e' => "A-E",
        'f'..='j' => "F-J",
        'k'..='o' => "K-O",
        'p'..='t' => "P-T",
        'u'..='z' => "U-Z",
        _ => "ZZZ",
    }
}

fn alias_filename(lib_path: &str, name: &str) -> Option<PathBuf> {
    if name.is_empty() {
        return None;
    }
    let lower = name.to_ascii_lowercase();
    Some(
        Path::new(lib_path)
            .join("plralias")
            .join(alias_bucket(&lower))
            .join(format!("{lower}.alias")),
    )
}

/// C read_aliases(): load `plralias/<bucket>/<lowername>.alias` triples into
/// the live alias table. Missing files mean the character has no aliases.
pub fn read_aliases(lib_path: &str, name: &str, idnum: i64) -> std::io::Result<()> {
    clear_aliases(idnum);
    let Some(path) = alias_filename(lib_path, name) else {
        return Ok(());
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err),
    };

    let mut lines = text
        .lines()
        .map(|line| line.trim_end_matches('\r').to_string());
    let mut list = Vec::new();
    loop {
        let Some(alias) = lines.next() else {
            break;
        };
        let Some(replacement) = lines.next() else {
            break;
        };
        let Some(atype) = lines.next() else {
            break;
        };
        list.push(AliasEntry {
            alias,
            replacement,
            atype: atype.parse().unwrap_or(ALIAS_SIMPLE),
        });
    }
    if !list.is_empty() {
        set_aliases(idnum, list);
    }
    Ok(())
}

/// C write_aliases(): rewrite the whole alias sidecar. Empty alias lists remove
/// the old file and leave no replacement.
pub fn write_aliases(lib_path: &str, name: &str, idnum: i64) -> std::io::Result<()> {
    let Some(path) = alias_filename(lib_path, name) else {
        return Ok(());
    };
    let aliases = get_aliases(idnum);
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(err),
    }
    if aliases.is_empty() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::File::create(&path)?;
    for alias in aliases {
        writeln!(file, "{}", alias.alias)?;
        writeln!(file, "{}", alias.replacement)?;
        writeln!(file, "{}", alias.atype)?;
    }
    Ok(())
}

fn persist_aliases(g: &GameState, idnum: i64, name: &str) {
    if let Err(err) = write_aliases(&g.config.lib_path, name, idnum) {
        log::warn!("write_aliases({name}) failed: {err}");
    }
}

// ---------------------------------------------------------------------------
// Helpers (C find_alias / perform_complex_alias).
// ---------------------------------------------------------------------------

/// C find_alias(): first list entry whose alias string equals `str` exactly
/// (case-sensitive strcmp, matching C).
fn find_alias_index(list: &[AliasEntry], str: &str) -> Option<usize> {
    list.iter().position(|a| a.alias == str)
}

/// C any_one_arg(): first whitespace-delimited token (case PRESERVED) + the
/// rest with leading whitespace trimmed. The interpreter's any_one_arg
/// lowercases the first token; alias words are case-sensitive in C, so this
/// module keeps its own case-preserving variant.
fn any_one_arg(argument: &str) -> (&str, &str) {
    let s = argument.trim_start();
    match s.find(char::is_whitespace) {
        Some(pos) => (&s[..pos], s[pos..].trim_start()),
        None => (s, ""),
    }
}

/// delete_doubledollar(): collapse "$$" -> "$" (utils.c). do_alias runs this on
/// the replacement before storing it, so a stored "$$" represents a literal "$".
fn delete_doubledollar(s: &str) -> String {
    s.replace("$$", "$")
}

/// C perform_complex_alias(): substitute $1..$9 / $* / $$ in `a.replacement`
/// using the whitespace tokens of `orig` (the line after the alias word), and
/// split the result on ALIAS_SEP_CHAR (';') into one or more commands.
fn perform_complex_alias(orig: &str, a: &AliasEntry) -> Vec<String> {
    // First, parse the original string into up to NUM_TOKENS whitespace tokens
    // (C: strtok on " "). $0 is unused; $1 maps to tokens[0].
    let tokens: Vec<&str> = orig.split_whitespace().take(NUM_TOKENS).collect();

    let mut commands: Vec<String> = Vec::new();
    let mut buf = String::new();

    let mut chars = a.replacement.chars().peekable();
    while let Some(c) = chars.next() {
        if c == ALIAS_SEP_CHAR {
            // End of a command — flush it (C truncates to MAX_INPUT_LENGTH).
            truncate_to(&mut buf, MAX_INPUT_LENGTH - 1);
            commands.push(std::mem::take(&mut buf));
        } else if c == ALIAS_VAR_CHAR {
            // $<x>: the following char selects the substitution. C branch order
            // is load-bearing — it tests in-range $1..$9 first, then $*, then
            // falls through to a literal copy (with $ redoubled).
            match chars.next() {
                None => {
                    // Trailing lone '$' — C reads the NUL terminator here (the
                    // num test fails, glob test fails, and the literal-copy
                    // branch writes the NUL), so nothing is appended.
                }
                Some(d) => {
                    // C: num = *temp - '1'; valid token slot iff 0 <= num <
                    // num_of_tokens. '*' and other non-digits fail this (num is
                    // negative or huge) and fall through.
                    let num = (d as i32) - ('1' as i32);
                    if num >= 0 && (num as usize) < tokens.len() {
                        buf.push_str(tokens[num as usize]);
                    } else if d == ALIAS_GLOB_CHAR {
                        // $* — the entire original argument line.
                        buf.push_str(orig);
                    } else {
                        // Literal copy; a literal '$' is redoubled for
                        // act()-safety (C: redouble $ ). This is also where an
                        // out-of-range $N (e.g. $5 with <5 tokens) lands, so it
                        // is emitted verbatim as "$5", matching C.
                        buf.push(ALIAS_VAR_CHAR);
                        buf.push(d);
                    }
                }
            }
        } else {
            buf.push(c);
        }
    }

    // Flush the final command (C always writes the tail to the queue).
    truncate_to(&mut buf, MAX_INPUT_LENGTH - 1);
    commands.push(buf);

    commands
}

/// Truncate `s` to at most `max` chars without splitting a UTF-8 boundary
/// (C clamps the C-string at MAX_INPUT_LENGTH-1 bytes).
fn truncate_to(s: &mut String, max: usize) {
    if s.chars().count() > max {
        let end = s.char_indices().nth(max).map(|(i, _)| i).unwrap_or(s.len());
        s.truncate(end);
    }
}

// ---------------------------------------------------------------------------
// alias_expand (C perform_alias): the expansion hook.
// ---------------------------------------------------------------------------

/// C perform_alias(): given the raw input line, expand a matching alias.
///
/// Returns:
///   * None  — the first word is not an alias (or the actor is an NPC / has no
///     aliases). The caller dispatches `input` unchanged.
///   * Some(Simple) — ALIAS_SIMPLE replaced the current command.
///   * Some(Complex) — ALIAS_COMPLEX should be queued at the descriptor front.
pub fn alias_expand(g: &GameState, ch: CharId, input: &str) -> Option<AliasExpansion> {
    let idnum = match g.get_char(ch) {
        // Mobs never have aliases (C: GET_ALIASES on an NPC is NULL).
        Some(c) if !c.is_npc => c.idnum,
        _ => return None,
    };

    // Find the alias matching the first word; clone it so we drop the lock
    // before doing any expansion work.
    let (first_arg, rest) = any_one_arg(input);
    if first_arg.is_empty() {
        return None;
    }

    let matched = {
        let guard = table().lock().unwrap();
        let list = guard.get(&idnum)?;
        let idx = find_alias_index(list, first_arg)?;
        list[idx].clone()
    };

    if matched.atype == ALIAS_SIMPLE {
        Some(AliasExpansion::Simple(matched.replacement.clone()))
    } else {
        Some(AliasExpansion::Complex(perform_complex_alias(
            rest, &matched,
        )))
    }
}

/// Compatibility helper for older direct tests/callers. Complex aliases are
/// flattened with newlines, matching the previous Rust representation.
pub fn alias_replace(g: &GameState, ch: CharId, input: &str) -> Option<String> {
    match alias_expand(g, ch, input)? {
        AliasExpansion::Simple(s) => Some(s),
        AliasExpansion::Complex(lines) => Some(lines.join("\n")),
    }
}

// ---------------------------------------------------------------------------
// do_alias (C interpreter.c do_alias): the `alias` command.
// ---------------------------------------------------------------------------

/// `alias` — with no argument, list defined aliases; with an alias word and no
/// replacement, delete it; with both, add or redefine it.
pub fn do_alias(g: &mut GameState, ch: CharId, argument: &str, _subcmd: i32) {
    let (idnum, name) = match g.get_char(ch) {
        Some(c) if !c.is_npc => (c.idnum, c.get_name().to_string()), // C: IS_NPC(ch) -> return
        _ => return,
    };

    // C: repl = any_one_arg(argument, arg);  (arg = alias word, repl = rest)
    let (arg, repl_raw) = any_one_arg(argument);
    let repl = repl_raw.trim();

    if arg.is_empty() {
        // No argument — list currently defined aliases.
        let list = get_aliases(idnum);
        g.send_to_char(ch, "Currently defined aliases:\r\n");
        if list.is_empty() {
            g.send_to_char(ch, " None.\r\n");
        } else {
            // C iterates the list (newest-first) printing "%-15s %s".
            for a in &list {
                let line = format!("{:<15} {}\r\n", a.alias, a.replacement);
                g.send_to_char(ch, &line);
            }
        }
        return;
    }

    // Otherwise, add or remove. First, remove any existing alias of this name
    // (C: REMOVE_FROM_LIST + free_alias). `existed` tracks whether one was
    // present, to choose the delete-vs-no-such message below.
    let existed = {
        let mut guard = table().lock().unwrap();
        let list = guard.entry(idnum).or_default();
        if let Some(idx) = find_alias_index(list, arg) {
            list.remove(idx);
            true
        } else {
            false
        }
    };

    if repl.is_empty() {
        // No replacement specified — treat as a delete request.
        if existed {
            g.send_to_char(ch, "Alias deleted.\r\n");
            persist_aliases(g, idnum, &name);
        } else {
            g.send_to_char(ch, "No such alias.\r\n");
        }
        return;
    }

    // Add or redefine. You can't alias 'alias' (C: str_cmp guard).
    if arg.eq_ignore_ascii_case("alias") {
        g.send_to_char(ch, "You can't alias 'alias'.\r\n");
        return;
    }

    // C: delete_doubledollar(repl) before storing; type is COMPLEX if the
    // replacement contains a ';' or a '$', else SIMPLE.
    let replacement = delete_doubledollar(repl);
    let atype = if replacement.contains(ALIAS_SEP_CHAR) || replacement.contains(ALIAS_VAR_CHAR) {
        ALIAS_COMPLEX
    } else {
        ALIAS_SIMPLE
    };

    // Prepend (C: a->next = GET_ALIASES(ch); GET_ALIASES(ch) = a) so the list
    // stays newest-first.
    {
        let mut guard = table().lock().unwrap();
        let list = guard.entry(idnum).or_default();
        list.insert(
            0,
            AliasEntry {
                alias: arg.to_string(),
                replacement,
                atype,
            },
        );
    }

    g.send_to_char(ch, "Alias added.\r\n");
    persist_aliases(g, idnum, &name);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);

    fn temp_lib(name: &str) -> PathBuf {
        let n = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "deltamud_alias_{name}_{}_{}",
            std::process::id(),
            n
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn alias_filename_matches_c_bucket_and_lowercase() {
        let path = alias_filename("/mud/lib", "Zed").unwrap();
        assert_eq!(
            path,
            Path::new("/mud/lib")
                .join("plralias")
                .join("U-Z")
                .join("zed.alias")
        );
    }

    #[test]
    fn alias_sidecar_round_trips_c_triples_and_removes_empty_file() {
        let lib = temp_lib("round_trip");
        let idnum = 42;
        set_aliases(
            idnum,
            vec![
                AliasEntry {
                    alias: "zz".to_string(),
                    replacement: "sleep".to_string(),
                    atype: ALIAS_SIMPLE,
                },
                AliasEntry {
                    alias: "combo".to_string(),
                    replacement: "say $1;wave".to_string(),
                    atype: ALIAS_COMPLEX,
                },
            ],
        );

        write_aliases(lib.to_str().unwrap(), "Tester", idnum).unwrap();
        let path = alias_filename(lib.to_str().unwrap(), "Tester").unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "zz\nsleep\n0\ncombo\nsay $1;wave\n1\n"
        );

        clear_aliases(idnum);
        read_aliases(lib.to_str().unwrap(), "Tester", idnum).unwrap();
        assert_eq!(
            get_aliases(idnum),
            vec![
                AliasEntry {
                    alias: "zz".to_string(),
                    replacement: "sleep".to_string(),
                    atype: ALIAS_SIMPLE,
                },
                AliasEntry {
                    alias: "combo".to_string(),
                    replacement: "say $1;wave".to_string(),
                    atype: ALIAS_COMPLEX,
                },
            ]
        );

        set_aliases(idnum, Vec::new());
        write_aliases(lib.to_str().unwrap(), "Tester", idnum).unwrap();
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(lib);
    }
}
