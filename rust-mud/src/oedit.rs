// oedit.rs — the OasisOLC object editor (CircleMUD oedit.c), ported to the
// single-owner GameState. Full menu-driven editor: namelist, short/long/action
// descriptions, item type, extra flags, wear flags, weight, cost, cost-per-day
// (rent), timer, the four type-aware object values (with their spell / liquid /
// container-flag / weapon sub-menus), the applies (affect) menu, extra
// descriptions, permanent AFF_ affects, intended class, and minimum level.
//
// Save writes the object back into GameState.obj_protos (the prototype subset
// it can hold) AND rewrites the zone's .obj file byte-faithfully (inverse of
// file_loader::load_object_file), including the DeltaMUD `c`/`L`/`BV`/`E`/`A`
// extension lines. Fields the ObjectProto struct can't carry (action_desc,
// level, timer, affects, extra-descs, perm-affect bitvector, durability,
// class) are held in the edit state and written to disk, so the on-disk file
// stays complete even though they don't round-trip into the in-memory proto.

use crate::constants::{
    AFFECTED_BITS, APPLY_TYPES, CONTAINER_BITS, DRINKS, EXTRA_BITS, ITEM_TYPES, WEAR_BITS,
};
use crate::object::{ExtraFlags, ObjectType, WearFlags};
use crate::olc::{self, EditorKind, CYN, GRN, NRM, YEL};
use crate::state::GameState;
use crate::types::*;
use crate::world::ObjectProto;
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::OnceLock;

// MAX_OBJ_AFFECT (structs.h) — DeltaMUD object affect slots.
const MAX_OBJ_AFFECT: usize = 6;
const MAX_MESSAGE_LENGTH: usize = 4096;
// Object value/spell/attack bounds used by the value menus.
const NUM_SPELLS: i32 = 51;
const NUM_ATTACK_TYPES: i32 = 16;

// PC class names for the intended-class menu (DeltaMUD pc_class_types[]).
const PC_CLASS_TYPES: &[&str] = &["Magic User", "Cleric", "Thief", "Warrior", "Artisan"];
const CLASS_UNDEFINED: i32 = -1;

// Counts of real entries in each name table (excluding the "\n" sentinel).
fn real_len(table: &[&str]) -> usize {
    table.iter().take_while(|&&s| s != "\n").count()
}

// ---------------------------------------------------------------------------
// Edit modes (OEDIT_* in oedit.c).
// ---------------------------------------------------------------------------
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OeditMode {
    MainMenu,
    ConfirmSave,
    Namelist,
    ShortDesc,
    LongDesc,
    ActDesc, // multi-line
    Type,
    Extras,
    Wear,
    Weight,
    Cost,
    CostPerDay,
    Timer,
    Value1,
    Value2,
    Value3,
    Value4,
    PromptApply,
    Apply,
    ApplyMod,
    ExtradescKey,
    ExtradescMenu,
    ExtradescDescription, // multi-line
    PermAffects,
    Class,
    Level,
    Durability,
    Durability2,
    Script(olc::DgScriptEditMode),
}

#[derive(Clone)]
struct ObjEdit {
    vnum: ObjVnum,
    name: String,
    short_description: String,
    description: String,
    action_description: Option<String>,
    obj_type: ObjectType,
    extra_flags: ExtraFlags,
    wear_flags: WearFlags,
    values: [i32; 4],
    weight: i32,
    cost: i32,
    rent: i32,
    timer: i32,
    level: i32,
    obj_class: i32,
    cslots: i32,                           // current durability (DeltaMUD GET_OBJ_CSLOTS)
    tslots: i32,                           // total durability   (GET_OBJ_TSLOTS)
    bitvector: i64,                        // permanent AFF_ affects
    affects: [(i32, i32); MAX_OBJ_AFFECT], // (location, modifier)
    extra_descriptions: Vec<(String, String)>,
}

struct OeditState {
    znum: usize,
    obj: ObjEdit,
    mode: OeditMode,
    modified: bool,
    apply_slot: usize, // OLC_VAL: which affect slot is being edited
    cur_desc: usize,
}

fn states() -> &'static Mutex<HashMap<ConnId, OeditState>> {
    static S: OnceLock<Mutex<HashMap<ConnId, OeditState>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(HashMap::new()))
}

fn with_state<R>(conn: ConnId, f: impl FnOnce(&mut OeditState) -> R) -> Option<R> {
    states().lock().unwrap().get_mut(&conn).map(f)
}

fn send(g: &mut GameState, conn: ConnId, msg: &str) {
    if let Some(d) = g.descriptors.get_mut(&conn) {
        d.outbuf.push_str(msg);
    }
}

fn conn_char(g: &GameState, conn: ConnId) -> Option<CharId> {
    g.descriptors.get(&conn).and_then(|d| d.character)
}

fn obj_type_num(t: ObjectType) -> i32 {
    t as i32
}

// ===========================================================================
// do_oedit — start the object editor (existing or new).
// ===========================================================================
pub fn do_oedit(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let conn = match g.get_char(ch).and_then(|c| c.desc) {
        Some(c) => c,
        None => return,
    };
    let vnum: ObjVnum = arg.trim().parse().unwrap_or(NOTHING);
    if vnum < 0 {
        g.send_to_char(ch, "That's not a valid object number.\r\n");
        return;
    }
    let znum = match olc::real_zone(g, vnum) {
        Some(z) => z,
        None => {
            g.send_to_char(ch, "Sorry, there is no zone for that number!\r\n");
            return;
        }
    };

    let conflict = states()
        .lock()
        .unwrap()
        .values()
        .any(|s| s.obj.vnum == vnum);
    if conflict {
        g.send_to_char(ch, "That object is currently being edited.\r\n");
        return;
    }

    let obj = if let Some(p) = g.obj_protos.get(&vnum) {
        from_proto(p)
    } else {
        // oedit_setup_new.
        ObjEdit {
            vnum,
            name: "unfinished object".to_string(),
            short_description: "an unfinished object".to_string(),
            description: "An unfinished object is lying here.".to_string(),
            action_description: None,
            obj_type: ObjectType::Other,
            extra_flags: ExtraFlags::empty(),
            wear_flags: WearFlags::TAKE,
            values: [0; 4],
            weight: 0,
            cost: 0,
            rent: 0,
            timer: 0,
            level: 0,
            obj_class: CLASS_UNDEFINED,
            cslots: 0,
            tslots: 0,
            bitvector: 0,
            affects: [(0, 0); MAX_OBJ_AFFECT],
            extra_descriptions: Vec::new(),
        }
    };

    states().lock().unwrap().insert(
        conn,
        OeditState {
            znum,
            obj,
            mode: OeditMode::MainMenu,
            modified: false,
            apply_slot: 0,
            cur_desc: 0,
        },
    );
    olc::set_active(conn, EditorKind::Oedit);
    crate::act::act(
        g,
        "$n starts using OLC.",
        true,
        ch,
        None,
        crate::act::ActArg::None,
        crate::act::To::Room,
    );
    disp_menu(g, conn);
}

fn from_proto(p: &ObjectProto) -> ObjEdit {
    // C oedit_setup_existing (oedit.c:97-136) copies the WHOLE proto and
    // re-dups only the strings. The field-slice here zeroed 9 persisted
    // fields, so editing + saving destroyed applies/perm-affects/class/
    // level/durability/extra-descs on the object (#258).
    ObjEdit {
        vnum: p.vnum,
        name: p.name.clone(),
        short_description: p.short_desc.clone(),
        description: p.description.clone(),
        action_description: if p.action_description.is_empty() {
            None
        } else {
            Some(p.action_description.clone())
        },
        obj_type: p.obj_type,
        extra_flags: p.extra_flags,
        wear_flags: p.wear_flags,
        values: p.values,
        weight: p.weight,
        cost: p.cost,
        rent: p.rent,
        timer: 0, // protos carry no timer (C copies it, default 0 on read)
        level: p.min_level,
        obj_class: p.obj_class,
        cslots: p.curr_slots,
        tslots: p.total_slots,
        bitvector: p.bitvector,
        affects: {
            let mut a = [(0, 0); MAX_OBJ_AFFECT];
            for (i, af) in p.affects.iter().take(MAX_OBJ_AFFECT).enumerate() {
                a[i] = (af.location, af.modifier);
            }
            a
        },
        extra_descriptions: p.ex_descriptions.clone(),
    }
}

// ===========================================================================
// Multi-line text sub-editor (action desc / extra desc).
// ===========================================================================
fn text_bufs() -> &'static Mutex<HashMap<ConnId, String>> {
    static B: OnceLock<Mutex<HashMap<ConnId, String>>> = OnceLock::new();
    B.get_or_init(|| Mutex::new(HashMap::new()))
}

fn begin_text(g: &mut GameState, conn: ConnId, seed: &str, mode: OeditMode) {
    text_bufs().lock().unwrap().insert(conn, seed.to_string());
    let _ = with_state(conn, |s| s.mode = mode);
    send(g, conn, "Enter text: (/s saves /h for help)\r\n\r\n");
    if !seed.is_empty() {
        send(g, conn, seed);
        if !seed.ends_with('\n') {
            send(g, conn, "\r\n");
        }
    }
}

fn text_input(g: &mut GameState, conn: ConnId, line: &str) -> Option<Option<String>> {
    let trimmed = line.trim_end_matches(['\r', '\n']);
    if trimmed == "~" {
        return Some(Some(
            text_bufs()
                .lock()
                .unwrap()
                .remove(&conn)
                .unwrap_or_default(),
        ));
    }
    let mut buf = text_bufs()
        .lock()
        .unwrap()
        .remove(&conn)
        .unwrap_or_default();
    match crate::modify::editor_buffer_input(g, conn, &mut buf, MAX_MESSAGE_LENGTH, line) {
        crate::modify::BufferEditorResult::Continue => {
            text_bufs().lock().unwrap().insert(conn, buf);
            None
        }
        crate::modify::BufferEditorResult::Save => Some(Some(buf)),
        crate::modify::BufferEditorResult::Abort => Some(None),
    }
}

// ===========================================================================
// Main menu display.
// ===========================================================================
fn disp_menu(g: &mut GameState, conn: ConnId) {
    let o = match with_state(conn, |s| s.obj.clone()) {
        Some(o) => o,
        None => return,
    };
    let type_str = olc::sprinttype(obj_type_num(o.obj_type), ITEM_TYPES);
    let extra_str = olc::sprintbit(o.extra_flags.bits() as i64, EXTRA_BITS);
    let wear_str = olc::sprintbit(o.wear_flags.bits() as i64, WEAR_BITS);
    let aff_str = olc::sprintbit(o.bitvector, AFFECTED_BITS);
    let class_str = if o.obj_class == CLASS_UNDEFINED {
        "Generic".to_string()
    } else {
        PC_CLASS_TYPES
            .get(o.obj_class as usize)
            .copied()
            .unwrap_or("Generic")
            .to_string()
    };
    let act_desc = o
        .action_description
        .clone()
        .unwrap_or_else(|| "<not set>\r\n".to_string());

    let body = format!(
        "-- Item number : [{cyn}{vnum}{nrm}]\r\n\
         {grn}1{nrm}) Namelist : {yel}{name}\r\n\
         {grn}2{nrm}) S-Desc   : {yel}{sdesc}\r\n\
         {grn}3{nrm}) L-Desc   :-\r\n{yel}{ldesc}\r\n\
         {grn}4{nrm}) A-Desc   :-\r\n{yel}{adesc}\
         {grn}5{nrm}) Type        : {cyn}{type}\r\n\
         {grn}6{nrm}) Extra flags : {cyn}{extra}\r\n\
         {grn}7{nrm}) Wear flags  : {cyn}{wear}\r\n\
         {grn}8{nrm}) Weight      : {cyn}{weight}\r\n\
         {grn}9{nrm}) Cost        : {cyn}{cost}\r\n\
         {grn}A{nrm}) Cost/Day    : {cyn}{rent}\r\n\
         {grn}B{nrm}) Timer       : {cyn}{timer}\r\n\
         {grn}C{nrm}) Values      : {cyn}{v0} {v1} {v2} {v3}\r\n\
         {grn}D{nrm}) Applies menu\r\n\
         {grn}E{nrm}) Extra descriptions menu\r\n\
         {grn}F{nrm}) Current Durability : {cyn}{cslots}\r\n\
         {grn}G{nrm}) Maximum Durability : {cyn}{tslots}\r\n\
         {grn}H{nrm}) Permanent Affects : {cyn}{aff}\r\n\
         {grn}K{nrm}) Intended Class : {cyn}{class}\r\n\
         {grn}L{nrm}) Level       : {cyn}{level}\r\n\
         {grn}S{nrm}) Script      : {cyn}{script}\r\n\
         {grn}Q{nrm}) Quit\r\n\
         Enter choice : ",
        cyn = CYN, nrm = NRM, grn = GRN, yel = YEL,
        vnum = o.vnum, name = o.name, sdesc = o.short_description, ldesc = o.description,
        adesc = act_desc, type = type_str, extra = extra_str, wear = wear_str,
        weight = o.weight, cost = o.cost, rent = o.rent, timer = o.timer,
        v0 = o.values[0], v1 = o.values[1], v2 = o.values[2], v3 = o.values[3],
        cslots = o.cslots, tslots = o.tslots, aff = aff_str, class = class_str, level = o.level,
        script = if crate::dg_db_scripts::proto_trigger_vnums(crate::dg_handler::OBJ_TRIGGER, o.vnum).is_empty() {
            "Not Set."
        } else {
            "Set."
        },
    );
    send(g, conn, &body);
    let _ = with_state(conn, |s| s.mode = OeditMode::MainMenu);
}

// Two-column bit menu (type/extra/wear/affect tables; offset = first label's
// menu index, lower = lowest valid choice).
fn disp_bit_menu(g: &mut GameState, conn: ConnId, table: &[&str], one_based: bool) {
    let mut out = String::new();
    let mut columns = 0;
    for (i, name) in table.iter().take(real_len(table)).enumerate() {
        columns += 1;
        let n = if one_based { i + 1 } else { i };
        out.push_str(&format!(
            "{grn}{n:2}{nrm}) {name:<20.20} {sep}",
            grn = GRN,
            nrm = NRM,
            n = n,
            name = name,
            sep = if columns % 2 == 0 { "\r\n" } else { "" },
        ));
    }
    send(g, conn, &out);
}

fn disp_type_menu(g: &mut GameState, conn: ConnId) {
    disp_bit_menu(g, conn, ITEM_TYPES, false);
    send(g, conn, "\r\nEnter object type : ");
    let _ = with_state(conn, |s| s.mode = OeditMode::Type);
}

fn disp_extra_menu(g: &mut GameState, conn: ConnId) {
    let cur = with_state(conn, |s| s.obj.extra_flags.bits() as i64).unwrap_or(0);
    disp_bit_menu(g, conn, EXTRA_BITS, true);
    send(
        g,
        conn,
        &format!(
            "\r\nObject flags: {cyn}{f}{nrm}\r\nEnter object extra flag (0 to quit) : ",
            cyn = CYN,
            nrm = NRM,
            f = olc::sprintbit(cur, EXTRA_BITS),
        ),
    );
}

fn disp_wear_menu(g: &mut GameState, conn: ConnId) {
    let cur = with_state(conn, |s| s.obj.wear_flags.bits() as i64).unwrap_or(0);
    disp_bit_menu(g, conn, WEAR_BITS, true);
    send(
        g,
        conn,
        &format!(
            "\r\nWear flags: {cyn}{f}{nrm}\r\nEnter wear flag, 0 to quit : ",
            cyn = CYN,
            nrm = NRM,
            f = olc::sprintbit(cur, WEAR_BITS),
        ),
    );
}

fn disp_perm_affects_menu(g: &mut GameState, conn: ConnId) {
    let cur = with_state(conn, |s| s.obj.bitvector).unwrap_or(0);
    disp_bit_menu(g, conn, AFFECTED_BITS, true);
    send(
        g,
        conn,
        &format!(
            "\r\nObject permanent affects: {cyn}{f}{nrm}\r\nEnter object affect (0 to quit) : ",
            cyn = CYN,
            nrm = NRM,
            f = olc::sprintbit(cur, AFFECTED_BITS),
        ),
    );
}

fn disp_spells_menu(g: &mut GameState, conn: ConnId) {
    // The spell name table is in spell_parser; render a generic numbered list
    // 0..NUM_SPELLS using skill_name where available.
    let mut out = String::new();
    let mut columns = 0;
    for i in 0..NUM_SPELLS {
        columns += 1;
        let name = crate::spell_parser::skill_name(i);
        out.push_str(&format!(
            "{grn}{n:2}{nrm}) {yel}{name:<20.20} {sep}",
            grn = GRN,
            nrm = NRM,
            yel = YEL,
            n = i,
            name = name,
            sep = if columns % 3 == 0 { "\r\n" } else { "" },
        ));
    }
    out.push_str(&format!(
        "\r\n{nrm}Enter spell choice (0 for none) : ",
        nrm = NRM
    ));
    send(g, conn, &out);
}

fn disp_liquid_menu(g: &mut GameState, conn: ConnId) {
    let mut out = String::new();
    let mut columns = 0;
    for (i, name) in DRINKS.iter().take(real_len(DRINKS)).enumerate() {
        columns += 1;
        out.push_str(&format!(
            "{grn}{n:2}{nrm}) {yel}{name:<20.20} {sep}",
            grn = GRN,
            nrm = NRM,
            yel = YEL,
            n = i,
            name = name,
            sep = if columns % 2 == 0 { "\r\n" } else { "" },
        ));
    }
    out.push_str(&format!("\r\n{nrm}Enter drink type : ", nrm = NRM));
    send(g, conn, &out);
    let _ = with_state(conn, |s| s.mode = OeditMode::Value3);
}

fn disp_weapon_menu(g: &mut GameState, conn: ConnId) {
    // attack_hit_text[].singular — use a faithful DeltaMUD attack list.
    const ATTACKS: &[&str] = &[
        "hit", "sting", "whip", "slash", "bite", "bludgeon", "crush", "pound", "claw", "maul",
        "thrash", "pierce", "blast", "punch", "stab",
    ]; // fight.c:69-85 attack_hit_text[] - 15 entries, no 'shock' (#288)
    let mut out = String::new();
    let mut columns = 0;
    for (i, name) in ATTACKS.iter().enumerate() {
        columns += 1;
        out.push_str(&format!(
            "{grn}{n:2}{nrm}) {name:<20.20} {sep}",
            grn = GRN,
            nrm = NRM,
            n = i,
            name = name,
            sep = if columns % 2 == 0 { "\r\n" } else { "" },
        ));
    }
    out.push_str("\r\nEnter weapon type : ");
    send(g, conn, &out);
}

fn disp_container_flags_menu(g: &mut GameState, conn: ConnId) {
    let cur = with_state(conn, |s| s.obj.values[1] as i64).unwrap_or(0);
    let body = format!(
        "{grn}1{nrm}) CLOSEABLE\r\n\
         {grn}2{nrm}) PICKPROOF\r\n\
         {grn}3{nrm}) CLOSED\r\n\
         {grn}4{nrm}) LOCKED\r\n\
         Container flags: {cyn}{f}{nrm}\r\n\
         Enter flag, 0 to quit : ",
        grn = GRN,
        nrm = NRM,
        cyn = CYN,
        f = olc::sprintbit(cur, CONTAINER_BITS),
    );
    send(g, conn, &body);
}

fn disp_prompt_apply_menu(g: &mut GameState, conn: ConnId) {
    let affects = with_state(conn, |s| s.obj.affects).unwrap_or([(0, 0); MAX_OBJ_AFFECT]);
    let mut out = String::new();
    for (i, (loc, modi)) in affects.iter().enumerate() {
        if *modi != 0 {
            let ty = olc::sprinttype(*loc, APPLY_TYPES);
            out.push_str(&format!(
                " {grn}{n}{nrm}) {modi:+} to {ty}\r\n",
                grn = GRN,
                nrm = NRM,
                n = i + 1,
                modi = modi,
                ty = ty,
            ));
        } else {
            out.push_str(&format!(
                " {grn}{n}{nrm}) None.\r\n",
                grn = GRN,
                nrm = NRM,
                n = i + 1
            ));
        }
    }
    out.push_str(&format!(
        " {grn}{n}{nrm}) Autoset object applies.\r\nEnter affection to modify (0 to quit) : ",
        grn = GRN,
        nrm = NRM,
        n = MAX_OBJ_AFFECT + 1,
    ));
    send(g, conn, &out);
    let _ = with_state(conn, |s| s.mode = OeditMode::PromptApply);
}

fn disp_apply_menu(g: &mut GameState, conn: ConnId) {
    // C oedit.c:563: NUM_APPLIES = 22 - DAMAGE (the 23rd entry) is never
    // offered or accepted ('This is BOGUS, don't use it.') (#289).
    disp_bit_menu(g, conn, &APPLY_TYPES[..22], false);
    send(g, conn, "\r\nEnter apply type (0 is no apply) : ");
    let _ = with_state(conn, |s| s.mode = OeditMode::Apply);
}

fn disp_extradesc_menu(g: &mut GameState, conn: ConnId) {
    let (kw, desc, has_next) = match with_state(conn, |s| {
        let idx = s.cur_desc;
        let (k, d) = s
            .obj
            .extra_descriptions
            .get(idx)
            .cloned()
            .unwrap_or_default();
        let next = idx + 1 < s.obj.extra_descriptions.len();
        (
            if k.is_empty() {
                "<NONE>".to_string()
            } else {
                k
            },
            if d.is_empty() {
                "<NONE>".to_string()
            } else {
                d
            },
            next,
        )
    }) {
        Some(v) => v,
        None => return,
    };
    let next_str = if has_next { "Set." } else { "<Not set>\r\n" };
    let body = format!(
        "Extra desc menu\r\n\
         {grn}1{nrm}) Keyword: {yel}{kw}\r\n\
         {grn}2{nrm}) Description:\r\n{yel}{desc}\r\n\
         {grn}3{nrm}) Goto next description: {next}\r\n\
         {grn}0{nrm}) Quit\r\n\
         Enter choice : ",
        grn = GRN,
        nrm = NRM,
        yel = YEL,
        kw = kw,
        desc = desc,
        next = next_str,
    );
    send(g, conn, &body);
    let _ = with_state(conn, |s| s.mode = OeditMode::ExtradescMenu);
}

// ===========================================================================
// Type-aware value menus (oedit_disp_valN_menu).
// ===========================================================================
fn disp_val1_menu(g: &mut GameState, conn: ConnId) {
    let _ = with_state(conn, |s| s.mode = OeditMode::Value1);
    let t = with_state(conn, |s| s.obj.obj_type).unwrap_or(ObjectType::Other);
    match t {
        ObjectType::Light => disp_val3_menu(g, conn),
        ObjectType::Scroll | ObjectType::Wand | ObjectType::Staff | ObjectType::Potion => {
            send(g, conn, "Spell level : ");
        }
        ObjectType::Armor => send(g, conn, "Apply to Defense : "),
        ObjectType::Container => send(g, conn, "Max weight to contain : "),
        ObjectType::LiqContainer | ObjectType::Fountain => send(g, conn, "Max drink units : "),
        ObjectType::Food => send(g, conn, "Hours to fill stomach : "),
        ObjectType::Money => send(g, conn, "Number of gold coins : "),
        ObjectType::Note => {} // language, unused; falls to "modified" -> menu
        _ => disp_menu(g, conn),
    }
}

fn disp_val2_menu(g: &mut GameState, conn: ConnId) {
    let _ = with_state(conn, |s| s.mode = OeditMode::Value2);
    let t = with_state(conn, |s| s.obj.obj_type).unwrap_or(ObjectType::Other);
    match t {
        ObjectType::Scroll | ObjectType::Potion => disp_spells_menu(g, conn),
        ObjectType::Wand | ObjectType::Staff => send(g, conn, "Max number of charges : "),
        ObjectType::Weapon => send(g, conn, "Number of damage dice : "),
        ObjectType::Food => disp_val4_menu(g, conn),
        ObjectType::Container => disp_container_flags_menu(g, conn),
        ObjectType::LiqContainer | ObjectType::Fountain => send(g, conn, "Initial drink units : "),
        _ => disp_menu(g, conn),
    }
}

fn disp_val3_menu(g: &mut GameState, conn: ConnId) {
    let _ = with_state(conn, |s| s.mode = OeditMode::Value3);
    let t = with_state(conn, |s| s.obj.obj_type).unwrap_or(ObjectType::Other);
    match t {
        ObjectType::Light => {
            send(g, conn, "Number of hours (0 = burnt, -1 is infinite) : ");
        }
        ObjectType::Scroll | ObjectType::Potion => disp_spells_menu(g, conn),
        ObjectType::Wand | ObjectType::Staff => send(g, conn, "Number of charges remaining : "),
        ObjectType::Weapon => send(g, conn, "Size of damage dice : "),
        ObjectType::Container => {
            send(g, conn, "Vnum of key to open container (-1 for no key) : ");
        }
        ObjectType::LiqContainer | ObjectType::Fountain => disp_liquid_menu(g, conn),
        _ => disp_menu(g, conn),
    }
}

fn disp_val4_menu(g: &mut GameState, conn: ConnId) {
    let _ = with_state(conn, |s| s.mode = OeditMode::Value4);
    let t = with_state(conn, |s| s.obj.obj_type).unwrap_or(ObjectType::Other);
    match t {
        ObjectType::Scroll | ObjectType::Potion | ObjectType::Wand | ObjectType::Staff => {
            disp_spells_menu(g, conn);
        }
        ObjectType::Weapon => disp_weapon_menu(g, conn),
        ObjectType::LiqContainer | ObjectType::Fountain | ObjectType::Food => {
            send(g, conn, "Poisoned (0 = not poison) : ");
        }
        _ => disp_menu(g, conn),
    }
}

// ===========================================================================
// oedit_parse — per-line input handler.
// ===========================================================================
pub fn oedit_parse(g: &mut GameState, conn: ConnId, line: &str) {
    let mode = match with_state(conn, |s| s.mode) {
        Some(m) => m,
        None => return,
    };
    let arg = line.trim();

    match mode {
        OeditMode::ConfirmSave => match arg.chars().next().map(|c| c.to_ascii_lowercase()) {
            Some('y') => {
                send(g, conn, "Saving object to memory.\r\n");
                save_internally(g, conn);
                finish(g, conn);
            }
            Some('n') => finish(g, conn),
            _ => {
                send(g, conn, "Invalid choice!\r\n");
                send(g, conn, "Do you wish to save this object internally?\r\n");
            }
        },

        OeditMode::MainMenu => parse_main_menu(g, conn, arg),

        OeditMode::Namelist => {
            let v = if arg.is_empty() {
                "undefined".to_string()
            } else {
                arg.to_string()
            };
            with_state(conn, |s| {
                s.obj.name = v;
            });
            commit_and_menu(g, conn);
        }
        OeditMode::ShortDesc => {
            let v = if arg.is_empty() {
                "undefined".to_string()
            } else {
                arg.to_string()
            };
            with_state(conn, |s| {
                s.obj.short_description = v;
            });
            commit_and_menu(g, conn);
        }
        OeditMode::LongDesc => {
            let v = if arg.is_empty() {
                "undefined".to_string()
            } else {
                arg.to_string()
            };
            with_state(conn, |s| {
                s.obj.description = v;
            });
            commit_and_menu(g, conn);
        }
        OeditMode::ActDesc => {
            if let Some(result) = text_input(g, conn, line) {
                if let Some(text) = result {
                    with_state(conn, |s| {
                        s.obj.action_description = if text.is_empty() { None } else { Some(text) };
                        s.modified = true;
                    });
                }
                disp_menu(g, conn);
            }
        }

        OeditMode::Type => {
            let number: i32 = arg.parse().unwrap_or(-1);
            if number < 1 || number as usize >= real_len(ITEM_TYPES) {
                send(g, conn, "Invalid choice, try again : ");
            } else {
                with_state(conn, |s| s.obj.obj_type = ObjectType::from_i32(number));
                commit_and_menu(g, conn);
            }
        }

        OeditMode::Extras => {
            let number: i32 = arg.parse().unwrap_or(-1);
            if number < 0 || number as usize > real_len(EXTRA_BITS) {
                disp_extra_menu(g, conn);
            } else if number == 0 {
                commit_and_menu(g, conn);
            } else {
                with_state(conn, |s| {
                    let bit = 1u64 << (number - 1);
                    s.obj.extra_flags ^= ExtraFlags::from_bits_truncate(bit);
                    s.modified = true;
                });
                disp_extra_menu(g, conn);
            }
        }

        OeditMode::Wear => {
            let number: i32 = arg.parse().unwrap_or(-1);
            if number < 0 || number as usize > real_len(WEAR_BITS) {
                send(g, conn, "That's not a valid choice!\r\n");
                disp_wear_menu(g, conn);
            } else if number == 0 {
                commit_and_menu(g, conn);
            } else {
                with_state(conn, |s| {
                    let bit = 1u32 << (number - 1);
                    s.obj.wear_flags ^= WearFlags::from_bits_truncate(bit);
                    s.modified = true;
                });
                disp_wear_menu(g, conn);
            }
        }

        OeditMode::Weight => {
            let v: i32 = arg.parse().unwrap_or(0);
            with_state(conn, |s| s.obj.weight = v);
            commit_and_menu(g, conn);
        }
        OeditMode::Cost => {
            let v: i32 = arg.parse().unwrap_or(0);
            with_state(conn, |s| s.obj.cost = v);
            commit_and_menu(g, conn);
        }
        OeditMode::CostPerDay => {
            let v: i32 = arg.parse().unwrap_or(0);
            with_state(conn, |s| s.obj.rent = v);
            commit_and_menu(g, conn);
        }
        OeditMode::Timer => {
            let v: i32 = arg.parse().unwrap_or(0);
            with_state(conn, |s| s.obj.timer = v);
            commit_and_menu(g, conn);
        }
        OeditMode::Durability => {
            let v: i32 = arg.parse().unwrap_or(0);
            with_state(conn, |s| s.obj.cslots = v);
            commit_and_menu(g, conn);
        }
        OeditMode::Durability2 => {
            let v: i32 = arg.parse().unwrap_or(0);
            with_state(conn, |s| s.obj.tslots = v);
            commit_and_menu(g, conn);
        }
        OeditMode::Level => {
            let v: i32 = arg.parse().unwrap_or(0);
            with_state(conn, |s| s.obj.level = v);
            commit_and_menu(g, conn);
        }
        OeditMode::Class => {
            let v = class_str2num(arg);
            with_state(conn, |s| s.obj.obj_class = v);
            commit_and_menu(g, conn);
        }

        OeditMode::PermAffects => {
            let number: i32 = arg.parse().unwrap_or(-1);
            if number == 0 {
                commit_and_menu(g, conn);
            } else if number < 1 || number as usize > real_len(AFFECTED_BITS) {
                send(g, conn, "Invalid choice, try again : ");
            } else {
                with_state(conn, |s| {
                    s.obj.bitvector ^= 1i64 << (number - 1);
                    s.modified = true;
                });
                disp_perm_affects_menu(g, conn);
            }
        }

        OeditMode::Value1 => {
            let v: i32 = arg.parse().unwrap_or(0);
            with_state(conn, |s| s.obj.values[0] = v);
            disp_val2_menu(g, conn);
        }
        OeditMode::Value2 => parse_value2(g, conn, arg),
        OeditMode::Value3 => parse_value3(g, conn, arg),
        OeditMode::Value4 => parse_value4(g, conn, arg),

        OeditMode::PromptApply => {
            let number: i32 = arg.parse().unwrap_or(-1);
            if number == 0 {
                commit_and_menu(g, conn);
            } else if number < 0 || number as usize > MAX_OBJ_AFFECT + 1 {
                disp_prompt_apply_menu(g, conn);
            } else if number as usize == MAX_OBJ_AFFECT + 1 {
                autoset_applies(g, conn);
                disp_prompt_apply_menu(g, conn);
            } else {
                with_state(conn, |s| s.apply_slot = (number - 1) as usize);
                disp_apply_menu(g, conn);
            }
        }
        OeditMode::Apply => {
            let number: i32 = arg.parse().unwrap_or(-1);
            if number == 0 {
                with_state(conn, |s| {
                    let slot = s.apply_slot;
                    s.obj.affects[slot] = (0, 0);
                });
                disp_prompt_apply_menu(g, conn);
            } else if number < 0 || number as usize >= 22 { // NUM_APPLIES (#289)
                disp_apply_menu(g, conn);
            } else {
                with_state(conn, |s| {
                    let slot = s.apply_slot;
                    s.obj.affects[slot].0 = number;
                });
                send(g, conn, "Modifier : ");
                let _ = with_state(conn, |s| s.mode = OeditMode::ApplyMod);
            }
        }
        OeditMode::ApplyMod => {
            let v: i32 = arg.parse().unwrap_or(0);
            with_state(conn, |s| {
                let slot = s.apply_slot;
                s.obj.affects[slot].1 = v;
            });
            disp_prompt_apply_menu(g, conn);
        }

        OeditMode::ExtradescKey => {
            let v = if arg.is_empty() {
                "undefined".to_string()
            } else {
                arg.to_string()
            };
            with_state(conn, |s| {
                let idx = s.cur_desc;
                if let Some(ed) = s.obj.extra_descriptions.get_mut(idx) {
                    ed.0 = v;
                }
            });
            disp_extradesc_menu(g, conn);
        }
        OeditMode::ExtradescDescription => {
            if let Some(result) = text_input(g, conn, line) {
                if let Some(text) = result {
                    with_state(conn, |s| {
                        let idx = s.cur_desc;
                        if let Some(ed) = s.obj.extra_descriptions.get_mut(idx) {
                            ed.1 = text;
                        }
                        s.modified = true;
                    });
                }
                disp_extradesc_menu(g, conn);
            }
        }
        OeditMode::ExtradescMenu => parse_extradesc_menu(g, conn, arg),
        OeditMode::Script(mut script_mode) => {
            let vnum = with_state(conn, |s| s.obj.vnum).unwrap_or(0);
            let keep = olc::dg_script_edit_parse(
                g,
                conn,
                crate::dg_handler::OBJ_TRIGGER,
                vnum,
                &mut script_mode,
                arg,
            );
            if keep {
                let _ = with_state(conn, |s| {
                    s.mode = OeditMode::Script(script_mode);
                    s.modified = true;
                });
            } else {
                disp_menu(g, conn);
            }
        }
    }
}

fn parse_main_menu(g: &mut GameState, conn: ConnId, arg: &str) {
    match arg.chars().next().map(|c| c.to_ascii_lowercase()) {
        Some('q') => {
            let modified = with_state(conn, |s| s.modified).unwrap_or(false);
            if modified {
                send(g, conn, "Do you wish to save this object internally? : ");
                let _ = with_state(conn, |s| s.mode = OeditMode::ConfirmSave);
            } else {
                finish(g, conn);
            }
        }
        Some('1') => {
            send(g, conn, "Enter namelist : ");
            set_mode(conn, OeditMode::Namelist);
        }
        Some('2') => {
            send(g, conn, "Enter short desc : ");
            set_mode(conn, OeditMode::ShortDesc);
        }
        Some('3') => {
            send(g, conn, "Enter long desc :-\r\n| ");
            set_mode(conn, OeditMode::LongDesc);
        }
        Some('4') => {
            let seed = with_state(conn, |s| {
                s.obj.action_description.clone().unwrap_or_default()
            })
            .unwrap_or_default();
            begin_text(g, conn, &seed, OeditMode::ActDesc);
        }
        Some('5') => disp_type_menu(g, conn),
        Some('6') => {
            disp_extra_menu(g, conn);
            set_mode(conn, OeditMode::Extras);
        }
        Some('7') => {
            disp_wear_menu(g, conn);
            set_mode(conn, OeditMode::Wear);
        }
        Some('8') => {
            send(g, conn, "Enter weight : ");
            set_mode(conn, OeditMode::Weight);
        }
        Some('9') => {
            send(g, conn, "Enter cost : ");
            set_mode(conn, OeditMode::Cost);
        }
        Some('a') => {
            send(g, conn, "Enter cost per day : ");
            set_mode(conn, OeditMode::CostPerDay);
        }
        Some('b') => {
            send(g, conn, "Enter timer : ");
            set_mode(conn, OeditMode::Timer);
        }
        Some('c') => {
            with_state(conn, |s| s.obj.values = [0; 4]);
            disp_val1_menu(g, conn);
        }
        Some('d') => disp_prompt_apply_menu(g, conn),
        Some('e') => {
            with_state(conn, |s| {
                if s.obj.extra_descriptions.is_empty() {
                    s.obj
                        .extra_descriptions
                        .push((String::new(), String::new()));
                }
                s.cur_desc = 0;
            });
            disp_extradesc_menu(g, conn);
        }
        Some('f') => {
            send(g, conn, "Enter numerator : ");
            set_mode(conn, OeditMode::Durability);
        }
        Some('g') => {
            send(g, conn, "Enter denominator : ");
            set_mode(conn, OeditMode::Durability2);
        }
        Some('h') => {
            let level = conn_char(g, conn)
                .and_then(|c| g.get_char(c))
                .map(|c| c.player.level)
                .unwrap_or(0);
            if level < LVL_GRGOD {
                send(g, conn, "Your level isn't high enough to do that...\r\n");
            } else {
                disp_perm_affects_menu(g, conn);
                set_mode(conn, OeditMode::PermAffects);
            }
        }
        Some('k') => {
            send(g, conn, "Enter Intended Class For Object : ");
            set_mode(conn, OeditMode::Class);
        }
        Some('l') => {
            send(g, conn, "Enter Minimum Level For Object : ");
            set_mode(conn, OeditMode::Level);
        }
        Some('s') => {
            let vnum = with_state(conn, |s| s.obj.vnum).unwrap_or(0);
            olc::dg_script_menu(g, conn, crate::dg_handler::OBJ_TRIGGER, vnum);
            set_mode(conn, OeditMode::Script(olc::DgScriptEditMode::Main));
        }
        _ => disp_menu(g, conn),
    }
}

fn parse_value2(g: &mut GameState, conn: ConnId, arg: &str) {
    let number: i32 = arg.parse().unwrap_or(0);
    let t = with_state(conn, |s| s.obj.obj_type).unwrap_or(ObjectType::Other);
    match t {
        ObjectType::Scroll | ObjectType::Potion => {
            if number < 0 || number >= NUM_SPELLS {
                disp_val2_menu(g, conn);
            } else {
                with_state(conn, |s| s.obj.values[1] = number);
                disp_val3_menu(g, conn);
            }
        }
        ObjectType::Container => {
            if number < 0 || number > 4 {
                disp_container_flags_menu(g, conn);
            } else if number != 0 {
                with_state(conn, |s| {
                    s.obj.values[1] ^= 1 << (number - 1);
                    s.modified = true;
                });
                disp_val2_menu(g, conn);
            } else {
                disp_val3_menu(g, conn);
            }
        }
        _ => {
            with_state(conn, |s| s.obj.values[1] = number);
            disp_val3_menu(g, conn);
        }
    }
}

fn parse_value3(g: &mut GameState, conn: ConnId, arg: &str) {
    let number: i32 = arg.parse().unwrap_or(0);
    let t = with_state(conn, |s| s.obj.obj_type).unwrap_or(ObjectType::Other);
    let (min_val, max_val) = match t {
        ObjectType::Scroll | ObjectType::Potion => (0, NUM_SPELLS - 1),
        // NOTE: C falls through ITEM_WEAPON into WAND/STAFF (missing break), so
        // weapon's val3 (dice size) ends up clamped 0..20 too — replicated.
        ObjectType::Weapon | ObjectType::Wand | ObjectType::Staff => (0, 20),
        ObjectType::LiqContainer | ObjectType::Fountain => (0, real_len(DRINKS) as i32 - 1),
        _ => (-32000, 32000),
    };
    let clamped = number.max(min_val).min(max_val);
    with_state(conn, |s| s.obj.values[2] = clamped);
    disp_val4_menu(g, conn);
}

fn parse_value4(g: &mut GameState, conn: ConnId, arg: &str) {
    let number: i32 = arg.parse().unwrap_or(0);
    let t = with_state(conn, |s| s.obj.obj_type).unwrap_or(ObjectType::Other);
    let (min_val, max_val) = match t {
        ObjectType::Scroll | ObjectType::Potion => (0, NUM_SPELLS - 1),
        ObjectType::Wand | ObjectType::Staff => (1, NUM_SPELLS - 1),
        ObjectType::Weapon => (0, NUM_ATTACK_TYPES - 1),
        _ => (-32000, 32000),
    };
    let clamped = number.max(min_val).min(max_val);
    with_state(conn, |s| s.obj.values[3] = clamped);
    commit_and_menu(g, conn);
}

fn parse_extradesc_menu(g: &mut GameState, conn: ConnId, arg: &str) {
    let number: i32 = arg.parse().unwrap_or(-1);
    match number {
        0 => {
            with_state(conn, |s| {
                let idx = s.cur_desc;
                if let Some(ed) = s.obj.extra_descriptions.get(idx) {
                    if ed.0.is_empty() || ed.1.is_empty() {
                        s.obj.extra_descriptions.remove(idx);
                    }
                }
            });
            commit_and_menu(g, conn);
        }
        1 => {
            send(g, conn, "Enter keywords, separated by spaces :-\r\n| ");
            set_mode(conn, OeditMode::ExtradescKey);
        }
        2 => {
            let seed = with_state(conn, |s| {
                let idx = s.cur_desc;
                s.obj
                    .extra_descriptions
                    .get(idx)
                    .map(|e| e.1.clone())
                    .unwrap_or_default()
            })
            .unwrap_or_default();
            begin_text(g, conn, &seed, OeditMode::ExtradescDescription);
        }
        3 => {
            let (complete, at_end) = with_state(conn, |s| {
                let idx = s.cur_desc;
                let cur = s.obj.extra_descriptions.get(idx);
                let complete = cur
                    .map(|e| !e.0.is_empty() && !e.1.is_empty())
                    .unwrap_or(false);
                let at_end = idx + 1 >= s.obj.extra_descriptions.len();
                (complete, at_end)
            })
            .unwrap_or((false, true));
            if complete {
                with_state(conn, |s| {
                    if at_end {
                        s.obj
                            .extra_descriptions
                            .push((String::new(), String::new()));
                    }
                    s.cur_desc += 1;
                });
            }
            disp_extradesc_menu(g, conn);
        }
        _ => disp_extradesc_menu(g, conn),
    }
}

fn set_mode(conn: ConnId, mode: OeditMode) {
    let _ = with_state(conn, |s| s.mode = mode);
}

/// After a simple field set: mark modified and redisplay the main menu.
fn commit_and_menu(g: &mut GameState, conn: ConnId) {
    with_state(conn, |s| s.modified = true);
    disp_menu(g, conn);
}

fn class_str2num(s: &str) -> i32 {
    for (i, name) in PC_CLASS_TYPES.iter().enumerate() {
        if crate::interpreter::is_abbrev(s, name) {
            return i as i32;
        }
    }
    CLASS_UNDEFINED
}

// autoset_obj_applies — set the five DeltaMUD combat applies from class/level.
fn autoset_applies(g: &mut GameState, conn: ConnId) {
    // class_strengths[6][5] (oedit.c). Index 0 = Generic, then the 5 PC classes.
    const STRENGTHS: [[i32; 5]; 6] = [
        [3, 3, 3, 3, 3],
        [1, 5, 1, 5, 4],
        [2, 3, 4, 4, 3],
        [4, 1, 4, 2, 5],
        [5, 1, 5, 2, 3],
        [2, 5, 2, 2, 5],
    ];
    let (level, class, name) = match with_state(conn, |s| {
        (
            s.obj.level,
            s.obj.obj_class,
            s.obj.short_description.clone(),
        )
    }) {
        Some(v) => v,
        None => return,
    };
    let _ = name;
    if level == 0 {
        send(
            g,
            conn,
            "Object's minimum level must be set before you can do this.\r\n",
        );
        return;
    }
    let class_name = if class == CLASS_UNDEFINED {
        "Generic Character".to_string()
    } else {
        PC_CLASS_TYPES
            .get(class as usize)
            .copied()
            .unwrap_or("Generic Character")
            .to_string()
    };
    send(
        g,
        conn,
        &format!(
            "Setting object's standard applies based off of a level {} {}.\r\n",
            level, class_name
        ),
    );
    let row = STRENGTHS[(class + 1) as usize];
    // CLASS_APPMODNUM(num) = 750 * strength * level / 5 / 100 / NUM_WEARS
    let modnum = |num: usize| -> i32 { 750 * row[num] * level / 5 / 100 / (NUM_WEARS as i32) };
    use crate::flags::{APPLY_DEFENSE, APPLY_MDEFENSE, APPLY_MPOWER, APPLY_POWER, APPLY_TECHNIQUE};
    with_state(conn, |s| {
        s.obj.affects[0] = (APPLY_POWER, modnum(0));
        s.obj.affects[1] = (APPLY_MPOWER, modnum(1));
        s.obj.affects[2] = (APPLY_DEFENSE, modnum(2));
        s.obj.affects[3] = (APPLY_MDEFENSE, modnum(3));
        s.obj.affects[4] = (APPLY_TECHNIQUE, modnum(4));
        s.modified = true;
    });
}

// ===========================================================================
// Save to memory: install the editable subset into obj_protos and live
// instances of this prototype.
// ===========================================================================
fn save_internally(g: &mut GameState, conn: ConnId) {
    let (znum, edit) = match with_state(conn, |s| (s.znum, s.obj.clone())) {
        Some(v) => v,
        None => return,
    };
    let proto = ObjectProto {
        vnum: edit.vnum,
        name: edit.name.clone(),
        short_desc: edit.short_description.clone(),
        description: edit.description.clone(),
        obj_type: edit.obj_type,
        wear_flags: edit.wear_flags,
        extra_flags: edit.extra_flags,
        weight: edit.weight,
        cost: edit.cost,
        rent: edit.rent,
        values: edit.values,
        curr_slots: edit.cslots,
        total_slots: edit.tslots,
        obj_class: edit.obj_class,
        min_level: edit.level,
        bitvector: edit.bitvector,
        action_description: edit.action_description.clone().unwrap_or_default(),
        affects: edit
            .affects
            .iter()
            .filter(|(loc, _)| *loc != crate::flags::APPLY_NONE)
            .map(|&(location, modifier)| crate::object::ObjectAffect { location, modifier })
            .collect(),
        ex_descriptions: edit.extra_descriptions.clone(),
    };
    g.obj_protos.insert(edit.vnum, proto);

    // Update live instances of this prototype (oedit_save_internally) — copy the
    // editable fields onto every object whose item_number matches.
    let live: Vec<ObjId> = g
        .objs
        .iter()
        .filter(|(_, o)| o.item_number == edit.vnum)
        .map(|(id, _)| *id)
        .collect();
    for oid in live {
        if let Some(o) = g.objs.get_mut(&oid) {
            o.name = edit.name.clone();
            o.short_description = edit.short_description.clone();
            o.description = edit.description.clone();
            o.action_description = edit.action_description.clone();
            o.obj_type = edit.obj_type;
            o.wear_flags = edit.wear_flags;
            o.extra_flags = edit.extra_flags;
            o.weight = edit.weight;
            o.cost = edit.cost;
            o.rent = edit.rent;
            o.timer = edit.timer;
            o.level = edit.level as Level;
            o.values = edit.values;
            o.affects = edit
                .affects
                .iter()
                .filter(|(_, m)| *m != 0)
                .map(|&(location, modifier)| crate::object::ObjectAffect { location, modifier })
                .collect();
        }
    }

    let zone_number = g
        .zones
        .get(znum)
        .map(|z| z.number)
        .unwrap_or(edit.vnum / 100);
    olc::olc_add_to_save_list(zone_number, olc::OLC_SAVE_OBJ);
}

/// abort: drop this conn's editor state without saving (player disconnected
/// mid-edit). Releases the per-conn working copy and text buffer so nothing
/// lingers until reboot. `olc::abort_editor` calls `olc::clear_active`.
pub fn abort(conn: ConnId) {
    states().lock().unwrap().remove(&conn);
    text_bufs().lock().unwrap().remove(&conn);
}

fn finish(g: &mut GameState, conn: ConnId) {
    states().lock().unwrap().remove(&conn);
    text_bufs().lock().unwrap().remove(&conn);
    olc::clear_active(conn);
    if let Some(ch) = conn_char(g, conn) {
        crate::act::act(
            g,
            "$n stops using OLC.",
            true,
            ch,
            None,
            crate::act::ActArg::None,
            crate::act::To::Room,
        );
    }
}

// ===========================================================================
// oedit_save_to_disk — rewrite a zone's .obj file (inverse of
// file_loader::load_object_file), with DeltaMUD's extension lines.
//
// Mirrors C oedit_save_to_disk (oedit.c). ObjectProto carries every field the
// loader round-trips: name/descs/type/flags/weight/cost/rent, values[0..3],
// curr_slots/total_slots (value[4]/[5]), obj_class, min_level, bitvector,
// the perm-affect (`A`) blocks, and the extra-description (`E`) blocks — so the
// full object is written back, not a lossy subset.
// ===========================================================================
pub fn oedit_save_to_disk(g: &mut GameState, zone_rnum: usize) {
    let (zone_number, top) = match g.zones.get(zone_rnum) {
        Some(z) => (z.number, z.top),
        None => return,
    };
    let start = zone_number * 100;

    let mut out = String::new();
    for vnum in start..=top {
        let proto = match g.obj_protos.get(&vnum) {
            Some(p) => p,
            None => continue,
        };
        out.push_str(&format!("#{}\n", vnum));
        out.push_str(if proto.name.is_empty() {
            "undefined"
        } else {
            &proto.name
        });
        out.push_str("~\n");
        out.push_str(if proto.short_desc.is_empty() {
            "undefined"
        } else {
            &proto.short_desc
        });
        out.push_str("~\n");
        out.push_str(if proto.description.is_empty() {
            "undefined"
        } else {
            &proto.description
        });
        out.push_str("~\n");
        // action description (4th tilde string; C strip_string'd, empty if none).
        out.push_str(&olc::strip_cr(&proto.action_description));
        out.push_str("~\n");

        // type extra wear
        out.push_str(&format!(
            "{} {} {}\n",
            obj_type_num(proto.obj_type),
            proto.extra_flags.bits(),
            proto.wear_flags.bits()
        ));
        // values (DeltaMUD writes 6 ints; values[4]/[5] = durability cslots/tslots).
        out.push_str(&format!(
            "{} {} {} {} {} {}\n",
            proto.values[0],
            proto.values[1],
            proto.values[2],
            proto.values[3],
            proto.curr_slots,
            proto.total_slots
        ));
        // weight cost rent
        out.push_str(&format!("{} {} {}\n", proto.weight, proto.cost, proto.rent));
        // DeltaMUD class line: loader reads obj_class = atoi(line+2) - 1.
        out.push_str(&format!("c {}\n", proto.obj_class + 1));

        // Min-level / perm-affect bitvector: emit only when non-default
        // (matching C oedit_save_to_disk's conditionals).
        if proto.min_level != 0 {
            out.push_str(&format!("L {}\n", proto.min_level));
        }
        if proto.bitvector != 0 {
            out.push_str(&format!("BV {}\n", proto.bitvector));
        }

        // DG object trigger attachments (C oedit.c:403
        // script_save_to_disk(fp, obj, OBJ_TRIGGER), after BV). The bindings
        // live in the dg proto-script map keyed (OBJ_TRIGGER, vnum); without
        // re-emitting them, any zone save strips every object's triggers.
        for tv in crate::dg_db_scripts::proto_trigger_vnums(1, vnum) {
            out.push_str(&format!("T {}\n", tv));
        }

        // Extra descriptions (`E` blocks): keyword~ then description~ (CR-stripped).
        for (kw, desc) in &proto.ex_descriptions {
            if kw.is_empty() || desc.is_empty() {
                continue;
            }
            out.push_str("E\n");
            out.push_str(kw);
            out.push_str("~\n");
            out.push_str(&olc::strip_cr(desc));
            out.push_str("~\n");
        }

        // Stat applies (`A` blocks): one per affect with a non-zero modifier.
        for aff in &proto.affects {
            if aff.modifier != 0 {
                out.push_str("A\n");
                out.push_str(&format!("{} {}\n", aff.location, aff.modifier));
            }
        }
    }
    out.push_str("$~\n");

    let path = std::path::Path::new(&g.config.lib_path)
        .join("world")
        .join("obj")
        .join(format!("{}.obj", zone_number));
    let tmp = path.with_extension("new");
    if std::fs::write(&tmp, &out).is_err() {
        log::warn!("OLC: cannot write {:?}", tmp);
        return;
    }
    let _ = std::fs::remove_file(&path);
    if std::fs::rename(&tmp, &path).is_err() {
        log::warn!("OLC: cannot rename {:?} -> {:?}", tmp, path);
    }

    olc::olc_remove_from_save_list(zone_number, olc::OLC_SAVE_OBJ);
}
