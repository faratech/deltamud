// Time + weather (CircleMUD weather.c / db.c reset_time / comm.c send_to_outdoor).
//
// DeltaMUD's weather model is minimal: the only weather state is `sunlight`
// (one of SUN_DARK/SUN_RISE/SUN_LIGHT/SUN_SET), driven entirely by the mud
// clock. `another_hour()` advances the clock by one mud-hour and, at the four
// transition hours (5/6/21/22), flips the sun state and broadcasts a sunrise/
// sunset message to every awake outdoor player — exactly as the C heartbeat
// does once per SECS_PER_MUD_HOUR (75 real seconds).
//
// GameState owns no time/weather fields and we may not add any, so the clock
// lives in a module static (OnceLock<Mutex<TimeWeather>>), initialized on the
// first heartbeat call the same way C's reset_time() seeds it, then advanced
// each subsequent call. `weather_and_time(g)` is the per-mud-hour entry point;
// `time_now()` exposes the current clock for `do_time`.

use crate::config::Config;
use crate::connection::ConState;
use crate::room::RoomFlags;
use crate::state::GameState;
use crate::types::{CharId, Position};
use log::warn;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

// ---------------------------------------------------------------------------
// Sun state (structs.h SUN_*). Kept as plain consts to match C numeric values
// (saved/compared elsewhere as ints; e.g. utils.h IS_DARK).
// ---------------------------------------------------------------------------
pub const SUN_DARK: i32 = 0;
pub const SUN_RISE: i32 = 1;
pub const SUN_LIGHT: i32 = 2;
pub const SUN_SET: i32 = 3;

// Real-seconds-per-mud-unit (utils.h). Only used to seed the initial clock the
// way reset_time() does (mud_time_passed over a whole number of mud-years, so
// the seeded hour/day/month are all 0 and only the year advances).
const SECS_PER_MUD_HOUR: i64 = 75;
const SECS_PER_MUD_DAY: i64 = 24 * SECS_PER_MUD_HOUR;
const SECS_PER_MUD_MONTH: i64 = 35 * SECS_PER_MUD_DAY;
const SECS_PER_MUD_YEAR: i64 = 17 * SECS_PER_MUD_MONTH;

/// The whole of DeltaMUD's time + weather state (time_info_data + weather_data).
#[derive(Debug, Clone, Copy)]
pub struct TimeWeather {
    pub hours: i32,
    pub day: i32,
    pub month: i32,
    pub year: i64,
    pub sunlight: i32,
}

static CLOCK: OnceLock<Mutex<TimeWeather>> = OnceLock::new();

/// CircleMUD mud_time_passed(t2, t1): break a real-seconds span into mud
/// hours/day/month/year. Ported verbatim (integer division, modular wrap).
fn mud_time_passed(t2: i64, t1: i64) -> TimeWeather {
    let mut secs = t2 - t1;

    let hours = ((secs / SECS_PER_MUD_HOUR) % 24) as i32; // 0..23 hours
    secs -= SECS_PER_MUD_HOUR * hours as i64;

    let day = ((secs / SECS_PER_MUD_DAY) % 35) as i32; // 0..34 days
    secs -= SECS_PER_MUD_DAY * day as i64;

    let month = ((secs / SECS_PER_MUD_MONTH) % 17) as i32; // 0..16 months
    secs -= SECS_PER_MUD_MONTH * month as i64;

    let year = secs / SECS_PER_MUD_YEAR; // 0..XX years

    TimeWeather {
        hours,
        day,
        month,
        year,
        sunlight: SUN_DARK,
    }
}

/// CircleMUD db.c reset_time(): seed the clock from real time, overlay the
/// persisted mud date from etc/date_record when present, and pick the initial
/// sun state from the seeded hour.
fn reset_time(now: i64, lib_path: &str) -> TimeWeather {
    let beginning_of_time = now - (SECS_PER_MUD_YEAR * 1000);
    let mut tw = mud_time_passed(now, beginning_of_time);
    read_mud_date_from_file(lib_path, &mut tw);

    tw.sunlight = if tw.hours <= 4 {
        SUN_DARK
    } else if tw.hours == 5 {
        SUN_RISE
    } else if tw.hours <= 20 {
        SUN_LIGHT
    } else if tw.hours == 21 {
        SUN_SET
    } else {
        SUN_DARK
    };
    tw
}

fn read_mud_date_from_file(lib_path: &str, tw: &mut TimeWeather) {
    let path = Path::new(lib_path).join("etc").join("date_record");
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(_) => {
            warn!("SYSERR: File etc/date_record not found, mud date will be reset to default!");
            return;
        }
    };
    if bytes.len() < 12 {
        warn!("SYSERR: File etc/date_record is too short, mud date will be reset to default!");
        return;
    }
    let year = i32::from_ne_bytes(bytes[0..4].try_into().unwrap());
    let month = i32::from_ne_bytes(bytes[4..8].try_into().unwrap());
    let day = i32::from_ne_bytes(bytes[8..12].try_into().unwrap());
    tw.year = year as i64;
    tw.month = month;
    tw.day = day;
}

pub fn write_mud_date_to_file(g: &GameState) {
    let mtx = CLOCK.get_or_init(|| {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        Mutex::new(reset_time(now, &g.config.lib_path))
    });
    let tw = match mtx.lock() {
        Ok(g) => *g,
        Err(p) => *p.into_inner(),
    };
    let mut bytes = Vec::with_capacity(12);
    bytes.extend_from_slice(&(tw.year as i32).to_ne_bytes());
    bytes.extend_from_slice(&tw.month.to_ne_bytes());
    bytes.extend_from_slice(&tw.day.to_ne_bytes());
    let path = Path::new(&g.config.lib_path)
        .join("etc")
        .join("date_record");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(e) = std::fs::write(&path, bytes) {
        warn!("SYSERR: Could not write etc/date_record: {}", e);
    }
}

/// comm.c send_to_outdoor(): deliver a message to every connected, awake
/// player standing in a non-indoor room. Mirrors C: iterate the descriptor
/// list, require Playing state, a character, AWAKE (position > Sleeping), and
/// OUTSIDE (!ROOM_FLAGGED(in_room, ROOM_INDOORS)).
fn send_to_outdoor(g: &mut GameState, msg: &str) {
    if msg.is_empty() {
        return;
    }

    let mut recipients: Vec<CharId> = Vec::new();
    for d in g.descriptors.values() {
        if d.state != ConState::Playing {
            continue;
        }
        let cid = match d.character {
            Some(c) => c,
            None => continue,
        };
        let ch = match g.get_char(cid) {
            Some(c) => c,
            None => continue,
        };
        // AWAKE(ch): position > SLEEPING.
        if ch.position <= Position::Sleeping {
            continue;
        }
        // OUTSIDE(ch): in a room that is not flagged INDOORS.
        let outside = match ch.in_room {
            Some(rnum) => match g.room_opt(rnum) {
                Some(room) => !room.room_flags.contains(RoomFlags::INDOORS),
                None => false,
            },
            None => false,
        };
        if !outside {
            continue;
        }
        recipients.push(cid);
    }

    for cid in recipients {
        g.send_to_char(cid, msg);
    }
}

/// CircleMUD weather.c another_hour(mode): advance the mud clock by one hour.
/// When `mode` is set (the heartbeat path), cross the dawn/dusk thresholds,
/// update the sun state, and announce sunrise/sunset to outdoor players. Then
/// roll hours -> day -> month -> year exactly as C does.
fn another_hour(g: &mut GameState, tw: &mut TimeWeather, mode: bool) {
    tw.hours += 1;

    if mode {
        match tw.hours {
            5 => {
                tw.sunlight = SUN_RISE;
                send_to_outdoor(g, "The sun rises in the east.\r\n");
            }
            6 => {
                tw.sunlight = SUN_LIGHT;
                send_to_outdoor(g, "The day has begun.\r\n");
            }
            21 => {
                tw.sunlight = SUN_SET;
                send_to_outdoor(g, "The sun slowly disappears in the west.\r\n");
            }
            22 => {
                tw.sunlight = SUN_DARK;
                send_to_outdoor(g, "The night has begun.\r\n");
            }
            _ => {}
        }
    }

    if tw.hours > 23 {
        tw.hours -= 24;
        tw.day += 1;

        if tw.day > 34 {
            tw.day = 0;
            tw.month += 1;

            if tw.month > 16 {
                tw.month = 0;
                tw.year += 1;
            }
        }
    }
}

/// Per-mud-hour entry point, called once every SECS_PER_MUD_HOUR from the
/// heartbeat (alongside affect_update/point_update). Initializes the clock the
/// way reset_time() does on the very first call, then advances it one hour and
/// broadcasts any sunrise/sunset transition. (CircleMUD's stock weather_and_time
/// also rolled pressure/sky; DeltaMUD reduced weather to the sun cycle, so
/// another_hour is the whole of it.)
pub fn weather_and_time(g: &mut GameState) {
    let mtx = CLOCK.get_or_init(|| {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        Mutex::new(reset_time(now, &g.config.lib_path))
    });

    // Single-threaded heartbeat; the lock can't be poisoned by a concurrent
    // panic in practice, but recover defensively rather than unwrap.
    let mut tw = match mtx.lock() {
        Ok(g) => *g,
        Err(p) => *p.into_inner(),
    };

    another_hour(g, &mut tw, true);

    if let Ok(mut guard) = mtx.lock() {
        *guard = tw;
    }
}

/// Current mud clock for `do_time` (CircleMUD time_info.{hours,day,month,year}).
/// Initializes the clock on first access if the heartbeat hasn't run yet, so a
/// `time` command issued before the first mud-hour still reports a sane value.
pub fn time_now() -> (i32, i32, i32, i64) {
    let mtx = CLOCK.get_or_init(|| {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        Mutex::new(reset_time(now, &Config::default().lib_path))
    });
    let tw = match mtx.lock() {
        Ok(g) => *g,
        Err(p) => *p.into_inner(),
    };
    (tw.hours, tw.day, tw.month, tw.year)
}

pub fn mud_minute_of_day() -> i64 {
    let (hours, _, _, _) = time_now();
    hours as i64 * 60
}

/// Current sun state (utils.h weather_info.sunlight), for IS_DARK / look logic.
pub fn sunlight() -> i32 {
    let mtx = CLOCK.get_or_init(|| {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        Mutex::new(reset_time(now, &Config::default().lib_path))
    });
    match mtx.lock() {
        Ok(g) => g.sunlight,
        Err(p) => p.into_inner().sunlight,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn date_record_binary_overrides_seeded_calendar() {
        let mut dir = std::env::temp_dir();
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        dir.push(format!("deltamud-date-{}-{}", std::process::id(), unique));
        let etc = dir.join("etc");
        std::fs::create_dir_all(&etc).unwrap();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&32523i32.to_ne_bytes());
        bytes.extend_from_slice(&3i32.to_ne_bytes());
        bytes.extend_from_slice(&14i32.to_ne_bytes());
        std::fs::write(etc.join("date_record"), bytes).unwrap();

        let tw = reset_time(1_000_000, dir.to_str().unwrap());

        assert_eq!(tw.year, 32523);
        assert_eq!(tw.month, 3);
        assert_eq!(tw.day, 14);
        let _ = std::fs::remove_dir_all(dir);
    }
}
