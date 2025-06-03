// Full database compatibility layer for original DeltaMUD schema

use mysql_async::{Pool, prelude::*, Row};
use crate::character::{Character, Affect};
use anyhow::{Result, anyhow};
use sha2::{Sha256, Digest};
use log::warn;

// Complete player_main schema matching C version
#[derive(Debug)]
pub struct PlayerMainRow {
    // Core identity
    pub idnum: i32,
    pub name: String,
    pub password: String,
    pub pwd_new: i8,  // 0 = old crypt, 1 = SHA256
    
    // Basic info
    pub title: Option<String>,
    pub sex: i8,
    pub class: i8,
    pub race: i8,
    pub level: i8,
    pub admlevel: i8,
    pub hometown: i32,
    
    // Time tracking
    pub birth: i64,
    pub played: i32,
    pub last_logon: i64,
    
    // Physical
    pub weight: i8,
    pub height: i8,
    
    // Location
    pub room_vnum: i32,
    pub load_room: i32,
    
    // Stats
    pub hit: i16,
    pub max_hit: i16,
    pub mana: i16,
    pub max_mana: i16,
    pub move_points: i16,
    pub max_move: i16,
    
    // Abilities
    pub str: i8,
    pub str_add: i8,
    pub intel: i8,
    pub wis: i8,
    pub dex: i8,
    pub con: i8,
    pub cha: i8,
    
    // Combat
    pub armor: i16,
    pub gold: i32,
    pub bank_gold: i32,
    pub bank_amethyst: i32,
    pub bank_bronze: i32,
    pub bank_silver: i32,
    pub bank_copper: i32,
    pub bank_steel: i32,
    pub exp: i32,
    pub hitroll: i8,
    pub damroll: i8,
    pub power: i16,
    pub defense: i16,
    pub technique: i16,
    
    // Points/Status
    pub points: i16,
    pub death_count: i32,
    pub pk_deaths: i32,
    pub mob_deaths: i32,
    pub dt_deaths: i32,
    pub login_count: i32,
    pub align: i16,
    pub position: i8,
    pub drunkenness: i8,
    pub hunger: i8,
    pub thirst: i8,
    
    // Flags
    pub act: i64,
    pub plr: i64,
    pub prf: i64,
    pub aff: i64,
    
    // System
    pub page_length: i8,
    pub wimp_level: i8,
    pub freeze_level: i8,
    pub bad_pws: i8,
    pub invis_level: i8,
    pub host: String,
    
    // Clan
    pub clan_id: i32,
    pub clan_rank: i32,
    
    // Arena
    pub arena_wins: i16,
    pub arena_losses: i16,
    
    // Quest
    pub quest_points: i32,
    pub quest_current: i32,
    pub quest_timer: i32,
    
    // Description
    pub description: Option<String>,
    
    // Language
    pub speaks: i32,
    
    // Deity
    pub deity: i8,
    
    // Spare fields
    pub spare0: i32,
    pub spare1: i32,
    pub spare2: i32,
    pub spare3: i32,
    pub spare4: i32,
    pub spare5: i32,
}

impl PlayerMainRow {
    // Convert from C database row to Rust Character
    pub fn to_character(&self) -> Character {
        let mut ch = Character::new_player(
            self.name.clone(),
            unsafe { std::mem::transmute(self.class) },
            unsafe { std::mem::transmute(self.race) },
        );
        
        ch.id = self.idnum as u64;
        ch.player.title = self.title.clone();
        ch.player.sex = unsafe { std::mem::transmute(self.sex) };
        ch.player.level = self.level as u8;
        ch.player.hometown = self.hometown;
        ch.player.time_played = self.played as i64;
        ch.player.weight = self.weight as u8;
        ch.player.height = self.height as u8;
        
        // Stats
        ch.points.hit = self.hit as i32;
        ch.points.max_hit = self.max_hit as i32;
        ch.points.mana = self.mana as i32;
        ch.points.max_mana = self.max_mana as i32;
        ch.points.move_points = self.move_points as i32;
        ch.points.max_move = self.max_move as i32;
        ch.points.armor = self.armor;
        ch.points.gold = self.gold;
        ch.points.exp = self.exp as i64;
        ch.points.hitroll = self.hitroll as i16;
        ch.points.damroll = self.damroll as i16;
        
        // Abilities
        ch.real_abils.str = self.str;
        ch.real_abils.int = self.intel;
        ch.real_abils.wis = self.wis;
        ch.real_abils.dex = self.dex;
        ch.real_abils.con = self.con;
        ch.real_abils.cha = self.cha;
        ch.aff_abils = ch.real_abils.clone();
        
        // Position and flags
        ch.position = unsafe { std::mem::transmute(self.position) };
        ch.act_flags = self.act;
        ch.affect_flags = self.aff;
        
        ch
    }
    
    // Convert from Rust Character to database row
    pub fn from_character(ch: &Character) -> Self {
        PlayerMainRow {
            idnum: ch.id as i32,
            name: ch.player.name.clone(),
            password: String::new(), // Don't update password on normal saves
            pwd_new: 1,
            
            title: ch.player.title.clone(),
            sex: ch.player.sex as i8,
            class: ch.player.class as i8,
            race: ch.player.race as i8,
            level: ch.player.level as i8,
            admlevel: if ch.is_immortal() { ch.player.level as i8 - 30 } else { 0 },
            hometown: ch.player.hometown,
            
            birth: ch.created_at.timestamp(),
            played: ch.player.time_played as i32,
            last_logon: ch.last_logon.timestamp(),
            
            weight: ch.player.weight as i8,
            height: ch.player.height as i8,
            
            room_vnum: ch.in_room.as_ref()
                .and_then(|r| r.upgrade())
                .map(|r| r.read().number)
                .unwrap_or(ch.player.hometown),
            load_room: ch.player.hometown,
            
            hit: ch.points.hit as i16,
            max_hit: ch.points.max_hit as i16,
            mana: ch.points.mana as i16,
            max_mana: ch.points.max_mana as i16,
            move_points: ch.points.move_points as i16,
            max_move: ch.points.max_move as i16,
            
            str: ch.real_abils.str,
            str_add: 0,
            intel: ch.real_abils.int,
            wis: ch.real_abils.wis,
            dex: ch.real_abils.dex,
            con: ch.real_abils.con,
            cha: ch.real_abils.cha,
            
            armor: ch.points.armor,
            gold: ch.points.gold,
            bank_gold: 0,
            bank_amethyst: 0,
            bank_bronze: 0,
            bank_silver: 0,
            bank_copper: 0,
            bank_steel: 0,
            exp: ch.points.exp as i32,
            hitroll: ch.points.hitroll as i8,
            damroll: ch.points.damroll as i8,
            power: 100,
            defense: 100,
            technique: 100,
            
            points: 0,
            death_count: 0,
            pk_deaths: 0,
            mob_deaths: 0,
            dt_deaths: 0,
            login_count: 1,
            align: 0,
            position: ch.position as i8,
            drunkenness: 0,
            hunger: 24,
            thirst: 24,
            
            act: ch.act_flags,
            plr: 0,
            prf: 0,
            aff: ch.affect_flags,
            
            page_length: 24,
            wimp_level: 0,
            freeze_level: 0,
            bad_pws: 0,
            invis_level: 0,
            host: String::new(),
            
            clan_id: 0,
            clan_rank: 0,
            
            arena_wins: 0,
            arena_losses: 0,
            
            quest_points: 0,
            quest_current: 0,
            quest_timer: 0,
            
            description: Some(ch.player.description.clone()),
            speaks: 0,
            deity: 0,
            
            spare0: 0,
            spare1: 0,
            spare2: 0,
            spare3: 0,
            spare4: 0,
            spare5: 0,
        }
    }
}

// Full compatibility database interface
pub struct CompatDatabase {
    pub pool: Pool,
}

impl CompatDatabase {
    pub fn new(database_url: &str) -> Result<Self> {
        let pool = Pool::new(database_url);
        Ok(CompatDatabase { pool })
    }
    
    // Load player with full schema compatibility
    pub async fn load_player_compat(&self, name: &str) -> Result<Character> {
        let mut conn = self.pool.get_conn().await?;
        
        let row = conn.exec_first(
            "SELECT * FROM player_main WHERE name = ?",
            (name,)
        ).await?;
        
        if let Some(row) = row {
            let data = self.parse_full_row(row)?;
            let mut character = data.to_character();
            
            // Load affects
            let affects: Vec<Row> = conn.exec(
                "SELECT * FROM player_affects WHERE idnum = ?",
                (data.idnum,)
            ).await?;
            
            for affect_row in affects {
                character.affected.push(Affect {
                    spell_type: affect_row.get("type").unwrap(),
                    duration: affect_row.get("duration").unwrap(),
                    modifier: affect_row.get("modifier").unwrap(),
                    location: affect_row.get("location").unwrap(),
                    bitvector: affect_row.get("bitvector").unwrap_or(0),
                });
            }
            
            Ok(character)
        } else {
            Err(anyhow!("Player not found"))
        }
    }
    
    // Save player with full schema compatibility
    pub async fn save_player_compat(&self, ch: &Character) -> Result<()> {
        let mut conn = self.pool.get_conn().await?;
        let data = PlayerMainRow::from_character(ch);
        
        // Update in multiple smaller queries to avoid parameter limits
        conn.exec_drop(
            r"UPDATE player_main SET
                title = ?, sex = ?, class = ?, race = ?, level = ?, admlevel = ?,
                hometown = ?, played = ?, last_logon = FROM_UNIXTIME(?)
            WHERE idnum = ?",
            (
                &data.title, data.sex, data.class, data.race, data.level, data.admlevel,
                data.hometown, data.played, data.last_logon,
                data.idnum
            )
        ).await?;
        
        conn.exec_drop(
            r"UPDATE player_main SET
                weight = ?, height = ?, room_vnum = ?, load_room = ?
            WHERE idnum = ?",
            (
                data.weight, data.height, data.room_vnum, data.load_room,
                data.idnum
            )
        ).await?;
        
        conn.exec_drop(
            r"UPDATE player_main SET
                hit = ?, max_hit = ?, mana = ?, max_mana = ?, move = ?, max_move = ?,
                armor = ?, gold = ?, exp = ?, hitroll = ?, damroll = ?
            WHERE idnum = ?",
            (
                data.hit, data.max_hit, data.mana, data.max_mana, data.move_points, data.max_move,
                data.armor, data.gold, data.exp, data.hitroll, data.damroll,
                data.idnum
            )
        ).await?;
        
        conn.exec_drop(
            r"UPDATE player_main SET
                str = ?, str_add = ?, intel = ?, wis = ?, dex = ?, con = ?, cha = ?,
                bank_gold = ?
            WHERE idnum = ?",
            (
                data.str, data.str_add, data.intel, data.wis, data.dex, data.con, data.cha,
                data.bank_gold,
                data.idnum
            )
        ).await?;
        
        conn.exec_drop(
            r"UPDATE player_main SET
                power = ?, defense = ?, technique = ?, points = ?, death_count = ?,
                pk_deaths = ?, mob_deaths = ?, dt_deaths = ?, login_count = ?
            WHERE idnum = ?",
            (
                data.power, data.defense, data.technique, data.points, data.death_count,
                data.pk_deaths, data.mob_deaths, data.dt_deaths, data.login_count,
                data.idnum
            )
        ).await?;
        
        conn.exec_drop(
            r"UPDATE player_main SET
                align = ?, position = ?, drunkenness = ?, hunger = ?, thirst = ?
            WHERE idnum = ?",
            (
                data.align, data.position, data.drunkenness, data.hunger, data.thirst,
                data.idnum
            )
        ).await?;
        
        conn.exec_drop(
            r"UPDATE player_main SET
                act = ?, plr = ?, prf = ?, aff = ?, page_length = ?, wimp_level = ?,
                freeze_level = ?, invis_level = ?, clan_id = ?, clan_rank = ?
            WHERE idnum = ?",
            (
                data.act, data.plr, data.prf, data.aff, data.page_length, data.wimp_level,
                data.freeze_level, data.invis_level, data.clan_id, data.clan_rank,
                data.idnum
            )
        ).await?;
        
        conn.exec_drop(
            r"UPDATE player_main SET
                arena_wins = ?, arena_losses = ?, quest_points = ?, quest_current = ?,
                quest_timer = ?, description = ?, speaks = ?, deity = ?
            WHERE idnum = ?",
            (
                data.arena_wins, data.arena_losses, data.quest_points, data.quest_current,
                data.quest_timer, &data.description, data.speaks, data.deity,
                data.idnum
            )
        ).await?;
        
        // Save affects (same as before)
        conn.exec_drop(
            "DELETE FROM player_affects WHERE idnum = ?",
            (data.idnum,)
        ).await?;
        
        for affect in &ch.affected {
            conn.exec_drop(
                r"INSERT INTO player_affects 
                (idnum, type, duration, modifier, location, bitvector)
                VALUES (?, ?, ?, ?, ?, ?)",
                (
                    data.idnum,
                    affect.spell_type,
                    affect.duration,
                    affect.modifier,
                    affect.location,
                    affect.bitvector,
                )
            ).await?;
        }
        
        Ok(())
    }
    
    // Parse all 83 columns from database
    fn parse_full_row(&self, row: Row) -> Result<PlayerMainRow> {
        Ok(PlayerMainRow {
            idnum: row.get("idnum").unwrap(),
            name: row.get("name").unwrap(),
            password: row.get("pwd").unwrap(),
            pwd_new: row.get("pwd_new").unwrap_or(0),
            title: row.get("title").unwrap(),
            sex: row.get("sex").unwrap(),
            class: row.get("class").unwrap(),
            race: row.get("race").unwrap(),
            level: row.get("level").unwrap(),
            admlevel: row.get("admlevel").unwrap_or(0),
            hometown: row.get("hometown").unwrap(),
            birth: row.get("birth").unwrap(),
            played: row.get("played").unwrap(),
            last_logon: row.get("last_logon").unwrap(),
            weight: row.get("weight").unwrap(),
            height: row.get("height").unwrap(),
            room_vnum: row.get("room_vnum").unwrap(),
            load_room: row.get("load_room").unwrap(),
            hit: row.get("hit").unwrap(),
            max_hit: row.get("max_hit").unwrap(),
            mana: row.get("mana").unwrap(),
            max_mana: row.get("max_mana").unwrap(),
            move_points: row.get("move").unwrap(),
            max_move: row.get("max_move").unwrap(),
            str: row.get("str").unwrap(),
            str_add: row.get("str_add").unwrap(),
            intel: row.get("intel").unwrap(),
            wis: row.get("wis").unwrap(),
            dex: row.get("dex").unwrap(),
            con: row.get("con").unwrap(),
            cha: row.get("cha").unwrap(),
            armor: row.get("armor").unwrap(),
            gold: row.get("gold").unwrap(),
            bank_gold: row.get("bank_gold").unwrap(),
            bank_amethyst: row.get("bank_amethyst").unwrap_or(0),
            bank_bronze: row.get("bank_bronze").unwrap_or(0),
            bank_silver: row.get("bank_silver").unwrap_or(0),
            bank_copper: row.get("bank_copper").unwrap_or(0),
            bank_steel: row.get("bank_steel").unwrap_or(0),
            exp: row.get("exp").unwrap(),
            hitroll: row.get("hitroll").unwrap(),
            damroll: row.get("damroll").unwrap(),
            power: row.get("power").unwrap_or(100),
            defense: row.get("defense").unwrap_or(100),
            technique: row.get("technique").unwrap_or(100),
            points: row.get("points").unwrap(),
            death_count: row.get("death_count").unwrap_or(0),
            pk_deaths: row.get("pk_deaths").unwrap_or(0),
            mob_deaths: row.get("mob_deaths").unwrap_or(0),
            dt_deaths: row.get("dt_deaths").unwrap_or(0),
            login_count: row.get("login_count").unwrap_or(1),
            align: row.get("align").unwrap(),
            position: row.get("position").unwrap(),
            drunkenness: row.get("drunkenness").unwrap(),
            hunger: row.get("hunger").unwrap(),
            thirst: row.get("thirst").unwrap(),
            act: row.get("act").unwrap(),
            plr: row.get("plr").unwrap_or(0),
            prf: row.get("prf").unwrap_or(0),
            aff: row.get("aff").unwrap(),
            page_length: row.get("page_length").unwrap(),
            wimp_level: row.get("wimp_level").unwrap(),
            freeze_level: row.get("freeze_level").unwrap(),
            bad_pws: row.get("bad_pws").unwrap(),
            invis_level: row.get("invis_level").unwrap(),
            host: row.get("host").unwrap(),
            clan_id: row.get("clan_id").unwrap_or(0),
            clan_rank: row.get("clan_rank").unwrap_or(0),
            arena_wins: row.get("arena_wins").unwrap_or(0),
            arena_losses: row.get("arena_losses").unwrap_or(0),
            quest_points: row.get("quest_points").unwrap_or(0),
            quest_current: row.get("quest_current").unwrap_or(0),
            quest_timer: row.get("quest_timer").unwrap_or(0),
            description: row.get("description").unwrap(),
            speaks: row.get("speaks").unwrap_or(0),
            deity: row.get("deity").unwrap_or(0),
            spare0: row.get("spare0").unwrap_or(0),
            spare1: row.get("spare1").unwrap_or(0),
            spare2: row.get("spare2").unwrap_or(0),
            spare3: row.get("spare3").unwrap_or(0),
            spare4: row.get("spare4").unwrap_or(0),
            spare5: row.get("spare5").unwrap_or(0),
        })
    }
    
    // Handle old crypt() passwords
    pub async fn verify_password_compat(&self, name: &str, password: &str) -> Result<bool> {
        let mut conn = self.pool.get_conn().await?;
        
        let row: Option<Row> = conn.exec_first(
            "SELECT pwd, pwd_new FROM player_main WHERE name = ?",
            (name,)
        ).await?;
        
        if let Some(row) = row {
            let stored_pwd: String = row.get("pwd").unwrap();
            let pwd_new: i8 = row.get("pwd_new").unwrap_or(0);
            
            if pwd_new == 0 {
                // Old crypt() password - would need C interop or conversion
                warn!("Player {} has old crypt() password, needs reset", name);
                return Ok(false);
            } else {
                // SHA-256 password
                let mut hasher = Sha256::new();
                hasher.update(password.as_bytes());
                let hash = format!("{:x}", hasher.finalize());
                return Ok(hash == stored_pwd);
            }
        }
        
        Ok(false)
    }
}