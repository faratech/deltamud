// cformat.rs — byte-exact codecs for the C MUD's raw-struct persistence
// files (issue #95). Layouts were verified EMPIRICALLY by compiling an
// offsetof/sizeof probe against the real headers (gcc -std=gnu89):
//
//   rent_info          56 B  time@0 rentcode@4 net_cost@8 gold@12
//                            account@16 nitems@20 spare0..7@24
//   obj_file_elem      80 B  item_number@0(long) locate@8(long)
//                            curr_slots@16 total_slots@20 value@24[16]
//                            extra_flags@40 weight@44 timer@48 pad@52
//                            bitvector@56(long) min_level@64
//                            affected[6]@68 (byte location, sbyte modifier)
//   house_control_rec 928 B  vnum@0(long) atrium@8(long) exit_num@16(long)
//                            built_on@24(long) mode@32 pad@36 owner@40(long)
//                            num_of_guests@48 pad@52 guests@56(long[100])
//                            last_payment@856(long) spare0..7@864(long[8])
//   clan_info         304 B  number@0 members@4 ranks@8 privilege@12(int[6])
//                            clan_room@36 pad@40 gold@40(long)... (gold is
//                            at 40, 8-aligned), rank_name@48(char[9][20])
//                            leader@228(char[23]) name@251(char[32])
//                            who_name@283(char[16]) tail pad -> 304
//   board_msginfo      32 B  slot_num@0 pad@4 heading(ptr, dead)@8 level@16
//                            heading_len@20 message_len@24 pad@28
//
// All little-endian x86-64 LP64. Records are plain LE byte packing — no
// unsafe, no #[repr(C)].

use crate::object::{ExtraFlags, ObjectAffect, ObjectType, WearFlags};
use std::path::Path;

pub const C_MAX_OBJ_AFFECT: usize = 6;
pub const C_MAX_GUESTS: usize = 100;
pub const C_MAX_HOUSES: usize = 100;
pub const C_MAX_RANK_NAME_ROWS: usize = 9;
pub const C_MAX_CLANS: usize = 300;
pub const C_MAX_BOARD_MESSAGES: usize = 60;
/// structs.h:583 MAX_ROOM_VNUM - the C loader stops at this vnum.
pub const C_MAX_ROOM_VNUM: i64 = 500000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PersistenceFormat {
    #[default]
    Rust,
    C,
}

/// Selection for a brand-new or intrinsically ambiguous empty runtime file.
/// Once a non-empty file is detected, callers retain that detected format.
pub fn default_persistence_format() -> PersistenceFormat {
    if std::env::var("MUD_CFORMAT_FILES")
        .map(|value| value.eq_ignore_ascii_case("true") || value == "1")
        .unwrap_or(false)
    {
        PersistenceFormat::C
    } else {
        PersistenceFormat::Rust
    }
}

// ---- little-endian primitives ---------------------------------------------

fn put_i32(out: &mut Vec<u8>, v: i32) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn put_i64(out: &mut Vec<u8>, v: i64) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn put_u8(out: &mut Vec<u8>, v: u8) {
    out.push(v);
}
fn put_i8(out: &mut Vec<u8>, v: i8) {
    out.push(v as u8);
}
/// Fixed NUL-padded char field; copies at most `width` bytes like C's
/// `strncpy`. A source that fills the field exactly is intentionally not
/// terminated, matching the raw struct layout.
fn put_char_field(out: &mut Vec<u8>, s: &str, width: usize) {
    if width == 0 {
        return;
    }
    let bytes = s.as_bytes();
    let n = bytes.len().min(width);
    out.extend_from_slice(&bytes[..n]);
    out.resize(out.len() + (width - n), 0);
}

/// Atomically replace a persistence file with durable bytes. All runtime
/// stores use a sibling temporary file so rename stays on the same filesystem.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Phase 3: route every C-format runtime-file save through the canonical
    // durable publication layer (unique sibling temp, file fsync, rename,
    // parent-directory fsync) instead of the old guessable `.tmp` + rename.
    crate::durable::replace(path, bytes)
}

fn get_i32(src: &[u8], off: usize) -> Option<i32> {
    if off + 4 > src.len() {
        return None;
    }
    Some(i32::from_le_bytes(src[off..off + 4].try_into().ok()?))
}
fn get_i64(src: &[u8], off: usize) -> Option<i64> {
    if off + 8 > src.len() {
        return None;
    }
    Some(i64::from_le_bytes(src[off..off + 8].try_into().ok()?))
}
fn get_char_field(src: &[u8], off: usize, width: usize) -> Option<String> {
    if off + width > src.len() {
        return None;
    }
    let end = src[off..off + width]
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(width);
    Some(String::from_utf8_lossy(&src[off..off + end]).into_owned())
}

// ---- rent_info (56 B) -------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CRentInfo {
    pub time: i32,
    pub rentcode: i32,
    pub net_cost_per_diem: i32,
    pub gold: i32,
    pub account: i32,
    pub nitems: i32,
}

pub const C_RENT_INFO_SIZE: usize = 56;

pub fn encode_rent_info(r: &CRentInfo) -> Vec<u8> {
    let mut out = Vec::with_capacity(C_RENT_INFO_SIZE);
    put_i32(&mut out, r.time);
    put_i32(&mut out, r.rentcode);
    put_i32(&mut out, r.net_cost_per_diem);
    put_i32(&mut out, r.gold);
    put_i32(&mut out, r.account);
    put_i32(&mut out, r.nitems);
    for _ in 0..8 {
        put_i32(&mut out, 0); // spare0..7
    }
    debug_assert_eq!(out.len(), C_RENT_INFO_SIZE);
    out
}

pub fn decode_rent_info(src: &[u8]) -> Option<CRentInfo> {
    if src.len() < C_RENT_INFO_SIZE {
        return None;
    }
    Some(CRentInfo {
        time: get_i32(src, 0)?,
        rentcode: get_i32(src, 4)?,
        net_cost_per_diem: get_i32(src, 8)?,
        gold: get_i32(src, 12)?,
        account: get_i32(src, 16)?,
        nitems: get_i32(src, 20)?,
    })
}

// ---- obj_file_elem (80 B) ---------------------------------------------------

#[derive(Debug, Clone)]
pub struct CObjFileElem {
    pub item_number: i64,
    pub locate: i64,
    pub curr_slots: i32,
    pub total_slots: i32,
    pub value: [i32; 4],
    pub extra_flags: i32,
    pub weight: i32,
    pub timer: i32,
    pub bitvector: i64,
    pub min_level: i32,
    /// (location, modifier) pairs.
    pub affected: [(u8, i8); C_MAX_OBJ_AFFECT],
}

pub const C_OBJ_FILE_ELEM_SIZE: usize = 80;

pub fn encode_obj_file_elem(e: &CObjFileElem) -> Vec<u8> {
    let mut out = Vec::with_capacity(C_OBJ_FILE_ELEM_SIZE);
    put_i64(&mut out, e.item_number);
    put_i64(&mut out, e.locate);
    put_i32(&mut out, e.curr_slots);
    put_i32(&mut out, e.total_slots);
    for v in &e.value {
        put_i32(&mut out, *v);
    }
    put_i32(&mut out, e.extra_flags);
    put_i32(&mut out, e.weight);
    put_i32(&mut out, e.timer);
    put_i32(&mut out, 0); // pad @52
    put_i64(&mut out, e.bitvector);
    put_i32(&mut out, e.min_level);
    for (loc, modifier) in &e.affected {
        put_u8(&mut out, *loc);
        put_i8(&mut out, *modifier);
    }
    debug_assert_eq!(out.len(), C_OBJ_FILE_ELEM_SIZE);
    out
}

pub fn decode_obj_file_elem(src: &[u8]) -> Option<CObjFileElem> {
    if src.len() < C_OBJ_FILE_ELEM_SIZE {
        return None;
    }
    let mut value = [0i32; 4];
    for (i, v) in value.iter_mut().enumerate() {
        *v = get_i32(src, 24 + i * 4)?;
    }
    let mut affected = [(0u8, 0i8); C_MAX_OBJ_AFFECT];
    for (i, a) in affected.iter_mut().enumerate() {
        let off = 68 + i * 2;
        a.0 = *src.get(off)?;
        a.1 = *src.get(off + 1)? as i8;
    }
    Some(CObjFileElem {
        item_number: get_i64(src, 0)?,
        locate: get_i64(src, 8)?,
        curr_slots: get_i32(src, 16)?,
        total_slots: get_i32(src, 20)?,
        value,
        extra_flags: get_i32(src, 40)?,
        weight: get_i32(src, 44)?,
        timer: get_i32(src, 48)?,
        bitvector: get_i64(src, 56)?,
        min_level: get_i32(src, 64)?,
        affected,
    })
}

// ---- house_control_rec (928 B) ----------------------------------------------

#[derive(Debug, Clone)]
pub struct CHouseControlRec {
    pub vnum: i64,
    pub atrium: i64,
    pub exit_num: i64,
    pub built_on: i64,
    pub mode: i32,
    pub owner: i64,
    pub guests: Vec<i64>,
    pub last_payment: i64,
}

pub const C_HOUSE_CONTROL_REC_SIZE: usize = 928;

pub fn encode_house_control_rec(h: &CHouseControlRec) -> Vec<u8> {
    let mut out = Vec::with_capacity(C_HOUSE_CONTROL_REC_SIZE);
    put_i64(&mut out, h.vnum);
    put_i64(&mut out, h.atrium);
    put_i64(&mut out, h.exit_num);
    put_i64(&mut out, h.built_on);
    put_i32(&mut out, h.mode);
    put_i32(&mut out, 0); // pad @36
    put_i64(&mut out, h.owner);
    put_i32(&mut out, h.guests.len() as i32); // num_of_guests
    put_i32(&mut out, 0); // pad @52
    for i in 0..C_MAX_GUESTS {
        put_i64(&mut out, h.guests.get(i).copied().unwrap_or(0));
    }
    put_i64(&mut out, h.last_payment);
    for _ in 0..8 {
        put_i64(&mut out, 0); // spare0..7
    }
    debug_assert_eq!(out.len(), C_HOUSE_CONTROL_REC_SIZE);
    out
}

pub fn decode_house_control_rec(src: &[u8]) -> Option<CHouseControlRec> {
    if src.len() < C_HOUSE_CONTROL_REC_SIZE {
        return None;
    }
    let num_guests = get_i32(src, 48)?.max(0) as usize;
    let mut guests = Vec::with_capacity(num_guests.min(C_MAX_GUESTS));
    for i in 0..num_guests.min(C_MAX_GUESTS) {
        guests.push(get_i64(src, 56 + i * 8)?);
    }
    Some(CHouseControlRec {
        vnum: get_i64(src, 0)?,
        atrium: get_i64(src, 8)?,
        exit_num: get_i64(src, 16)?,
        built_on: get_i64(src, 24)?,
        mode: get_i32(src, 32)?,
        owner: get_i64(src, 40)?,
        guests,
        last_payment: get_i64(src, 856)?,
    })
}

// ---- clan_info (304 B) ------------------------------------------------------

#[derive(Debug, Clone)]
pub struct CClanInfo {
    pub number: i32,
    pub members: i32,
    pub ranks: i32,
    pub privilege: [i32; 6],
    pub clan_room: i32,
    pub gold: i64,
    /// 9 rows x 20 chars.
    pub rank_name: Vec<String>,
    pub leader: String,
    pub name: String,
    pub who_name: String,
}

pub const C_CLAN_INFO_SIZE: usize = 304;

pub fn encode_clan_info(c: &CClanInfo) -> Vec<u8> {
    let mut out = Vec::with_capacity(C_CLAN_INFO_SIZE);
    put_i32(&mut out, c.number); // @0
    put_i32(&mut out, c.members); // @4
    put_i32(&mut out, c.ranks); // @8
    for i in 0..6 {
        put_i32(&mut out, c.privilege.get(i).copied().unwrap_or(0));
    } // @12..36
    put_i32(&mut out, c.clan_room); // @36..40
    put_i64(&mut out, c.gold); // @40..48 (8-aligned)
    for row in 0..C_MAX_RANK_NAME_ROWS {
        put_char_field(
            &mut out,
            c.rank_name.get(row).map(|s| s.as_str()).unwrap_or(""),
            20,
        );
    } // @48..228
    put_char_field(&mut out, &c.leader, 23); // @228..251
    put_char_field(&mut out, &c.name, 32); // @251..283
    put_char_field(&mut out, &c.who_name, 16); // @283..299
    out.resize(C_CLAN_INFO_SIZE, 0); // tail pad to 304
    debug_assert_eq!(out.len(), C_CLAN_INFO_SIZE);
    out
}

pub fn decode_clan_info(src: &[u8]) -> Option<CClanInfo> {
    if src.len() < C_CLAN_INFO_SIZE {
        return None;
    }
    let mut privilege = [0i32; 6];
    for (i, p) in privilege.iter_mut().enumerate() {
        *p = get_i32(src, 12 + i * 4)?;
    }
    let mut rank_name = Vec::with_capacity(C_MAX_RANK_NAME_ROWS);
    for row in 0..C_MAX_RANK_NAME_ROWS {
        rank_name.push(get_char_field(src, 48 + row * 20, 20)?);
    }
    Some(CClanInfo {
        number: get_i32(src, 0)?,
        members: get_i32(src, 4)?,
        ranks: get_i32(src, 8)?,
        privilege,
        clan_room: get_i32(src, 36)?,
        gold: get_i64(src, 40)?,
        rank_name,
        leader: get_char_field(src, 228, 23)?,
        name: get_char_field(src, 251, 32)?,
        who_name: get_char_field(src, 283, 16)?,
    })
}

// ---- board_msginfo (32 B) + blob --------------------------------------------

#[derive(Debug, Clone)]
pub struct CBoardMsg {
    pub slot_num: i32,
    pub level: i32,
    pub heading: Vec<u8>,
    pub message: Vec<u8>,
}

pub const C_BOARD_MSGINFO_SIZE: usize = 32;

pub fn encode_board_msg(m: &CBoardMsg) -> Vec<u8> {
    let mut out = Vec::with_capacity(C_BOARD_MSGINFO_SIZE + m.heading.len() + m.message.len());
    put_i32(&mut out, m.slot_num);
    put_i32(&mut out, 0); // pad @4
    put_i64(&mut out, 0); // heading pointer field - dead on read (boards.h)
    put_i32(&mut out, m.level);
    put_i32(&mut out, m.heading.len() as i32);
    put_i32(&mut out, m.message.len() as i32);
    put_i32(&mut out, 0); // pad @28
    debug_assert_eq!(out.len(), C_BOARD_MSGINFO_SIZE);
    out.extend_from_slice(&m.heading);
    out.extend_from_slice(&m.message);
    out
}

pub fn decode_board_msg(src: &[u8]) -> Option<(CBoardMsg, usize)> {
    if src.len() < C_BOARD_MSGINFO_SIZE {
        return None;
    }
    let slot_num = get_i32(src, 0)?;
    let level = get_i32(src, 16)?;
    let hlen = get_i32(src, 20)?.max(0) as usize;
    let mlen = get_i32(src, 24)?.max(0) as usize;
    let mut off = C_BOARD_MSGINFO_SIZE;
    if hlen > src.len().saturating_sub(off) {
        return None;
    }
    let heading = src[off..off + hlen].to_vec();
    off += hlen;
    if mlen > src.len().saturating_sub(off) {
        return None;
    }
    let message = src[off..off + mlen].to_vec();
    off += mlen;
    Some((
        CBoardMsg {
            slot_num,
            level,
            heading,
            message,
        },
        off,
    ))
}

// ---- file-level (de)serialization -------------------------------------------

/// C rent_info header + obj_file_elem records for one plrobjs rent/crash file.
pub fn encode_rent_file(rent: &CRentInfo, objs: &[CObjFileElem]) -> Vec<u8> {
    let mut out = encode_rent_info(rent);
    for o in objs {
        out.extend_from_slice(&encode_obj_file_elem(o));
    }
    out
}

/// Returns (rent, elems) or None when the buffer is not a C rent file.
pub fn decode_rent_file(src: &[u8]) -> Option<(CRentInfo, Vec<CObjFileElem>)> {
    if src.len() < C_RENT_INFO_SIZE || (src.len() - C_RENT_INFO_SIZE) % C_OBJ_FILE_ELEM_SIZE != 0 {
        return None;
    }
    let rent = decode_rent_info(src)?;
    let mut elems = Vec::new();
    let mut off = C_RENT_INFO_SIZE;
    // The C writers do not initialise nitems on every save path; Crash_load
    // reads records until EOF. Do the same rather than truncating to garbage.
    while off + C_OBJ_FILE_ELEM_SIZE <= src.len() {
        elems.push(decode_obj_file_elem(&src[off..])?);
        off += C_OBJ_FILE_ELEM_SIZE;
    }
    Some((rent, elems))
}

/// lib/etc/hcontrol: a bare array of house_control_rec records.
pub fn decode_hcontrol(src: &[u8]) -> Option<Vec<CHouseControlRec>> {
    if src.len() % C_HOUSE_CONTROL_REC_SIZE != 0
        || src.len() / C_HOUSE_CONTROL_REC_SIZE > C_MAX_HOUSES
    {
        return None;
    }
    src.chunks_exact(C_HOUSE_CONTROL_REC_SIZE)
        .map(decode_house_control_rec)
        .collect()
}

pub fn encode_hcontrol(houses: &[CHouseControlRec]) -> Vec<u8> {
    let mut out = Vec::with_capacity(houses.len() * C_HOUSE_CONTROL_REC_SIZE);
    for h in houses {
        out.extend_from_slice(&encode_house_control_rec(h));
    }
    out
}

/// lib/etc/clans.dat: i32 count + count × clan_info records.
pub fn decode_clans_dat(src: &[u8]) -> Option<Vec<CClanInfo>> {
    if src.len() < 4 {
        return None;
    }
    let count = usize::try_from(get_i32(src, 0)?).ok()?;
    if count > C_MAX_CLANS
        || src.len() != 4usize.checked_add(count.checked_mul(C_CLAN_INFO_SIZE)?)?
    {
        return None;
    }
    let mut out = Vec::with_capacity(count);
    let mut off = 4;
    for _ in 0..count {
        out.push(decode_clan_info(&src[off..])?);
        off += C_CLAN_INFO_SIZE;
    }
    Some(out)
}

pub fn encode_clans_dat(clans: &[CClanInfo]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + clans.len() * C_CLAN_INFO_SIZE);
    put_i32(&mut out, clans.len() as i32);
    for c in clans {
        out.extend_from_slice(&encode_clan_info(c));
    }
    out
}

/// A bare sequence of obj_file_elem records (house object files).
pub fn decode_obj_file(src: &[u8]) -> Option<Vec<CObjFileElem>> {
    if src.len() % C_OBJ_FILE_ELEM_SIZE != 0 {
        return None;
    }
    src.chunks_exact(C_OBJ_FILE_ELEM_SIZE)
        .map(decode_obj_file_elem)
        .collect()
}

pub fn encode_obj_file(objs: &[CObjFileElem]) -> Vec<u8> {
    let mut out = Vec::with_capacity(objs.len() * C_OBJ_FILE_ELEM_SIZE);
    for obj in objs {
        out.extend_from_slice(&encode_obj_file_elem(obj));
    }
    out
}

/// boards.c Board_save_board: i32 count + count × (msginfo + heading + body).
pub fn encode_board_file(msgs: &[CBoardMsg]) -> Vec<u8> {
    let mut out = Vec::new();
    put_i32(&mut out, msgs.len() as i32);
    for m in msgs {
        out.extend_from_slice(&encode_board_msg(m));
    }
    out
}

pub fn decode_board_file(src: &[u8]) -> Option<Vec<CBoardMsg>> {
    if src.len() < 4 {
        return None;
    }
    let count = get_i32(src, 0)?.max(0) as usize;
    if count == 0 || count > C_MAX_BOARD_MESSAGES {
        // C: 'SYSERR: Board file corrupt.  Resetting.'
        return None;
    }
    let mut out = Vec::new();
    let mut off = 4;
    for _ in 0..count {
        let (m, used) = decode_board_msg(&src[off..])?;
        if m.heading.is_empty() || *m.heading.last()? != 0 {
            return None;
        }
        if !m.message.is_empty() && *m.message.last()? != 0 {
            return None;
        }
        out.push(m);
        off += used;
    }
    (off == src.len()).then_some(out)
}

// ---- helpers bridging to the Rust object model -------------------------------

/// Build a CObjFileElem from the live object model (ObjectProto-derived).
pub fn obj_to_c_elem(
    item_number: i64,
    locate: i64,
    curr_slots: i32,
    total_slots: i32,
    value: [i32; 4],
    extra_flags: i32,
    weight: i32,
    timer: i32,
    bitvector: i64,
    min_level: i32,
    affects: &[ObjectAffect],
) -> CObjFileElem {
    let mut affected = [(0u8, 0i8); C_MAX_OBJ_AFFECT];
    for (i, a) in affects.iter().take(C_MAX_OBJ_AFFECT).enumerate() {
        affected[i] = (a.location as u8, a.modifier as i8);
    }
    CObjFileElem {
        item_number,
        locate,
        curr_slots,
        total_slots,
        value,
        extra_flags,
        weight,
        timer,
        bitvector,
        min_level,
        affected,
    }
}

/// Convenience: map raw extra/wear bits into the typed enums (lossless for
/// the values the C writes).
pub fn extra_flags_from_raw(bits: i32) -> ExtraFlags {
    ExtraFlags::from_bits_truncate(bits as u64)
}
pub fn wear_flags_from_raw(bits: i32) -> WearFlags {
    WearFlags::from_bits_truncate(bits as u32)
}
pub fn obj_type_from_raw(bits: i32) -> ObjectType {
    ObjectType::from_i32(bits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rent_info_is_56_bytes_and_round_trips() {
        let r = CRentInfo {
            time: 123456,
            rentcode: 2,
            net_cost_per_diem: 1000,
            gold: 500,
            account: 42,
            nitems: 3,
        };
        let bytes = encode_rent_info(&r);
        assert_eq!(bytes.len(), C_RENT_INFO_SIZE);
        assert_eq!(decode_rent_info(&bytes).unwrap(), r);
    }

    #[test]
    fn obj_file_elem_is_80_bytes_and_round_trips() {
        // Verified against gcc offsetof probe: bitvector@56, min_level@64,
        // affected@68, sizeof 80.
        let e = obj_to_c_elem(
            1234, // item_number
            5,    // locate (wear slot)
            7,
            42,
            [1, 2, 3, 4],
            -559038737, // extra flags (negative int exercises sign bits)
            33,
            -1,
            0x4000_0000,
            17,
            &[
                ObjectAffect {
                    location: 1,
                    modifier: 3,
                },
                ObjectAffect {
                    location: 19,
                    modifier: -5,
                },
            ],
        );
        let bytes = encode_obj_file_elem(&e);
        assert_eq!(bytes.len(), C_OBJ_FILE_ELEM_SIZE);
        let back = decode_obj_file_elem(&bytes).unwrap();
        assert_eq!(back.item_number, 1234);
        assert_eq!(back.locate, 5);
        assert_eq!(back.curr_slots, 7);
        assert_eq!(back.total_slots, 42);
        assert_eq!(back.value, [1, 2, 3, 4]);
        assert_eq!(back.bitvector, 0x4000_0000);
        assert_eq!(back.min_level, 17);
        assert_eq!(back.affected[0], (1, 3));
        assert_eq!(back.affected[1], (19, -5));
    }

    #[test]
    fn house_control_rec_is_928_bytes_and_round_trips() {
        let h = CHouseControlRec {
            vnum: 30290,
            atrium: 30280,
            exit_num: 1,
            built_on: 1_700_000_000,
            mode: 0,
            owner: 7,
            guests: vec![11, 22, 33],
            last_payment: 1_700_000_100,
        };
        let bytes = encode_house_control_rec(&h);
        assert_eq!(bytes.len(), C_HOUSE_CONTROL_REC_SIZE);
        let back = decode_house_control_rec(&bytes).unwrap();
        assert_eq!(back.vnum, 30290);
        assert_eq!(back.atrium, 30280);
        assert_eq!(back.owner, 7);
        assert_eq!(back.guests, vec![11, 22, 33]);
        assert_eq!(back.last_payment, 1_700_000_100);
    }

    #[test]
    fn clan_info_is_304_bytes_and_round_trips() {
        let c = CClanInfo {
            number: 1,
            members: 12,
            ranks: 9,
            privilege: [1, 0, 1, 1, 0, 1],
            clan_room: 5400,
            gold: 123_456_789,
            rank_name: (0..9).map(|i| format!("Rank {}", i)).collect(),
            leader: "Mulder".into(),
            name: "The Wardens".into(),
            who_name: "Wardens".into(),
        };
        let bytes = encode_clan_info(&c);
        assert_eq!(bytes.len(), C_CLAN_INFO_SIZE);
        let back = decode_clan_info(&bytes).unwrap();
        assert_eq!(back.number, 1);
        assert_eq!(back.privilege[3], 1);
        assert_eq!(back.gold, 123_456_789);
        assert_eq!(back.rank_name[8], "Rank 8");
        assert_eq!(back.leader, "Mulder");
        assert_eq!(back.name, "The Wardens");
        assert_eq!(back.who_name, "Wardens");
    }

    #[test]
    fn board_file_round_trips_and_rejects_corrupt_counts() {
        let msgs = vec![
            CBoardMsg {
                slot_num: 0,
                level: 1,
                heading: b"Mon Jun 23 (Name)     :: hello\0".to_vec(),
                message: b"body line one\r\nbody line two\0".to_vec(),
            },
            CBoardMsg {
                slot_num: 1,
                level: 99,
                heading: b"Mon Jun 23 (Name)     :: god post\0".to_vec(),
                message: b"immortal only\0".to_vec(),
            },
        ];
        let bytes = encode_board_file(&msgs);
        let back = decode_board_file(&bytes).unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!(back[1].heading, msgs[1].heading);
        assert_eq!(back[0].message, msgs[0].message);
        // corrupt count
        assert!(decode_board_file(&i32::to_le_bytes(999)).is_none());
    }

    #[test]
    fn rent_file_round_trips_header_plus_records() {
        let rent = CRentInfo {
            time: 42,
            rentcode: 1,
            net_cost_per_diem: 0,
            gold: 0,
            account: 0,
            nitems: 2,
        };
        let e1 = obj_to_c_elem(10, 0, 1, 2, [0; 4], 0, 5, -1, 0, 0, &[]);
        let e2 = obj_to_c_elem(11, -1, 0, 3, [9; 4], 0, 1, -1, 1, 10, &[]);
        let bytes = encode_rent_file(&rent, &[e1, e2]);
        let (rent_back, elems) = decode_rent_file(&bytes).unwrap();
        assert_eq!(rent_back.rentcode, 1);
        assert_eq!(elems.len(), 2);
        assert_eq!(elems[1].value[0], 9);
    }

    #[test]
    fn exact_c_house_control_fixture_decodes_and_reencodes() {
        let mut fixture = vec![0u8; C_HOUSE_CONTROL_REC_SIZE];
        fixture[0..8].copy_from_slice(&30290i64.to_le_bytes());
        fixture[8..16].copy_from_slice(&30280i64.to_le_bytes());
        fixture[16..24].copy_from_slice(&1i64.to_le_bytes());
        fixture[24..32].copy_from_slice(&1_700_000_000i64.to_le_bytes());
        fixture[32..36].copy_from_slice(&1i32.to_le_bytes());
        fixture[40..48].copy_from_slice(&77i64.to_le_bytes());
        fixture[48..52].copy_from_slice(&2i32.to_le_bytes());
        fixture[56..64].copy_from_slice(&88i64.to_le_bytes());
        fixture[64..72].copy_from_slice(&99i64.to_le_bytes());
        fixture[856..864].copy_from_slice(&1_700_000_100i64.to_le_bytes());

        let records = decode_hcontrol(&fixture).expect("C hcontrol fixture");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].vnum, 30290);
        assert_eq!(records[0].guests, vec![88, 99]);
        assert_eq!(encode_hcontrol(&records), fixture);
    }

    #[test]
    fn exact_c_clan_fixture_decodes_and_reencodes() {
        let mut fixture = vec![0u8; 4 + C_CLAN_INFO_SIZE];
        fixture[0..4].copy_from_slice(&1i32.to_le_bytes());
        let record = &mut fixture[4..];
        record[0..4].copy_from_slice(&7i32.to_le_bytes());
        record[4..8].copy_from_slice(&12i32.to_le_bytes());
        record[8..12].copy_from_slice(&9i32.to_le_bytes());
        for (index, value) in [1i32, 2, 3, 4, 5, 6].into_iter().enumerate() {
            record[12 + index * 4..16 + index * 4].copy_from_slice(&value.to_le_bytes());
        }
        record[36..40].copy_from_slice(&5400i32.to_le_bytes());
        record[40..48].copy_from_slice(&123_456_789i64.to_le_bytes());
        record[48..54].copy_from_slice(b"Member");
        record[228..234].copy_from_slice(b"Mulder");
        record[251..262].copy_from_slice(b"The Wardens");
        record[283..290].copy_from_slice(b"Wardens");

        let clans = decode_clans_dat(&fixture).expect("C clans.dat fixture");
        assert_eq!(clans.len(), 1);
        assert_eq!(clans[0].rank_name[0], "Member");
        assert_eq!(clans[0].gold, 123_456_789);
        assert_eq!(encode_clans_dat(&clans), fixture);
    }

    #[test]
    fn exact_c_board_fixture_ignores_dead_pointer_and_writes_valid_zero_pointer() {
        let heading = b"Tue Sep  1 (Tester)     :: fixture\0";
        let body = b"raw body\r\n\0";
        let mut fixture = vec![0u8; 4 + C_BOARD_MSGINFO_SIZE];
        fixture[0..4].copy_from_slice(&1i32.to_le_bytes());
        fixture[4..8].copy_from_slice(&37i32.to_le_bytes());
        fixture[12..20].copy_from_slice(&0x1122_3344_5566_7788i64.to_le_bytes());
        fixture[20..24].copy_from_slice(&55i32.to_le_bytes());
        fixture[24..28].copy_from_slice(&(heading.len() as i32).to_le_bytes());
        fixture[28..32].copy_from_slice(&(body.len() as i32).to_le_bytes());
        fixture.extend_from_slice(heading);
        fixture.extend_from_slice(body);

        let msgs = decode_board_file(&fixture).expect("C board fixture");
        assert_eq!(msgs[0].slot_num, 37);
        assert_eq!(msgs[0].heading, heading);
        assert_eq!(msgs[0].message, body);

        let rewritten = encode_board_file(&msgs);
        assert_eq!(&rewritten[12..20], &[0; 8]);
        assert_eq!(decode_board_file(&rewritten).unwrap()[0].message, body);
    }

    #[test]
    fn exact_c_rent_fixture_reads_records_to_eof_despite_bad_nitems() {
        let mut fixture = vec![0u8; C_RENT_INFO_SIZE + C_OBJ_FILE_ELEM_SIZE];
        fixture[0..4].copy_from_slice(&1_700_000_000i32.to_le_bytes());
        fixture[4..8].copy_from_slice(&1i32.to_le_bytes());
        fixture[20..24].copy_from_slice(&(-77i32).to_le_bytes());
        let record = &mut fixture[C_RENT_INFO_SIZE..];
        record[0..8].copy_from_slice(&1234i64.to_le_bytes());
        record[8..16].copy_from_slice(&(-2i64).to_le_bytes());
        record[16..20].copy_from_slice(&7i32.to_le_bytes());
        record[20..24].copy_from_slice(&42i32.to_le_bytes());
        record[24..28].copy_from_slice(&11i32.to_le_bytes());
        record[40..44].copy_from_slice(&0x40i32.to_le_bytes());
        record[44..48].copy_from_slice(&33i32.to_le_bytes());
        record[48..52].copy_from_slice(&(-1i32).to_le_bytes());
        record[56..64].copy_from_slice(&0x4000_0000i64.to_le_bytes());
        record[64..68].copy_from_slice(&17i32.to_le_bytes());
        record[68] = 3;
        record[69] = (-5i8) as u8;

        let (rent, objects) = decode_rent_file(&fixture).expect("C rent fixture");
        assert_eq!(rent.nitems, -77);
        assert_eq!(objects.len(), 1);
        assert_eq!(objects[0].locate, -2);
        assert_eq!(objects[0].affected[0], (3, -5));
        assert_eq!(encode_rent_file(&rent, &objects), fixture);
    }
}
