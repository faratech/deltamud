// C-schema-compatible player persistence helpers.
//
// This module is the single source of truth for translating between the C
// DeltaMUD `player_main` / `player_affects` / `player_skills` tables and the
// Rust `Character`. The column set, order, and source fields mirror
// dbinterface.c::init_querystring (83 columns) exactly so the on-disk format
// is cross-compatible with the original C MUD. `database.rs` reuses these
// helpers for its real MySQL path.
//
// Column->field mapping (see src/dbinterface.c init_querystring):
//   idnum,name,description,title,sex,class,race,deity,level,hometown,birth,
//   played,weight,height,pwd,last_logon,host,mana,max_mana,hit,max_hit,move,
//   max_move,gold,bank_gold,exp,power,mpower,defense,mdefense,technique,str,
//   str_add,intel,wis,dex,con,cha,PADDING0,talks1,talks2,talks3,wimp_level,
//   freeze_level,invis_level,load_room,pref,bad_pws,cond1,cond2,cond3,
//   death_timer,citizen,training,newbie,arena,spells_to_learn,questpoints,
//   nextquest,countdown,questobj,questmob,recall_level,retreat_level,trust,
//   bail_amt,wins,losses,pref2,godcmds1,godcmds2,godcmds3,godcmds4,clan,
//   clan_rank,mapx,mapy,buildmodezone,buildmoderoom,tloadroom,alignment,act,
//   affected_by  (= 83 columns)
//
// Note on `talks`: C reads talks[0], talks[2], talks[3]. The struct is
// `bool talks[MAX_TONGUE]` with MAX_TONGUE==3, so talks[3] is a C out-of-bounds
// read (a latent C bug that aliases wimp_level's low byte). We persist the real
// tongue data — talks1<-talks[0], talks2<-talks[2] — and write talks3 as 0
// rather than replicate the overread; loading ignores talks3.

use crate::character::{Affect, Character};
use crate::types::*;
use mysql_async::prelude::*;
use mysql_async::{Row, Value};

/// The ordered list of all 83 `player_main` columns (matches dbinterface.c).
pub const PLAYER_MAIN_COLUMNS: &[&str] = &[
    "idnum",
    "name",
    "description",
    "title",
    "sex",
    "class",
    "race",
    "deity",
    "level",
    "hometown",
    "birth",
    "played",
    "weight",
    "height",
    "pwd",
    "last_logon",
    "host",
    "mana",
    "max_mana",
    "hit",
    "max_hit",
    "move",
    "max_move",
    "gold",
    "bank_gold",
    "exp",
    "power",
    "mpower",
    "defense",
    "mdefense",
    "technique",
    "str",
    "str_add",
    "intel",
    "wis",
    "dex",
    "con",
    "cha",
    "PADDING0",
    "talks1",
    "talks2",
    "talks3",
    "wimp_level",
    "freeze_level",
    "invis_level",
    "load_room",
    "pref",
    "bad_pws",
    "cond1",
    "cond2",
    "cond3",
    "death_timer",
    "citizen",
    "training",
    "newbie",
    "arena",
    "spells_to_learn",
    "questpoints",
    "nextquest",
    "countdown",
    "questobj",
    "questmob",
    "recall_level",
    "retreat_level",
    "trust",
    "bail_amt",
    "wins",
    "losses",
    "pref2",
    "godcmds1",
    "godcmds2",
    "godcmds3",
    "godcmds4",
    "clan",
    "clan_rank",
    "mapx",
    "mapy",
    "buildmodezone",
    "buildmoderoom",
    "tloadroom",
    "alignment",
    "act",
    "affected_by",
];

/// Build the ordered Value list for an INSERT/UPDATE of `player_main`, in the
/// same column order as `PLAYER_MAIN_COLUMNS`. `pwd_hash` is the already-hashed
/// password (the C MUD stores the crypt/hash directly in `pwd`); `host` is the
/// connecting host string ("" if unknown).
pub fn player_main_values(ch: &Character, pwd_hash: &str, host: &str) -> Vec<Value> {
    vec![
        Value::from(ch.idnum),
        Value::from(ch.player.name.clone()),
        Value::from(ch.player.description.clone()),
        // title: NULL when empty, matching C's NULL-on-empty-string behaviour.
        match &ch.player.title {
            Some(t) if !t.is_empty() => Value::from(t.clone()),
            _ => Value::NULL,
        },
        Value::from(ch.player.sex as u8),
        Value::from(ch.player.class as u8),
        Value::from(ch.player.race as u8),
        Value::from(ch.player.deity),
        Value::from(ch.player.level),
        Value::from(ch.player.hometown),
        Value::from(ch.player.time_birth),
        Value::from(ch.player.time_played),
        Value::from(ch.player.weight),
        Value::from(ch.player.height),
        Value::from(pwd_hash),
        Value::from(ch.last_logon.timestamp()),
        Value::from(host),
        Value::from(ch.points.mana),
        Value::from(ch.points.max_mana),
        Value::from(ch.points.hit),
        Value::from(ch.points.max_hit),
        Value::from(ch.points.move_points),
        Value::from(ch.points.max_move),
        Value::from(ch.points.gold),
        Value::from(ch.points.bank_gold),
        Value::from(ch.points.exp),
        Value::from(ch.points.power),
        Value::from(ch.points.mpower),
        Value::from(ch.points.defense),
        Value::from(ch.points.mdefense),
        Value::from(ch.points.technique),
        Value::from(ch.real_abils.str),
        Value::from(ch.real_abils.str_add),
        Value::from(ch.real_abils.intel),
        Value::from(ch.real_abils.wis),
        Value::from(ch.real_abils.dex),
        Value::from(ch.real_abils.con),
        Value::from(ch.real_abils.cha),
        Value::from(ch.padding0),
        Value::from(ch.talks[0] as i32),
        Value::from(ch.talks[2] as i32),
        Value::from(0_i32), // talks3 (C overread; see module note)
        Value::from(ch.wimp_level),
        Value::from(ch.freeze_level),
        Value::from(ch.invis_level),
        Value::from(ch.load_room),
        Value::from(ch.prf_flags),
        Value::from(ch.bad_pws),
        Value::from(ch.conditions[0]),
        Value::from(ch.conditions[1]),
        Value::from(ch.conditions[2]),
        Value::from(ch.death_timer),
        Value::from(ch.citizen),
        Value::from(ch.training),
        Value::from(ch.newbie),
        Value::from(ch.arena),
        Value::from(ch.spells_to_learn),
        Value::from(ch.quest_points),
        Value::from(ch.next_quest),
        Value::from(ch.quest_countdown),
        Value::from(ch.quest_obj),
        Value::from(ch.quest_mob),
        Value::from(ch.recall_level),
        Value::from(ch.retreat_level),
        Value::from(ch.trust),
        Value::from(ch.bail_amt),
        Value::from(ch.wins),
        Value::from(ch.losses),
        Value::from(ch.prf2_flags),
        Value::from(ch.godcmds1),
        Value::from(ch.godcmds2),
        Value::from(ch.godcmds3),
        Value::from(ch.godcmds4),
        Value::from(ch.clan),
        Value::from(ch.clan_rank),
        Value::from(ch.mapx),
        Value::from(ch.mapy),
        Value::from(ch.buildmodezone),
        Value::from(ch.buildmoderoom),
        Value::from(ch.tloadroom),
        Value::from(ch.alignment),
        Value::from(ch.act_flags),
        Value::from(ch.affect_flags),
    ]
}

/// Small helper: pull a column, falling back to a default if NULL/missing/typed
/// wrong, so a partially-populated row never panics.
fn col<T: FromValue + Default>(row: &Row, name: &str) -> T {
    match row.get_opt::<T, _>(name) {
        Some(Ok(v)) => v,
        _ => T::default(),
    }
}

fn col_opt_string(row: &Row, name: &str) -> Option<String> {
    match row.get_opt::<Option<String>, _>(name) {
        Some(Ok(v)) => v,
        _ => None,
    }
}

/// Convert a fully-selected `player_main` row into a `Character`, mirroring the
/// C retrieve_player_entry field assignments. Skills and affects are loaded
/// separately (see `load_skills` / `load_affects`).
pub fn player_main_to_character(row: &Row) -> Character {
    let mut ch = Character::new_player(
        col::<String>(row, "name"),
        Class::from_u8(col::<i32>(row, "class") as u8),
        Race::from_u8(col::<i32>(row, "race") as u8),
    );

    ch.idnum = col::<i64>(row, "idnum");
    ch.player.description = col::<String>(row, "description");
    ch.player.title = col_opt_string(row, "title");
    ch.player.sex = Gender::from_u8(col::<i32>(row, "sex") as u8);
    ch.player.deity = col::<i32>(row, "deity") as u8;
    ch.player.level = col::<i32>(row, "level") as u8;
    ch.player.hometown = col::<i32>(row, "hometown");
    ch.player.time_birth = col::<i64>(row, "birth");
    ch.player.time_played = col::<i64>(row, "played");
    ch.player.weight = col::<i32>(row, "weight") as u8;
    ch.player.height = col::<i32>(row, "height") as u8;

    // points
    ch.points.mana = col::<i32>(row, "mana");
    ch.points.max_mana = col::<i32>(row, "max_mana");
    ch.points.hit = col::<i32>(row, "hit");
    ch.points.max_hit = col::<i32>(row, "max_hit");
    ch.points.move_points = col::<i32>(row, "move");
    ch.points.max_move = col::<i32>(row, "max_move");
    ch.points.gold = col::<i32>(row, "gold");
    ch.points.bank_gold = col::<i32>(row, "bank_gold");
    ch.points.exp = col::<i64>(row, "exp");
    ch.points.power = col::<i32>(row, "power") as i16;
    ch.points.mpower = col::<i32>(row, "mpower") as i16;
    ch.points.defense = col::<i32>(row, "defense") as i16;
    ch.points.mdefense = col::<i32>(row, "mdefense") as i16;
    ch.points.technique = col::<i32>(row, "technique") as i16;

    // abilities
    ch.real_abils.str = col::<i32>(row, "str") as i8;
    ch.real_abils.str_add = col::<i32>(row, "str_add") as i8;
    ch.real_abils.intel = col::<i32>(row, "intel") as i8;
    ch.real_abils.wis = col::<i32>(row, "wis") as i8;
    ch.real_abils.dex = col::<i32>(row, "dex") as i8;
    ch.real_abils.con = col::<i32>(row, "con") as i8;
    ch.real_abils.cha = col::<i32>(row, "cha") as i8;
    ch.aff_abils = ch.real_abils;

    // player_special_data_saved
    ch.padding0 = col::<i32>(row, "PADDING0");
    ch.talks[0] = col::<i32>(row, "talks1") != 0;
    ch.talks[2] = col::<i32>(row, "talks2") != 0;
    // talks3 intentionally ignored (C overread; see module note).
    ch.wimp_level = col::<i32>(row, "wimp_level");
    ch.freeze_level = col::<i32>(row, "freeze_level") as u8;
    ch.invis_level = col::<i32>(row, "invis_level");
    ch.load_room = col::<i32>(row, "load_room");
    ch.prf_flags = col::<i64>(row, "pref");
    ch.bad_pws = col::<i32>(row, "bad_pws") as u8;
    ch.conditions[0] = col::<i32>(row, "cond1") as i8;
    ch.conditions[1] = col::<i32>(row, "cond2") as i8;
    ch.conditions[2] = col::<i32>(row, "cond3") as i8;
    ch.death_timer = col::<i32>(row, "death_timer") as u8;
    ch.citizen = col::<i32>(row, "citizen") as u8;
    ch.training = col::<i32>(row, "training") as u8;
    ch.newbie = col::<i32>(row, "newbie") as u8;
    ch.arena = col::<i32>(row, "arena") as u8;
    ch.spells_to_learn = col::<i32>(row, "spells_to_learn");
    ch.quest_points = col::<i32>(row, "questpoints");
    ch.next_quest = col::<i32>(row, "nextquest");
    ch.quest_countdown = col::<i32>(row, "countdown");
    ch.quest_obj = col::<i32>(row, "questobj");
    ch.quest_mob = col::<i32>(row, "questmob");
    ch.recall_level = col::<i32>(row, "recall_level");
    ch.retreat_level = col::<i32>(row, "retreat_level");
    ch.trust = col::<i32>(row, "trust");
    ch.bail_amt = col::<i32>(row, "bail_amt");
    ch.wins = col::<i32>(row, "wins") as u8;
    ch.losses = col::<i32>(row, "losses") as u8;
    ch.prf2_flags = col::<i64>(row, "pref2");
    ch.godcmds1 = col::<i64>(row, "godcmds1");
    ch.godcmds2 = col::<i64>(row, "godcmds2");
    ch.godcmds3 = col::<i64>(row, "godcmds3");
    ch.godcmds4 = col::<i64>(row, "godcmds4");
    ch.clan = col::<i32>(row, "clan");
    ch.clan_rank = col::<i32>(row, "clan_rank");
    ch.mapx = col::<i64>(row, "mapx");
    ch.mapy = col::<i64>(row, "mapy");
    ch.buildmodezone = col::<i64>(row, "buildmodezone");
    ch.buildmoderoom = col::<i64>(row, "buildmoderoom");
    ch.tloadroom = col::<i64>(row, "tloadroom");

    // char_special_data_saved
    ch.alignment = col::<i32>(row, "alignment");
    ch.act_flags = col::<i64>(row, "act");
    ch.affect_flags = col::<i64>(row, "affected_by");

    if ch.points.max_mana < 100 {
        ch.points.max_mana = 100;
    }
    ch
}

/// Build the `player_affects` rows for a character (mirrors
/// dbmodify_player_affects MODE_STORE). Returns one (type,duration,modifier,
/// location,bitvector) tuple per affect.
pub fn affect_rows(ch: &Character) -> Vec<(i32, i32, i32, i32, i64)> {
    ch.affected
        .iter()
        .map(|a| {
            (
                a.spell_type,
                a.duration,
                a.modifier,
                a.location,
                a.bitvector,
            )
        })
        .collect()
}

/// Merge a fetched `player_affects` row set into a character (mirrors
/// dbmodify_player_affects MODE_RETRIEVE -> affect_to_char). Caller is
/// responsible for affect_total() afterward if it wants modifiers applied.
pub fn apply_affect_rows(ch: &mut Character, rows: &[Row]) {
    for r in rows {
        ch.affected.push(Affect {
            spell_type: col::<i32>(r, "type"),
            duration: col::<i32>(r, "duration"),
            modifier: col::<i32>(r, "modifier"),
            location: col::<i32>(r, "location"),
            bitvector: col::<i64>(r, "bitvector"),
            caster: None,
        });
    }
}

/// Build the `player_skills` rows for a character (mirrors
/// dbmodify_player_skills MODE_STORE — only skills with learned>0 are saved).
pub fn skill_rows(ch: &Character) -> Vec<(i32, i32)> {
    let mut rows: Vec<(i32, i32)> = ch
        .skills
        .iter()
        .filter(|(_, &v)| v > 0)
        .map(|(&k, &v)| (k as i32, v as i32))
        .collect();
    rows.sort_by_key(|&(k, _)| k); // deterministic ordering
    rows
}

/// Merge a fetched `player_skills` row set into a character (mirrors
/// dbmodify_player_skills MODE_RETRIEVE).
pub fn apply_skill_rows(ch: &mut Character, rows: &[Row]) {
    for r in rows {
        let skill = col::<i32>(r, "skill");
        let learned = col::<i32>(r, "learned");
        if (0..=MAX_SKILLS as i32).contains(&skill) {
            ch.skills.insert(skill as u16, learned as u8);
        }
    }
}
