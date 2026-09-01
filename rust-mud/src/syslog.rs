// syslog.rs — the shared `mudlog` facility (C: utils.c `mudlog`).
//
//   void mudlog (char *str, char type, int level, byte file)
//
// The C routine does two things, and so does this port:
//
//   (a) When `file` is TRUE, append a timestamped line to the on-disk syslog
//       file. In C the file is `logfile`, which `init_game` opens with
//       `freopen(LOGFILE, "w", stderr)` after `chdir(DFLT_DIR)`, where
//       LOGFILE = "syslog" and DFLT_DIR = "lib" (config.c). So the file lives
//       at `<lib>/syslog`. The write format is `"%-15.15s :: %s\n"` where the
//       15-char field is `asctime(localtime())` advanced past the weekday
//       ("Sun ") and truncated to "Mon DD HH:MM:SS".
//
//   (b) Echo the line to every playing immortal whose on-line syslog level
//       (the PRF_LOG{1,2,3} bit triple, 0..=4: Off/Brief/Normal/Perfect/
//       Complete) is at or above `type`, and whose character level is at or
//       above `level`. The Rust port uses the authenticated principal's
//       persisted trust, never a body's display level. Perfect (tp == 3) sees everything except "Auto zone
//       reset:" spam. The echoed line is `[ str ]\r\n`, with the trailing
//       CR/LF stripped from `str` first, wrapped in CCGRN/CCNRM (bright green
//       at colour level >= C_NRM).
//
// C kept `type` as one of the OFF/BRF/NRM/PFT/CMP constants (utils.h, 0..=4).
// We expose the same values as `LogType` so callers read 1:1 against the C.

use crate::state::GameState;
use crate::types::{CharId, LVL_IMMORT, Level};
use chrono::Local;
use std::fs::OpenOptions;
use std::io::Write;

// utils.h log-type constants (the immortal syslog filtering levels).
pub const OFF: u8 = 0;
pub const BRF: u8 = 1;
pub const NRM: u8 = 2;
pub const PFT: u8 = 3;
pub const CMP: u8 = 4;

// screen.h colour level: CCGRN/CCNRM in mudlog gate at C_NRM.
const C_NRM: i32 = 2;

// structs.h on-line syslog preference bits.
const PRF_LOG1: i64 = 1 << 16;
const PRF_LOG2: i64 = 1 << 17;
const PRF_LOG3: i64 = 1 << 29;

// structs.h:248-249 colour preference bits (COLOR_LEV / _clrlevel). The
// old bits 9/10 here were PRF_NOARENA/PRF_SUMMONABLE, inverting the mudlog
// colour gate for many immortals (#196).
const PRF_COLOR_1: i64 = 1 << 13;
const PRF_COLOR_2: i64 = 1 << 14;

// config.c: LOGFILE = "syslog", written inside DFLT_DIR ("lib") after chdir.
const SYSLOG_REL: &str = "syslog";

// Size-based rotation: once the live syslog grows past this many bytes, roll it
// to `syslog.1` (and `syslog.1`->`syslog.2`, ...), keeping SYSLOG_GENERATIONS
// rolled copies, and start a fresh `syslog`. C never rotated (it `freopen`s with
// "w" once per boot); we add this purely to bound unbounded growth.
const SYSLOG_MAX_BYTES: u64 = 5 * 1024 * 1024; // 5 MB
const SYSLOG_GENERATIONS: u32 = 2; // keep syslog.1 .. syslog.2

/// Roll `syslog` -> `syslog.1` -> `syslog.2` (dropping the oldest) when the live
/// file is over the size threshold. Best-effort: any rename failure is ignored
/// so logging never dies because rotation hit a permission/race issue.
fn rotate_if_needed(base: &str) {
    let over = std::fs::metadata(base)
        .map(|m| m.len() > SYSLOG_MAX_BYTES)
        .unwrap_or(false);
    if !over {
        return;
    }
    // Shift the existing generations up: syslog.(N-1) -> syslog.N, oldest first.
    for slot in (1..SYSLOG_GENERATIONS).rev() {
        let from = format!("{}.{}", base, slot);
        let to = format!("{}.{}", base, slot + 1);
        let _ = std::fs::rename(&from, &to);
    }
    // syslog -> syslog.1 (truncating any prior syslog.1 we didn't shift).
    let _ = std::fs::rename(base, format!("{}.1", base));
}

/// COLOR_LEV(ch): _clrlevel = (PRF_COLOR_1?1:0) + (PRF_COLOR_2?2:0).
fn color_lev(prf: i64) -> i32 {
    (if prf & PRF_COLOR_1 != 0 { 1 } else { 0 }) + (if prf & PRF_COLOR_2 != 0 { 2 } else { 0 })
}

/// Append a timestamped line to `<lib>/syslog` (C: `fprintf(logfile,
/// "%-15.15s :: %s\n", tmp+4, str); fflush(logfile);`). Failures are swallowed
/// exactly as C ignores the fprintf return — the game must not die because the
/// log file is unwritable.
fn append_syslog_file(lib_path: &str, str: &str) {
    // asctime is "Sun Jul  3 12:34:56 1992\n"; tmp+4 drops the weekday and the
    // %-15.15s field keeps "Jul  3 12:34:56" (chrono "%b %e %H:%M:%S", %e being
    // the space-padded day-of-month).
    let stamp = Local::now().format("%b %e %H:%M:%S").to_string();
    let path = format!("{}/{}", lib_path.trim_end_matches('/'), SYSLOG_REL);
    // Size-based rotation before the append so the live file stays bounded.
    rotate_if_needed(&path);
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{:<15.15} :: {}", stamp, str);
    }
}

/// mudlog(str, type, level, file=TRUE) — log a mud message to the syslog file
/// and to online immortals' syslogs (C: utils.c `mudlog`).
///
/// `min_level` mirrors the C `level` argument; a negative C `level` suppressed
/// the immortal echo entirely, but no caller passes one (the level is always a
/// real immortal threshold), so this signature takes an unsigned `Level` and
/// always performs the echo after writing the file.
pub fn mudlog(g: &mut GameState, str: &str, log_type: u8, min_level: Level) {
    // (a) on-disk file (the `file` argument is effectively TRUE for every
    //     ported caller; the immortal-only `file=FALSE` path is rare in C and
    //     not exercised here).
    let lib_path = g.config.lib_path.clone();
    append_syslog_file(&lib_path, str);

    // (b) immortal echo. Strip trailing CR/LF so the bracketed line renders
    //     cleanly (C: the str_dup + trailing-char strip loop).
    let trimmed = str.trim_end_matches(['\r', '\n']);
    let body = format!("[ {} ]\r\n", trimmed);

    let is_reset = str.starts_with("Auto zone reset:");

    let recipients: Vec<CharId> = g
        .players_by_name
        .values()
        .copied()
        .filter(|&id| {
            let c = match g.get_char(id) {
                Some(c) => c,
                None => return false,
            };
            let Some(authority) = g
                .principal_authority(id)
                .filter(|authority| authority.is_authenticated_player())
            else {
                return false;
            };
            let Some(principal) = g.get_char(authority.principal) else {
                return false;
            };
            if g.authority_quarantine.contains(&principal.idnum) {
                return false;
            }
            if authority.authority < i32::from(min_level)
                || authority.authority < i32::from(LVL_IMMORT)
            {
                return false;
            }
            // tp = the player's syslog level from the PRF_LOG bit triple.
            let tp = (if c.prf_flags & PRF_LOG1 != 0 { 1 } else { 0 })
                + (if c.prf_flags & PRF_LOG2 != 0 { 2 } else { 0 })
                + (if c.prf_flags & PRF_LOG3 != 0 { 4 } else { 0 });
            // C: tp >= type || (tp == 3 && str != "Auto zone reset:...")
            // — Perfect logs everything except auto resets.
            tp >= log_type as i32 || (tp == 3 && !is_reset)
        })
        .collect();

    for id in recipients {
        let prf = g.get_char(id).map(|c| c.prf_flags).unwrap_or(0);
        // CCGRN(ch, C_NRM) ... line ... CCNRM(ch, C_NRM): bright green only at
        // colour level >= C_NRM, else uncoloured.
        if color_lev(prf) >= C_NRM {
            g.send_to_char(id, "&G");
            g.send_to_char(id, &body);
            g.send_to_char(id, "&n");
        } else {
            g.send_to_char(id, &body);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::character::Character;
    use crate::config::Config;
    use crate::connection::{ConState, Descriptor};
    use crate::types::{Class, ConnId, LVL_GOD, LVL_IMPL, Race};

    fn connected_player(
        state: &mut GameState,
        conn: ConnId,
        name: &str,
        level: u8,
        trust: i32,
    ) -> CharId {
        state
            .descriptors
            .insert(conn, Descriptor::new(conn, "test".to_string()));
        let mut character = Character::new_player(name.to_string(), Class::Warrior, Race::Human);
        character.desc = Some(conn);
        character.idnum = conn.0 as i64;
        character.player.level = level;
        character.trust = trust;
        character.prf_flags |= PRF_LOG1 | PRF_LOG2 | PRF_LOG3;
        let id = state.create_char(character);
        let descriptor = state.descriptors.get_mut(&conn).unwrap();
        descriptor.state = ConState::Playing;
        descriptor.character = Some(id);
        state.players_by_name.insert(name.to_lowercase(), id);
        id
    }

    #[test]
    fn mudlog_recipient_gate_uses_persisted_principal_trust() {
        let mut config = Config::default();
        config.lib_path = std::env::temp_dir()
            .join(format!("deltamud-syslog-trust-{}", std::process::id()))
            .to_string_lossy()
            .into_owned();
        std::fs::create_dir_all(&config.lib_path).unwrap();
        let mut state = GameState::new(config);
        let trusted = connected_player(&mut state, ConnId(1), "Trusted", 1, i32::from(LVL_IMPL));
        let spoofed = connected_player(&mut state, ConnId(2), "Spoofed", LVL_IMPL, 1);

        mudlog(&mut state, "trust boundary", NRM, LVL_GOD);

        assert!(
            state.descriptors[&ConnId(1)]
                .outbuf
                .contains("trust boundary")
        );
        assert!(
            !state.descriptors[&ConnId(2)]
                .outbuf
                .contains("trust boundary")
        );
        assert_eq!(state.principal_authority(trusted).unwrap().authority, 105);
        assert_eq!(state.principal_authority(spoofed).unwrap().authority, 1);

        state
            .descriptors
            .get_mut(&ConnId(1))
            .unwrap()
            .outbuf
            .clear();
        state.authority_quarantine.insert(1);
        mudlog(&mut state, "quarantined disclosure", NRM, LVL_GOD);
        assert!(
            !state.descriptors[&ConnId(1)]
                .outbuf
                .contains("quarantined disclosure")
        );
        let _ = std::fs::remove_dir_all(&state.config.lib_path);
    }
}
