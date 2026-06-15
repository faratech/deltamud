// Character entity (PC or NPC) — id-indexed, full CircleMUD field set.
//
// References to other entities are ids (CharId/ObjId/RoomRnum). The struct
// carries the complete char_data field set from structs.h so later batches
// (skills, conditions, clans, quests, immortal tools) never have to reshape
// it; Tier-0 leaves the extra fields at their defaults.

use crate::types::*;
use chrono::{DateTime, Utc};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, Default)]
pub struct Abilities {
    pub str: i8,
    pub str_add: i8,
    pub intel: i8,
    pub wis: i8,
    pub dex: i8,
    pub con: i8,
    pub cha: i8,
}

/// char_point_data (structs.h).
#[derive(Debug, Clone, Default)]
pub struct CharPoints {
    pub hit: i32,
    pub max_hit: i32,
    pub mana: i32,
    pub max_mana: i32,
    pub move_points: i32,
    pub max_move: i32,
    pub armor: ArmorClass,
    pub gold: Gold,
    pub bank_gold: Gold,
    pub exp: Experience,
    pub hitroll: Hitroll,
    pub damroll: Damroll,
    // DeltaMUD combat modifiers.
    pub power: i16,
    pub mpower: i16,
    pub defense: i16,
    pub mdefense: i16,
    pub technique: i16,
}

/// char_player_data (structs.h).
#[derive(Debug, Clone)]
pub struct PlayerData {
    pub name: String,
    pub title: Option<String>,
    pub description: String,
    pub sex: Gender,
    pub class: Class,
    pub race: Race,
    pub deity: u8,
    pub level: Level,
    pub hometown: RoomVnum,
    pub time_birth: i64,
    pub time_played: i64,
    pub weight: u8,
    pub height: u8,
}

/// affected_type (structs.h) + spell_caster (assessment fix, needed for
/// "X casts Y on Z" attribution).
#[derive(Debug, Clone)]
pub struct Affect {
    pub spell_type: i32,
    pub duration: i32,
    pub modifier: i32,
    pub location: i32,
    pub bitvector: i64,
    pub caster: Option<CharId>,
}

#[derive(Debug, Clone)]
pub struct Character {
    pub id: CharId,    // runtime arena id (per session)
    pub idnum: i64,    // persistent player id (player_main.idnum); -1 for mobs
    pub nr: MobVnum,   // mob prototype vnum, or NOBODY(-1) for players
    pub is_npc: bool,
    /// Owning connection (None for NPCs and link-dead PCs). Mirrors C ch->desc.
    pub desc: Option<ConnId>,

    // Location
    pub in_room: Option<RoomRnum>,
    pub was_in_room: Option<RoomRnum>,

    // Core data
    pub player: PlayerData,
    pub real_abils: Abilities,
    pub aff_abils: Abilities,
    pub points: CharPoints,

    // NPC display strings (None for PCs).
    pub short_desc: Option<String>,
    pub long_desc: Option<String>,
    pub npc_description: Option<String>,

    // Inventory & equipment
    pub carrying: Vec<ObjId>,
    pub equipment: [Option<ObjId>; NUM_WEARS],
    pub carry_weight: i32,
    pub carry_items: u8,

    // Status / combat
    pub position: Position,
    pub affected: Vec<Affect>,
    pub fighting: Option<CharId>,
    pub hunting: Option<CharId>,
    pub alignment: i32,
    pub timer: i32,
    pub idle_tics: i32,

    // Group / follow
    pub master: Option<CharId>,
    pub followers: Vec<CharId>,

    // Mount system (CircleMUD/DeltaMUD RIDING/RIDDEN_BY pointers).
    pub riding: Option<CharId>,    // the mount this char is riding
    pub ridden_by: Option<CharId>, // the rider sitting on this char

    // Snoop link (descriptor-level in C: ch->desc->snooping / ->snoop_by).
    // Stored on the character so it survives the per-conn lookup; both ends
    // point at the *character* on the other side of the link.
    pub snooping: Option<CharId>, // the char whose output this char sees
    pub snoop_by: Option<CharId>, // the char snooping this char's output

    // Flag bitvectors (DB long columns).
    pub act_flags: i64,    // ACT_* for NPCs / PLR_* for PCs
    pub affect_flags: i64, // AFF_*
    pub prf_flags: i64,    // PRF_* preferences
    pub prf2_flags: i64,

    // player_special_data_saved
    pub skills: HashMap<u16, u8>, // spell/skill number -> proficiency
    pub padding0: i32, // PADDING0 (was spells_to_learn); saved for C cross-compat
    pub talks: [bool; MAX_TONGUE],
    pub conditions: [i8; NUM_CONDITIONS],
    pub wimp_level: i32,
    pub freeze_level: Level,
    pub invis_level: i32,
    pub load_room: RoomVnum,
    pub bad_pws: u8,
    pub death_timer: u8,
    pub citizen: u8,
    pub training: u8,
    pub newbie: u8,
    pub arena: u8,
    pub spells_to_learn: i32,
    pub clan: i32,
    pub clan_rank: i32,
    pub recall_level: i32,
    pub retreat_level: i32,
    pub trust: i32,
    pub bail_amt: i32,
    pub wins: u8,
    pub losses: u8,
    pub godcmds1: i64,
    pub godcmds2: i64,
    pub godcmds3: i64,
    pub godcmds4: i64,
    pub mapx: i64,
    pub mapy: i64,
    pub buildmodezone: i64,
    pub buildmoderoom: i64,
    pub tloadroom: i64,
    // Quest scaffolding (DeltaMUD autoquest).
    pub quest_points: i32,
    pub next_quest: i32,
    pub quest_countdown: i32,
    pub quest_obj: i32,
    pub quest_mob: i32,

    // player_special_data (non-saved)
    pub poofin: Option<String>,
    pub poofout: Option<String>,
    pub email: Option<String>,
    pub last_tell: Option<CharId>,

    // Timestamps
    pub created_at: DateTime<Utc>,
    pub last_logon: DateTime<Utc>,
}

impl Character {
    fn blank(is_npc: bool, nr: MobVnum) -> Self {
        let now = Utc::now();
        Character {
            id: CharId(0),
            idnum: -1,
            nr,
            is_npc,
            desc: None,
            in_room: None,
            was_in_room: None,
            player: PlayerData {
                name: String::new(),
                title: None,
                description: String::new(),
                sex: Gender::Neutral,
                class: Class::Warrior,
                race: Race::Human,
                deity: 0,
                level: 1,
                hometown: 100,
                time_birth: 0,
                time_played: 0,
                weight: 150,
                height: 170,
            },
            real_abils: Abilities::default(),
            aff_abils: Abilities::default(),
            points: CharPoints::default(),
            short_desc: None,
            long_desc: None,
            npc_description: None,
            carrying: Vec::new(),
            equipment: [None; NUM_WEARS],
            carry_weight: 0,
            carry_items: 0,
            position: Position::Standing,
            affected: Vec::new(),
            fighting: None,
            hunting: None,
            alignment: 0,
            timer: 0,
            idle_tics: 0,
            master: None,
            followers: Vec::new(),
            riding: None,
            ridden_by: None,
            snooping: None,
            snoop_by: None,
            act_flags: 0,
            affect_flags: 0,
            prf_flags: 0,
            prf2_flags: 0,
            skills: HashMap::new(),
            padding0: 0,
            talks: [false; MAX_TONGUE],
            conditions: [-1; NUM_CONDITIONS],
            wimp_level: 0,
            freeze_level: 0,
            invis_level: 0,
            load_room: NOWHERE,
            bad_pws: 0,
            death_timer: 0,
            citizen: 0,
            training: 0,
            newbie: 0,
            arena: 0,
            spells_to_learn: 0,
            clan: 0,
            clan_rank: 0,
            recall_level: 0,
            retreat_level: 0,
            trust: 0,
            bail_amt: 0,
            wins: 0,
            losses: 0,
            godcmds1: 0,
            godcmds2: 0,
            godcmds3: 0,
            godcmds4: 0,
            mapx: 0,
            mapy: 0,
            buildmodezone: 0,
            buildmoderoom: 0,
            tloadroom: 0,
            quest_points: 0,
            next_quest: 0,
            quest_countdown: 0,
            quest_obj: 0,
            quest_mob: 0,
            poofin: None,
            poofout: None,
            email: None,
            last_tell: None,
            created_at: now,
            last_logon: now,
        }
    }

    pub fn new_player(name: String, class: Class, race: Race) -> Self {
        let mut ch = Character::blank(false, NOBODY);
        ch.player.name = name;
        ch.player.class = class;
        ch.player.race = race;
        ch.player.level = 1;
        // Default ability scores (proper class-weighted rolling lands with
        // class.c / do_start in Batch 6). These keep level-1 combat sane.
        ch.real_abils = Abilities { str: 16, str_add: 0, intel: 13, wis: 13, dex: 13, con: 14, cha: 13 };
        ch.aff_abils = ch.real_abils;
        ch.points = CharPoints {
            hit: 20,
            max_hit: 20,
            mana: 100,
            max_mana: 100,
            move_points: 80,
            max_move: 80,
            armor: 100,
            ..Default::default()
        };
        // New PCs start hungry/thirsty clocks at 24 (CircleMUD do_start).
        ch.conditions = [0, 24, 24];
        // New characters are flagged newbie (DeltaMUD do_start / newbie zone).
        ch.newbie = 1;
        ch
    }

    pub fn new_npc(nr: MobVnum) -> Self {
        let mut ch = Character::blank(true, nr);
        ch.player.name = String::from("a mobile");
        ch
    }

    pub fn get_name(&self) -> &str {
        &self.player.name
    }

    /// GET_TITLE (utils.h): the title SUFFIX only (e.g. "the Implementor").
    /// Callers that want "Name Title" prepend the name explicitly, matching C.
    pub fn get_title(&self) -> String {
        self.player.title.clone().unwrap_or_default()
    }

    pub fn is_immortal(&self) -> bool {
        !self.is_npc && self.player.level >= LVL_IMMORT
    }

    /// READ_CITIZEN (utils.h): the gendered citizen-title prefix (e.g. "Knight ")
    /// for this character's citizen rank. Bounds-guarded against an out-of-range
    /// rank. Returns "" if the rank is 0 (no title), matching the caller gate
    /// `GET_CITIZEN(ch) > 0`.
    pub fn read_citizen(&self) -> &'static str {
        let idx = self.citizen as usize;
        let row = match crate::constants::CITIZEN_TITLES.get(idx) {
            Some(r) => r,
            None => return "",
        };
        match self.player.sex {
            crate::types::Gender::Male => row[0],
            crate::types::Gender::Female => row[1],
            _ => row[2],
        }
    }

    /// citizen_colors[rank] (constants.c), bounds-guarded.
    pub fn citizen_color(&self) -> &'static str {
        crate::constants::CITIZEN_COLORS.get(self.citizen as usize).copied().unwrap_or("&n")
    }

    pub fn skill(&self, num: u16) -> u8 {
        self.skills.get(&num).copied().unwrap_or(0)
    }

    pub fn set_skill(&mut self, num: u16, val: u8) {
        self.skills.insert(num, val);
    }

    pub fn class_abbrev(&self) -> &'static str {
        match self.player.class {
            Class::MagicUser => "Mag",
            Class::Cleric => "Cle",
            Class::Thief => "Thi",
            Class::Warrior => "War",
            Class::Artisan => "Art",
        }
    }

    /// Third-person reference (CircleMUD PERS/GET_NAME): NPC short_desc
    /// ("the baker"), or just the PC's name (NOT name+title).
    pub fn display_for_others(&self) -> String {
        if self.is_npc {
            if let Some(s) = &self.short_desc {
                if !s.is_empty() {
                    return s.clone();
                }
            }
        }
        self.player.name.clone()
    }

    /// Standing-in-room line: NPC long_desc, or "<name> is here." for PCs.
    pub fn display_in_room(&self) -> String {
        if self.is_npc {
            if let Some(long) = &self.long_desc {
                if !long.is_empty() {
                    return long.trim_end().to_string();
                }
            }
        }
        format!("{} is here.", self.display_for_others())
    }
}
