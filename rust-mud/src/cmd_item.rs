// cmd_item.rs — object-handling commands (CircleMUD/DeltaMUD act.item.c),
// ported 1:1 to the id-indexed GameState. Covers get / put / drop / junk /
// donate / give / take, wear / wield / hold / grab / remove, drink / eat /
// sip / taste / pour / fill, sacrifice and repair, plus the perform_* helpers.
//
// Output strings track the C source byte-for-byte. DG-script triggers
// (drop_otrigger / get_otrigger / wear_otrigger / receive_mtrigger / ...) are
// not yet ported; in the C they return TRUE on the common path, so they are
// treated here as always-true no-ops.

use crate::act::{ActArg, To, act};
use crate::character::Affect;
use crate::flags::*;
use crate::object::{ExtraFlags, ObjLoc, ObjectType, WearFlags};
use crate::room::SectorType;
use crate::state::GameState;
use crate::types::*;

// ---------------------------------------------------------------------------
// Local constants mirroring C #defines not present in the Rust contract.
// ---------------------------------------------------------------------------

// find_all_dots() return codes (handler.h FIND_*).
const FIND_INDIV: i32 = 0;
const FIND_ALL: i32 = 1;
const FIND_ALLDOT: i32 = 2;

// generic_find() modes (handler.h) — only the two we need.
const FIND_OBJ_INV: i32 = 4;
const FIND_OBJ_ROOM: i32 = 8;

// do_drop subcmds (interpreter.h).
const SCMD_DROP: i32 = 0;
const SCMD_JUNK: i32 = 1;
const SCMD_DONATE: i32 = 2;

// do_pour subcmds.
const SCMD_POUR: i32 = 0;
const SCMD_FILL: i32 = 1;

// do_eat / do_drink subcmds.
const SCMD_EAT: i32 = 0;
const SCMD_TASTE: i32 = 1;
const SCMD_DRINK: i32 = 2;
const SCMD_SIP: i32 = 3;

// Container value[1] flags (structs.h CONT_*).
const CONT_CLOSED: i32 = 1 << 2;

// Object extra flags (structs.h ITEM_*) — value mirrors ExtraFlags bits.
const ITEM_NODROP: u64 = 1 << 7;
const ITEM_NODONATE: u64 = 1 << 3;

// Object wear-location flags (structs.h ITEM_WEAR_*).
const ITEM_WEAR_TAKE: u32 = 1 << 0;
const ITEM_WEAR_FINGER: u32 = 1 << 1;
const ITEM_WEAR_NECK: u32 = 1 << 2;
const ITEM_WEAR_BODY: u32 = 1 << 3;
const ITEM_WEAR_HEAD: u32 = 1 << 4;
const ITEM_WEAR_LEGS: u32 = 1 << 5;
const ITEM_WEAR_FEET: u32 = 1 << 6;
const ITEM_WEAR_HANDS: u32 = 1 << 7;
const ITEM_WEAR_ARMS: u32 = 1 << 8;
const ITEM_WEAR_SHIELD: u32 = 1 << 9;
const ITEM_WEAR_ABOUT: u32 = 1 << 10;
const ITEM_WEAR_WAIST: u32 = 1 << 11;
const ITEM_WEAR_WRIST: u32 = 1 << 12;
const ITEM_WEAR_WIELD: u32 = 1 << 13;
const ITEM_WEAR_HOLD: u32 = 1 << 14;
const ITEM_WEAR_SHOULDERS: u32 = 1 << 15;
const ITEM_WEAR_ANKLE: u32 = 1 << 16;
const ITEM_WEAR_FACE: u32 = 1 << 17;

// Equipment wear SLOTS (structs.h WEAR_*). NOTE: the C set runs 0..=21 with
// NUM_WEARS == 22 (SHOULDERS=18, ANKLE_R=19, ANKLE_L=20, FACE=21). The current
// Rust `types.rs` diverges (WEAR_FLOAT=18, WEAR_FACE=19, NUM_WEARS=20) and has
// no shoulders/ankle slots, and `Character::equipment` is only 20 wide. We use
// the C-accurate slot numbers locally so the wear-message / bitvector / "already
// wearing" arrays index identically to C; `equip_char`/`unequip_char` bounds-
// check against NUM_WEARS(20), so slots 20/21 currently no-op until the
// contract widens the array. See the manifest notes.
const W_LIGHT: usize = 0;
const W_FINGER_R: usize = 1;
const W_NECK_1: usize = 3;
const W_BODY: usize = 5;
const W_HEAD: usize = 6;
const W_LEGS: usize = 7;
const W_FEET: usize = 8;
const W_HANDS: usize = 9;
const W_ARMS: usize = 10;
const W_SHIELD: usize = 11;
const W_ABOUT: usize = 12;
const W_WAIST: usize = 13;
const W_WRIST_R: usize = 14;
const W_WIELD: usize = 16;
const W_HOLD: usize = 17;
const W_SHOULDERS: usize = 18;
const W_ANKLE_R: usize = 19;
const W_FACE: usize = 21;

// Config (config.c) — jail / donation rooms, PvP, weapon restrictions.
const DONATION_ROOM_1: RoomVnum = 146;
// C config.c:93 `int weaponrestrictions = YES` (=1).
const WEAPONRESTRICTIONS: i32 = 1;

pub(crate) fn weapon_restrictions() -> i32 {
    WEAPONRESTRICTIONS
}

/// C config.c:332-346 lvl_maxdmg_weapon[LVL_IMMORT]: the maximum weapon
/// damage potential ((val[2]+1)/2 * val[1]) a mortal of each level may
/// wield. 15 at 0-9 rising to 100 at 90+ (#122).
pub(crate) fn lvl_maxdmg_weapon(level: usize) -> i64 {
    const BANDS: [(usize, i64); 11] = [
        (0, 15),
        (10, 20),
        (20, 25),
        (30, 30),
        (40, 35),
        (50, 45),
        (60, 50),
        (70, 60),
        (80, 75),
        (90, 100),
        (100, 100),
    ];
    let mut cap = 15;
    for (lo, v) in BANDS.iter() {
        if level >= *lo {
            cap = *v;
        }
    }
    cap
}

// Spell/affect numbers (spells.h).
const SPELL_POISON: i32 = 23;

// Player flag (structs.h PLR_KILLER).
const PLR_KILLER: i64 = 1 << 0;
// Player2 flag (structs.h PRF2_MBUILDING).
const PRF2_MBUILDING: i64 = 1 << 6;
// Room flag (structs.h ROOM_HOUSE_CRASH).
const ROOM_HOUSE_CRASH: u32 = 1 << 12;

// C ships -100 so the box can never exist; the finish-the-game program
// authored obj 180 in zone 1 and points the define at it (#358).
const PANDORAS_BOX_VNUM: ObjVnum = 180;

// ---------------------------------------------------------------------------
// Tiny ports of helpers act.item.c reaches into utils.c / handler.c for.
// ---------------------------------------------------------------------------

/// AN(string): "an" if the first char is a vowel, else "a" (utils.h AN macro).
fn an(s: &str) -> &'static str {
    match s.chars().next() {
        Some(c) if "aeiouAEIOU".contains(c) => "an",
        _ => "a",
    }
}

/// is_number(): optional leading '-' then all digits (interpreter.c).
fn is_number(s: &str) -> bool {
    let t = s.strip_prefix('-').unwrap_or(s);
    !t.is_empty() && t.chars().all(|c| c.is_ascii_digit())
}

/// find_all_dots(): "all" -> FIND_ALL, "all.x" -> (FIND_ALLDOT, "x"),
/// else FIND_INDIV. Returns the (mode, stripped-name) pair; the C version
/// mutates `arg` in place to strip the "all." prefix, which we return.
fn find_all_dots(arg: &str) -> (i32, String) {
    if arg == "all" {
        (FIND_ALL, arg.to_string())
    } else if let Some(rest) = arg.strip_prefix("all.") {
        (FIND_ALLDOT, rest.to_string())
    } else {
        (FIND_INDIV, arg.to_string())
    }
}

/// two_arguments(): one_argument(one_argument(...)).
fn two_arguments(argument: &str) -> (String, String) {
    let (a1, rest) = crate::interpreter::one_argument(argument);
    let (a2, _) = crate::interpreter::one_argument(rest);
    (a1, a2)
}

/// str_app[].carry_w / .wield_w table (constants.c). Index by
/// STRENGTH_APPLY_INDEX. Only the two columns act.item.c needs.
const STR_APP: [(i32, i32); 31] = [
    (0, 0),
    (3, 1),
    (3, 2),
    (10, 3),
    (25, 4),
    (55, 5),
    (80, 6),
    (90, 7),
    (100, 8),
    (100, 9),
    (115, 10),
    (115, 11),
    (140, 12),
    (140, 13),
    (170, 14),
    (170, 15),
    (195, 16),
    (220, 18),
    (255, 20),
    (640, 40),
    (700, 40),
    (810, 40),
    (970, 40),
    (1130, 40),
    (1440, 40),
    (1750, 40),
    (280, 22),
    (305, 24),
    (330, 26),
    (380, 28),
    (480, 30),
];

/// STRENGTH_APPLY_INDEX(ch) (utils.h). Uses aff_abils.str / str_add.
fn strength_apply_index(g: &GameState, ch: CharId) -> usize {
    let c = match g.get_char(ch) {
        Some(c) => c,
        None => return 0,
    };
    let str_ = c.aff_abils.str as i32;
    let add = c.aff_abils.str_add as i32;
    if add == 0 || str_ != 18 {
        str_.clamp(0, 30) as usize
    } else if add <= 50 {
        26
    } else if add <= 75 {
        27
    } else if add <= 90 {
        28
    } else if add <= 99 {
        29
    } else {
        30
    }
}

/// CAN_CARRY_W(ch) = str_app[STRENGTH_APPLY_INDEX(ch)].carry_w.
fn can_carry_w(g: &GameState, ch: CharId) -> i32 {
    STR_APP[strength_apply_index(g, ch)].0
}

/// CAN_CARRY_N(ch) = 5 + (DEX>>1) + (LEVEL>>1).
fn can_carry_n(g: &GameState, ch: CharId) -> i32 {
    let c = match g.get_char(ch) {
        Some(c) => c,
        None => return 0,
    };
    5 + (c.aff_abils.dex as i32 >> 1) + (c.player.level as i32 >> 1)
}

fn is_carrying_n(g: &GameState, ch: CharId) -> i32 {
    g.get_char(ch).map(|c| c.carry_items as i32).unwrap_or(0)
}
fn is_carrying_w(g: &GameState, ch: CharId) -> i32 {
    g.get_char(ch).map(|c| c.carry_weight).unwrap_or(0)
}

fn get_level(g: &GameState, ch: CharId) -> Level {
    g.get_char(ch).map(|c| c.player.level).unwrap_or(0)
}
fn is_immort(g: &GameState, ch: CharId) -> bool {
    get_level(g, ch) >= LVL_IMMORT
}

fn obj_type(g: &GameState, oid: ObjId) -> Option<ObjectType> {
    g.get_obj(oid).map(|o| o.obj_type)
}
fn obj_val(g: &GameState, oid: ObjId, idx: usize) -> i32 {
    g.get_obj(oid).map(|o| o.values[idx]).unwrap_or(0)
}
fn obj_weight(g: &GameState, oid: ObjId) -> i32 {
    g.get_obj(oid).map(|o| o.weight).unwrap_or(0)
}
fn obj_vnum(g: &GameState, oid: ObjId) -> ObjVnum {
    g.get_obj(oid).map(|o| o.item_number).unwrap_or(NOTHING)
}
fn obj_wear(g: &GameState, oid: ObjId, bit: u32) -> bool {
    g.get_obj(oid)
        .map(|o| o.wear_flags.bits() & bit != 0)
        .unwrap_or(false)
}
fn obj_stat(g: &GameState, oid: ObjId, bit: u64) -> bool {
    g.get_obj(oid)
        .map(|o| o.extra_flags.bits() & bit != 0)
        .unwrap_or(false)
}
/// CAN_SEE_OBJ — Tier-0 visibility just defers to obj existence (matches
/// the contract's get_obj_in_list_vis, which ignores invisibility).
fn can_see_obj(g: &GameState, _ch: CharId, oid: ObjId) -> bool {
    g.get_obj(oid).is_some()
}

/// GET_EQ(ch, pos) — bounds-safe against the live equipment array (which is
/// only types::NUM_WEARS(20) wide while C slots run to 21).
fn eq_at(g: &GameState, ch: CharId, pos: usize) -> Option<ObjId> {
    g.get_char(ch)
        .and_then(|c| c.equipment.get(pos).copied().flatten())
}

/// money_desc(amount) (handler.c).
fn money_desc(amount: i32) -> &'static str {
    if amount == 1 {
        "a gold coin"
    } else if amount <= 10 {
        "a tiny pile of gold coins"
    } else if amount <= 20 {
        "a handful of gold coins"
    } else if amount <= 75 {
        "a little pile of gold coins"
    } else if amount <= 200 {
        "a small pile of gold coins"
    } else if amount <= 1000 {
        "a pile of gold coins"
    } else if amount <= 5000 {
        "a big pile of gold coins"
    } else if amount <= 10000 {
        "a large heap of gold coins"
    } else if amount <= 20000 {
        "a huge mound of gold coins"
    } else if amount <= 75000 {
        "an enormous mound of gold coins"
    } else if amount <= 150000 {
        "a small mountain of gold coins"
    } else if amount <= 250000 {
        "a mountain of gold coins"
    } else if amount <= 500000 {
        "a huge mountain of gold coins"
    } else if amount <= 1000000 {
        "an enormous mountain of gold coins"
    } else {
        "an absolutely colossal mountain of gold coins"
    }
}

/// Capitalise first char (utils.h CAP) for descriptions.
fn cap(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(f) => f.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// create_money(amount) (handler.c) — builds a synthetic money object.
fn create_money(g: &mut GameState, amount: i32) -> Option<ObjId> {
    if amount <= 0 {
        return None;
    }
    let (name, short, descr) = if amount == 1 {
        (
            "coin gold".to_string(),
            "a gold coin".to_string(),
            "One miserable gold coin is lying here.".to_string(),
        )
    } else {
        let short = money_desc(amount).to_string();
        let descr = cap(&format!("{} is lying here.", money_desc(amount)));
        ("coins gold".to_string(), short, descr)
    };
    let mut obj = crate::object::Object::new(NOTHING, name, short);
    obj.description = descr;
    obj.obj_type = ObjectType::Money;
    obj.wear_flags = WearFlags::TAKE;
    obj.weight = 1;
    obj.values[0] = amount;
    obj.cost = amount;
    Some(g.create_obj(obj))
}

// ---------------------------------------------------------------------------
// weight_change_object / drinkcon name helpers (act.item.c).
// ---------------------------------------------------------------------------

/// weight_change_object(): adjust an object's weight while keeping carry
/// totals in sync if it is in a character's inventory.
fn weight_change_object(g: &mut GameState, oid: ObjId, weight: i32) {
    let loc = match g.get_obj(oid) {
        Some(o) => o.loc,
        None => return,
    };
    match loc {
        ObjLoc::Carried(cid) => {
            // obj_from_char; +weight; obj_to_char (re-syncs carry_weight).
            g.obj_from_anywhere(oid);
            if let Some(o) = g.get_obj_mut(oid) {
                o.weight += weight;
            }
            g.obj_to_char(oid, cid);
        }
        _ => {
            if let Some(o) = g.get_obj_mut(oid) {
                o.weight += weight;
            }
        }
    }
}

/// name_from_drinkcon(): strip the leading "<liquid> " keyword from a drink
/// container's name once it is emptied.
fn name_from_drinkcon(g: &mut GameState, oid: ObjId) {
    if let Some(o) = g.get_obj_mut(oid) {
        if let Some(idx) = o.name.find(' ') {
            o.name = o.name[idx + 1..].to_string();
        }
    }
}

/// name_to_drinkcon(): prepend the liquid's one-word alias to a container's
/// keyword name (drinknames[type]).
fn name_to_drinkcon(g: &mut GameState, oid: ObjId, liquid: i32) {
    let alias = crate::constants::DRINKNAMES
        .get(liquid as usize)
        .copied()
        .unwrap_or("water");
    if let Some(o) = g.get_obj_mut(oid) {
        o.name = format!("{} {}", alias, o.name);
    }
}

// ===========================================================================
// PUT
// ===========================================================================

fn perform_put(g: &mut GameState, ch: CharId, obj: ObjId, cont: ObjId) {
    // C act.item.c:39: perform_put vetoes on drop_otrigger first (#139).
    if !crate::dg_triggers::drop_otrigger(g, obj, ch) {
        return;
    }
    if obj_weight(g, cont) + obj_weight(g, obj) > obj_val(g, cont, 0) {
        act(
            g,
            "$p won't fit in $P.",
            false,
            ch,
            Some(obj),
            ActArg::Obj(cont),
            To::Char,
        );
    } else {
        g.obj_from_anywhere(obj);
        g.obj_to_obj(obj, cont);
        act(
            g,
            "You put $p in $P.",
            false,
            ch,
            Some(obj),
            ActArg::Obj(cont),
            To::Char,
        );
        act(
            g,
            "$n puts $p in $P.",
            true,
            ch,
            Some(obj),
            ActArg::Obj(cont),
            To::Room,
        );
    }
}

pub fn do_put(g: &mut GameState, ch: CharId, argument: &str, _subcmd: i32) {
    let (arg1, arg2) = two_arguments(argument);
    let (obj_dotmode, obj_name) = find_all_dots(&arg1);
    let (cont_dotmode, _) = find_all_dots(&arg2);

    if arg1.is_empty() {
        g.send_to_char(ch, "Put what in what?\r\n");
        return;
    }
    if cont_dotmode != FIND_INDIV {
        g.send_to_char(
            ch,
            "You can only put things into one container at a time.\r\n",
        );
        return;
    }
    if arg2.is_empty() {
        let it = if obj_dotmode == FIND_INDIV {
            "it"
        } else {
            "them"
        };
        g.send_to_char(ch, &format!("What do you want to put {} in?\r\n", it));
        return;
    }

    // generic_find(arg2, FIND_OBJ_INV | FIND_OBJ_ROOM): inventory then room.
    let cont = find_obj_inv_room(g, ch, &arg2);
    let cont = match cont {
        Some(c) => c,
        None => {
            g.send_to_char(
                ch,
                &format!("You don't see {} {} here.\r\n", an(&arg2), arg2),
            );
            return;
        }
    };
    if obj_type(g, cont) != Some(ObjectType::Container) {
        act(
            g,
            "$p is not a container.",
            false,
            ch,
            Some(cont),
            ActArg::None,
            To::Char,
        );
        return;
    }
    if obj_val(g, cont, 1) & CONT_CLOSED != 0 {
        g.send_to_char(ch, "You'd better open it first!\r\n");
        return;
    }

    if obj_dotmode == FIND_INDIV {
        let inv = g
            .get_char(ch)
            .map(|c| c.carrying.clone())
            .unwrap_or_default();
        match g.get_obj_in_list_vis(ch, &arg1, &inv) {
            None => {
                g.send_to_char(
                    ch,
                    &format!("You aren't carrying {} {}.\r\n", an(&arg1), arg1),
                );
            }
            Some(obj) if obj == cont => {
                g.send_to_char(ch, "You attempt to fold it into itself, but fail.\r\n");
            }
            Some(obj) => perform_put(g, ch, obj, cont),
        }
    } else {
        let mut found = false;
        let inv = g
            .get_char(ch)
            .map(|c| c.carrying.clone())
            .unwrap_or_default();
        for obj in inv {
            if obj != cont
                && can_see_obj(g, ch, obj)
                && (obj_dotmode == FIND_ALL || obj_isname(g, &obj_name, obj))
            {
                found = true;
                perform_put(g, ch, obj, cont);
            }
        }
        if !found {
            if obj_dotmode == FIND_ALL {
                g.send_to_char(ch, "You don't seem to have anything to put in it.\r\n");
            } else {
                g.send_to_char(
                    ch,
                    &format!("You don't seem to have any {}s.\r\n", obj_name),
                );
            }
        }
    }
}

// ===========================================================================
// GET
// ===========================================================================

/// can_take_obj() (act.item.c).
fn can_take_obj(g: &mut GameState, ch: CharId, obj: ObjId) -> bool {
    if !is_immort(g, ch) {
        if is_carrying_n(g, ch) >= can_carry_n(g, ch) {
            act(
                g,
                "$p: you can't carry that many items.",
                false,
                ch,
                Some(obj),
                ActArg::None,
                To::Char,
            );
            return false;
        } else if is_carrying_w(g, ch) + obj_weight(g, obj) > can_carry_w(g, ch) {
            act(
                g,
                "$p: you can't carry that much weight.",
                false,
                ch,
                Some(obj),
                ActArg::None,
                To::Char,
            );
            return false;
        } else if !obj_wear(g, obj, ITEM_WEAR_TAKE) {
            act(
                g,
                "$p: you can't take that!",
                false,
                ch,
                Some(obj),
                ActArg::None,
                To::Char,
            );
            return false;
        }
    }
    true
}

/// get_check_money() (act.item.c): money items dissolve into gold.
fn get_check_money(g: &mut GameState, ch: CharId, obj: ObjId) {
    if obj_type(g, obj) == Some(ObjectType::Money) && obj_val(g, obj, 0) > 0 {
        let amount = obj_val(g, obj, 0);
        let short = g
            .get_obj(obj)
            .map(|o| o.short_description.clone())
            .unwrap_or_default();
        if !g.extract_obj(obj) {
            return;
        }
        let mbuilding = g
            .get_char(ch)
            .map(|c| c.prf2_flags & PRF2_MBUILDING != 0)
            .unwrap_or(false);
        let mut msg = String::new();
        if !mbuilding {
            if amount > 1 {
                msg = format!("There were {} coins.\r\n", amount);
            }
            if let Some(c) = g.get_char_mut(ch) {
                crate::gold::credit(c, crate::gold::Account::Carried, i64::from(amount));
            }
        } else {
            msg = format!("{} disintegrates in your hands.\r\n", short);
        }
        g.send_to_char(ch, &msg);
    }
}

/// Pandora's Box spec proc (boxkill). Never fires for normal items (vnum -100
/// is not loaded), but kept faithful to the C.
fn boxkill(g: &mut GameState, ch: CharId, obj: ObjId) {
    if is_immort(g, ch) {
        return;
    }
    g.send_to_char(
        ch,
        "You shriek as the box suddenly wraps around your hand!\r\n\
         The box folds along your arm and over your head, encasing your whole body!\r\n\
         It begins to condense, and the last sound you hear is the swift snapping of your spine...",
    );
    act(
        g,
        "$p wraps around $n's body, encasing it!",
        true,
        ch,
        Some(obj),
        ActArg::None,
        To::Room,
    );
    act(
        g,
        "$p condenses!",
        true,
        ch,
        Some(obj),
        ActArg::None,
        To::Room,
    );
    act(
        g,
        "$p coldly flips back on to the floor into the center of the room, and vanishes!",
        true,
        ch,
        Some(obj),
        ActArg::None,
        To::Room,
    );
    if let Some(c) = g.get_char_mut(ch) {
        c.fighting = None;
        c.affected.clear();
    }
    g.affect_total(ch);
    // C act.item.c:207-214: death cry, corpse, and the immortal mudlog
    // accompany the box death; the port skipped all three (#132). Dead code
    // in practice (Pandora's Box never loads) - ported for fidelity.
    crate::combat::death_cry(g, ch);
    g.extract_obj(obj);
    let (rname, is_npc) = match g.get_char(ch) {
        Some(c) => (
            c.in_room
                .map(|r| g.room(r).name.clone())
                .unwrap_or_default(),
            c.is_npc,
        ),
        None => (String::new(), false),
    };
    crate::combat::make_corpse_for_victim(g, ch);
    let name = g
        .get_char(ch)
        .map(|c| c.get_name().to_string())
        .unwrap_or_else(|| "someone".into());
    g.extract_char(ch);
    if !is_npc {
        crate::syslog::mudlog(
            g,
            &format!("{} killed by Pandora's Box ({}) at {}", name, -100, rname),
            crate::syslog::BRF,
            LVL_IMMORT,
        );
    }
}

fn perform_get_from_container(g: &mut GameState, ch: CharId, obj: ObjId, cont: ObjId, mode: i32) {
    // C act.item.c:223: containers veto via get_otrigger too (#140).
    if mode == FIND_OBJ_INV
        || (can_take_obj(g, ch, obj) && crate::dg_triggers::get_otrigger(g, obj, ch))
    {
        if is_carrying_n(g, ch) >= can_carry_n(g, ch) {
            act(
                g,
                "$p: you can't hold any more items.",
                false,
                ch,
                Some(obj),
                ActArg::None,
                To::Char,
            );
        } else {
            g.obj_from_anywhere(obj);
            g.obj_to_char(obj, ch);
            act(
                g,
                "You get $p from $P.",
                false,
                ch,
                Some(obj),
                ActArg::Obj(cont),
                To::Char,
            );
            act(
                g,
                "$n gets $p from $P.",
                true,
                ch,
                Some(obj),
                ActArg::Obj(cont),
                To::Room,
            );
            get_check_money(g, ch, obj);
            if obj_vnum(g, obj) == PANDORAS_BOX_VNUM {
                boxkill(g, ch, obj);
            }
        }
    }
}

fn get_from_container(g: &mut GameState, ch: CharId, cont: ObjId, arg: &str, mode: i32) {
    let (obj_dotmode, name) = find_all_dots(arg);

    if obj_val(g, cont, 1) & CONT_CLOSED != 0 {
        act(
            g,
            "$p is closed.",
            false,
            ch,
            Some(cont),
            ActArg::None,
            To::Char,
        );
    } else if obj_dotmode == FIND_INDIV {
        let contains = g
            .get_obj(cont)
            .map(|o| o.contains.clone())
            .unwrap_or_default();
        match g.get_obj_in_list_vis(ch, arg, &contains) {
            None => {
                let msg = format!("There doesn't seem to be {} {} in $p.", an(arg), arg);
                act(g, &msg, false, ch, Some(cont), ActArg::None, To::Char);
            }
            Some(obj) => perform_get_from_container(g, ch, obj, cont, mode),
        }
    } else {
        if obj_dotmode == FIND_ALLDOT && name.is_empty() {
            g.send_to_char(ch, "Get all of what?\r\n");
            return;
        }
        let mut found = false;
        let contains = g
            .get_obj(cont)
            .map(|o| o.contains.clone())
            .unwrap_or_default();
        for obj in contains {
            if can_see_obj(g, ch, obj) && (obj_dotmode == FIND_ALL || obj_isname(g, &name, obj)) {
                found = true;
                perform_get_from_container(g, ch, obj, cont, mode);
            }
        }
        if !found {
            if obj_dotmode == FIND_ALL {
                act(
                    g,
                    "$p seems to be empty.",
                    false,
                    ch,
                    Some(cont),
                    ActArg::None,
                    To::Char,
                );
            } else {
                let msg = format!("You can't seem to find any {}s in $p.", name);
                act(g, &msg, false, ch, Some(cont), ActArg::None, To::Char);
            }
        }
    }
}

fn perform_get_from_room(g: &mut GameState, ch: CharId, obj: ObjId) -> bool {
    // C act.item.c:302: `if (can_take_obj(ch, obj) && get_otrigger(obj, ch))`
    // - an OTRIG_GET script can veto the pickup (issue #140/#C3).
    if can_take_obj(g, ch, obj) && crate::dg_triggers::get_otrigger(g, obj, ch) {
        g.obj_from_anywhere(obj);
        g.obj_to_char(obj, ch);
        act(
            g,
            "You get $p.",
            false,
            ch,
            Some(obj),
            ActArg::None,
            To::Char,
        );
        act(
            g,
            "$n gets $p.",
            true,
            ch,
            Some(obj),
            ActArg::None,
            To::Room,
        );
        {
            let (on, ovnum, rn, rvnum) = item_room_context(g, ch, obj);
            watchdog_mudlog(
                g,
                ch,
                format!(
                    "[WATCHDOG] {} gets {} ({}) in {} ({})",
                    name_of(g, ch),
                    on,
                    ovnum,
                    rn,
                    rvnum
                ),
            );
        }
        get_check_money(g, ch, obj);
        if obj_vnum(g, obj) == PANDORAS_BOX_VNUM {
            boxkill(g, ch, obj);
        }
        return true;
    }
    false
}

fn get_from_room(g: &mut GameState, ch: CharId, arg: &str) {
    let (dotmode, name) = find_all_dots(arg);
    let rnum = match g.get_char(ch).and_then(|c| c.in_room) {
        Some(r) => r,
        None => return,
    };

    if dotmode == FIND_INDIV {
        let contents = g.room(rnum).contents.clone();
        match g.get_obj_in_list_vis(ch, arg, &contents) {
            None => {
                g.send_to_char(ch, &format!("You don't see {} {} here.\r\n", an(arg), arg));
            }
            Some(obj) => {
                perform_get_from_room(g, ch, obj);
            }
        }
    } else {
        if dotmode == FIND_ALLDOT && name.is_empty() {
            g.send_to_char(ch, "Get all of what?\r\n");
            return;
        }
        let mut found = false;
        let contents = g.room(rnum).contents.clone();
        for obj in contents {
            if can_see_obj(g, ch, obj) && (dotmode == FIND_ALL || obj_isname(g, &name, obj)) {
                found = true;
                perform_get_from_room(g, ch, obj);
            }
        }
        if !found {
            if dotmode == FIND_ALL {
                g.send_to_char(ch, "There doesn't seem to be anything here.\r\n");
            } else {
                g.send_to_char(ch, &format!("You don't see any {}s here.\r\n", name));
            }
        }
    }
}

pub fn do_get(g: &mut GameState, ch: CharId, argument: &str, _subcmd: i32) {
    let (arg1, arg2) = two_arguments(argument);

    if is_carrying_n(g, ch) >= can_carry_n(g, ch) {
        g.send_to_char(ch, "Your arms are already full!\r\n");
    } else if arg1.is_empty() {
        g.send_to_char(ch, "Get what?\r\n");
    } else if arg2.is_empty() {
        get_from_room(g, ch, &arg1);
    } else {
        let (cont_dotmode, cont_name) = find_all_dots(&arg2);
        if cont_dotmode == FIND_INDIV {
            // generic_find(arg2, FIND_OBJ_INV | FIND_OBJ_ROOM): record mode.
            let (cont, mode) = find_obj_inv_room_mode(g, ch, &arg2);
            match cont {
                None => {
                    g.send_to_char(ch, &format!("You don't have {} {}.\r\n", an(&arg2), arg2));
                }
                Some(cont) if obj_type(g, cont) != Some(ObjectType::Container) => {
                    act(
                        g,
                        "$p is not a container.",
                        false,
                        ch,
                        Some(cont),
                        ActArg::None,
                        To::Char,
                    );
                }
                Some(cont) => get_from_container(g, ch, cont, &arg1, mode),
            }
        } else {
            if cont_dotmode == FIND_ALLDOT && arg2.is_empty() {
                g.send_to_char(ch, "Get from all of what?\r\n");
                return;
            }
            let mut found = false;
            let inv = g
                .get_char(ch)
                .map(|c| c.carrying.clone())
                .unwrap_or_default();
            for cont in inv {
                if can_see_obj(g, ch, cont)
                    && (cont_dotmode == FIND_ALL || obj_isname(g, &cont_name, cont))
                {
                    if obj_type(g, cont) == Some(ObjectType::Container) {
                        found = true;
                        get_from_container(g, ch, cont, &arg1, FIND_OBJ_INV);
                    } else if cont_dotmode == FIND_ALLDOT {
                        found = true;
                        act(
                            g,
                            "$p is not a container.",
                            false,
                            ch,
                            Some(cont),
                            ActArg::None,
                            To::Char,
                        );
                    }
                }
            }
            let rnum = g.get_char(ch).and_then(|c| c.in_room);
            if let Some(rnum) = rnum {
                let contents = g.room(rnum).contents.clone();
                for cont in contents {
                    if can_see_obj(g, ch, cont)
                        && (cont_dotmode == FIND_ALL || obj_isname(g, &cont_name, cont))
                    {
                        if obj_type(g, cont) == Some(ObjectType::Container) {
                            get_from_container(g, ch, cont, &arg1, FIND_OBJ_ROOM);
                            found = true;
                        } else if cont_dotmode == FIND_ALLDOT {
                            act(
                                g,
                                "$p is not a container.",
                                false,
                                ch,
                                Some(cont),
                                ActArg::None,
                                To::Char,
                            );
                            found = true;
                        }
                    }
                }
            }
            if !found {
                if cont_dotmode == FIND_ALL {
                    g.send_to_char(ch, "You can't seem to find any containers.\r\n");
                } else {
                    g.send_to_char(
                        ch,
                        &format!("You can't seem to find any {}s here.\r\n", cont_name),
                    );
                }
            }
        }
    }
}

// ===========================================================================
// DROP / JUNK / DONATE
// ===========================================================================

fn in_jail(g: &GameState, ch: CharId) -> bool {
    let in_room = g.get_char(ch).and_then(|c| c.in_room);
    let jail = g.real_room(g.config.jail_num);
    in_room.is_some() && in_room == jail
}

fn is_killer(g: &GameState, ch: CharId) -> bool {
    g.get_char(ch)
        .map(|c| c.act_flags & PLR_KILLER != 0)
        .unwrap_or(false)
}

/// Immortal item-flow audit trail (C act.item.c WATCHDOG mudlogs, #131):
/// `mudlog(buf, CMP, LVL_IMPL, TRUE)` whenever an immortal manipulates
/// items/gold.
fn watchdog_mudlog(g: &mut GameState, ch: CharId, what: String) {
    let staff_or_invalid_player = match g.principal_authority(ch) {
        Some(authority) if authority.principal_is_player => {
            g.get_char(authority.principal).is_none_or(|principal| {
                g.authority_quarantine.contains(&principal.idnum)
                    || authority.authority >= i32::from(LVL_IMMORT)
            })
        }
        Some(_) => false,
        None => g.get_char(ch).is_some_and(|character| !character.is_npc),
    };
    if staff_or_invalid_player {
        crate::syslog::mudlog(g, &what, crate::syslog::CMP, LVL_IMPL);
    }
}

fn perform_drop_gold(g: &mut GameState, ch: CharId, amount: i32, mode: i32, rdr: Option<RoomRnum>) {
    if !g.pk_allowed && is_killer(g, ch) && in_jail(g, ch) {
        g.send_to_char(ch, "Sorry. You can't do that when you're in jail.\r\n");
        return;
    }
    // C act.item.c:488-493: drop_wtrigger may veto a gold drop; the veto
    // extracts the minted money object and stops (#139).
    if amount > 0 && mode == SCMD_DROP {
        let money = crate::combat::create_money(g, amount);
        if !crate::dg_triggers::drop_wtrigger(g, money, ch) {
            g.extract_obj(money);
            return;
        }
        g.extract_obj(money);
    }

    let gold = g.get_char(ch).map(|c| c.points.gold).unwrap_or(0);
    if amount <= 0 {
        g.send_to_char(ch, "Heh heh heh.. we are jolly funny today, eh?\r\n");
    } else if gold < amount {
        g.send_to_char(ch, "You don't have that many coins!\r\n");
    } else {
        if mode != SCMD_JUNK {
            g.set_wait_state(ch, PULSE_VIOLENCE as i32); // to prevent coin-bombing
            let obj = match create_money(g, amount) {
                Some(o) => o,
                None => return,
            };
            if mode == SCMD_DONATE {
                g.send_to_char(
                    ch,
                    "You throw some gold into the air..\r\nIt disappears in a puff of smoke!\r\n",
                );
                act(
                    g,
                    "$n throws some gold into the air..\r\nIt disappears in a puff of smoke!",
                    false,
                    ch,
                    None,
                    ActArg::None,
                    To::Room,
                );
                if let Some(rdr) = rdr {
                    g.obj_to_room(obj, rdr);
                    // act with ch=NULL in C; render to the donation room directly.
                    let line = format!(
                        "{} suddenly appears in a puff of orange smoke!\r\n",
                        cap(&g
                            .get_obj(obj)
                            .map(|o| o.short_description.clone())
                            .unwrap_or_default())
                    );
                    g.send_to_room(rdr, &line, None);
                }
            } else {
                g.send_to_char(ch, "You drop some gold.\r\n");
                let line = format!("$n drops {}.", money_desc(amount));
                act(g, &line, true, ch, None, ActArg::None, To::Room);
                let rnum = g.get_char(ch).and_then(|c| c.in_room);
                if let Some(rnum) = rnum {
                    g.obj_to_room(obj, rnum);
                }
            }
        } else {
            let line = format!(
                "$n drops {} which disappears in a puff of smoke!",
                money_desc(amount)
            );
            act(g, &line, false, ch, None, ActArg::None, To::Room);
            g.send_to_char(
                ch,
                "You drop some gold which disappears in a puff of smoke!\r\n",
            );
        }
        let (rn, rvnum) = match g.get_char(ch).and_then(|c| c.in_room) {
            Some(r) => {
                let room = g.room(r);
                (room.name.clone(), room.number)
            }
            None => ("Nowhere".into(), -1),
        };
        watchdog_mudlog(
            g,
            ch,
            format!(
                "[WATCHDOG] {} drops {} gold coins in {} ({}).",
                name_of(g, ch),
                amount,
                rn,
                rvnum
            ),
        );
        if let Some(c) = g.get_char_mut(ch) {
            crate::gold::debit(c, crate::gold::Account::Carried, i64::from(amount));
        }
    }
}

fn vanish(mode: i32) -> &'static str {
    if mode == SCMD_DONATE || mode == SCMD_JUNK {
        "  It vanishes in a puff of smoke!"
    } else {
        ""
    }
}

fn perform_drop(
    g: &mut GameState,
    ch: CharId,
    obj: ObjId,
    mut mode: i32,
    sname: &str,
    rdr: Option<RoomRnum>,
) -> i32 {
    if !g.pk_allowed && is_killer(g, ch) && in_jail(g, ch) {
        g.send_to_char(ch, "Sorry. You can't do that when you're in jail.\r\n");
        return 0;
    }

    // C act.item.c:534-536: drop_otrigger vetoes any drop/put-away; for a
    // real SCMD_DROP, drop_wtrigger (a world drop trigger) vetoes too (#139).
    if !crate::dg_triggers::drop_otrigger(g, obj, ch) {
        return 0;
    }
    if mode == SCMD_DROP && !crate::dg_triggers::drop_wtrigger(g, obj, ch) {
        return 0;
    }
    if obj_stat(g, obj, ITEM_NODROP) {
        let line = format!("You can't {} $p, it must be CURSED!", sname);
        act(g, &line, false, ch, Some(obj), ActArg::None, To::Char);
        return 0;
    }
    let line = format!("You {} $p.{}", sname, vanish(mode));
    act(g, &line, false, ch, Some(obj), ActArg::None, To::Char);
    let line = format!("$n {}s $p.{}", sname, vanish(mode));
    act(g, &line, true, ch, Some(obj), ActArg::None, To::Room);
    g.obj_from_anywhere(obj);
    {
        let (on, ovnum) = item_names(g, obj);
        let (rn, rvnum) = match g.get_char(ch).and_then(|c| c.in_room) {
            Some(r) => {
                let room = g.room(r);
                (room.name.clone(), room.number)
            }
            None => ("Nowhere".into(), -1),
        };
        watchdog_mudlog(
            g,
            ch,
            format!(
                "[WATCHDOG] {} drops {} ({}) in {} ({})",
                name_of(g, ch),
                on,
                ovnum,
                rn,
                rvnum
            ),
        );
    }

    if mode == SCMD_DONATE && obj_stat(g, obj, ITEM_NODONATE) {
        mode = SCMD_JUNK;
    }

    match mode {
        SCMD_DROP => {
            let rnum = g.get_char(ch).and_then(|c| c.in_room);
            if let Some(rnum) = rnum {
                g.obj_to_room(obj, rnum);
            }
            0
        }
        SCMD_DONATE => {
            if let Some(rdr) = rdr {
                g.obj_to_room(obj, rdr);
                let line = format!(
                    "{} suddenly appears in a puff a smoke!\r\n",
                    cap(&g
                        .get_obj(obj)
                        .map(|o| o.short_description.clone())
                        .unwrap_or_default())
                );
                g.send_to_room(rdr, &line, None);
            }
            0
        }
        SCMD_JUNK => {
            let cost = g.get_obj(obj).map(|o| o.cost).unwrap_or(0);
            let value = (cost >> 4).clamp(1, 200);
            g.extract_obj(obj);
            value
        }
        _ => 0,
    }
}

pub fn do_drop(g: &mut GameState, ch: CharId, argument: &str, subcmd: i32) {
    let mut rdr: Option<RoomRnum> = None;
    let mut mode = SCMD_DROP;
    let sname: &str;

    match subcmd {
        SCMD_JUNK => {
            sname = "junk";
            mode = SCMD_JUNK;
        }
        SCMD_DONATE => {
            sname = "donate";
            mode = SCMD_DONATE;
            match g.rng.number(0, 2) {
                0 => mode = SCMD_JUNK,
                // C act.item.c ships donation_room_2/3 as NOWHERE "for
                // expansion" with the selection commented out; the towns
                // now exist, so the roll routes across all three (the
                // junk 1-in-3 chance preserved).
                1 => rdr = g.real_room(crate::config::DONATION_ROOM_2),
                2 => rdr = g.real_room(crate::config::DONATION_ROOM_3),
                _ => rdr = g.real_room(DONATION_ROOM_1),
            }
            if mode != SCMD_JUNK && rdr.is_none() {
                g.send_to_char(ch, "Sorry, you can't donate anything right now.\r\n");
                return;
            }
        }
        _ => {
            sname = "drop";
        }
    }

    // C: argument = one_argument(argument, arg) — `rest` is the remainder.
    let (arg, rest) = crate::interpreter::one_argument(argument);
    let mut amount = 0;

    if arg.is_empty() {
        g.send_to_char(ch, &format!("What do you want to {}?\r\n", sname));
        return;
    }

    // Water-sector safety prompt for plain drop (C tests the remainder).
    if subcmd == SCMD_DROP {
        let sect = g
            .get_char(ch)
            .and_then(|c| c.in_room)
            .map(|r| g.room(r).sector_type);
        if (sect == Some(SectorType::WaterSwim) || sect == Some(SectorType::WaterNoSwim))
            && !rest.contains("water")
        {
            g.send_to_char(
                ch,
                "You must type 'water' after the object name if you really want to drop it.\r\n",
            );
            return;
        }
    }

    if is_number(&arg) {
        amount = match crate::text::parse_i32_strict(&arg) {
            Ok(value) => value,
            Err(crate::text::ParseIntError::Overflow) => {
                g.send_to_char(ch, "That amount is out of range.\r\n");
                return;
            }
            Err(_) => return,
        };
        // Second token after the amount.
        let (rest_first, _) = crate::interpreter::one_argument(rest);
        if rest_first == "coins" || rest_first == "coin" {
            let house_crash = g
                .get_char(ch)
                .and_then(|c| c.in_room)
                .map(|r| g.room(r).room_flags.bits() & ROOM_HOUSE_CRASH != 0)
                .unwrap_or(false);
            if house_crash {
                g.send_to_char(
                    ch,
                    "I'd suggest you put those coins in the bank, not under your mattress.\r\n",
                );
                return;
            }
            perform_drop_gold(g, ch, amount, mode, rdr);
        } else {
            g.send_to_char(
                ch,
                "Sorry, you can't do that to more than one item at a time.\r\n",
            );
        }
        return;
    }

    let (dotmode, name) = find_all_dots(&arg);

    // Can't junk or donate all.
    if dotmode == FIND_ALL && (subcmd == SCMD_JUNK || subcmd == SCMD_DONATE) {
        if subcmd == SCMD_JUNK {
            g.send_to_char(ch, "You can't junk everything at the same time!\r\n");
        } else {
            g.send_to_char(ch, "You can't donate everything at the same time!\r\n");
        }
        return;
    }

    if dotmode == FIND_ALL {
        let inv = g
            .get_char(ch)
            .map(|c| c.carrying.clone())
            .unwrap_or_default();
        if inv.is_empty() {
            g.send_to_char(ch, "You don't seem to be carrying anything.\r\n");
        } else {
            for obj in inv {
                amount = amount.saturating_add(perform_drop(g, ch, obj, mode, sname, rdr));
            }
        }
    } else if dotmode == FIND_ALLDOT {
        if name.is_empty() {
            g.send_to_char(ch, &format!("What do you want to {} all of?\r\n", sname));
            return;
        }
        let inv = g
            .get_char(ch)
            .map(|c| c.carrying.clone())
            .unwrap_or_default();
        let first = g.get_obj_in_list_vis(ch, &name, &inv);
        if first.is_none() {
            g.send_to_char(ch, &format!("You don't seem to have any {}s.\r\n", name));
        }
        // Iterate every matching item in inventory.
        let inv = g
            .get_char(ch)
            .map(|c| c.carrying.clone())
            .unwrap_or_default();
        for obj in inv {
            if obj_isname(g, &name, obj) {
                amount = amount.saturating_add(perform_drop(g, ch, obj, mode, sname, rdr));
            }
        }
    } else {
        let inv = g
            .get_char(ch)
            .map(|c| c.carrying.clone())
            .unwrap_or_default();
        match g.get_obj_in_list_vis(ch, &arg, &inv) {
            None => {
                g.send_to_char(
                    ch,
                    &format!("You don't seem to have {} {}.\r\n", an(&arg), arg),
                );
            }
            Some(obj) => amount = amount.saturating_add(perform_drop(g, ch, obj, mode, sname, rdr)),
        }
    }

    if amount != 0 && subcmd == SCMD_JUNK && !is_immort(g, ch) {
        g.send_to_char(ch, "You have been rewarded by the gods!\r\n");
        act(
            g,
            "$n has been rewarded by the gods!",
            true,
            ch,
            None,
            ActArg::None,
            To::Room,
        );
        if let Some(c) = g.get_char_mut(ch) {
            crate::gold::credit(c, crate::gold::Account::Carried, i64::from(amount));
        }
    }
}

// ===========================================================================
// GIVE
// ===========================================================================

fn perform_give(g: &mut GameState, ch: CharId, vict: CharId, obj: ObjId) {
    if !is_immort(g, ch) {
        if obj_stat(g, obj, ITEM_NODROP) {
            act(
                g,
                "You can't let go of $p!!  Yeech!",
                false,
                ch,
                Some(obj),
                ActArg::None,
                To::Char,
            );
            return;
        }
        if is_carrying_n(g, vict) >= can_carry_n(g, vict) {
            act(
                g,
                "$N seems to have $S hands full.",
                false,
                ch,
                None,
                ActArg::Char(vict),
                To::Char,
            );
            return;
        }
        // C act.item.c:753: give_otrigger / receive_mtrigger gate the gift
        // before the transfer (#141).
        if !crate::dg_triggers::give_otrigger(g, obj, ch, vict)
            || !crate::dg_triggers::receive_mtrigger(g, vict, ch, obj)
        {
            return;
        }
        if obj_weight(g, obj) + is_carrying_w(g, vict) > can_carry_w(g, vict) {
            act(
                g,
                "$E can't carry that much weight.",
                false,
                ch,
                None,
                ActArg::Char(vict),
                To::Char,
            );
            return;
        }
    }

    g.obj_from_anywhere(obj);
    g.obj_to_char(obj, vict);
    act(
        g,
        "You give $p to $N.",
        false,
        ch,
        Some(obj),
        ActArg::Char(vict),
        To::Char,
    );
    act(
        g,
        "$n gives you $p.",
        false,
        ch,
        Some(obj),
        ActArg::Char(vict),
        To::Vict,
    );
    act(
        g,
        "$n gives $p to $N.",
        true,
        ch,
        Some(obj),
        ActArg::Char(vict),
        To::NotVict,
    );
    let (on, ovnum) = item_names(g, obj);
    watchdog_mudlog(
        g,
        ch,
        format!(
            "[WATCHDOG] {} gives {} ({}) to {}.",
            name_of(g, ch),
            on,
            ovnum,
            name_of(g, vict)
        ),
    );

    // DELIVER quests (Deltania Breathes): handing the sealed courier pouch to
    // its named recipient completes the delivery leg.
    crate::quest::quest_deliver_give(g, ch, obj, vict);
}

fn item_names(g: &GameState, obj: ObjId) -> (String, i32) {
    g.get_obj(obj)
        .map(|o| (o.short_description.clone(), o.item_number))
        .unwrap_or_default()
}

fn name_of(g: &GameState, cid: CharId) -> String {
    g.get_char(cid)
        .map(|c| c.get_name().to_string())
        .unwrap_or_else(|| "someone".into())
}

fn item_room_context(g: &GameState, ch: CharId, obj: ObjId) -> (String, i32, String, i32) {
    let (on, ovnum) = item_names(g, obj);
    match g.get_char(ch).and_then(|c| c.in_room) {
        Some(r) => {
            let room = g.room(r);
            (on, ovnum, room.name.clone(), room.number)
        }
        None => (on, ovnum, "Nowhere".into(), -1),
    }
}

/// give_find_vict() (act.item.c).
fn give_find_vict(g: &mut GameState, ch: CharId, arg: &str) -> Option<CharId> {
    if arg.is_empty() {
        g.send_to_char(ch, "To who?\r\n");
        return None;
    }
    match g.get_char_room_vis(ch, arg) {
        None => {
            g.send_to_char(ch, "&CNo-one by that name here.&n\r\n");
            None
        }
        Some(v) if v == ch => {
            g.send_to_char(ch, "What's the point of that?\r\n");
            None
        }
        Some(v) => Some(v),
    }
}

fn perform_give_gold(g: &mut GameState, ch: CharId, vict: CharId, amount: i32) {
    if amount <= 0 {
        g.send_to_char(ch, "Heh heh heh ... we are jolly funny today, eh?\r\n");
        return;
    }
    let gold = g.get_char(ch).map(|c| c.points.gold).unwrap_or(0);
    let is_npc = g.get_char(ch).map(|c| c.is_npc).unwrap_or(false);
    let may_mint = !is_npc
        && crate::interpreter::authenticated_input_authority(g, ch)
            .is_some_and(|authority| authority.authority >= i32::from(LVL_GOD));
    if gold < amount && !may_mint {
        g.send_to_char(ch, "You don't have that many coins!\r\n");
        return;
    }
    let debited = !may_mint;
    let moved = if debited {
        crate::gold::transfer_between(
            g,
            ch,
            crate::gold::Account::Carried,
            vict,
            crate::gold::Account::Carried,
            i64::from(amount),
        )
    } else {
        g.get_char_mut(vict)
            .map(|v| {
                let amount = i64::from(amount);
                if crate::gold::balance(v, crate::gold::Account::Carried).saturating_add(amount)
                    > crate::gold::GOLD_CAP
                {
                    false
                } else {
                    crate::gold::credit(v, crate::gold::Account::Carried, amount) == amount
                }
            })
            .unwrap_or(false)
    };
    if !moved {
        g.send_to_char(
            ch,
            "That transfer would exceed the recipient's gold limit.\r\n",
        );
        return;
    }

    g.send_to_char(ch, "&YOkay.&n\r\n");
    let line = format!(
        "$n gives you {} gold coin{}.",
        amount,
        if amount == 1 { "" } else { "s" }
    );
    act(g, &line, false, ch, None, ActArg::Char(vict), To::Vict);
    let line = format!("$n gives {} to $N.", money_desc(amount));
    act(g, &line, true, ch, None, ActArg::Char(vict), To::NotVict);
    watchdog_mudlog(
        g,
        ch,
        if debited {
            format!(
                "[WATCHDOG] {} gives {} gold coins to {}.",
                name_of(g, ch),
                amount,
                name_of(g, vict)
            )
        } else {
            format!(
                "[WATCHDOG] {} mints {} gold coins for {}.",
                name_of(g, ch),
                amount,
                name_of(g, vict)
            )
        },
    );
    // C act.item.c:823: MTRIG_BRIBE fires after the gold changes hands (#142).
    crate::dg_triggers::bribe_mtrigger(g, vict, ch, amount);
}

pub fn do_give(g: &mut GameState, ch: CharId, argument: &str, _subcmd: i32) {
    let (arg, rest) = crate::interpreter::one_argument(argument);

    if arg.is_empty() {
        g.send_to_char(ch, "Give what to who?\r\n");
    } else if is_number(&arg) {
        let amount = match crate::text::parse_i32_strict(&arg) {
            Ok(value) => value,
            Err(crate::text::ParseIntError::Overflow) => {
                g.send_to_char(ch, "That amount is out of range.\r\n");
                return;
            }
            Err(_) => return,
        };
        let (kw, rest2) = crate::interpreter::one_argument(rest);
        if kw == "coins" || kw == "coin" {
            let (who, _) = crate::interpreter::one_argument(rest2);
            if let Some(vict) = give_find_vict(g, ch, &who) {
                perform_give_gold(g, ch, vict, amount);
            }
        } else {
            g.send_to_char(ch, "You can't give more than one item at a time.\r\n");
        }
    } else {
        // The victim is the SECOND token (one_argument(argument, buf1)) which
        // is the word after the item keyword.
        let (who, _) = crate::interpreter::one_argument(rest);
        let vict = match give_find_vict(g, ch, &who) {
            Some(v) => v,
            None => return,
        };
        let (dotmode, name) = find_all_dots(&arg);
        if dotmode == FIND_INDIV {
            let inv = g
                .get_char(ch)
                .map(|c| c.carrying.clone())
                .unwrap_or_default();
            match g.get_obj_in_list_vis(ch, &arg, &inv) {
                None => {
                    g.send_to_char(
                        ch,
                        &format!("You don't seem to have {} {}.\r\n", an(&arg), arg),
                    );
                }
                Some(obj) => perform_give(g, ch, vict, obj),
            }
        } else {
            if dotmode == FIND_ALLDOT && name.is_empty() {
                g.send_to_char(ch, "All of what?\r\n");
                return;
            }
            let inv = g
                .get_char(ch)
                .map(|c| c.carrying.clone())
                .unwrap_or_default();
            if inv.is_empty() {
                g.send_to_char(ch, "You don't seem to be holding anything.\r\n");
            } else {
                for obj in inv {
                    if can_see_obj(g, ch, obj) && (dotmode == FIND_ALL || obj_isname(g, &name, obj))
                    {
                        perform_give(g, ch, vict, obj);
                    }
                }
            }
        }
    }
}

// ===========================================================================
// DRINK / EAT / SIP / TASTE / POUR / FILL
// ===========================================================================

/// GET_COND(ch, idx).
fn get_cond(g: &GameState, ch: CharId, idx: usize) -> i32 {
    g.get_char(ch)
        .map(|c| c.conditions[idx] as i32)
        .unwrap_or(0)
}

/// gain_condition(ch, condition, value) (limits.c).
fn gain_condition(g: &mut GameState, ch: CharId, condition: usize, value: i32) {
    let cur = match g.get_char(ch) {
        Some(c) => c.conditions[condition] as i32,
        None => return,
    };
    if cur == -100 {
        return;
    }
    let intoxicated = get_cond(g, ch, DRUNK) > 4;
    let mut newv = cur + value;
    if condition == DRUNK {
        newv = newv.max(0);
    } else {
        newv = newv.max(-72);
    }
    newv = newv.min(24);
    if let Some(c) = g.get_char_mut(ch) {
        c.conditions[condition] = newv as i8;
    }
    if newv != 0 {
        return;
    }
    match condition {
        FULL => g.send_to_char(ch, "You are hungry.\r\n"),
        THIRST => g.send_to_char(ch, "You are thirsty.\r\n"),
        DRUNK => {
            if intoxicated {
                g.send_to_char(ch, "You are now sober.\r\n");
            }
        }
        _ => {}
    }
}

/// affect_join for the poison applied by tainted food/drink. We have no
/// affect_join in the contract, so append a poison Affect and recompute.
fn apply_poison(g: &mut GameState, ch: CharId, duration: i32) {
    let af = Affect {
        spell_type: SPELL_POISON,
        duration,
        modifier: 0,
        location: APPLY_NONE,
        bitvector: AFF_POISON,
        caster: None,
    };
    if let Some(c) = g.get_char_mut(ch) {
        c.affected.push(af);
    }
    g.affect_total(ch);
}

fn drink_name(idx: i32) -> &'static str {
    crate::constants::DRINKS
        .get(idx as usize)
        .copied()
        .unwrap_or("water")
}
fn drink_aff(idx: i32, cond: usize) -> i32 {
    crate::constants::DRINK_AFF
        .get(idx as usize)
        .map(|row| row[cond])
        .unwrap_or(0)
}

pub fn do_drink(g: &mut GameState, ch: CharId, argument: &str, subcmd: i32) {
    let (arg, _) = crate::interpreter::one_argument(argument);

    if arg.is_empty() {
        g.send_to_char(ch, "Drink from what?\r\n");
        return;
    }

    let mut on_ground = false;
    let inv = g
        .get_char(ch)
        .map(|c| c.carrying.clone())
        .unwrap_or_default();
    let temp = match g.get_obj_in_list_vis(ch, &arg, &inv) {
        Some(o) => o,
        None => {
            let rnum = g.get_char(ch).and_then(|c| c.in_room);
            let contents = rnum.map(|r| g.room(r).contents.clone()).unwrap_or_default();
            match g.get_obj_in_list_vis(ch, &arg, &contents) {
                Some(o) => {
                    on_ground = true;
                    o
                }
                None => {
                    act(
                        g,
                        "You can't find it!",
                        false,
                        ch,
                        None,
                        ActArg::None,
                        To::Char,
                    );
                    return;
                }
            }
        }
    };

    let ty = obj_type(g, temp);
    if ty != Some(ObjectType::LiqContainer) && ty != Some(ObjectType::Fountain) {
        g.send_to_char(ch, "You can't drink from that!\r\n");
        return;
    }
    if on_ground && ty == Some(ObjectType::LiqContainer) {
        g.send_to_char(ch, "You have to be holding that to drink from it.\r\n");
        return;
    }
    if get_cond(g, ch, DRUNK) > 14 && get_cond(g, ch, THIRST) > 0 {
        g.send_to_char(ch, "You can't seem to get close enough to your mouth.\r\n");
        act(
            g,
            "$n tries to drink but misses $s mouth!",
            true,
            ch,
            None,
            ActArg::None,
            To::Room,
        );
        return;
    }
    if get_cond(g, ch, FULL) > 20 && get_cond(g, ch, THIRST) > 0 {
        g.send_to_char(ch, "Your stomach can't contain anymore!\r\n");
        return;
    }
    if obj_val(g, temp, 1) == 0 {
        g.send_to_char(ch, "It's empty.\r\n");
        return;
    }

    let liq = obj_val(g, temp, 2);
    let mut amount;
    if subcmd == SCMD_DRINK {
        let line = format!("$n drinks {} from $p.", drink_name(liq));
        act(g, &line, true, ch, Some(temp), ActArg::None, To::Room);
        g.send_to_char(ch, &format!("You drink the {}.\r\n", drink_name(liq)));
        if drink_aff(liq, DRUNK) > 0 {
            amount = (25 - get_cond(g, ch, THIRST)) / drink_aff(liq, DRUNK);
        } else {
            amount = g.rng.number(3, 10);
        }
    } else {
        act(
            g,
            "$n sips from $p.",
            true,
            ch,
            Some(temp),
            ActArg::None,
            To::Room,
        );
        g.send_to_char(ch, &format!("It tastes like {}.\r\n", drink_name(liq)));
        amount = 1;
    }

    amount = amount.min(obj_val(g, temp, 1));
    let weight = amount.min(obj_weight(g, temp));
    weight_change_object(g, temp, -weight);

    gain_condition(g, ch, DRUNK, drink_aff(liq, DRUNK) * amount / 4);
    gain_condition(g, ch, FULL, drink_aff(liq, FULL) * amount / 4);
    gain_condition(g, ch, THIRST, drink_aff(liq, THIRST) * amount / 4);

    if get_cond(g, ch, DRUNK) > 10 {
        g.send_to_char(ch, "You feel drunk.\r\n");
    }
    if get_cond(g, ch, THIRST) > 20 {
        g.send_to_char(ch, "You don't feel thirsty any more.\r\n");
    }
    if get_cond(g, ch, FULL) > 20 {
        g.send_to_char(ch, "You are full.\r\n");
    }

    if obj_val(g, temp, 3) != 0 {
        // Poisoned.
        g.send_to_char(ch, "Oops, it tasted rather strange!\r\n");
        act(
            g,
            "$n chokes and utters some strange sounds.",
            true,
            ch,
            None,
            ActArg::None,
            To::Room,
        );
        apply_poison(g, ch, amount * 3);
    }

    // Empty the container; clear liquid/poison on the last drop.
    let remaining = obj_val(g, temp, 1) - amount;
    if let Some(o) = g.get_obj_mut(temp) {
        o.values[1] = remaining;
    }
    if remaining == 0 {
        if let Some(o) = g.get_obj_mut(temp) {
            o.values[2] = 0;
            o.values[3] = 0;
        }
        name_from_drinkcon(g, temp);
    }
}

pub fn do_eat(g: &mut GameState, ch: CharId, argument: &str, subcmd: i32) {
    let (arg, _) = crate::interpreter::one_argument(argument);

    if arg.is_empty() {
        g.send_to_char(ch, "Eat what?\r\n");
        return;
    }
    let inv = g
        .get_char(ch)
        .map(|c| c.carrying.clone())
        .unwrap_or_default();
    let food = match g.get_obj_in_list_vis(ch, &arg, &inv) {
        Some(o) => o,
        None => {
            g.send_to_char(
                ch,
                &format!("You don't seem to have {} {}.\r\n", an(&arg), arg),
            );
            return;
        }
    };

    let ty = obj_type(g, food);
    if subcmd == SCMD_TASTE
        && (ty == Some(ObjectType::LiqContainer) || ty == Some(ObjectType::Fountain))
    {
        do_drink(g, ch, argument, SCMD_SIP);
        return;
    }
    if ty != Some(ObjectType::Food) && !is_immort(g, ch) {
        g.send_to_char(ch, "You can't eat THAT!\r\n");
        return;
    }
    if get_cond(g, ch, FULL) > 20 {
        act(
            g,
            "You are too full to eat more!",
            false,
            ch,
            None,
            ActArg::None,
            To::Char,
        );
        return;
    }

    if subcmd == SCMD_EAT {
        act(
            g,
            "You eat the $o.",
            false,
            ch,
            Some(food),
            ActArg::None,
            To::Char,
        );
        act(
            g,
            "$n eats $p.",
            true,
            ch,
            Some(food),
            ActArg::None,
            To::Room,
        );
    } else {
        act(
            g,
            "You nibble a little bit of the $o.",
            false,
            ch,
            Some(food),
            ActArg::None,
            To::Char,
        );
        act(
            g,
            "$n tastes a little bit of $p.",
            true,
            ch,
            Some(food),
            ActArg::None,
            To::Room,
        );
    }

    let amount = if subcmd == SCMD_EAT {
        obj_val(g, food, 0)
    } else {
        1
    };
    gain_condition(g, ch, FULL, amount);

    if get_cond(g, ch, FULL) > 20 {
        act(g, "You are full.", false, ch, None, ActArg::None, To::Char);
    }

    if obj_val(g, food, 3) != 0 && !is_immort(g, ch) {
        g.send_to_char(ch, "Oops, that tasted rather strange!\r\n");
        act(
            g,
            "$n coughs and utters some strange sounds.",
            false,
            ch,
            None,
            ActArg::None,
            To::Room,
        );
        apply_poison(g, ch, amount * 2);
    }

    if subcmd == SCMD_EAT {
        g.extract_obj(food);
    } else {
        let left = obj_val(g, food, 0) - 1;
        if let Some(o) = g.get_obj_mut(food) {
            o.values[0] = left;
        }
        if left == 0 {
            g.send_to_char(ch, "There's nothing left now.\r\n");
            g.extract_obj(food);
        }
    }
}

pub fn do_pour(g: &mut GameState, ch: CharId, argument: &str, subcmd: i32) {
    // The unwraps below are safe because the command table only ever calls
    // this with SCMD_POUR(0) or SCMD_FILL(1); any other subcmd leaves
    // from_obj/to_obj unset. Guard the contract explicitly so a future
    // caller (DG force, wand) can't panic the Game task.
    if subcmd != SCMD_POUR && subcmd != SCMD_FILL {
        return;
    }
    let (arg1, arg2) = two_arguments(argument);
    let mut from_obj: Option<ObjId> = None;
    let mut to_obj: Option<ObjId> = None;

    if subcmd == SCMD_POUR {
        if arg1.is_empty() {
            act(
                g,
                "From what do you want to pour?",
                false,
                ch,
                None,
                ActArg::None,
                To::Char,
            );
            return;
        }
        let inv = g
            .get_char(ch)
            .map(|c| c.carrying.clone())
            .unwrap_or_default();
        match g.get_obj_in_list_vis(ch, &arg1, &inv) {
            None => {
                act(
                    g,
                    "You can't find it!",
                    false,
                    ch,
                    None,
                    ActArg::None,
                    To::Char,
                );
                return;
            }
            Some(o) => from_obj = Some(o),
        }
        if obj_type(g, from_obj.unwrap()) != Some(ObjectType::LiqContainer) {
            act(
                g,
                "You can't pour from that!",
                false,
                ch,
                None,
                ActArg::None,
                To::Char,
            );
            return;
        }
    }
    if subcmd == SCMD_FILL {
        if arg1.is_empty() {
            g.send_to_char(
                ch,
                "What do you want to fill?  And what are you filling it from?\r\n",
            );
            return;
        }
        let inv = g
            .get_char(ch)
            .map(|c| c.carrying.clone())
            .unwrap_or_default();
        match g.get_obj_in_list_vis(ch, &arg1, &inv) {
            None => {
                g.send_to_char(ch, "You can't find it!");
                return;
            }
            Some(o) => to_obj = Some(o),
        }
        if obj_type(g, to_obj.unwrap()) != Some(ObjectType::LiqContainer) {
            act(
                g,
                "You can't fill $p!",
                false,
                ch,
                to_obj,
                ActArg::None,
                To::Char,
            );
            return;
        }
        if arg2.is_empty() {
            act(
                g,
                "What do you want to fill $p from?",
                false,
                ch,
                to_obj,
                ActArg::None,
                To::Char,
            );
            return;
        }
        let rnum = g.get_char(ch).and_then(|c| c.in_room);
        let contents = rnum.map(|r| g.room(r).contents.clone()).unwrap_or_default();
        match g.get_obj_in_list_vis(ch, &arg2, &contents) {
            None => {
                g.send_to_char(
                    ch,
                    &format!("There doesn't seem to be {} {} here.\r\n", an(&arg2), arg2),
                );
                return;
            }
            Some(o) => from_obj = Some(o),
        }
        if obj_type(g, from_obj.unwrap()) != Some(ObjectType::Fountain) {
            act(
                g,
                "You can't fill something from $p.",
                false,
                ch,
                from_obj,
                ActArg::None,
                To::Char,
            );
            return;
        }
    }

    let from = from_obj.unwrap();
    if obj_val(g, from, 1) == 0 {
        act(
            g,
            "The $p is empty.",
            false,
            ch,
            Some(from),
            ActArg::None,
            To::Char,
        );
        return;
    }

    if subcmd == SCMD_POUR {
        if arg2.is_empty() {
            act(
                g,
                "Where do you want it?  Out or in what?",
                false,
                ch,
                None,
                ActArg::None,
                To::Char,
            );
            return;
        }
        if arg2 == "out" {
            act(
                g,
                "$n empties $p.",
                true,
                ch,
                Some(from),
                ActArg::None,
                To::Room,
            );
            act(
                g,
                "You empty $p.",
                false,
                ch,
                Some(from),
                ActArg::None,
                To::Char,
            );
            let val1 = obj_val(g, from, 1);
            weight_change_object(g, from, -val1);
            if let Some(o) = g.get_obj_mut(from) {
                o.values[1] = 0;
                o.values[2] = 0;
                o.values[3] = 0;
            }
            name_from_drinkcon(g, from);
            return;
        }
        let inv = g
            .get_char(ch)
            .map(|c| c.carrying.clone())
            .unwrap_or_default();
        match g.get_obj_in_list_vis(ch, &arg2, &inv) {
            None => {
                act(
                    g,
                    "You can't find it!",
                    false,
                    ch,
                    None,
                    ActArg::None,
                    To::Char,
                );
                return;
            }
            Some(o) => to_obj = Some(o),
        }
        let to_ty = obj_type(g, to_obj.unwrap());
        if to_ty != Some(ObjectType::LiqContainer) && to_ty != Some(ObjectType::Fountain) {
            act(
                g,
                "You can't pour anything into that.",
                false,
                ch,
                None,
                ActArg::None,
                To::Char,
            );
            return;
        }
    }

    let to = to_obj.unwrap();
    if to == from {
        act(
            g,
            "A most unproductive effort.",
            false,
            ch,
            None,
            ActArg::None,
            To::Char,
        );
        return;
    }
    if obj_val(g, to, 1) != 0 && obj_val(g, to, 2) != obj_val(g, from, 2) {
        act(
            g,
            "There is already another liquid in it!",
            false,
            ch,
            None,
            ActArg::None,
            To::Char,
        );
        return;
    }
    if !(obj_val(g, to, 1) < obj_val(g, to, 0)) {
        act(
            g,
            "There is no room for more.",
            false,
            ch,
            None,
            ActArg::None,
            To::Char,
        );
        return;
    }

    if subcmd == SCMD_POUR {
        let liq = obj_val(g, from, 2);
        g.send_to_char(
            ch,
            &format!("You pour the {} into the {}.", drink_name(liq), arg2),
        );
    }
    if subcmd == SCMD_FILL {
        act(
            g,
            "You gently fill $p from $P.",
            false,
            ch,
            Some(to),
            ActArg::Obj(from),
            To::Char,
        );
        act(
            g,
            "$n gently fills $p from $P.",
            true,
            ch,
            Some(to),
            ActArg::Obj(from),
            To::Room,
        );
    }

    if obj_val(g, to, 1) == 0 {
        name_to_drinkcon(g, to, obj_val(g, from, 2));
    }

    // First same type liq.
    let from_liq = obj_val(g, from, 2);
    if let Some(o) = g.get_obj_mut(to) {
        o.values[2] = from_liq;
    }

    // Then how much to pour.
    let mut amount = obj_val(g, to, 0) - obj_val(g, to, 1);
    if let Some(o) = g.get_obj_mut(from) {
        o.values[1] -= amount;
    }
    if let Some(o) = g.get_obj_mut(to) {
        o.values[1] = o.values[0];
    }

    if obj_val(g, from, 1) < 0 {
        // There was too little.
        let from1 = obj_val(g, from, 1);
        if let Some(o) = g.get_obj_mut(to) {
            o.values[1] += from1;
        }
        amount += from1;
        if let Some(o) = g.get_obj_mut(from) {
            o.values[1] = 0;
            o.values[2] = 0;
            o.values[3] = 0;
        }
        name_from_drinkcon(g, from);
    }

    // Poison boogie.
    let poisoned = (obj_val(g, to, 3) != 0) || (obj_val(g, from, 3) != 0);
    if let Some(o) = g.get_obj_mut(to) {
        o.values[3] = if poisoned { 1 } else { 0 };
    }

    // Weight boogie.
    weight_change_object(g, from, -amount);
    weight_change_object(g, to, amount);
}

// ===========================================================================
// WEAR / WIELD / HOLD / GRAB / REMOVE
// ===========================================================================

/// wear_message() (act.item.c): {room, char} per WEAR_* slot.
fn wear_message(g: &mut GameState, ch: CharId, obj: ObjId, where_: usize) {
    const MSGS: [(&str, &str); 22] = [
        ("$n lights $p and holds it.", "You light $p and hold it."),
        (
            "$n slides $p on to $s right ring finger.",
            "You slide $p on to your right ring finger.",
        ),
        (
            "$n slides $p on to $s left ring finger.",
            "You slide $p on to your left ring finger.",
        ),
        (
            "$n wears $p around $s neck.",
            "You wear $p around your neck.",
        ),
        (
            "$n wears $p around $s neck.",
            "You wear $p around your neck.",
        ),
        ("$n wears $p on $s body.", "You wear $p on your body."),
        ("$n wears $p on $s head.", "You wear $p on your head."),
        ("$n puts $p on $s legs.", "You put $p on your legs."),
        ("$n wears $p on $s feet.", "You wear $p on your feet."),
        ("$n puts $p on $s hands.", "You put $p on your hands."),
        ("$n wears $p on $s arms.", "You wear $p on your arms."),
        (
            "$n straps $p around $s arm as a shield.",
            "You start to use $p as a shield.",
        ),
        (
            "$n wears $p about $s body.",
            "You wear $p around your body.",
        ),
        (
            "$n wears $p around $s waist.",
            "You wear $p around your waist.",
        ),
        (
            "$n puts $p on around $s right wrist.",
            "You put $p on around your right wrist.",
        ),
        (
            "$n puts on $p around $s left wrist.",
            "You put on $p around your left wrist.",
        ),
        ("$n wields $p.", "You wield $p."),
        ("$n grabs $p.", "You grab $p."),
        (
            "$n puts $p over $s shoulders.",
            "You put $p over your shoulders.",
        ),
        (
            "$n puts $p around $s right ankle.",
            "You put $p around your right ankle.",
        ),
        (
            "$n puts $p around $s left ankle.",
            "You put $p around your left ankle.",
        ),
        ("$n puts $p on $s face.", "You put $p on your face."),
    ];
    let (room_msg, char_msg) = MSGS.get(where_).copied().unwrap_or(("", ""));
    act(g, room_msg, true, ch, Some(obj), ActArg::None, To::Room);
    act(g, char_msg, false, ch, Some(obj), ActArg::None, To::Char);
}

fn perform_wear(g: &mut GameState, ch: CharId, obj: ObjId, mut where_: usize) {
    // wear_bitvectors[where] — required wear flag per slot.
    const WEAR_BITVECTORS: [u32; 22] = [
        ITEM_WEAR_TAKE,
        ITEM_WEAR_FINGER,
        ITEM_WEAR_FINGER,
        ITEM_WEAR_NECK,
        ITEM_WEAR_NECK,
        ITEM_WEAR_BODY,
        ITEM_WEAR_HEAD,
        ITEM_WEAR_LEGS,
        ITEM_WEAR_FEET,
        ITEM_WEAR_HANDS,
        ITEM_WEAR_ARMS,
        ITEM_WEAR_SHIELD,
        ITEM_WEAR_ABOUT,
        ITEM_WEAR_WAIST,
        ITEM_WEAR_WRIST,
        ITEM_WEAR_WRIST,
        ITEM_WEAR_WIELD,
        ITEM_WEAR_TAKE,
        ITEM_WEAR_SHOULDERS,
        ITEM_WEAR_ANKLE,
        ITEM_WEAR_ANKLE,
        ITEM_WEAR_FACE,
    ];
    const ALREADY_WEARING: [&str; 22] = [
        "You're already using a light.\r\n",
        "YOU SHOULD NEVER SEE THIS MESSAGE.  PLEASE REPORT.\r\n",
        "You're already wearing something on both of your ring fingers.\r\n",
        "YOU SHOULD NEVER SEE THIS MESSAGE.  PLEASE REPORT.\r\n",
        "You can't wear anything else around your neck.\r\n",
        "You're already wearing something on your body.\r\n",
        "You're already wearing something on your head.\r\n",
        "You're already wearing something on your legs.\r\n",
        "You're already wearing something on your feet.\r\n",
        "You're already wearing something on your hands.\r\n",
        "You're already wearing something on your arms.\r\n",
        "You're already using a shield.\r\n",
        "You're already wearing something about your body.\r\n",
        "You already have something around your waist.\r\n",
        "YOU SHOULD NEVER SEE THIS MESSAGE.  PLEASE REPORT.\r\n",
        "You're already wearing something around both of your wrists.\r\n",
        "You're already wielding a weapon.\r\n",
        "You're already holding something.\r\n",
        "You're already wearing something over your shoulders.\r\n",
        "YOU SHOULD NEVER SEE THIS MESSAGE.  PLEASE REPORT.\r\n",
        "You're already wearing something on both of your ankles.\r\n",
        "You're already wearing something on your face.\r\n",
    ];

    if !obj_wear(g, obj, WEAR_BITVECTORS[where_]) {
        act(
            g,
            "You can't wear $p there.",
            false,
            ch,
            Some(obj),
            ActArg::None,
            To::Char,
        );
        return;
    }
    // For neck/finger/wrist/ankle, try slot 2 if slot 1 is full.
    if where_ == W_FINGER_R || where_ == W_NECK_1 || where_ == W_WRIST_R || where_ == W_ANKLE_R {
        if eq_at(g, ch, where_).is_some() {
            where_ += 1;
        }
    }
    if eq_at(g, ch, where_).is_some() {
        g.send_to_char(ch, ALREADY_WEARING[where_]);
        return;
    }
    if !crate::dg_triggers::wear_otrigger(g, obj, ch, where_) {
        return;
    }
    // C order (act.item.c:1459-1466 + handler.c:653-665): the wear message
    // prints FIRST, then equip_char runs the zap check - so a zapped item
    // shows 'You wear $p...' followed by the zap line. The port zapped
    // before any message (#130).
    wear_message(g, ch, obj, where_);
    g.obj_from_anywhere(obj);
    if wear_restriction_zaps(g, ch, obj) {
        act(
            g,
            "You are zapped by $p and instantly let go of it.",
            false,
            ch,
            Some(obj),
            ActArg::None,
            To::Char,
        );
        act(
            g,
            "$n is zapped by $p and instantly lets go of it.",
            false,
            ch,
            Some(obj),
            ActArg::None,
            To::Room,
        );
        // Back to inventory, never equipped.
        g.obj_to_char(obj, ch);
        return;
    }
    g.equip_char(ch, obj, where_);
}

fn wear_restriction_zaps(g: &GameState, ch: CharId, obj: ObjId) -> bool {
    let (level, class, alignment) = match g.get_char(ch) {
        Some(c) => (c.player.level, c.player.class, c.alignment),
        None => return true,
    };
    if level >= LVL_IMMORT {
        return false;
    }

    let obj = match g.get_obj(obj) {
        Some(o) => o,
        None => return true,
    };
    let flags = obj.extra_flags;
    let anti_align = (flags.contains(ExtraFlags::ANTI_EVIL) && alignment <= -350)
        || (flags.contains(ExtraFlags::ANTI_GOOD) && alignment >= 350)
        || (flags.contains(ExtraFlags::ANTI_NEUTRAL) && alignment > -350 && alignment < 350);
    anti_align
        || crate::class::invalid_class(class, flags.bits() as i64)
        || obj.min_level > level as i32
}

/// find_eq_pos() (act.item.c). Returns Some(slot) or None on failure (the C
/// returns -1, which the caller distinguishes). When `arg` is given but does
/// not match a body part, this also emits the error and returns None.
fn find_eq_pos(g: &mut GameState, ch: CharId, obj: ObjId, arg: Option<&str>) -> Option<usize> {
    // keywords[] indices into the slot list.
    const KEYWORDS: [&str; 22] = [
        "!RESERVED!",
        "finger",
        "!RESERVED!",
        "neck",
        "!RESERVED!",
        "body",
        "head",
        "legs",
        "feet",
        "hands",
        "arms",
        "shield",
        "about",
        "waist",
        "wrist",
        "!RESERVED!",
        "!RESERVED!",
        "!RESERVED!",
        "shoulders",
        "ankle",
        "face",
        "!RESERVED!",
    ];

    match arg {
        None | Some("") => {
            let mut where_: Option<usize> = None;
            if obj_wear(g, obj, ITEM_WEAR_FINGER) {
                where_ = Some(W_FINGER_R);
            }
            if obj_wear(g, obj, ITEM_WEAR_NECK) {
                where_ = Some(W_NECK_1);
            }
            if obj_wear(g, obj, ITEM_WEAR_BODY) {
                where_ = Some(W_BODY);
            }
            if obj_wear(g, obj, ITEM_WEAR_HEAD) {
                where_ = Some(W_HEAD);
            }
            if obj_wear(g, obj, ITEM_WEAR_LEGS) {
                where_ = Some(W_LEGS);
            }
            if obj_wear(g, obj, ITEM_WEAR_FEET) {
                where_ = Some(W_FEET);
            }
            if obj_wear(g, obj, ITEM_WEAR_HANDS) {
                where_ = Some(W_HANDS);
            }
            if obj_wear(g, obj, ITEM_WEAR_ARMS) {
                where_ = Some(W_ARMS);
            }
            if obj_wear(g, obj, ITEM_WEAR_SHIELD) {
                where_ = Some(W_SHIELD);
            }
            if obj_wear(g, obj, ITEM_WEAR_ABOUT) {
                where_ = Some(W_ABOUT);
            }
            if obj_wear(g, obj, ITEM_WEAR_WAIST) {
                where_ = Some(W_WAIST);
            }
            if obj_wear(g, obj, ITEM_WEAR_WRIST) {
                where_ = Some(W_WRIST_R);
            }
            if obj_wear(g, obj, ITEM_WEAR_SHOULDERS) {
                where_ = Some(W_SHOULDERS);
            }
            if obj_wear(g, obj, ITEM_WEAR_ANKLE) {
                where_ = Some(W_ANKLE_R);
            }
            if obj_wear(g, obj, ITEM_WEAR_FACE) {
                where_ = Some(W_FACE);
            }
            where_
        }
        Some(a) => {
            // search_block(arg, keywords, FALSE): first prefix match.
            let lower = a.to_lowercase();
            let pos = KEYWORDS
                .iter()
                .position(|k| k.to_lowercase().starts_with(&lower));
            match pos {
                Some(idx) if !a.starts_with('!') => Some(idx),
                _ => {
                    g.send_to_char(
                        ch,
                        &format!("'{}'?  What part of your body is THAT?\r\n", a),
                    );
                    None
                }
            }
        }
    }
}

pub fn do_wear(g: &mut GameState, ch: CharId, argument: &str, _subcmd: i32) {
    let (arg1, arg2) = two_arguments(argument);

    if arg1.is_empty() {
        g.send_to_char(ch, "Wear what?\r\n");
        return;
    }
    let (dotmode, name) = find_all_dots(&arg1);

    if !arg2.is_empty() && dotmode != FIND_INDIV {
        g.send_to_char(
            ch,
            "You can't specify the same body location for more than one item!\r\n",
        );
        return;
    }

    if dotmode == FIND_ALL {
        let mut items_worn = 0;
        let inv = g
            .get_char(ch)
            .map(|c| c.carrying.clone())
            .unwrap_or_default();
        for obj in inv {
            if can_see_obj(g, ch, obj) {
                if let Some(where_) = find_eq_pos(g, ch, obj, None) {
                    items_worn += 1;
                    perform_wear(g, ch, obj, where_);
                }
            }
        }
        if items_worn == 0 {
            g.send_to_char(ch, "You don't seem to have anything wearable.\r\n");
        }
    } else if dotmode == FIND_ALLDOT {
        if name.is_empty() {
            g.send_to_char(ch, "Wear all of what?\r\n");
            return;
        }
        let inv = g
            .get_char(ch)
            .map(|c| c.carrying.clone())
            .unwrap_or_default();
        if g.get_obj_in_list_vis(ch, &name, &inv).is_none() {
            g.send_to_char(ch, &format!("You don't seem to have any {}s.\r\n", name));
        } else {
            let inv = g
                .get_char(ch)
                .map(|c| c.carrying.clone())
                .unwrap_or_default();
            for obj in inv {
                if obj_isname(g, &name, obj) {
                    if let Some(where_) = find_eq_pos(g, ch, obj, None) {
                        perform_wear(g, ch, obj, where_);
                    } else {
                        act(
                            g,
                            "You can't wear $p.",
                            false,
                            ch,
                            Some(obj),
                            ActArg::None,
                            To::Char,
                        );
                    }
                }
            }
        }
    } else {
        let inv = g
            .get_char(ch)
            .map(|c| c.carrying.clone())
            .unwrap_or_default();
        match g.get_obj_in_list_vis(ch, &arg1, &inv) {
            None => {
                g.send_to_char(
                    ch,
                    &format!("You don't seem to have {} {}.\r\n", an(&arg1), arg1),
                );
            }
            Some(obj) => {
                let arg2_opt = if arg2.is_empty() {
                    None
                } else {
                    Some(arg2.as_str())
                };
                if let Some(where_) = find_eq_pos(g, ch, obj, arg2_opt) {
                    perform_wear(g, ch, obj, where_);
                } else if arg2.is_empty() {
                    act(
                        g,
                        "You can't wear $p.",
                        false,
                        ch,
                        Some(obj),
                        ActArg::None,
                        To::Char,
                    );
                }
            }
        }
    }
}

pub fn do_wield(g: &mut GameState, ch: CharId, argument: &str, _subcmd: i32) {
    let (arg, _) = crate::interpreter::one_argument(argument);

    if arg.is_empty() {
        g.send_to_char(ch, "Wield what?\r\n");
        return;
    }
    let inv = g
        .get_char(ch)
        .map(|c| c.carrying.clone())
        .unwrap_or_default();
    let obj = match g.get_obj_in_list_vis(ch, &arg, &inv) {
        Some(o) => o,
        None => {
            g.send_to_char(
                ch,
                &format!("You don't seem to have {} {}.\r\n", an(&arg), arg),
            );
            return;
        }
    };

    if !obj_wear(g, obj, ITEM_WEAR_WIELD) {
        g.send_to_char(ch, "You can't wield that.\r\n");
    } else if obj_weight(g, obj) > STR_APP[strength_apply_index(g, ch)].1 {
        g.send_to_char(ch, "It's too heavy for you to use.\r\n");
    } else if !is_immort(g, ch)
        && WEAPONRESTRICTIONS > 0
        && ((((obj_val(g, obj, 2) + 1) as f64 / 2.0) * obj_val(g, obj, 1) as f64)
            > lvl_maxdmg_weapon(get_level(g, ch) as usize) as f64)
    {
        // C act.item.c:1651-1660 do_wield gate: the level/damage ceiling is
        // LIVE (config.c:93 weaponrestrictions = YES) - the port hardcoded
        // 0 and compared against nothing (#122).
        act(
            g,
            "$p fumbles out of your inexperienced hands...",
            false,
            ch,
            Some(obj),
            ActArg::None,
            To::Char,
        );
        act(
            g,
            "$p fumbles out of $n's inexperienced hands...",
            false,
            ch,
            Some(obj),
            ActArg::None,
            To::Room,
        );
    } else {
        perform_wear(g, ch, obj, W_WIELD);
    }
}

pub fn do_grab(g: &mut GameState, ch: CharId, argument: &str, _subcmd: i32) {
    let (arg, _) = crate::interpreter::one_argument(argument);

    if arg.is_empty() {
        g.send_to_char(ch, "Hold what?\r\n");
        return;
    }
    let inv = g
        .get_char(ch)
        .map(|c| c.carrying.clone())
        .unwrap_or_default();
    let obj = match g.get_obj_in_list_vis(ch, &arg, &inv) {
        Some(o) => o,
        None => {
            g.send_to_char(
                ch,
                &format!("You don't seem to have {} {}.\r\n", an(&arg), arg),
            );
            return;
        }
    };

    if obj_type(g, obj) == Some(ObjectType::Light) {
        perform_wear(g, ch, obj, W_LIGHT);
    } else {
        let ty = obj_type(g, obj);
        if !obj_wear(g, obj, ITEM_WEAR_HOLD)
            && ty != Some(ObjectType::Wand)
            && ty != Some(ObjectType::Staff)
            && ty != Some(ObjectType::Scroll)
            && ty != Some(ObjectType::Potion)
        {
            g.send_to_char(ch, "You can't hold that.\r\n");
        } else {
            perform_wear(g, ch, obj, W_HOLD);
        }
    }
}

fn perform_remove(g: &mut GameState, ch: CharId, pos: usize) {
    let obj = match eq_at(g, ch, pos) {
        Some(o) => o,
        None => return,
    };
    // C act.item.c:1712: remove_otrigger can veto taking an item off (#143).
    if !crate::dg_triggers::remove_otrigger(g, obj, ch) {
        return;
    }
    if is_carrying_n(g, ch) >= can_carry_n(g, ch) {
        act(
            g,
            "$p: you can't carry that many items!",
            false,
            ch,
            Some(obj),
            ActArg::None,
            To::Char,
        );
    } else {
        act(
            g,
            "You stop using $p.",
            false,
            ch,
            Some(obj),
            ActArg::None,
            To::Char,
        );
        act(
            g,
            "$n stops using $p.",
            true,
            ch,
            Some(obj),
            ActArg::None,
            To::Room,
        );
        if let Some(removed) = g.unequip_char(ch, pos) {
            g.obj_to_char(removed, ch);
        }
    }
}

pub fn do_remove(g: &mut GameState, ch: CharId, argument: &str, _subcmd: i32) {
    let (arg, _) = crate::interpreter::one_argument(argument);

    if arg.is_empty() {
        g.send_to_char(ch, "Remove what?\r\n");
        return;
    }
    let (dotmode, name) = find_all_dots(&arg);

    if dotmode == FIND_ALL {
        let mut found = false;
        for i in 0..NUM_WEARS {
            if g.get_char(ch).and_then(|c| c.equipment[i]).is_some() {
                perform_remove(g, ch, i);
                found = true;
            }
        }
        if !found {
            g.send_to_char(ch, "You're not using anything.\r\n");
        }
    } else if dotmode == FIND_ALLDOT {
        if name.is_empty() {
            g.send_to_char(ch, "Remove all of what?\r\n");
        } else {
            let mut found = false;
            for i in 0..NUM_WEARS {
                let eq = g.get_char(ch).and_then(|c| c.equipment[i]);
                if let Some(oid) = eq {
                    if can_see_obj(g, ch, oid) && obj_isname(g, &name, oid) {
                        perform_remove(g, ch, i);
                        found = true;
                    }
                }
            }
            if !found {
                g.send_to_char(
                    ch,
                    &format!("You don't seem to be using any {}s.\r\n", name),
                );
            }
        }
    } else {
        // get_object_in_equip_vis: find by keyword across equipment slots.
        let mut found_pos: Option<usize> = None;
        let (mut count, kw) = crate::handler::get_number(&arg);
        if count != 0 {
            for i in 0..NUM_WEARS {
                if let Some(oid) = g.get_char(ch).and_then(|c| c.equipment[i]) {
                    if can_see_obj(g, ch, oid) && obj_isname(g, &kw, oid) {
                        count -= 1;
                        if count == 0 {
                            found_pos = Some(i);
                            break;
                        }
                    }
                }
            }
        }
        match found_pos {
            None => {
                g.send_to_char(
                    ch,
                    &format!("You don't seem to be using {} {}.\r\n", an(&arg), arg),
                );
            }
            Some(i) => perform_remove(g, ch, i),
        }
    }
}

// ===========================================================================
// SACRIFICE / REPAIR
// ===========================================================================

pub fn do_sac(g: &mut GameState, ch: CharId, argument: &str, _subcmd: i32) {
    let (arg, _) = crate::interpreter::one_argument(argument);

    if arg.is_empty() {
        g.send_to_char(ch, "What do you want to sacrifice?\n\r");
        return;
    }
    let rnum = match g.get_char(ch).and_then(|c| c.in_room) {
        Some(r) => r,
        None => return,
    };
    let contents = g.room(rnum).contents.clone();
    let obj = match g.get_obj_in_list_vis(ch, &arg, &contents) {
        Some(o) => o,
        None => {
            g.send_to_char(ch, "You don't see such an object.\n\r");
            return;
        }
    };

    if !obj_wear(g, obj, ITEM_WEAR_TAKE) {
        g.send_to_char(ch, "You can't sacrifice that!\n\r");
        return;
    }
    if g.get_obj(obj)
        .map(|o| !o.contains.is_empty())
        .unwrap_or(false)
    {
        g.send_to_char(ch, "It's not empty!\r\n");
        return;
    }

    act(
        g,
        "$n sacrifices $p.",
        false,
        ch,
        Some(obj),
        ActArg::None,
        To::Room,
    );
    act(
        g,
        "You sacrifice $p.",
        false,
        ch,
        Some(obj),
        ActArg::None,
        To::Char,
    );
    if !is_immort(g, ch) {
        act(
            g,
            "You have been rewarded by the gods!",
            false,
            ch,
            Some(obj),
            ActArg::None,
            To::Char,
        );
        if let Some(c) = g.get_char_mut(ch) {
            c.points.exp += 1;
        }
    }
    g.extract_obj(obj);
}

pub fn do_repair(g: &mut GameState, ch: CharId, argument: &str, _subcmd: i32) {
    // SKILL_REPAIR proficiency (spells.h SKILL_REPAIR == 528).
    const SKILL_REPAIR: u16 = 528;
    let skill = g.get_char(ch).map(|c| c.skill(SKILL_REPAIR)).unwrap_or(0) as i32;
    if skill <= 0 {
        g.send_to_char(ch, "You don't know how to repairs things!\r\n");
        return;
    }

    let (arg, _) = crate::interpreter::one_argument(argument);
    if arg.is_empty() {
        g.send_to_char(ch, "Repair what?\r\n");
        return;
    }
    let inv = g
        .get_char(ch)
        .map(|c| c.carrying.clone())
        .unwrap_or_default();
    let repair = match g.get_obj_in_list_vis(ch, &arg, &inv) {
        Some(o) => o,
        None => {
            g.send_to_char(
                ch,
                &format!("You don't seem to have {} {}.\r\n", an(&arg), arg),
            );
            return;
        }
    };

    let percent = g.rng.number(1, 101);
    let prob = skill;

    // GET_OBJ_CSLOTS / GET_OBJ_TSLOTS are dedicated obj_flags.curr_slots /
    // total_slots fields in C, which the Object contract does not yet model.
    // Mapped here onto values[1] (current) / values[2] (total) so the command
    // is fully wired; the integrator should add curr_slots/total_slots to
    // Object and repoint these four accesses for exact durability parity.
    let cslots = obj_val(g, repair, 1);
    let tslots = obj_val(g, repair, 2);

    if cslots == 0 && tslots == 0 {
        act(
            g,
            "$p seems to already be indestructable!",
            false,
            ch,
            Some(repair),
            ActArg::None,
            To::Char,
        );
        return;
    }
    if cslots == tslots {
        act(
            g,
            "$p seems to already be in perfect condition!",
            false,
            ch,
            Some(repair),
            ActArg::None,
            To::Char,
        );
        return;
    }

    if !is_immort(g, ch) {
        let exp = g.get_char(ch).map(|c| c.points.exp).unwrap_or(0);
        if exp > 10000 {
            if let Some(c) = g.get_char_mut(ch) {
                c.points.exp -= 10000;
            }
            g.send_to_char(
                ch,
                "Your repair attempt costs you 10,000 experience points.\r\n",
            );
        } else {
            g.send_to_char(
                ch,
                "You do not have enough experience to attempt to repair it!\r\n",
            );
            return;
        }
    }

    if cslots < 0 {
        act(
            g,
            "You completely ruin $p and it crumbles away!",
            false,
            ch,
            Some(repair),
            ActArg::None,
            To::Char,
        );
        act(
            g,
            "$n tries to repair $p, but it crumbles away!",
            true,
            ch,
            Some(repair),
            ActArg::None,
            To::Room,
        );
        g.extract_obj(repair);
        return;
    }

    if percent > prob {
        act(
            g,
            "Your clumsy attempt at repairing $p damages it even more!",
            false,
            ch,
            Some(repair),
            ActArg::None,
            To::Char,
        );
        act(
            g,
            "$n tries to repair $p, but only makes it worse!",
            true,
            ch,
            Some(repair),
            ActArg::None,
            To::Room,
        );
        if let Some(o) = g.get_obj_mut(repair) {
            o.values[1] -= 2;
            o.values[2] -= 1;
        }
    } else {
        act(
            g,
            "You repair $p and it looks in excellent condition again!",
            false,
            ch,
            Some(repair),
            ActArg::None,
            To::Char,
        );
        act(
            g,
            "$n repairs $p, making it as good as new again!",
            true,
            ch,
            Some(repair),
            ActArg::None,
            To::Room,
        );
        if let Some(o) = g.get_obj_mut(repair) {
            o.values[2] -= 1;
            o.values[1] = o.values[2];
        }
    }
}

// ---------------------------------------------------------------------------
// Shared finders (generic_find subset used by put/get).
// ---------------------------------------------------------------------------

/// isname(arg, obj->name).
fn obj_isname(g: &GameState, arg: &str, oid: ObjId) -> bool {
    g.get_obj(oid)
        .map(|o| crate::handler::isname(arg, &o.name))
        .unwrap_or(false)
}

/// generic_find(FIND_OBJ_INV | FIND_OBJ_ROOM): inventory first, then room.
fn find_obj_inv_room(g: &GameState, ch: CharId, arg: &str) -> Option<ObjId> {
    find_obj_inv_room_mode(g, ch, arg).0
}

/// Same, but also reports which list the match came from (the C `mode`
/// return: FIND_OBJ_INV or FIND_OBJ_ROOM), used by get_from_container.
fn find_obj_inv_room_mode(g: &GameState, ch: CharId, arg: &str) -> (Option<ObjId>, i32) {
    let inv = g
        .get_char(ch)
        .map(|c| c.carrying.clone())
        .unwrap_or_default();
    if let Some(o) = g.get_obj_in_list_vis(ch, arg, &inv) {
        return (Some(o), FIND_OBJ_INV);
    }
    let rnum = g.get_char(ch).and_then(|c| c.in_room);
    if let Some(rnum) = rnum {
        let contents = g.room(rnum).contents.clone();
        if let Some(o) = g.get_obj_in_list_vis(ch, arg, &contents) {
            return (Some(o), FIND_OBJ_ROOM);
        }
    }
    (None, 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::character::Character;
    use crate::config::Config;
    use crate::connection::Descriptor;
    use crate::dg_handler::{
        self, DG_TEST_LOCK, OBJ_TRIGGER, OTRIG_WEAR, ScriptKey, TrigData, add_trigger, install_trig,
    };
    use crate::object::Object;
    use std::collections::HashMap;

    fn wearable_game(
        extra_flags: ExtraFlags,
        min_level: i32,
    ) -> (GameState, CharId, ObjId, ConnId) {
        let mut g = GameState::new(Config::default());
        let conn = ConnId(1);
        g.descriptors
            .insert(conn, Descriptor::new(conn, "test".to_string()));

        let mut ch = Character::new_player("Wearer".to_string(), Class::Warrior, Race::Human);
        ch.desc = Some(conn);
        ch.player.level = 10;
        let ch = g.create_char(ch);

        let mut obj = Object::new(1000, "armor".to_string(), "a test armor".to_string());
        obj.wear_flags = WearFlags::TAKE | WearFlags::BODY;
        obj.extra_flags = extra_flags;
        obj.min_level = min_level;
        let obj = g.create_obj(obj);
        g.obj_to_char(obj, ch);
        (g, ch, obj, conn)
    }

    #[test]
    fn currency_commands_reject_signed_i32_overflow_at_the_entry_point() {
        for (input, command) in [
            ("2147483648 coins", "drop"),
            ("-2147483649 coins Nobody", "give"),
        ] {
            let (mut g, ch, _obj, conn) = wearable_game(ExtraFlags::empty(), 0);
            match command {
                "drop" => do_drop(&mut g, ch, input, SCMD_DROP),
                "give" => do_give(&mut g, ch, input, 0),
                _ => unreachable!(),
            }
            assert!(
                g.descriptors
                    .get(&conn)
                    .unwrap()
                    .outbuf
                    .contains("That amount is out of range."),
                "command={command}, input={input:?}"
            );
        }
    }

    #[test]
    fn direct_staff_gold_mint_uses_persisted_trust_and_is_audited() {
        let lib = std::env::temp_dir().join(format!(
            "deltamud-mint-audit-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&lib).unwrap();
        let mut config = Config::default();
        config.lib_path = lib.to_string_lossy().into_owned();
        let mut g = GameState::new(config);
        let room = g.add_room(crate::room::Room::new(100, 0, "Room".into(), String::new()));

        let staff_conn = ConnId(101);
        let mut staff = Character::new_player("Staff".into(), Class::Warrior, Race::Human);
        staff.desc = Some(staff_conn);
        staff.player.level = 1;
        staff.trust = i32::from(LVL_GOD);
        let staff = g.create_char(staff);
        let mut descriptor = Descriptor::new(staff_conn, "staff.test".into());
        descriptor.state = crate::connection::ConState::Playing;
        descriptor.character = Some(staff);
        g.descriptors.insert(staff_conn, descriptor);
        g.players_by_name.insert("staff".into(), staff);

        let target_conn = ConnId(102);
        let mut target = Character::new_player("Target".into(), Class::Warrior, Race::Human);
        target.desc = Some(target_conn);
        target.player.level = LVL_GOD;
        target.trust = 1;
        let target = g.create_char(target);
        let mut descriptor = Descriptor::new(target_conn, "target.test".into());
        descriptor.state = crate::connection::ConState::Playing;
        descriptor.character = Some(target);
        g.descriptors.insert(target_conn, descriptor);
        g.players_by_name.insert("target".into(), target);
        g.char_to_room(staff, room);
        g.char_to_room(target, room);

        crate::interpreter::run_authenticated_command(&mut g, target, "give 10 coins Staff");
        assert_eq!(g.get_char(staff).unwrap().points.gold, 0);

        crate::interpreter::run_authenticated_command(&mut g, staff, "give 10 coins Target");
        assert_eq!(g.get_char(target).unwrap().points.gold, 10);
        let syslog = std::fs::read_to_string(lib.join("syslog")).unwrap();
        assert!(syslog.contains("[WATCHDOG] Staff mints 10 gold coins for Target."));

        std::fs::remove_dir_all(lib).unwrap();
    }

    fn assert_zapped(mut g: GameState, ch: CharId, obj: ObjId, conn: ConnId) {
        perform_wear(&mut g, ch, obj, W_BODY);

        let c = g.get_char(ch).unwrap();
        assert_eq!(c.equipment[W_BODY], None);
        assert_eq!(g.get_obj(obj).unwrap().loc, ObjLoc::Carried(ch));
        assert!(
            g.descriptors
                .get(&conn)
                .unwrap()
                .outbuf
                .contains("You are zapped by a test armor and instantly let go of it.\r\n")
        );
    }

    fn make_obj_trigger(cmds: &[&str]) -> crate::dg_handler::TrigId {
        install_trig(TrigData {
            nr: 0,
            vnum: 9999,
            attach_type: OBJ_TRIGGER,
            name: "veto armor".to_string(),
            trigger_type: OTRIG_WEAR,
            narg: 100,
            arglist: String::new(),
            cmdlist: cmds.iter().map(|s| s.to_string()).collect(),
            curr_line: 0,
            depth: 0,
            loops: 0,
            wait_event: None,
            var_list: Vec::new(),
            purged: false,
            loop_origin: HashMap::new(),
        })
    }

    #[test]
    fn perform_wear_rejects_anti_alignment_items() {
        // Serialize with the wear-trigger veto test: obj ids and the DG script
        // store are process-global across tests.
        let _dg = DG_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (mut g, ch, obj, conn) = wearable_game(ExtraFlags::ANTI_GOOD, 0);
        g.get_char_mut(ch).unwrap().alignment = 500;

        assert_zapped(g, ch, obj, conn);
    }

    #[test]
    fn perform_wear_rejects_anti_class_items() {
        // Serialize with the wear-trigger veto test: obj ids and the DG script
        // store are process-global across tests.
        let _dg = DG_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (g, ch, obj, conn) = wearable_game(ExtraFlags::ANTI_WARRIOR, 0);

        assert_zapped(g, ch, obj, conn);
    }

    #[test]
    fn perform_wear_rejects_min_level_items() {
        // Serialize with the wear-trigger veto test: obj ids and the DG script
        // store are process-global across tests.
        let _dg = DG_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (g, ch, obj, conn) = wearable_game(ExtraFlags::empty(), 20);

        assert_zapped(g, ch, obj, conn);
    }

    #[test]
    fn perform_wear_runs_wear_trigger_and_honors_veto() {
        let _dg = DG_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        dg_handler::boot_handler();
        let (mut g, ch, obj, conn) = wearable_game(ExtraFlags::empty(), 0);
        let trig = make_obj_trigger(&["set fired yes", "global fired", "return 0", "halt"]);
        add_trigger(ScriptKey::Obj(obj), trig, -1);

        perform_wear(&mut g, ch, obj, W_BODY);

        assert_eq!(
            dg_handler::get_global_var(ScriptKey::Obj(obj), "fired").as_deref(),
            Some("yes")
        );
        assert_eq!(g.get_char(ch).unwrap().equipment[W_BODY], None);
        assert_eq!(g.get_obj(obj).unwrap().loc, ObjLoc::Carried(ch));
        assert!(
            !g.descriptors
                .get(&conn)
                .unwrap()
                .outbuf
                .contains("You wear a test armor on your body.\r\n")
        );

        // Remove the veto: the script store is process-global and obj ids are
        // per-GameState, so a leftover veto would zap another test's armor.
        // isname matches the whole query against ONE name token, so search "veto".
        assert!(dg_handler::remove_trigger(
            &mut g,
            ScriptKey::Obj(obj),
            "veto"
        ));
    }
}
