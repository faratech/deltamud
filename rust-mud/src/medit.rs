// medit.rs — OASIS mobile editor (CircleMUD/DeltaMUD `medit.c`), ported to the
// id-indexed GameState. This is the menu-driven online creation editor for
// mobile (NPC) prototypes.
//
// Architecture (the shared OLC contract):
//   * `do_medit(g, ch, arg, subcmd)` starts the editor: it snapshots the target
//     mob prototype into a per-connection `MeditState`, registers the editor
//     with `olc::set_active(conn, EditorKind::Medit)`, and displays the main
//     menu.
//   * `medit_parse(g, conn, line)` is the per-line input handler the OLC router
//     (`olc::olc_input`) calls while this connection is in the mob editor.
//   * On save/quit the editor writes the mob back into `GameState::mob_protos`
//     and rewrites the on-disk `<zone>.mob` file byte-faithfully (CircleMUD
//     extended `X`-format), then calls `olc::clear_active(conn)`.
//
// The Rust `MobileProto` (world.rs, which this module must NOT modify) carries
// only the subset of fields the live game needs. medit, however, edits the
// FULL mob the way the C struct exposes it: NPC/affect flag bitvectors,
// alignment, the DeltaMUD power/mpower/defense/mdefense/technique block, bare-
// hand attack type, the six ability scores, and the HP/mana/move dice. To keep
// a faithful round-trip without reshaping `MobileProto`, the editor seeds its
// scratch state from the in-memory proto for the fields the proto keeps AND
// re-reads the on-disk `.mob` block for the extended fields, then writes the
// complete `.mob` block back on save. All per-connection edit state lives in a
// module-static keyed by ConnId, exactly as the brief prescribes.

use crate::constants::{ACTION_BITS, AFFECTED_BITS, GENDERS, POSITION_TYPES};
use crate::olc::{self, EditorKind};
use crate::state::GameState;
use crate::types::*;
use crate::world::MobileProto;
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::OnceLock;

// ---------------------------------------------------------------------------
// Constants mirrored from olc.h / structs.h / fight.c.
// ---------------------------------------------------------------------------

/// Number of MOB_x action-flag bits the editor offers (olc.h NUM_MOB_FLAGS).
const NUM_MOB_FLAGS: usize = 30;
/// Number of AFF_x affect-flag bits the editor offers (olc.h NUM_AFF_FLAGS).
const NUM_AFF_FLAGS: usize = 22;
/// Number of bare-hand attack types (olc.h NUM_ATTACK_TYPES, fight.c table).
const NUM_ATTACK_TYPES: usize = 15;
/// Number of positions offered (structs.h POS_* count; our table has 10).
const NUM_POSITIONS: usize = 10;
/// Number of genders (olc.h NUM_GENDERS).
const NUM_GENDERS: usize = 3;
/// CircleMUD MOB_DEFAULT_STAT (structs.h): the value an ability "defaults" to
/// and which therefore is NOT written to the espec block on save.
const MOB_DEFAULT_STAT: i32 = 13;
/// MOB_ISNPC bit (structs.h) — forced set on every mob the editor saves.
const MOB_ISNPC: i64 = 1 << 3;
/// Maximum mob description length (olc.h MAX_MOB_DESC).
const MAX_MOB_DESC: usize = 512;

/// `attack_hit_text[].singular` — bare-hand attack verbs (fight.c). Index is the
/// attack type stored in the espec `BareHandAttack:` line.
const ATTACK_HIT_TEXT: [&str; NUM_ATTACK_TYPES] = [
    "hit", "sting", "whip", "slash", "bite", "bludgeon", "crush", "pound", "claw", "maul",
    "thrash", "pierce", "blast", "punch", "stab",
];

// ---------------------------------------------------------------------------
// Editor sub-mode (CircleMUD OLC_MODE within CON_MEDIT). Distinguishes which
// field the next line of input feeds. The C numeric ordering of the
// MEDIT_NUMERICAL_RESPONSE gate is reproduced via the `is_numeric` predicate.
// ---------------------------------------------------------------------------
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    MainMenu,
    ConfirmSave,
    // String fields.
    Alias,
    ShortDesc,
    LongDesc,
    DDesc, // multi-line description editor
    // Menu-driven (display a list, read a number; not "numerical responses").
    Sex,
    Position,
    DefaultPos,
    Attack,
    NpcFlags,
    AffFlags,
    Script(olc::DgScriptEditMode),
    // Numerical responses.
    Level,
    Alignment,
    Defense,
    MDefense,
    Power,
    MPower,
    Technique,
    Exp,
    NumDamDice,
    SizeDamDice,
    NumHpDice,
    SizeHpDice,
    AddHp,
    Gold,
}

impl Mode {
    /// Mirrors C's `OLC_MODE(d) > MEDIT_NUMERICAL_RESPONSE` gate: numeric modes
    /// require the input to begin with a digit (or a leading minus).
    fn is_numeric(self) -> bool {
        matches!(
            self,
            Mode::Level
                | Mode::Alignment
                | Mode::Defense
                | Mode::MDefense
                | Mode::Power
                | Mode::MPower
                | Mode::Technique
                | Mode::Exp
                | Mode::NumDamDice
                | Mode::SizeDamDice
                | Mode::NumHpDice
                | Mode::SizeHpDice
                | Mode::AddHp
                | Mode::Gold
        )
    }
}

// ---------------------------------------------------------------------------
// The scratch mob being edited. Carries the FULL field set (see module docs),
// not just what MobileProto keeps, so the on-disk round-trip is lossless.
// ---------------------------------------------------------------------------
#[derive(Clone)]
struct EditMob {
    alias: String,
    short_desc: String,
    long_desc: String,
    description: String,
    mob_flags: i64,
    aff_flags: i64,
    alignment: i32,
    level: i32,
    power: i32,
    mpower: i32,
    defense: i32,
    mdefense: i32,
    technique: i32,
    // HP/mana/move "dice" — stored exactly as the C `X`-line: hit/mana/move are
    // the base values (points.hit/mana/move), num/size dam dice the bare-hand.
    hit: i32,   // GET_HIT  (Num HP Dice slot in the menu)
    mana: i32,  // GET_MANA (Size HP Dice slot)
    movep: i32, // GET_MOVE (HP Bonus slot)
    num_dam_dice: i32,
    size_dam_dice: i32,
    gold: i64,
    exp: i64,
    position: i32,
    default_pos: i32,
    sex: i32,
    attack_type: i32,
    // Ability scores (espec block).
    str_: i32,
    str_add: i32,
    intel: i32,
    wis: i32,
    dex: i32,
    con: i32,
    cha: i32,
}

struct MeditState {
    vnum: MobVnum,
    zone_number: i32,
    mob: EditMob,
    mode: Mode,
    changed: bool,     // OLC_VAL — has anything been edited?
    ddesc_buf: String, // accumulates the multi-line description
}

fn states() -> &'static Mutex<HashMap<ConnId, MeditState>> {
    static S: OnceLock<Mutex<HashMap<ConnId, MeditState>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(HashMap::new()))
}

/// abort: drop this conn's editor state without saving (player disconnected
/// mid-edit). The MeditState (including its in-progress description buffer) is
/// removed so nothing lingers until reboot. `olc::abort_editor` clears active.
pub fn abort(conn: ConnId) {
    states().lock().unwrap().remove(&conn);
}

// ---------------------------------------------------------------------------
// Small output helper: append text to the editing connection's buffer.
// ---------------------------------------------------------------------------
fn send(g: &mut GameState, conn: ConnId, msg: &str) {
    if let Some(d) = g.descriptors.get_mut(&conn) {
        d.outbuf.push_str(msg);
    }
}

// ===========================================================================
// Command entry — do_medit (CircleMUD do_oasis / medit dispatch).
// ===========================================================================

/// `medit <vnum>` — start editing the mob whose virtual number is <vnum>.
/// `medit new <vnum>` creates a fresh mob if it does not yet exist (CircleMUD
/// treats a missing vnum as "create new" automatically; we follow that).
pub fn do_medit(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let conn = match g.get_char(ch).and_then(|c| c.desc) {
        Some(c) => c,
        None => return,
    };

    // Immortal/builder gate (medit is wired at LVL_IMMORT in the command table).
    let level = g.get_char(ch).map(|c| c.player.level).unwrap_or(0);
    if level < LVL_IMMORT {
        send(g, conn, "You do not have access to the mobile editor.\r\n");
        return;
    }

    let arg = arg.trim();
    // Accept an optional leading "new" keyword (CircleMUD: `medit new <vnum>`).
    let vnum_tok = if let Some(rest) = arg
        .strip_prefix("new ")
        .or_else(|| arg.strip_prefix("New "))
    {
        rest.trim()
    } else {
        arg
    };

    let vnum: MobVnum = match vnum_tok.parse() {
        Ok(v) => v,
        Err(_) => {
            send(g, conn, "Specify a mob VNUM to edit.\r\n");
            return;
        }
    };
    if vnum < 0 {
        send(g, conn, "Specify a mob VNUM to edit.\r\n");
        return;
    }

    // Resolve the owning zone (the zone whose range covers vnum). Required so
    // we can rewrite the correct <zone>.mob file on save.
    let zone_number = match zone_for_vnum(g, vnum) {
        Some(z) => z,
        None => {
            send(g, conn, "That vnum is outside any existing zone.\r\n");
            return;
        }
    };

    // Builder permission: must be allowed to edit this zone (or be GRGOD+).
    if level < LVL_GRGOD && !can_edit_zone(g, ch, zone_number) {
        send(g, conn, "You do not have permission to edit this zone.\r\n");
        return;
    }

    // Seed the scratch mob: from the live proto if it exists, else a fresh mob.
    let mob = match g.mob_protos.get(&vnum) {
        Some(proto) => seed_from_proto(g, proto, vnum),
        None => {
            send(g, conn, "New mobile.\r\n");
            new_mob()
        }
    };

    let st = MeditState {
        vnum,
        zone_number,
        mob,
        mode: Mode::MainMenu,
        changed: false,
        ddesc_buf: String::new(),
    };
    states().lock().unwrap().insert(conn, st);
    olc::set_active(conn, EditorKind::Medit);
    disp_menu(g, conn);
}

/// Build a default new mob (CircleMUD medit_setup_new + init_mobile).
fn new_mob() -> EditMob {
    EditMob {
        alias: "mob unfinished".to_string(),
        short_desc: "the unfinished mob".to_string(),
        long_desc: "An unfinished mob stands here.".to_string(),
        description: "It looks unfinished.\n".to_string(),
        mob_flags: MOB_ISNPC,
        aff_flags: 0,
        alignment: 0,
        level: 1,
        power: 0,
        mpower: 0,
        defense: 0,
        mdefense: 0,
        technique: 0,
        hit: 1,
        mana: 1,
        movep: 100,
        num_dam_dice: 1,
        size_dam_dice: 1,
        gold: 0,
        exp: 100,
        position: Position::Standing as i32,
        default_pos: Position::Standing as i32,
        sex: Gender::Neutral as i32,
        attack_type: 0,
        str_: MOB_DEFAULT_STAT,
        str_add: 0,
        intel: MOB_DEFAULT_STAT,
        wis: MOB_DEFAULT_STAT,
        dex: MOB_DEFAULT_STAT,
        con: MOB_DEFAULT_STAT,
        cha: MOB_DEFAULT_STAT,
    }
}

/// Seed the scratch mob from the in-memory `MobileProto` (the live game's view)
/// plus the extended fields the proto does not keep, recovered by re-reading
/// the on-disk `.mob` block for this vnum. This guarantees medit does not
/// silently zero a mob's flags/abilities/alignment on the first save.
fn seed_from_proto(g: &GameState, proto: &MobileProto, vnum: MobVnum) -> EditMob {
    let mut m = EditMob {
        alias: proto.name.clone(),
        short_desc: proto.short_desc.clone(),
        long_desc: proto.long_desc.clone(),
        description: proto.description.clone(),
        mob_flags: MOB_ISNPC,
        aff_flags: 0,
        alignment: 0,
        level: proto.level as i32,
        power: 0,
        mpower: 0,
        defense: 0,
        mdefense: 0,
        technique: 0,
        hit: proto.hitpoints.max(1),
        mana: 1,
        movep: 100,
        num_dam_dice: proto.damnodice,
        size_dam_dice: proto.damsizedice,
        gold: proto.gold as i64,
        exp: proto.experience,
        position: proto.position as i32,
        default_pos: proto.default_pos as i32,
        sex: proto.sex as i32,
        attack_type: 0,
        str_: MOB_DEFAULT_STAT,
        str_add: 0,
        intel: MOB_DEFAULT_STAT,
        wis: MOB_DEFAULT_STAT,
        dex: MOB_DEFAULT_STAT,
        con: MOB_DEFAULT_STAT,
        cha: MOB_DEFAULT_STAT,
    };
    // Pull the extended fields out of the on-disk block if available.
    if let Some(zone) = zone_for_vnum(g, vnum) {
        let path = mob_file_path(g, zone);
        if let Some(ext) = read_disk_mob(&path, vnum) {
            m.mob_flags = ext.mob_flags | MOB_ISNPC;
            m.aff_flags = ext.aff_flags;
            m.alignment = ext.alignment;
            m.power = ext.power;
            m.mpower = ext.mpower;
            m.defense = ext.defense;
            m.mdefense = ext.mdefense;
            m.technique = ext.technique;
            m.hit = ext.hit;
            m.mana = ext.mana;
            m.movep = ext.movep;
            m.num_dam_dice = ext.num_dam_dice;
            m.size_dam_dice = ext.size_dam_dice;
            m.attack_type = ext.attack_type;
            m.str_ = ext.str_;
            m.str_add = ext.str_add;
            m.intel = ext.intel;
            m.wis = ext.wis;
            m.dex = ext.dex;
            m.con = ext.con;
            m.cha = ext.cha;
        }
    }
    m
}

fn trim_trailing_nl(s: &str) -> String {
    s.trim_end_matches(['\r', '\n']).to_string()
}

// ===========================================================================
// Zone / permission helpers (self-contained; no GameState methods added).
// ===========================================================================

/// Find the zone number whose range (number*100 ..= top) covers `vnum`.
fn zone_for_vnum(g: &GameState, vnum: MobVnum) -> Option<i32> {
    g.zones
        .iter()
        .find(|z| z.number * 100 <= vnum && z.top >= vnum)
        .map(|z| z.number)
}

/// Builder authorization for the zone owning this mobile vnum.
fn can_edit_zone(g: &GameState, ch: CharId, zone_number: i32) -> bool {
    g.zones
        .iter()
        .position(|z| z.number == zone_number)
        .map(|zr| crate::olc::can_edit_zone(g, ch, zr))
        .unwrap_or(false)
}

// ===========================================================================
// On-disk path resolution.
// ===========================================================================

fn mob_file_path(g: &GameState, zone_number: i32) -> std::path::PathBuf {
    std::path::Path::new(&g.config.lib_path)
        .join("world")
        .join("mob")
        .join(format!("{}.mob", zone_number))
}

// ===========================================================================
// Menu rendering.
// ===========================================================================

fn disp_menu(g: &mut GameState, conn: ConnId) {
    let st = match states().lock().unwrap().get(&conn).cloned_state() {
        Some(s) => s,
        None => return,
    };
    let m = &st.mob;
    let mut npc_flags = String::new();
    sprintbit(m.mob_flags, ACTION_BITS, &mut npc_flags);
    let mut aff_flags = String::new();
    sprintbit(m.aff_flags, AFFECTED_BITS, &mut aff_flags);

    let buf = format!(
        "-- Mob Number:  [&c{vnum}&n]\r\n\
         &g1&n) Sex: &y{sex:<7.7}&n          &g2&n) Alias: &y{alias}\r\n\
         &g3&n) S-Desc: &y{sdesc}\r\n\
         &g4&n) L-Desc:-\r\n&y{ldesc}&n\r\n\
         &g5&n) D-Desc:-\r\n&y{ddesc}&n\r\n\
         &g6&n) Level       : [&c{level:>5}&n],  &g7&n) Alignment    : [&c{align:>4}&n]\r\n\
         &g8&n) Defense     : [&c{def:>5}&n],  &g9&n) Magic Defense: [&c{mdef:>4}&n]\r\n\
         &gA&n) Power       : [&c{pow:>5}&n],  &gB&n) Magic Power  : [&c{mpow:>4}&n]\r\n\
         &gC&n) Technique   : [&c{tec:>5}&n],  &gD&n) Exp          : [&c{exp:>9}&n]\r\n\
         &gE&n) Num Dam Dice: [&c{ndd:>5}&n],  &gF&n) Size Dam Dice: [&c{sdd:>4}&n]\r\n\
         &gG&n) Num HP Dice : [&c{hit:>5}&n],  &gH&n) Size HP Dice : [&c{mana:>4}&n]\r\n\
         &gI&n) HP Bonus    : [&c{movep:>5}&n],  &gJ&n) Gold         : [&c{gold:>8}&n]\r\n",
        vnum = st.vnum,
        sex = gender_name(m.sex),
        alias = m.alias,
        sdesc = m.short_desc,
        ldesc = m.long_desc,
        ddesc = m.description.trim_end_matches(['\r', '\n']),
        level = m.level,
        align = m.alignment,
        def = m.defense,
        mdef = m.mdefense,
        pow = m.power,
        mpow = m.mpower,
        tec = m.technique,
        exp = m.exp,
        ndd = m.num_dam_dice,
        sdd = m.size_dam_dice,
        hit = m.hit,
        mana = m.mana,
        movep = m.movep,
        gold = m.gold,
    );
    send(g, conn, &buf);

    let buf2 = format!(
        "&gK&n) Position  : &y{pos}\r\n\
         &gL&n) Default   : &y{dpos}\r\n\
         &gM&n) Attack    : &y{atk}\r\n\
         &gN&n) NPC Flags : &c{npc}\r\n\
         &gO&n) AFF Flags : &c{aff}\r\n\
         &gS&n) Script    : &c{script}\r\n\
         &gQ&n) Quit\r\n\
         Enter choice : ",
        pos = position_name(m.position),
        dpos = position_name(m.default_pos),
        atk = attack_name(m.attack_type),
        npc = npc_flags,
        aff = aff_flags,
        script =
            if crate::dg_db_scripts::proto_trigger_vnums(crate::dg_handler::MOB_TRIGGER, st.vnum)
                .is_empty()
            {
                "Not Set."
            } else {
                "Set."
            },
    );
    send(g, conn, &buf2);

    set_mode(conn, Mode::MainMenu);
}

fn disp_positions(g: &mut GameState, conn: ConnId) {
    let mut out = String::new();
    for (i, name) in POSITION_TYPES.iter().enumerate() {
        if *name == "\n" {
            break;
        }
        out.push_str(&format!("&g{:>2}&n) {}\r\n", i, name));
    }
    out.push_str("Enter position number : ");
    send(g, conn, &out);
}

fn disp_sex(g: &mut GameState, conn: ConnId) {
    let mut out = String::new();
    for (i, name) in GENDERS.iter().enumerate().take(NUM_GENDERS) {
        out.push_str(&format!("&g{:>2}&n) {}\r\n", i, name));
    }
    out.push_str("Enter gender number : ");
    send(g, conn, &out);
}

fn disp_attack_types(g: &mut GameState, conn: ConnId) {
    let mut out = String::new();
    for (i, name) in ATTACK_HIT_TEXT.iter().enumerate() {
        out.push_str(&format!("&g{:>2}&n) {}\r\n", i, name));
    }
    out.push_str("Enter attack type : ");
    send(g, conn, &out);
}

fn disp_mob_flags(g: &mut GameState, conn: ConnId) {
    let st = match states().lock().unwrap().get(&conn).cloned_state() {
        Some(s) => s,
        None => return,
    };
    let mut out = String::new();
    let mut columns = 0;
    for i in 0..NUM_MOB_FLAGS {
        let name = ACTION_BITS.get(i).copied().unwrap_or("UNDEFINED");
        columns += 1;
        out.push_str(&format!(
            "&g{:>2}&n) {:<20.20}  {}",
            i + 1,
            name,
            if columns % 2 == 0 { "\r\n" } else { "" }
        ));
    }
    let mut cur = String::new();
    sprintbit(st.mob.mob_flags, ACTION_BITS, &mut cur);
    out.push_str(&format!(
        "\r\nCurrent flags : &c{}&n\r\nEnter mob flags (0 to quit) : ",
        cur
    ));
    send(g, conn, &out);
}

fn disp_aff_flags(g: &mut GameState, conn: ConnId) {
    let st = match states().lock().unwrap().get(&conn).cloned_state() {
        Some(s) => s,
        None => return,
    };
    let mut out = String::new();
    let mut columns = 0;
    for i in 0..NUM_AFF_FLAGS {
        let name = AFFECTED_BITS.get(i).copied().unwrap_or("UNDEFINED");
        columns += 1;
        out.push_str(&format!(
            "&g{:>2}&n) {:<20.20}  {}",
            i + 1,
            name,
            if columns % 2 == 0 { "\r\n" } else { "" }
        ));
    }
    let mut cur = String::new();
    sprintbit(st.mob.aff_flags, AFFECTED_BITS, &mut cur);
    out.push_str(&format!(
        "\r\nCurrent flags   : &c{}&n\r\nEnter aff flags (0 to quit) : ",
        cur
    ));
    send(g, conn, &out);
}

// ===========================================================================
// Name lookups for the menu.
// ===========================================================================

fn gender_name(i: i32) -> &'static str {
    GENDERS
        .get(i.clamp(0, (NUM_GENDERS - 1) as i32) as usize)
        .copied()
        .unwrap_or("Neutral")
}

fn position_name(i: i32) -> &'static str {
    let idx = i.clamp(0, (NUM_POSITIONS - 1) as i32) as usize;
    POSITION_TYPES.get(idx).copied().unwrap_or("Standing")
}

fn attack_name(i: i32) -> &'static str {
    ATTACK_HIT_TEXT
        .get(i.clamp(0, (NUM_ATTACK_TYPES - 1) as i32) as usize)
        .copied()
        .unwrap_or("hit")
}

/// CircleMUD `sprintbit`: render a flag bitvector as space-separated names from
/// `names` (a `"\n"`-terminated table). Bits past the table render as `UNDEF`.
fn sprintbit(bits: i64, names: &[&str], out: &mut String) {
    out.clear();
    for i in 0..64 {
        if bits & (1 << i) != 0 {
            let name = names
                .get(i)
                .copied()
                .filter(|n| *n != "\n")
                .unwrap_or("UNDEF");
            out.push_str(name);
            out.push(' ');
        }
    }
    if out.is_empty() {
        out.push_str("NOBITS ");
    }
}

// ===========================================================================
// Per-connection mode/state helpers.
// ===========================================================================

fn set_mode(conn: ConnId, mode: Mode) {
    if let Some(s) = states().lock().unwrap().get_mut(&conn) {
        s.mode = mode;
    }
}

// Small extension trait so `.get(&conn).cloned_state()` reads cleanly above.
trait ClonedState {
    fn cloned_state(self) -> Option<MeditState>;
}
impl ClonedState for Option<&MeditState> {
    fn cloned_state(self) -> Option<MeditState> {
        self.map(|s| MeditState {
            vnum: s.vnum,
            zone_number: s.zone_number,
            mob: s.mob.clone(),
            mode: s.mode,
            changed: s.changed,
            ddesc_buf: s.ddesc_buf.clone(),
        })
    }
}

// ===========================================================================
// The line parser — medit_parse. Dispatched by the OLC router each input line.
// ===========================================================================

pub fn medit_parse(g: &mut GameState, conn: ConnId, line: &str) {
    let mode = match states().lock().unwrap().get(&conn) {
        Some(s) => s.mode,
        None => {
            // Not actually editing — defensively clear and bail.
            olc::clear_active(conn);
            return;
        }
    };

    // Multi-line description editor consumes input until `/s` (or legacy `@`).
    if mode == Mode::DDesc {
        ddesc_input(g, conn, line);
        return;
    }

    // Numeric gate (C: OLC_MODE > MEDIT_NUMERICAL_RESPONSE).
    if mode.is_numeric() {
        let t = line.trim();
        let numeric = !t.is_empty()
            && (t
                .bytes()
                .next()
                .map(|b| b.is_ascii_digit())
                .unwrap_or(false)
                || (t.starts_with('-') && t.len() > 1 && t.as_bytes()[1].is_ascii_digit()));
        if !numeric {
            send(g, conn, "Field must be numerical, try again : ");
            return;
        }
    }

    match mode {
        Mode::ConfirmSave => parse_confirm_save(g, conn, line),
        Mode::MainMenu => parse_main_menu(g, conn, line),
        Mode::Alias => {
            let v = if line.trim().is_empty() {
                "undefined".to_string()
            } else {
                line.trim().to_string()
            };
            with_mob(conn, |m| m.alias = v);
            after_edit(g, conn);
        }
        Mode::ShortDesc => {
            let v = if line.trim().is_empty() {
                "undefined".to_string()
            } else {
                line.trim().to_string()
            };
            with_mob(conn, |m| m.short_desc = v);
            after_edit(g, conn);
        }
        Mode::LongDesc => {
            // C medit.c:1112-1115 stores the long description WITH its
            // trailing \r\n (render_mob_block must write 'desc\n~', not
            // 'desc~') (#266).
            let mut v = if line.trim().is_empty() {
                "undefined".to_string()
            } else {
                line.trim().to_string()
            };
            v.push_str("\r\n");
            with_mob(conn, |m| m.long_desc = v);
            after_edit(g, conn);
        }
        Mode::Sex => {
            let n = atoi(line).clamp(0, (NUM_GENDERS - 1) as i32);
            with_mob(conn, |m| m.sex = n);
            after_edit(g, conn);
        }
        Mode::Position => {
            let n = atoi(line).clamp(0, (NUM_POSITIONS - 1) as i32);
            with_mob(conn, |m| m.position = n);
            after_edit(g, conn);
        }
        Mode::DefaultPos => {
            let n = atoi(line).clamp(0, (NUM_POSITIONS - 1) as i32);
            with_mob(conn, |m| m.default_pos = n);
            after_edit(g, conn);
        }
        Mode::Attack => {
            let n = atoi(line).clamp(0, (NUM_ATTACK_TYPES - 1) as i32);
            with_mob(conn, |m| m.attack_type = n);
            after_edit(g, conn);
        }
        Mode::NpcFlags => {
            let i = atoi(line);
            if i == 0 {
                after_edit(g, conn);
                return;
            }
            if i > 0 && i <= NUM_MOB_FLAGS as i32 {
                with_mob(conn, |m| m.mob_flags ^= 1 << (i - 1));
            }
            disp_mob_flags(g, conn);
        }
        Mode::AffFlags => {
            let i = atoi(line);
            if i == 0 {
                after_edit(g, conn);
                return;
            }
            if i > 0 && i <= NUM_AFF_FLAGS as i32 {
                with_mob(conn, |m| m.aff_flags ^= 1 << (i - 1));
            }
            disp_aff_flags(g, conn);
        }
        Mode::Script(mut script_mode) => {
            let vnum = states()
                .lock()
                .unwrap()
                .get(&conn)
                .map(|s| s.vnum)
                .unwrap_or(0);
            let keep = olc::dg_script_edit_parse(
                g,
                conn,
                crate::dg_handler::MOB_TRIGGER,
                vnum,
                &mut script_mode,
                line.trim(),
            );
            if keep {
                if let Some(s) = states().lock().unwrap().get_mut(&conn) {
                    s.mode = Mode::Script(script_mode);
                    s.changed = true;
                }
            } else {
                disp_menu(g, conn);
            }
        }
        // Numerical responses (ranges mirror medit.c exactly).
        Mode::Level => {
            let n = atoi(line).clamp(1, 100);
            with_mob(conn, |m| {
                m.level = n;
                set_mob_stats(m, n);
            });
            after_edit(g, conn);
        }
        Mode::Alignment => num_set(g, conn, line, -1000, 1000, |m, v| m.alignment = v),
        Mode::Defense => num_set(g, conn, line, -1000, 1000, |m, v| m.defense = v),
        Mode::MDefense => num_set(g, conn, line, -1000, 1000, |m, v| m.mdefense = v),
        Mode::Power => num_set(g, conn, line, -1000, 1000, |m, v| m.power = v),
        Mode::MPower => num_set(g, conn, line, -1000, 1000, |m, v| m.mpower = v),
        Mode::Technique => num_set(g, conn, line, -1000, 1000, |m, v| m.technique = v),
        Mode::NumDamDice => num_set(g, conn, line, 0, 127, |m, v| m.num_dam_dice = v),
        Mode::SizeDamDice => num_set(g, conn, line, 0, 127, |m, v| m.size_dam_dice = v),
        Mode::NumHpDice => num_set(g, conn, line, 0, 127, |m, v| m.hit = v),
        Mode::SizeHpDice => num_set(g, conn, line, 0, 127, |m, v| m.mana = v),
        Mode::AddHp => num_set(g, conn, line, 0, 30000, |m, v| m.movep = v),
        Mode::Exp => {
            let n = (atoi64(line)).max(0);
            with_mob(conn, |m| m.exp = n);
            after_edit(g, conn);
        }
        Mode::Gold => {
            let n = (atoi64(line)).max(0);
            with_mob(conn, |m| m.gold = n);
            after_edit(g, conn);
        }
        // DDesc is consumed by the multi-line editor guard at the top of this fn;
        // this arm is unreachable but kept as a safe fallback (no panic).
        Mode::DDesc => after_edit(g, conn),
    }
}

/// Apply a clamped numeric edit then return to the main menu, marking changed.
fn num_set<F: FnOnce(&mut EditMob, i32)>(
    g: &mut GameState,
    conn: ConnId,
    line: &str,
    lo: i32,
    hi: i32,
    f: F,
) {
    let n = atoi(line).clamp(lo, hi);
    with_mob(conn, |m| f(m, n));
    after_edit(g, conn);
}

/// Mark the edit changed and redisplay the menu (the C fall-through tail).
fn after_edit(g: &mut GameState, conn: ConnId) {
    if let Some(s) = states().lock().unwrap().get_mut(&conn) {
        s.changed = true;
    }
    disp_menu(g, conn);
}

fn with_mob<F: FnOnce(&mut EditMob)>(conn: ConnId, f: F) {
    if let Some(s) = states().lock().unwrap().get_mut(&conn) {
        f(&mut s.mob);
    }
}

// ---- Main-menu choice dispatch -------------------------------------------

fn parse_main_menu(g: &mut GameState, conn: ConnId, line: &str) {
    // Ensure MOB_ISNPC stays set (C SET_BIT before any save decision).
    let c = line.trim().chars().next().unwrap_or('\0');
    match c {
        'q' | 'Q' => {
            let changed = states()
                .lock()
                .unwrap()
                .get(&conn)
                .map(|s| s.changed)
                .unwrap_or(false);
            if changed {
                send(
                    g,
                    conn,
                    "Do you wish to save the changes to the mobile? (y/n) : ",
                );
                set_mode(conn, Mode::ConfirmSave);
            } else {
                send(g, conn, "No changes made.\r\n");
                finish(g, conn);
            }
        }
        '1' => {
            set_mode(conn, Mode::Sex);
            disp_sex(g, conn);
        }
        '2' => {
            set_mode(conn, Mode::Alias);
            send(g, conn, "\r\nEnter new text :\r\n] ");
        }
        '3' => {
            set_mode(conn, Mode::ShortDesc);
            send(g, conn, "\r\nEnter new text :\r\n] ");
        }
        '4' => {
            set_mode(conn, Mode::LongDesc);
            send(g, conn, "\r\nEnter new text :\r\n] ");
        }
        '5' => {
            set_mode(conn, Mode::DDesc);
            if let Some(s) = states().lock().unwrap().get_mut(&conn) {
                s.ddesc_buf = s.mob.description.clone();
            }
            send(
                g,
                conn,
                "Enter mob description: (/s saves /h for help)\r\n\r\n",
            );
            let cur = states()
                .lock()
                .unwrap()
                .get(&conn)
                .map(|s| s.mob.description.clone())
                .unwrap_or_default();
            if !cur.is_empty() {
                send(g, conn, &cur);
            }
        }
        '6' => {
            set_mode(conn, Mode::Level);
            send(g, conn, "\r\nEnter new value : ");
        }
        '7' => {
            set_mode(conn, Mode::Alignment);
            send(g, conn, "\r\nEnter new value : ");
        }
        '8' => {
            set_mode(conn, Mode::Defense);
            send(g, conn, "\r\nEnter new value : ");
        }
        '9' => {
            set_mode(conn, Mode::MDefense);
            send(g, conn, "\r\nEnter new value : ");
        }
        'a' | 'A' => {
            set_mode(conn, Mode::Power);
            send(g, conn, "\r\nEnter new value : ");
        }
        'b' | 'B' => {
            set_mode(conn, Mode::MPower);
            send(g, conn, "\r\nEnter new value : ");
        }
        'c' | 'C' => {
            set_mode(conn, Mode::Technique);
            send(g, conn, "\r\nEnter new value : ");
        }
        'd' | 'D' => {
            set_mode(conn, Mode::Exp);
            send(g, conn, "\r\nEnter new value : ");
        }
        'e' | 'E' => {
            set_mode(conn, Mode::NumDamDice);
            send(g, conn, "\r\nEnter new value : ");
        }
        'f' | 'F' => {
            set_mode(conn, Mode::SizeDamDice);
            send(g, conn, "\r\nEnter new value : ");
        }
        'g' | 'G' => {
            set_mode(conn, Mode::NumHpDice);
            send(g, conn, "\r\nEnter new value : ");
        }
        'h' | 'H' => {
            set_mode(conn, Mode::SizeHpDice);
            send(g, conn, "\r\nEnter new value : ");
        }
        'i' | 'I' => {
            set_mode(conn, Mode::AddHp);
            send(g, conn, "\r\nEnter new value : ");
        }
        'j' | 'J' => {
            set_mode(conn, Mode::Gold);
            send(g, conn, "\r\nEnter new value : ");
        }
        'k' | 'K' => {
            set_mode(conn, Mode::Position);
            disp_positions(g, conn);
        }
        'l' | 'L' => {
            set_mode(conn, Mode::DefaultPos);
            disp_positions(g, conn);
        }
        'm' | 'M' => {
            set_mode(conn, Mode::Attack);
            disp_attack_types(g, conn);
        }
        'n' | 'N' => {
            set_mode(conn, Mode::NpcFlags);
            disp_mob_flags(g, conn);
        }
        'o' | 'O' => {
            set_mode(conn, Mode::AffFlags);
            disp_aff_flags(g, conn);
        }
        's' | 'S' => {
            let vnum = states()
                .lock()
                .unwrap()
                .get(&conn)
                .map(|s| s.vnum)
                .unwrap_or(0);
            olc::dg_script_menu(g, conn, crate::dg_handler::MOB_TRIGGER, vnum);
            set_mode(conn, Mode::Script(olc::DgScriptEditMode::Main));
        }
        _ => disp_menu(g, conn),
    }
}

fn parse_confirm_save(g: &mut GameState, conn: ConnId, line: &str) {
    // Force MOB_ISNPC regardless of which way the user answers (C medit_parse).
    with_mob(conn, |m| m.mob_flags |= MOB_ISNPC);
    match line.trim().chars().next().unwrap_or('\0') {
        'y' | 'Y' => {
            send(g, conn, "Saving mobile to memory.\r\n");
            save_internally(g, conn);
            save_to_disk(g, conn);
            finish(g, conn);
        }
        'n' | 'N' => {
            finish(g, conn);
        }
        _ => {
            send(g, conn, "Invalid choice!\r\n");
            send(g, conn, "Do you wish to save the mobile? : ");
        }
    }
}

/// Tear down the editor for this connection (CircleMUD cleanup_olc).
fn finish(g: &mut GameState, conn: ConnId) {
    states().lock().unwrap().remove(&conn);
    olc::clear_active(conn);
    send(g, conn, "Mobile editor exited.\r\n");
}

// ---- Multi-line description editor ---------------------------------------

fn ddesc_input(g: &mut GameState, conn: ConnId, line: &str) {
    if line.trim() == "@" {
        // Commit the gathered buffer into the mob description.
        let buf = states()
            .lock()
            .unwrap()
            .get(&conn)
            .map(|s| s.ddesc_buf.clone())
            .unwrap_or_default();
        let mut text = buf;
        if !text.ends_with('\n') {
            text.push('\n');
        }
        if text.len() > MAX_MOB_DESC {
            text.truncate(MAX_MOB_DESC);
        }
        with_mob(conn, |m| m.description = text);
        after_edit(g, conn);
        return;
    }

    let mut buf = states()
        .lock()
        .unwrap()
        .get(&conn)
        .map(|s| s.ddesc_buf.clone())
        .unwrap_or_default();
    match crate::modify::editor_buffer_input(g, conn, &mut buf, MAX_MOB_DESC, line) {
        crate::modify::BufferEditorResult::Continue => {
            if let Some(s) = states().lock().unwrap().get_mut(&conn) {
                s.ddesc_buf = buf;
            }
        }
        crate::modify::BufferEditorResult::Save => {
            with_mob(conn, |m| m.description = buf);
            after_edit(g, conn);
        }
        crate::modify::BufferEditorResult::Abort => {
            if let Some(s) = states().lock().unwrap().get_mut(&conn) {
                s.ddesc_buf.clear();
                s.mode = Mode::MainMenu;
            }
            disp_menu(g, conn);
        }
    }
}

// ===========================================================================
// Save: into memory (MobileProto + live mobs) and to the .mob file.
// ===========================================================================

/// Update the in-memory `MobileProto` (and live instances' display strings)
/// from the scratch mob (CircleMUD medit_save_internally — the parts that map
/// onto the fields the Rust proto keeps). Fields the proto does not store
/// (flags/abilities/alignment/etc.) live only on disk; they are preserved by
/// the byte-faithful `.mob` write that follows.
fn save_internally(g: &mut GameState, conn: ConnId) {
    let st = match states().lock().unwrap().get(&conn).cloned_state() {
        Some(s) => s,
        None => return,
    };
    let m = &st.mob;
    let vnum = st.vnum;

    let long_with_nl = {
        let mut s = m.long_desc.clone();
        if !s.ends_with('\n') {
            s.push('\n');
        }
        s
    };

    let proto = MobileProto {
        vnum,
        name: m.alias.clone(),
        short_desc: m.short_desc.clone(),
        long_desc: long_with_nl,
        description: m.description.clone(),
        level: m.level.clamp(0, 255) as Level,
        hitpoints: m.hit.max(1),
        hit_dice: (0, 0, m.hit.max(1)),
        experience: m.exp,
        gold: m.gold as Gold,
        position: Position::from_u8(m.position.clamp(0, 9) as u8),
        default_pos: Position::from_u8(m.default_pos.clamp(0, 9) as u8),
        sex: Gender::from_u8(m.sex.clamp(0, 2) as u8),
        alignment: m.alignment,
        act_flags: m.mob_flags | MOB_ISNPC,
        affect_flags: m.aff_flags,
        // Combat fields the proto keeps; armor not edited by medit (espec-only
        // in this DeltaMUD build), keep prior value if present.
        armor: g.mob_protos.get(&vnum).map(|p| p.armor).unwrap_or(0),
        hitroll: g.mob_protos.get(&vnum).map(|p| p.hitroll).unwrap_or(0),
        damroll: g.mob_protos.get(&vnum).map(|p| p.damroll).unwrap_or(0),
        damnodice: m.num_dam_dice,
        damsizedice: m.size_dam_dice,
        power: m.power.clamp(i16::MIN as i32, i16::MAX as i32) as i16,
        mpower: m.mpower.clamp(i16::MIN as i32, i16::MAX as i32) as i16,
        defense: m.defense.clamp(i16::MIN as i32, i16::MAX as i32) as i16,
        mdefense: m.mdefense.clamp(i16::MIN as i32, i16::MAX as i32) as i16,
        technique: m.technique.clamp(i16::MIN as i32, i16::MAX as i32) as i16,
        abilities: Some(crate::character::Abilities {
            str: m.str_.clamp(0, 127) as i8,
            str_add: m.str_add.clamp(0, 127) as i8,
            intel: m.intel.clamp(0, 127) as i8,
            wis: m.wis.clamp(0, 127) as i8,
            dex: m.dex.clamp(0, 127) as i8,
            con: m.con.clamp(0, 127) as i8,
            cha: m.cha.clamp(0, 127) as i8,
        }),
        attack_type: m.attack_type,
    };

    g.mob_protos.insert(vnum, proto);

    // Update display strings on any live instances of this mob (C only fixes
    // the strings on a live update, since those can fault; the rest waits for a
    // reset). Our live mobs reference vnum via `nr`.
    let live: Vec<CharId> = g
        .chars
        .iter()
        .filter(|(_, c)| c.is_npc && c.nr == vnum)
        .map(|(id, _)| *id)
        .collect();
    for id in live {
        if let Some(c) = g.get_char_mut(id) {
            c.player.name = m.alias.clone();
            c.short_desc = Some(m.short_desc.clone());
            let mut ld = m.long_desc.clone();
            if !ld.ends_with('\n') {
                ld.push('\n');
            }
            c.long_desc = Some(ld);
            c.npc_description = Some(m.description.clone());
        }
    }
}

/// Save every mob in this mob's zone to its `<zone>.mob` file in the DeltaMUD
/// extended `X`-format, byte-faithfully (CircleMUD medit_save_to_disk). We
/// re-emit every mob in the zone: the one just edited from the scratch state,
/// all others from their on-disk blocks (so their espec fields survive).
fn save_to_disk(g: &mut GameState, conn: ConnId) {
    let st = match states().lock().unwrap().get(&conn).cloned_state() {
        Some(s) => s,
        None => return,
    };
    let zone_number = st.zone_number;
    let path = mob_file_path(g, zone_number);

    // Collect, in vnum order, every mob block for this zone. The edited mob's
    // block is produced from scratch state; the rest are read off disk.
    let (zone_lo, zone_top) = match g.zones.iter().find(|z| z.number == zone_number) {
        Some(z) => (z.number * 100, z.top),
        None => (zone_number * 100, zone_number * 100 + 99),
    };

    // Existing on-disk blocks (so unedited mobs keep their exact text/espec).
    let disk_blocks = read_disk_blocks(&path);

    // The full set of vnums to write: every vnum present on disk in range, plus
    // the edited vnum (which may be new).
    let mut vnums: Vec<MobVnum> = disk_blocks.keys().copied().collect();
    if !vnums.contains(&st.vnum) {
        vnums.push(st.vnum);
    }
    vnums.retain(|v| *v >= zone_lo && *v <= zone_top || *v == st.vnum);
    vnums.sort_unstable();
    vnums.dedup();

    let mut out = String::new();
    for v in vnums {
        out.push_str(&format!("#{}\n", v));
        if v == st.vnum {
            out.push_str(&render_mob_block(&st.mob));
            // Re-emit the mob's DG trigger attachments after its `E` line
            // (C medit.c:557 script_save_to_disk(mob_file, mob, MOB_TRIGGER)).
            // The bindings live in the dg proto-script map; the editor scratch
            // state carries no trigger field, so without this the edited mob's
            // `T` lines are dropped on save while every unedited mob keeps its
            // (preserved verbatim from disk below).
            for tv in crate::dg_db_scripts::proto_trigger_vnums(0, st.vnum) {
                out.push_str(&format!("T {}\n", tv));
            }
        } else if let Some(block) = disk_blocks.get(&v) {
            out.push_str(block);
        }
    }
    out.push_str("$\n");

    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if std::fs::write(&path, out.as_bytes()).is_err() {
        send(
            g,
            conn,
            "Warning: could not write the mob file to disk.\r\n",
        );
    }
}

/// medit_save_to_disk(zone): central OLC save dispatcher entry. Rewrites every
/// mobile prototype in the zone using the same extended block renderer as the
/// editor save path.
pub fn medit_save_to_disk(g: &mut GameState, zone_rnum: usize) {
    let (zone_number, zone_lo, zone_top) = match g.zones.get(zone_rnum) {
        Some(z) => (z.number, z.number * 100, z.top),
        None => return,
    };
    let path = mob_file_path(g, zone_number);
    let mut vnums: Vec<MobVnum> = g
        .mob_protos
        .keys()
        .copied()
        .filter(|v| *v >= zone_lo && *v <= zone_top)
        .collect();
    vnums.sort_unstable();

    let mut out = String::new();
    for v in vnums {
        if let Some(proto) = g.mob_protos.get(&v) {
            let mob = seed_from_proto(g, proto, v);
            out.push_str(&format!("#{}\n", v));
            out.push_str(&render_mob_block(&mob));
            for tv in crate::dg_db_scripts::proto_trigger_vnums(0, v) {
                out.push_str(&format!("T {}\n", tv));
            }
        }
    }
    out.push_str("$\n");

    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&path, out.as_bytes());
    crate::olc::olc_remove_from_save_list(zone_number, crate::olc::OLC_SAVE_MOB);
}

/// Render one mob's body block (everything after the `#vnum` line, up to but
/// not including the next `#`/`$`) in DeltaMUD extended `X`-format. Mirrors
/// medit_save_to_disk's fprintf sequence exactly.
fn render_mob_block(m: &EditMob) -> String {
    let ldesc = strip_string(if m.long_desc.is_empty() {
        "undefined"
    } else {
        &m.long_desc
    });
    let ddesc = strip_string(if m.description.is_empty() {
        "undefined"
    } else {
        &m.description
    });
    let alias = if m.alias.is_empty() {
        "undefined"
    } else {
        &m.alias
    };
    let sdesc = if m.short_desc.is_empty() {
        "undefined"
    } else {
        &m.short_desc
    };

    let mut s = String::new();
    s.push_str(&format!("{}~\n", alias));
    s.push_str(&format!("{}~\n", sdesc));
    s.push_str(&format!("{}~\n", ldesc));
    s.push_str(&format!("{}~\n", ddesc));
    // Flag line: MOB_FLAGS AFF_FLAGS ALIGNMENT E
    s.push_str(&format!(
        "{} {} {} E\n",
        m.mob_flags, m.aff_flags, m.alignment
    ));
    // X-line: X<level> <power> <mpower> <defense> <mdefense> <technique>
    //         <hit>d<mana>+<move> <ndd>d<sdd>
    s.push_str(&format!(
        "X{} {} {} {} {} {} {}d{}+{} {}d{}\n",
        m.level,
        m.power,
        m.mpower,
        m.defense,
        m.mdefense,
        m.technique,
        m.hit,
        m.mana,
        m.movep,
        m.num_dam_dice,
        m.size_dam_dice
    ));
    // Gold + exp.
    s.push_str(&format!("{} {}\n", m.gold, m.exp));
    // Position / default_pos / sex.
    s.push_str(&format!("{} {} {}\n", m.position, m.default_pos, m.sex));
    // Espec block — only non-default values, exactly as the C does.
    if m.attack_type != 0 {
        s.push_str(&format!("BareHandAttack: {}\n", m.attack_type));
    }
    if m.str_ != MOB_DEFAULT_STAT {
        s.push_str(&format!("Str: {}\n", m.str_));
    }
    if m.str_add != 0 {
        s.push_str(&format!("StrAdd: {}\n", m.str_add));
    }
    if m.dex != MOB_DEFAULT_STAT {
        s.push_str(&format!("Dex: {}\n", m.dex));
    }
    if m.intel != MOB_DEFAULT_STAT {
        s.push_str(&format!("Int: {}\n", m.intel));
    }
    if m.wis != MOB_DEFAULT_STAT {
        s.push_str(&format!("Wis: {}\n", m.wis));
    }
    if m.con != MOB_DEFAULT_STAT {
        s.push_str(&format!("Con: {}\n", m.con));
    }
    if m.cha != MOB_DEFAULT_STAT {
        s.push_str(&format!("Cha: {}\n", m.cha));
    }
    s.push_str("E\n");
    s
}

/// CircleMUD strip_string: collapse trailing whitespace on each line and drop
/// embedded `\r` so the on-disk text is clean (mirrors utils.c strip_string).
fn strip_string(s: &str) -> String {
    s.replace('\r', "")
}

// ===========================================================================
// On-disk .mob reading (for unedited-mob preservation and extended-field seed).
// ===========================================================================

struct DiskMobExt {
    mob_flags: i64,
    aff_flags: i64,
    alignment: i32,
    power: i32,
    mpower: i32,
    defense: i32,
    mdefense: i32,
    technique: i32,
    hit: i32,
    mana: i32,
    movep: i32,
    num_dam_dice: i32,
    size_dam_dice: i32,
    attack_type: i32,
    str_: i32,
    str_add: i32,
    intel: i32,
    wis: i32,
    dex: i32,
    con: i32,
    cha: i32,
}

/// Read raw body blocks (text after each `#vnum` up to the next `#`/`$`),
/// keyed by vnum, preserving the exact on-disk bytes for round-trip.
fn read_disk_blocks(path: &std::path::Path) -> HashMap<MobVnum, String> {
    let mut map = HashMap::new();
    let contents = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return map,
    };
    let lines: Vec<&str> = contents.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let t = lines[i].trim_end();
        if t == "$" || t == "$~" {
            break;
        }
        if let Some(rest) = t.strip_prefix('#') {
            if let Ok(vnum) = rest.trim().parse::<MobVnum>() {
                i += 1;
                let mut block = String::new();
                while i < lines.len() {
                    let lt = lines[i].trim_end();
                    if lt.starts_with('#') || lt == "$" || lt == "$~" {
                        break;
                    }
                    block.push_str(lines[i]);
                    block.push('\n');
                    i += 1;
                }
                map.insert(vnum, block);
                continue;
            }
        }
        i += 1;
    }
    map
}

/// Parse the extended numeric fields out of a single mob's on-disk block.
fn read_disk_mob(path: &std::path::Path, vnum: MobVnum) -> Option<DiskMobExt> {
    let blocks = read_disk_blocks(path);
    let block = blocks.get(&vnum)?;
    let lines: Vec<&str> = block.lines().collect();
    let mut idx = 0;

    // Skip the four tilde-terminated strings.
    let mut skipped = 0;
    while idx < lines.len() && skipped < 4 {
        if lines[idx].contains('~') {
            skipped += 1;
        }
        idx += 1;
    }

    // Flag line: MOB_FLAGS AFF_FLAGS ALIGNMENT {S|E}
    let flag_line = lines.get(idx)?;
    idx += 1;
    let fp: Vec<&str> = flag_line.split_whitespace().collect();
    if fp.len() < 4 {
        return None;
    }
    let mob_flags = asciiflag_conv(fp[0]);
    let aff_flags = asciiflag_conv(fp[1]);
    let alignment: i32 = fp[2].parse().unwrap_or(0);
    let letter = fp[3].chars().next().unwrap_or('S').to_ascii_uppercase();

    // Stats line — classic ` # # # #d#+# #d#+#` or extended `X# # # # # # #d#+# #d#`.
    let stats_line = lines.get(idx)?.trim();
    idx += 1;

    let mut ext = DiskMobExt {
        mob_flags,
        aff_flags,
        alignment,
        power: 0,
        mpower: 0,
        defense: 0,
        mdefense: 0,
        technique: 0,
        hit: 1,
        mana: 1,
        movep: 100,
        num_dam_dice: 1,
        size_dam_dice: 1,
        attack_type: 0,
        str_: MOB_DEFAULT_STAT,
        str_add: 0,
        intel: MOB_DEFAULT_STAT,
        wis: MOB_DEFAULT_STAT,
        dex: MOB_DEFAULT_STAT,
        con: MOB_DEFAULT_STAT,
        cha: MOB_DEFAULT_STAT,
    };

    if let Some(rest) = stats_line
        .strip_prefix('X')
        .or_else(|| stats_line.strip_prefix('x'))
    {
        // X<level> <power> <mpower> <defense> <mdefense> <technique> <hit>d<mana>+<move> <ndd>d<sdd>
        let toks: Vec<&str> = rest.split_whitespace().collect();
        let n = |k: usize| toks.get(k).and_then(|s| s.parse::<i32>().ok()).unwrap_or(0);
        ext.power = n(1);
        ext.mpower = n(2);
        ext.defense = n(3);
        ext.mdefense = n(4);
        ext.technique = n(5);
        if let Some((h, ma, mo)) = parse_dice_plus(toks.get(6).copied().unwrap_or("")) {
            ext.hit = h;
            ext.mana = ma;
            ext.movep = mo;
        }
        if let Some((ndd, sdd)) = parse_dice(toks.get(7).copied().unwrap_or("")) {
            ext.num_dam_dice = ndd;
            ext.size_dam_dice = sdd;
        }
    } else {
        // Classic: ` # # # #d#+# #d#+#` — fields after level are thac0/ac then
        // the H/M/V dice and bare-hand dice. We extract the H dice (4th token)
        // and bare-hand dice (last token).
        let toks: Vec<&str> = stats_line.split_whitespace().collect();
        if let Some((h, ma, mo)) = parse_dice_plus(toks.get(3).copied().unwrap_or("")) {
            ext.hit = h;
            ext.mana = ma;
            ext.movep = mo;
        }
        if let Some((ndd, sdd)) = parse_dice(toks.get(4).copied().unwrap_or("")) {
            ext.num_dam_dice = ndd;
            ext.size_dam_dice = sdd;
        }
    }

    // Skip gold/exp and pos/default/sex (already on the proto), then read the
    // espec keyword block until a lone 'E' (only when the flag letter was 'E').
    idx += 2; // gold/exp line + pos line
    if letter == 'E' {
        while idx < lines.len() {
            let lt = lines[idx].trim();
            idx += 1;
            if lt == "E" {
                break;
            }
            if let Some((kw, val)) = lt.split_once(':') {
                let num: i32 = val.trim().parse().unwrap_or(0);
                match kw.trim() {
                    "BareHandAttack" => ext.attack_type = num.clamp(0, 99),
                    "Str" => ext.str_ = num.clamp(3, 25),
                    "StrAdd" => ext.str_add = num.clamp(0, 100),
                    "Int" => ext.intel = num.clamp(3, 25),
                    "Wis" => ext.wis = num.clamp(3, 25),
                    "Dex" => ext.dex = num.clamp(3, 25),
                    "Con" => ext.con = num.clamp(3, 25),
                    "Cha" => ext.cha = num.clamp(3, 25),
                    _ => {}
                }
            }
        }
    }

    Some(ext)
}

/// Parse `NdM+K` -> (N, M, K). Returns None if not in that form.
fn parse_dice_plus(tok: &str) -> Option<(i32, i32, i32)> {
    let (n_part, rest) = tok.split_once('d')?;
    let (m_part, plus) = match rest.split_once('+') {
        Some((m, p)) => (m, Some(p)),
        None => (rest, None),
    };
    let n: i32 = n_part.parse().ok()?;
    let m: i32 = m_part.parse().ok()?;
    let k: i32 = plus.and_then(|p| p.parse().ok()).unwrap_or(0);
    Some((n, m, k))
}

/// Parse `NdM` -> (N, M).
fn parse_dice(tok: &str) -> Option<(i32, i32)> {
    let (n_part, m_part) = tok.split_once('d')?;
    let n: i32 = n_part.parse().ok()?;
    // Drop any trailing +K (defensive; bare-hand dice has no +).
    let m: i32 = m_part.split('+').next().unwrap_or(m_part).parse().ok()?;
    Some((n, m))
}

/// CircleMUD asciiflag_conv: a flag field is either a plain integer or a string
/// of letters (a-z = bits 0-25, A-Z = bits 26-51).
fn asciiflag_conv(flag: &str) -> i64 {
    let flag = flag.trim();
    if let Ok(n) = flag.parse::<i64>() {
        return n;
    }
    let mut bits: i64 = 0;
    for c in flag.chars() {
        if c.is_ascii_lowercase() {
            bits |= 1 << (c as i64 - 'a' as i64);
        } else if c.is_ascii_uppercase() {
            bits |= 1 << (26 + c as i64 - 'A' as i64);
        }
    }
    bits
}

// ===========================================================================
// set_mob_stats — DeltaMUD autoset on level change (medit.c set_mob_stats).
// ===========================================================================

fn set_mob_stats(m: &mut EditMob, _level: i32) {
    let level = m.level as f64;
    // C medit.c:1442-1447: '7.5' is double, so the whole expression
    // evaluates in FP and truncates ONCE on assignment. The intermediate
    // i32 cast lost 1 point per odd level (#265).
    m.defense = (level * 7.5 * 7.0 / 10.0) as i32;
    m.mdefense = (level * 7.5 * 7.0 / 10.0) as i32;
    m.power = (level * 7.5 * 8.0 / 10.0) as i32;
    m.mpower = (level * 7.5 * 7.0 / 10.0) as i32;
    m.technique = (level * 7.5 * 7.0 / 10.0) as i32;
    m.num_dam_dice = 1;
    m.size_dam_dice = 1 + (m.level - 1) * 3;
    m.movep = 20 + (m.level - 1) * 17;
    m.gold = (m.level * 10) as i64;
    let lvl = m.level;
    let etl_now = exp_to_level(lvl);
    let etl_prev = exp_to_level(lvl - 1);
    m.exp = (((etl_now - etl_prev) / (19 + lvl as i64)) as f64 * 1.5) as i64;
}

/// DeltaMUD exp_to_level (utils.c). Recursive; bounded by level <= LVL_IMPL.
fn exp_to_level(arg: i32) -> i64 {
    if arg < 1 || arg as Level > LVL_IMPL {
        return 0;
    }
    let modifier = if arg < 10 {
        mlog(3.0, arg)
    } else if arg < 20 {
        mlog(2.9, arg)
    } else if arg < 30 {
        mlog(2.8, arg)
    } else if arg < 40 {
        mlog(2.7, arg)
    } else if arg < 50 {
        mlog(2.6, arg)
    } else if arg < 60 {
        mlog(2.5, arg)
    } else if arg < 70 {
        mlog(2.4, arg)
    } else if arg < 80 {
        mlog(2.3, arg)
    } else if arg < 90 {
        mlog(2.2, arg)
    } else if arg < 96 {
        mlog(2.05, arg)
    } else {
        mlog(2.0, arg)
    };
    if arg == 1 {
        3000
    } else {
        (4000.0 * (arg as f64 * modifier)) as i64 + exp_to_level(arg - 1)
    }
}

/// DeltaMUD `mlog(base, x)` = log of x in the given base (utils.c).
fn mlog(base: f64, x: i32) -> f64 {
    (x as f64).ln() / base.ln()
}

// ===========================================================================
// Tiny numeric helpers (C atoi semantics: leading int, else 0).
// ===========================================================================

fn atoi(s: &str) -> i32 {
    let t = s.trim();
    let mut end = 0;
    let bytes = t.as_bytes();
    if !bytes.is_empty() && (bytes[0] == b'-' || bytes[0] == b'+') {
        end = 1;
    }
    while end < bytes.len() && bytes[end].is_ascii_digit() {
        end += 1;
    }
    t[..end].parse().unwrap_or(0)
}

fn atoi64(s: &str) -> i64 {
    let t = s.trim();
    let mut end = 0;
    let bytes = t.as_bytes();
    if !bytes.is_empty() && (bytes[0] == b'-' || bytes[0] == b'+') {
        end = 1;
    }
    while end < bytes.len() && bytes[end].is_ascii_digit() {
        end += 1;
    }
    t[..end].parse().unwrap_or(0)
}
