use mysql_async::{Pool, prelude::*, Row};
use crate::character::{Character, Affect};
use anyhow::{Result, anyhow};
use sha2::{Sha256, Digest};
use log::info;

pub struct Database {
    pool: Pool,
}

impl Database {
    pub fn new(database_url: &str) -> Result<Self> {
        let pool = Pool::new(database_url);
        Ok(Database { pool })
    }
    
    pub async fn init_tables(&self) -> Result<()> {
        let mut conn = self.pool.get_conn().await?;
        
        // Create player_main table
        conn.exec_drop(r"
            CREATE TABLE IF NOT EXISTS player_main (
                idnum INT AUTO_INCREMENT PRIMARY KEY,
                name VARCHAR(20) UNIQUE NOT NULL,
                password VARCHAR(64) NOT NULL,
                title VARCHAR(80),
                description TEXT,
                sex TINYINT DEFAULT 0,
                class TINYINT DEFAULT 0,
                race TINYINT DEFAULT 0,
                level TINYINT DEFAULT 1,
                hometown INT DEFAULT 3001,
                birth TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                played INT DEFAULT 0,
                weight TINYINT DEFAULT 150,
                height TINYINT DEFAULT 170,
                pwd_new TINYINT DEFAULT 0,
                last_logon TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                host VARCHAR(100),
                hit INT DEFAULT 20,
                max_hit INT DEFAULT 20,
                mana INT DEFAULT 100,
                max_mana INT DEFAULT 100,
                move_points INT DEFAULT 80,
                max_move INT DEFAULT 80,
                gold INT DEFAULT 0,
                exp BIGINT DEFAULT 0,
                armor INT DEFAULT 100,
                hitroll SMALLINT DEFAULT 0,
                damroll SMALLINT DEFAULT 0,
                str_base TINYINT DEFAULT 10,
                dex_base TINYINT DEFAULT 10,
                int_base TINYINT DEFAULT 10,
                wis_base TINYINT DEFAULT 10,
                con_base TINYINT DEFAULT 10,
                cha_base TINYINT DEFAULT 10,
                str_add TINYINT DEFAULT 0,
                room_vnum INT DEFAULT 3001,
                position TINYINT DEFAULT 9,
                act_flags BIGINT DEFAULT 0,
                affect_flags BIGINT DEFAULT 0,
                clan_id INT DEFAULT 0,
                clan_rank TINYINT DEFAULT 0
            ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4
        ", ()).await?;
        
        // Create player_affects table
        conn.exec_drop(r"
            CREATE TABLE IF NOT EXISTS player_affects (
                idnum INT NOT NULL,
                spell_type INT NOT NULL,
                duration INT NOT NULL,
                modifier INT NOT NULL,
                location INT NOT NULL,
                bitvector BIGINT DEFAULT 0,
                KEY idnum_idx (idnum),
                FOREIGN KEY (idnum) REFERENCES player_main(idnum) ON DELETE CASCADE
            ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4
        ", ()).await?;
        
        // Create player_skills table
        conn.exec_drop(r"
            CREATE TABLE IF NOT EXISTS player_skills (
                idnum INT NOT NULL,
                skill_num INT NOT NULL,
                skill_level TINYINT DEFAULT 0,
                PRIMARY KEY (idnum, skill_num),
                FOREIGN KEY (idnum) REFERENCES player_main(idnum) ON DELETE CASCADE
            ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4
        ", ()).await?;
        
        // Create player_objects table (for saved equipment/inventory)
        conn.exec_drop(r"
            CREATE TABLE IF NOT EXISTS player_objects (
                owner_id INT NOT NULL,
                obj_vnum INT NOT NULL,
                worn_on INT DEFAULT -1,
                in_room INT DEFAULT -1,
                values VARCHAR(255),
                extra_flags BIGINT DEFAULT 0,
                weight INT DEFAULT 1,
                timer INT DEFAULT -1,
                affects TEXT,
                KEY owner_idx (owner_id),
                FOREIGN KEY (owner_id) REFERENCES player_main(idnum) ON DELETE CASCADE
            ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4
        ", ()).await?;
        
        info!("Database tables initialized");
        Ok(())
    }
    
    pub async fn player_exists(&self, name: &str) -> Result<bool> {
        let mut conn = self.pool.get_conn().await?;
        let result: Option<Row> = conn
            .exec_first("SELECT idnum FROM player_main WHERE name = ?", (name,))
            .await?;
        Ok(result.is_some())
    }
    
    pub async fn create_player(&self, character: &Character, password: &str) -> Result<u64> {
        let mut conn = self.pool.get_conn().await?;
        
        // Hash password
        let password_hash = self.hash_password(password);
        
        // Split into multiple smaller queries to avoid tuple size limits
        conn.exec_drop(
            r"INSERT INTO player_main 
            (name, password, title, sex, class, race, level, hometown)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            (
                &character.player.name,
                &password_hash,
                &character.player.title,
                character.player.sex as u8,
                character.player.class as u8,
                character.player.race as u8,
                character.player.level,
                character.player.hometown,
            )
        ).await?;
        
        let id = conn.last_insert_id().unwrap();
        
        // Update with stats (max 12 params)
        conn.exec_drop(
            r"UPDATE player_main SET
             hit = ?, max_hit = ?, mana = ?, max_mana = ?, move_points = ?, max_move = ?, 
             gold = ?, exp = ?, armor = ?, hitroll = ?, damroll = ?
             WHERE idnum = ?",
            (
                character.points.hit,
                character.points.max_hit,
                character.points.mana,
                character.points.max_mana,
                character.points.move_points,
                character.points.max_move,
                character.points.gold,
                character.points.exp,
                character.points.armor,
                character.points.hitroll,
                character.points.damroll,
                id,
            )
        ).await?;
        
        // Update abilities and position
        conn.exec_drop(
            r"UPDATE player_main SET
             str_base = ?, dex_base = ?, int_base = ?, wis_base = ?, con_base = ?, cha_base = ?,
             room_vnum = ?, position = ?, act_flags = ?, affect_flags = ?
             WHERE idnum = ?",
            (
                character.real_abils.str,
                character.real_abils.dex,
                character.real_abils.int,
                character.real_abils.wis,
                character.real_abils.con,
                character.real_abils.cha,
                character.player.hometown,
                character.position as u8,
                character.act_flags,
                character.affect_flags,
                id,
            )
        ).await?;
        
        Ok(id)
    }
    
    pub async fn load_player(&self, name: &str) -> Result<Character> {
        let mut conn = self.pool.get_conn().await?;
        
        let row: Row = conn
            .exec_first(
                "SELECT * FROM player_main WHERE name = ?",
                (name,)
            )
            .await?
            .ok_or_else(|| anyhow!("Player not found"))?;
        
        let mut character = self.row_to_character(row)?;
        
        // Load affects
        let affects: Vec<Row> = conn
            .exec(
                "SELECT * FROM player_affects WHERE idnum = ?",
                (character.id,)
            )
            .await?;
        
        for affect_row in affects {
            character.affected.push(Affect {
                spell_type: affect_row.get("spell_type").unwrap(),
                duration: affect_row.get("duration").unwrap(),
                modifier: affect_row.get("modifier").unwrap(),
                location: affect_row.get("location").unwrap(),
                bitvector: affect_row.get("bitvector").unwrap(),
            });
        }
        
        Ok(character)
    }
    
    pub async fn save_player(&self, character: &Character) -> Result<()> {
        let mut conn = self.pool.get_conn().await?;
        
        // Update main player data in smaller chunks
        conn.exec_drop(
            r"UPDATE player_main SET
                title = ?, sex = ?, class = ?, race = ?, level = ?
            WHERE idnum = ?",
            (
                &character.player.title,
                character.player.sex as u8,
                character.player.class as u8,
                character.player.race as u8,
                character.player.level,
                character.id,
            )
        ).await?;
        
        conn.exec_drop(
            r"UPDATE player_main SET
                hit = ?, max_hit = ?, mana = ?, max_mana = ?,
                move_points = ?, max_move = ?, gold = ?, exp = ?
            WHERE idnum = ?",
            (
                character.points.hit,
                character.points.max_hit,
                character.points.mana,
                character.points.max_mana,
                character.points.move_points,
                character.points.max_move,
                character.points.gold,
                character.points.exp,
                character.id,
            )
        ).await?;
        
        conn.exec_drop(
            r"UPDATE player_main SET
                armor = ?, hitroll = ?, damroll = ?,
                str_base = ?, dex_base = ?, int_base = ?, 
                wis_base = ?, con_base = ?, cha_base = ?
            WHERE idnum = ?",
            (
                character.points.armor,
                character.points.hitroll,
                character.points.damroll,
                character.real_abils.str,
                character.real_abils.dex,
                character.real_abils.int,
                character.real_abils.wis,
                character.real_abils.con,
                character.real_abils.cha,
                character.id,
            )
        ).await?;
        
        conn.exec_drop(
            r"UPDATE player_main SET
                room_vnum = ?, position = ?, act_flags = ?, affect_flags = ?,
                last_logon = NOW()
            WHERE idnum = ?",
            (
                character.in_room.as_ref()
                    .and_then(|r| r.upgrade())
                    .map(|r| r.read().number)
                    .unwrap_or(3001),
                character.position as u8,
                character.act_flags,
                character.affect_flags,
                character.id,
            )
        ).await?;
        
        // Save affects
        conn.exec_drop(
            "DELETE FROM player_affects WHERE idnum = ?",
            (character.id,)
        ).await?;
        
        for affect in &character.affected {
            conn.exec_drop(
                r"INSERT INTO player_affects 
                (idnum, spell_type, duration, modifier, location, bitvector)
                VALUES (?, ?, ?, ?, ?, ?)",
                (
                    character.id,
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
    
    pub async fn verify_password(&self, name: &str, password: &str) -> Result<bool> {
        let mut conn = self.pool.get_conn().await?;
        
        let row: Option<Row> = conn
            .exec_first(
                "SELECT password, pwd_new FROM player_main WHERE name = ?",
                (name,)
            )
            .await?;
        
        if let Some(row) = row {
            let stored_hash: String = row.get("password").unwrap();
            let pwd_new: i8 = row.get("pwd_new").unwrap();
            
            if pwd_new == 0 {
                // Old crypt() password, needs upgrade
                // For now, just return false to force password reset
                return Ok(false);
            }
            
            let password_hash = self.hash_password(password);
            Ok(password_hash == stored_hash)
        } else {
            Ok(false)
        }
    }
    
    fn hash_password(&self, password: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(password.as_bytes());
        format!("{:x}", hasher.finalize())
    }
    
    fn row_to_character(&self, row: Row) -> Result<Character> {
        let id: u64 = row.get("idnum").unwrap();
        let name: String = row.get("name").unwrap();
        let title: Option<String> = row.get("title").unwrap();
        let sex: u8 = row.get("sex").unwrap();
        let class: u8 = row.get("class").unwrap();
        let race: u8 = row.get("race").unwrap();
        let level: u8 = row.get("level").unwrap();
        let hometown: i32 = row.get("hometown").unwrap();
        
        let mut character = Character::new_player(
            name,
            unsafe { std::mem::transmute(class) },
            unsafe { std::mem::transmute(race) },
        );
        
        character.id = id;
        character.player.title = title;
        character.player.sex = unsafe { std::mem::transmute(sex) };
        character.player.level = level;
        character.player.hometown = hometown;
        
        // Load points
        character.points.hit = row.get("hit").unwrap();
        character.points.max_hit = row.get("max_hit").unwrap();
        character.points.mana = row.get("mana").unwrap();
        character.points.max_mana = row.get("max_mana").unwrap();
        character.points.move_points = row.get("move_points").unwrap();
        character.points.max_move = row.get("max_move").unwrap();
        character.points.gold = row.get("gold").unwrap();
        character.points.exp = row.get("exp").unwrap();
        character.points.armor = row.get("armor").unwrap();
        character.points.hitroll = row.get("hitroll").unwrap();
        character.points.damroll = row.get("damroll").unwrap();
        
        // Load abilities
        character.real_abils.str = row.get("str_base").unwrap();
        character.real_abils.dex = row.get("dex_base").unwrap();
        character.real_abils.int = row.get("int_base").unwrap();
        character.real_abils.wis = row.get("wis_base").unwrap();
        character.real_abils.con = row.get("con_base").unwrap();
        character.real_abils.cha = row.get("cha_base").unwrap();
        
        // Copy to affected abilities (will be modified by affects)
        character.aff_abils = character.real_abils.clone();
        
        // Load position and flags
        let pos: u8 = row.get("position").unwrap();
        character.position = unsafe { std::mem::transmute(pos) };
        character.act_flags = row.get("act_flags").unwrap();
        character.affect_flags = row.get("affect_flags").unwrap();
        
        Ok(character)
    }
}