// Standard persistence (MySQL via mysql_async). Tier-0 round-trips the core
// playable fields in an isolated `rust_player` table; Batch 2 reconciles this
// to the full 83-column `player_main` schema for C-MUD cross-compat.

use crate::character::Character;
use crate::types::*;
use anyhow::Result;
use mysql_async::{params, prelude::*, Pool, Row};
use sha2::{Digest, Sha256};

pub struct Database {
    pool: Pool,
}

fn hash_password(pw: &str) -> String {
    let mut h = Sha256::new();
    h.update(pw.as_bytes());
    format!("{:x}", h.finalize())
}

impl Database {
    pub fn new(database_url: &str) -> Result<Self> {
        Ok(Database { pool: Pool::new(database_url) })
    }

    pub async fn init_tables(&self) -> Result<()> {
        let mut conn = self.pool.get_conn().await?;
        conn.query_drop(
            r"CREATE TABLE IF NOT EXISTS rust_player (
                idnum BIGINT AUTO_INCREMENT PRIMARY KEY,
                name VARCHAR(20) UNIQUE NOT NULL,
                password VARCHAR(64) NOT NULL,
                title VARCHAR(80) DEFAULT NULL,
                description TEXT,
                sex TINYINT DEFAULT 0,
                class TINYINT DEFAULT 0,
                race TINYINT DEFAULT 0,
                level TINYINT DEFAULT 1,
                hometown INT DEFAULT 3001,
                weight SMALLINT DEFAULT 150,
                height SMALLINT DEFAULT 170,
                hit INT DEFAULT 20, max_hit INT DEFAULT 20,
                mana INT DEFAULT 100, max_mana INT DEFAULT 100,
                move_pts INT DEFAULT 80, max_move INT DEFAULT 80,
                armor INT DEFAULT 100, gold INT DEFAULT 0, bank_gold INT DEFAULT 0,
                exp BIGINT DEFAULT 0, alignment INT DEFAULT 0,
                str_ TINYINT DEFAULT 13, intel TINYINT DEFAULT 13, wis TINYINT DEFAULT 13,
                dex TINYINT DEFAULT 13, con TINYINT DEFAULT 13, cha TINYINT DEFAULT 13,
                hometown_room INT DEFAULT 3001
            )",
        )
        .await?;
        Ok(())
    }

    pub async fn player_exists(&self, name: &str) -> Result<bool> {
        let mut conn = self.pool.get_conn().await?;
        let row: Option<Row> = conn
            .exec_first("SELECT idnum FROM rust_player WHERE name = ?", (name,))
            .await?;
        Ok(row.is_some())
    }

    pub async fn verify_password(&self, name: &str, password: &str) -> Result<bool> {
        let mut conn = self.pool.get_conn().await?;
        let stored: Option<String> = conn
            .exec_first("SELECT password FROM rust_player WHERE name = ?", (name,))
            .await?;
        Ok(matches!(stored, Some(h) if h == hash_password(password)))
    }

    pub async fn create_player(&self, ch: &Character, password: &str) -> Result<i64> {
        let mut conn = self.pool.get_conn().await?;
        conn.exec_drop(
            r"INSERT INTO rust_player
                (name, password, sex, class, race, level, hometown, weight, height,
                 hit, max_hit, mana, max_mana, move_pts, max_move, armor, gold, exp,
                 str_, intel, wis, dex, con, cha)
              VALUES (:name,:pw,:sex,:class,:race,:level,:hometown,:weight,:height,
                 :hit,:max_hit,:mana,:max_mana,:move_pts,:max_move,:armor,:gold,:exp,
                 :str,:intel,:wis,:dex,:con,:cha)",
            params! {
                "name" => ch.player.name.clone(),
                "pw" => hash_password(password),
                "sex" => ch.player.sex as u8,
                "class" => ch.player.class as u8,
                "race" => ch.player.race as u8,
                "level" => ch.player.level,
                "hometown" => ch.player.hometown,
                "weight" => ch.player.weight,
                "height" => ch.player.height,
                "hit" => ch.points.hit,
                "max_hit" => ch.points.max_hit,
                "mana" => ch.points.mana,
                "max_mana" => ch.points.max_mana,
                "move_pts" => ch.points.move_points,
                "max_move" => ch.points.max_move,
                "armor" => ch.points.armor,
                "gold" => ch.points.gold,
                "exp" => ch.points.exp,
                "str" => ch.real_abils.str,
                "intel" => ch.real_abils.intel,
                "wis" => ch.real_abils.wis,
                "dex" => ch.real_abils.dex,
                "con" => ch.real_abils.con,
                "cha" => ch.real_abils.cha,
            },
        )
        .await?;
        Ok(conn.last_insert_id().unwrap_or(0) as i64)
    }

    pub async fn load_player(&self, name: &str) -> Result<Character> {
        let mut conn = self.pool.get_conn().await?;
        let row: Option<Row> = conn
            .exec_first("SELECT * FROM rust_player WHERE name = ?", (name,))
            .await?;
        let row = row.ok_or_else(|| anyhow::anyhow!("Player not found"))?;

        let mut ch = Character::new_player(
            row.get::<String, _>("name").unwrap_or_default(),
            Class::from_u8(row.get::<i8, _>("class").unwrap_or(3) as u8),
            Race::from_u8(row.get::<i8, _>("race").unwrap_or(0) as u8),
        );
        ch.idnum = row.get::<i64, _>("idnum").unwrap_or(-1);
        ch.player.sex = Gender::from_u8(row.get::<i8, _>("sex").unwrap_or(0) as u8);
        ch.player.level = row.get::<u8, _>("level").unwrap_or(1);
        ch.player.hometown = row.get::<i32, _>("hometown").unwrap_or(3001);
        ch.player.weight = row.get::<u16, _>("weight").unwrap_or(150) as u8;
        ch.player.height = row.get::<u16, _>("height").unwrap_or(170) as u8;
        ch.points.hit = row.get::<i32, _>("hit").unwrap_or(20);
        ch.points.max_hit = row.get::<i32, _>("max_hit").unwrap_or(20);
        ch.points.mana = row.get::<i32, _>("mana").unwrap_or(100);
        ch.points.max_mana = row.get::<i32, _>("max_mana").unwrap_or(100);
        ch.points.move_points = row.get::<i32, _>("move_pts").unwrap_or(80);
        ch.points.max_move = row.get::<i32, _>("max_move").unwrap_or(80);
        ch.points.armor = row.get::<i32, _>("armor").unwrap_or(100) as ArmorClass;
        ch.points.gold = row.get::<i32, _>("gold").unwrap_or(0);
        ch.points.bank_gold = row.get::<i32, _>("bank_gold").unwrap_or(0);
        ch.points.exp = row.get::<i64, _>("exp").unwrap_or(0);
        ch.alignment = row.get::<i32, _>("alignment").unwrap_or(0);
        ch.real_abils.str = row.get::<i8, _>("str_").unwrap_or(13);
        ch.real_abils.intel = row.get::<i8, _>("intel").unwrap_or(13);
        ch.real_abils.wis = row.get::<i8, _>("wis").unwrap_or(13);
        ch.real_abils.dex = row.get::<i8, _>("dex").unwrap_or(13);
        ch.real_abils.con = row.get::<i8, _>("con").unwrap_or(13);
        ch.real_abils.cha = row.get::<i8, _>("cha").unwrap_or(13);
        ch.aff_abils = ch.real_abils;
        Ok(ch)
    }

    pub async fn save_player(&self, ch: &Character) -> Result<()> {
        let mut conn = self.pool.get_conn().await?;
        conn.exec_drop(
            r"UPDATE rust_player SET
                level=:level, hit=:hit, max_hit=:max_hit, mana=:mana, max_mana=:max_mana,
                move_pts=:move_pts, max_move=:max_move, armor=:armor, gold=:gold,
                bank_gold=:bank_gold, exp=:exp, alignment=:alignment, hometown=:hometown
              WHERE name=:name",
            params! {
                "level" => ch.player.level,
                "hit" => ch.points.hit,
                "max_hit" => ch.points.max_hit,
                "mana" => ch.points.mana,
                "max_mana" => ch.points.max_mana,
                "move_pts" => ch.points.move_points,
                "max_move" => ch.points.max_move,
                "armor" => ch.points.armor,
                "gold" => ch.points.gold,
                "bank_gold" => ch.points.bank_gold,
                "exp" => ch.points.exp,
                "alignment" => ch.alignment,
                "hometown" => ch.player.hometown,
                "name" => ch.player.name.clone(),
            },
        )
        .await?;
        Ok(())
    }
}
