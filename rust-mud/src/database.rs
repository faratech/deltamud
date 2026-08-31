// Standard persistence (MySQL via mysql_async).
//
// This targets the C-compatible 83-column `player_main` table plus
// `player_affects` and `player_skills`, so the on-disk format round-trips
// losslessly and is cross-compatible with the original C DeltaMUD. All
// row<->Character translation lives in database_compat.rs (the single source
// of truth for the column set/order); this file only owns the SQL plumbing.

use crate::character::Character;
use crate::database_compat as compat;
use anyhow::Result;
use mysql_async::{params, prelude::*, Pool, Row, Value};

pub struct Database {
    pool: Pool,
}

impl Database {
    pub fn new(database_url: &str) -> Result<Self> {
        Ok(Database {
            pool: Pool::new(database_url),
        })
    }

    pub async fn init_tables(&self) -> Result<()> {
        let mut conn = self.pool.get_conn().await?;
        // DDL mirrors deltamud_schema.sql (player_main / player_affects /
        // player_skills). CREATE ... IF NOT EXISTS is a no-op against a live
        // C-MUD database that already has these tables.
        conn.query_drop(
            r"CREATE TABLE IF NOT EXISTS player_main (
                idnum INT PRIMARY KEY,
                name VARCHAR(30) NOT NULL UNIQUE,
                description TEXT,
                title VARCHAR(80),
                sex TINYINT, class TINYINT, race TINYINT, deity TINYINT,
                level TINYINT, hometown INT, birth BIGINT, played BIGINT,
                weight INT, height INT, pwd VARCHAR(255),
                last_logon BIGINT, host VARCHAR(80),
                act BIGINT, str TINYINT, str_add TINYINT, intel TINYINT,
                wis TINYINT, dex TINYINT, con TINYINT, cha TINYINT,
                hit INT, max_hit INT, mana INT, max_mana INT, move INT,
                max_move INT, gold INT, bank_gold INT, exp BIGINT, power INT,
                mpower INT, defense INT, mdefense INT, technique INT,
                PADDING0 INT, talks1 INT, talks2 INT, talks3 INT,
                wimp_level INT, freeze_level TINYINT, invis_level INT,
                load_room INT, pref BIGINT, bad_pws TINYINT, cond1 TINYINT,
                cond2 TINYINT, cond3 TINYINT, death_timer INT, citizen INT,
                training TINYINT, newbie TINYINT, arena INT,
                spells_to_learn INT, questpoints INT, nextquest INT,
                countdown INT, questobj INT, questmob INT,
                recall_level TINYINT, retreat_level TINYINT, trust TINYINT,
                bail_amt INT, wins INT, losses INT, pref2 BIGINT,
                godcmds1 BIGINT, godcmds2 BIGINT, godcmds3 BIGINT,
                godcmds4 BIGINT, clan INT, clan_rank TINYINT, mapx INT,
                mapy INT, buildmodezone INT, buildmoderoom INT, tloadroom INT,
                alignment INT, affected_by BIGINT,
                INDEX idx_name (name), INDEX idx_level (level)
            ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4",
        )
        .await?;
        // A C-MUD-created player_main has pwd VARCHAR(50) (sized for crypt()),
        // but the Rust stores a 64-char SHA-256 hex digest. Widen it idempotently
        // so the modern hash fits (no-op if the column is already wide enough).
        let _ = conn
            .query_drop("ALTER TABLE player_main MODIFY COLUMN pwd VARCHAR(255)")
            .await;
        conn.query_drop(
            r"CREATE TABLE IF NOT EXISTS player_affects (
                idnum INT NOT NULL, type INT, duration INT, modifier INT,
                location TINYINT, bitvector BIGINT, INDEX idx_idnum (idnum)
            ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4",
        )
        .await?;
        conn.query_drop(
            r"CREATE TABLE IF NOT EXISTS player_skills (
                idnum INT NOT NULL, skill INT, learned TINYINT,
                INDEX idx_idnum (idnum), INDEX idx_skill (skill)
            ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4",
        )
        .await?;
        Ok(())
    }

    pub async fn player_exists(&self, name: &str) -> Result<bool> {
        let mut conn = self.pool.get_conn().await?;
        let row: Option<Row> = conn
            .exec_first("SELECT idnum FROM player_main WHERE name = ?", (name,))
            .await?;
        Ok(row.is_some())
    }

    pub async fn get_password_hash(&self, name: &str) -> Result<Option<String>> {
        let mut conn = self.pool.get_conn().await?;
        let stored: Option<String> = conn
            .exec_first("SELECT pwd FROM player_main WHERE name = ?", (name,))
            .await?;
        Ok(stored)
    }

    pub async fn verify_password(&self, name: &str, password: &str) -> Result<bool> {
        let mut conn = self.pool.get_conn().await?;
        let stored: Option<String> = conn
            .exec_first("SELECT pwd FROM player_main WHERE name = ?", (name,))
            .await?;
        Ok(matches!(stored, Some(h) if crate::password::check_password(&h, password)))
    }

    /// Allocate the next idnum (C uses a top_idnum counter). With an empty
    /// table the first player is idnum 1 (the Implementor).
    async fn next_idnum(&self) -> Result<i64> {
        let mut conn = self.pool.get_conn().await?;
        let max: Option<i64> = conn
            .query_first("SELECT MAX(idnum) FROM player_main")
            .await?
            .flatten();
        Ok(max.unwrap_or(0) + 1)
    }

    pub async fn create_player(&self, ch: &Character, password: &str) -> Result<i64> {
        let idnum = self.next_idnum().await?;
        let mut stored = ch.clone();
        stored.idnum = idnum;
        self.write_player_main(&stored, &crate::password::hash_password(password), "")
            .await?;
        self.write_skills(&stored).await?;
        self.write_affects(&stored).await?;
        Ok(idnum)
    }

    pub async fn load_player(&self, name: &str) -> Result<Character> {
        let mut conn = self.pool.get_conn().await?;
        let row: Option<Row> = conn
            .exec_first("SELECT * FROM player_main WHERE name = ?", (name,))
            .await?;
        let row = row.ok_or_else(|| anyhow::anyhow!("Player not found"))?;

        let mut ch = compat::player_main_to_character(&row);

        // Merge affects (dbmodify_player_affects MODE_RETRIEVE).
        let aff_rows: Vec<Row> = conn
            .exec(
                "SELECT type,duration,modifier,location,bitvector FROM player_affects WHERE idnum = ?",
                (ch.idnum,),
            )
            .await?;
        compat::apply_affect_rows(&mut ch, &aff_rows);

        // Merge skills (dbmodify_player_skills MODE_RETRIEVE).
        let skill_rows: Vec<Row> = conn
            .exec(
                "SELECT idnum,skill,learned FROM player_skills WHERE idnum = ?",
                (ch.idnum,),
            )
            .await?;
        compat::apply_skill_rows(&mut ch, &skill_rows);

        Ok(ch)
    }

    /// build_player_index() (db.c): read every player's index row. C selects
    /// Pull the player_main fields needed for the in-memory player index and
    /// offline reports. last_logon is a unix BIGINT in player_main (see
    /// init_tables DDL), so it maps directly to the PlayerIndex i64.
    pub async fn list_players(&self) -> Result<Vec<crate::state::PlayerIndex>> {
        let mut conn = self.pool.get_conn().await?;
        let rows: Vec<Row> = conn
            .exec(
                "SELECT idnum,name,level,class,last_logon,host,act,clan,clan_rank FROM player_main",
                (),
            )
            .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let idnum: i64 = row.get(0).unwrap_or(-1);
            let name: String = row.get(1).unwrap_or_default();
            let level: u8 = row.get(2).unwrap_or(1);
            let class = crate::types::Class::from_u8(row.get::<u8, _>(3).unwrap_or(3));
            let last_logon: i64 = row.get(4).unwrap_or(0);
            let host: String = row.get(5).unwrap_or_default();
            let act_flags: i64 = row.get(6).unwrap_or(0);
            let clan: i32 = row.get(7).unwrap_or(-1);
            let clan_rank: i32 = row.get(8).unwrap_or(-1);
            out.push(crate::state::PlayerIndex {
                idnum,
                name,
                level,
                class,
                last_logon,
                host,
                act_flags,
                clan,
                clan_rank,
            });
        }
        Ok(out)
    }

    /// delete_player_entry() for every PLR_DELETED player. Child tables are
    /// joined before player_main is removed so the act bit remains available.
    pub async fn delete_deleted_players(&self) -> Result<u64> {
        let mut conn = self.pool.get_conn().await?;
        let bit = crate::flags::PLR_DELETED;
        conn.exec_drop(
            r"DELETE pa FROM player_affects pa
              JOIN player_main pm ON pm.idnum = pa.idnum
              WHERE (pm.act & ?) <> 0",
            (bit,),
        )
        .await?;
        conn.exec_drop(
            r"DELETE ps FROM player_skills ps
              JOIN player_main pm ON pm.idnum = ps.idnum
              WHERE (pm.act & ?) <> 0",
            (bit,),
        )
        .await?;
        conn.exec_drop("DELETE FROM player_main WHERE (act & ?) <> 0", (bit,))
            .await?;
        Ok(conn.affected_rows())
    }

    /// C boot_clans() recounts from player_main on every boot.
    pub async fn clan_member_counts(&self) -> Result<Vec<(i32, i32)>> {
        let mut conn = self.pool.get_conn().await?;
        let rows: Vec<Row> = conn
            .exec(
                "SELECT clan, COUNT(*) FROM player_main WHERE clan >= 0 AND clan_rank != -1 GROUP BY clan",
                (),
            )
            .await?;
        Ok(rows
            .into_iter()
            .filter_map(|row| {
                let clan: Option<i32> = row.get(0);
                let count: Option<u64> = row.get(1);
                clan.zip(count).map(|(clan, count)| (clan, count as i32))
            })
            .collect())
    }

    /// C clan.c:242-255 clan_destroy SQL: clear the destroyed clan's members
    /// and shift higher clans down one (offline rows included) (#165).
    pub async fn clan_destroy_fixup(&self, destroyed: i32) -> Result<()> {
        let mut conn = self.pool.get_conn().await?;
        conn.exec_drop(
            "UPDATE player_main SET clan=-1, clan_rank=-1 WHERE clan=?",
            (destroyed,),
        )
        .await?;
        conn.exec_drop(
            "UPDATE player_main SET clan=clan-1 WHERE clan>?",
            (destroyed,),
        )
        .await?;
        Ok(())
    }

    /// C clan.c:388-405 lower_entire_clan SQL (#165).
    pub async fn clan_lower_ranks(&self, clan: i32) -> Result<()> {
        let mut conn = self.pool.get_conn().await?;
        conn.exec_drop(
            "UPDATE player_main SET clan_rank=1 WHERE clan=? AND clan_rank!=-1",
            (clan,),
        )
        .await?;
        Ok(())
    }

    pub async fn save_player(&self, ch: &Character) -> Result<()> {
        self.save_player_with_host(ch, "").await
    }

    pub async fn save_player_with_host(&self, ch: &Character, host: &str) -> Result<()> {
        // The password is never rewritten on a normal save; preserve the
        // stored hash (C re-supplies ch->player.passwd, which is unchanged).
        let mut conn = self.pool.get_conn().await?;
        let existing: Option<(String, String)> = conn
            .exec_first(
                "SELECT pwd, COALESCE(host, '') FROM player_main WHERE idnum = ?",
                (ch.idnum,),
            )
            .await?;
        let existing_pwd = existing
            .as_ref()
            .map(|(pwd, _)| pwd.clone())
            .unwrap_or_default();
        let existing_host = existing.map(|(_, host)| host).unwrap_or_default();
        let pwd = match &ch.pending_password_hash {
            Some(hash) => hash.clone(),
            None => existing_pwd,
        };
        let host_to_save = if host.is_empty() {
            existing_host.as_str()
        } else {
            host
        };
        drop(conn);
        self.write_player_main(ch, &pwd, host_to_save).await?;
        self.write_skills(ch).await?;
        self.write_affects(ch).await?;
        Ok(())
    }

    // ---- internal write helpers ----------------------------------------

    /// INSERT-or-UPDATE all 83 player_main columns (REPLACE keyed on the unique
    /// idnum/name), reusing the canonical column set + value order from
    /// database_compat.rs.
    async fn write_player_main(&self, ch: &Character, pwd_hash: &str, host: &str) -> Result<()> {
        let values: Vec<Value> = compat::player_main_values(ch, pwd_hash, host);
        let cols = compat::PLAYER_MAIN_COLUMNS;

        let collist = cols
            .iter()
            .map(|c| format!("`{}`", c))
            .collect::<Vec<_>>()
            .join(",");
        let placeholders = vec!["?"; cols.len()].join(",");
        let sql = format!(
            "REPLACE INTO player_main ({}) VALUES ({})",
            collist, placeholders
        );

        let mut conn = self.pool.get_conn().await?;
        conn.exec_drop(sql, values).await?;
        Ok(())
    }

    /// DELETE + re-INSERT skills>0 (dbmodify_player_skills MODE_DELETE/STORE).
    async fn write_skills(&self, ch: &Character) -> Result<()> {
        let mut conn = self.pool.get_conn().await?;
        conn.exec_drop("DELETE FROM player_skills WHERE idnum = ?", (ch.idnum,))
            .await?;
        let rows = compat::skill_rows(ch);
        if !rows.is_empty() {
            let params: Vec<_> = rows
                .into_iter()
                .map(|(skill, learned)| {
                    params! { "idnum" => ch.idnum, "skill" => skill, "learned" => learned }
                })
                .collect();
            conn.exec_batch(
                "INSERT INTO player_skills (idnum,skill,learned) VALUES (:idnum,:skill,:learned)",
                params,
            )
            .await?;
        }
        Ok(())
    }

    /// DELETE + re-INSERT affects (dbmodify_player_affects MODE_DELETE/STORE).
    async fn write_affects(&self, ch: &Character) -> Result<()> {
        let mut conn = self.pool.get_conn().await?;
        conn.exec_drop("DELETE FROM player_affects WHERE idnum = ?", (ch.idnum,))
            .await?;
        let rows = compat::affect_rows(ch);
        if !rows.is_empty() {
            let params: Vec<_> = rows
                .into_iter()
                .map(|(t, dur, m, loc, bv)| {
                    params! {
                        "idnum" => ch.idnum, "type" => t, "duration" => dur,
                        "modifier" => m, "location" => loc, "bitvector" => bv,
                    }
                })
                .collect();
            conn.exec_batch(
                r"INSERT INTO player_affects (idnum,type,duration,modifier,location,bitvector)
                  VALUES (:idnum,:type,:duration,:modifier,:location,:bitvector)",
                params,
            )
            .await?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// MySQL integration tests (Deltania Breathes W1). These run against a REAL
// MariaDB and are opt-in: each test returns early unless MUD_TEST_DATABASE_URL
// is set (scripts/db-check.sh boots a throwaway mariadbd on 127.0.0.1:3307 and
// exports it). The production database must never be a target — the URL the
// script exports points at its own throwaway instance only.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod mysql_integration {
    use super::*;
    use crate::character::Character;
    use crate::types::{Class, Race};

    fn test_db() -> Option<Database> {
        let url = std::env::var("MUD_TEST_DATABASE_URL").ok()?;
        Some(Database::new(&url).expect("connect to throwaway test db"))
    }

    /// Unique per-run player names: the throwaway db is reused across runs,
    /// and name collisions would fail create_player.
    fn unique_name(base: &str) -> String {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() % 1_000_000)
            .unwrap_or(0);
        format!("{base}{n}")
    }

    fn rich_player(name: &str) -> Character {
        let mut ch = Character::new_player(name.to_string(), Class::Cleric, Race::Dwarf);
        ch.player.level = 34;
        ch.player.hometown = 300;
        ch.player.sex = crate::types::Gender::Female;
        ch.player.deity = 7;
        ch.alignment = -420;
        // The DB row carries the BARE BASE (real_points), not the eq/affect-
        // layered points (see Character.real_points doc); set the base and
        // mirror it into points the way affect_total would.
        ch.real_points.hit = 88;
        ch.real_points.max_hit = 210;
        ch.real_points.mana = 40;
        ch.real_points.max_mana = 333;
        ch.real_points.move_points = 22;
        ch.real_points.max_move = 144;
        ch.real_points.armor = -87;
        ch.real_points.hitroll = 9;
        ch.real_points.damroll = 11;
        ch.points = ch.real_points.clone();
        ch.points.gold = 12_345;
        ch.points.bank_gold = 98_765;
        ch.points.exp = 4_242_424;
        ch.real_abils.str = 18;
        ch.real_abils.str_add = 30;
        ch.real_abils.intel = 14;
        ch.real_abils.wis = 19;
        ch.real_abils.dex = 12;
        ch.real_abils.con = 17;
        ch.real_abils.cha = 8;
        ch.aff_abils = ch.real_abils;
        ch.player.title = Some("the Courier".to_string());
        ch.spells_to_learn = 6;
        ch.quest_points = 77;
        ch.quest_mob = -3;
        ch.quest_obj = 9011;
        ch.next_quest = 12;
        ch.quest_countdown = 9;
        ch.tloadroom = 310;
        ch.act_flags |= (1 << 16) | (1 << 2); // PLR_QUESTOR | POSTALIZED whatever bits round-trip
        ch
    }

    #[tokio::test]
    async fn create_load_roundtrip_preserves_the_row() {
        let Some(db) = test_db() else { return };
        db.init_tables().await.unwrap();

        let name = unique_name("RoundTripper");
        let mut ch = rich_player(&name);
        let idnum = db.create_player(&ch, "s3cretpw").await.unwrap();
        ch.idnum = idnum;
        ch.act_flags |= 0;
        db.save_player_with_host(&ch, "courier.example.test").await.unwrap();

        let loaded = db.load_player(&name).await.unwrap();
        assert_eq!(loaded.idnum, idnum);
        assert_eq!(loaded.player.name, name);
        assert_eq!(loaded.player.level, 34);
        assert_eq!(loaded.points.gold, 12_345);
        assert_eq!(loaded.points.bank_gold, 98_765);
        assert_eq!(loaded.points.exp, 4_242_424);
        assert_eq!(loaded.points.max_hit, 210);
        assert_eq!(loaded.points.max_mana, 333);
        assert_eq!(loaded.points.max_move, 144);
        // NOTE: armor/hitroll/damroll are apply targets recomputed by
        // affect_total on load — they are derived stats, not persisted ones.
        assert_eq!(loaded.real_abils.str, 18);
        assert_eq!(loaded.real_abils.str_add, 30);
        assert_eq!(loaded.real_abils.wis, 19);
        assert_eq!(loaded.player.title.as_deref(), Some("the Courier"));
        assert_eq!(loaded.alignment, -420);
        assert_eq!(loaded.spells_to_learn, 6);
        assert_eq!(loaded.quest_points, 77);
        assert_eq!(loaded.quest_mob, -3);
        assert_eq!(loaded.quest_obj, 9011);
        assert_eq!(loaded.next_quest, 12);
        assert_eq!(loaded.quest_countdown, 9);
        assert_eq!(loaded.tloadroom, 310);
        assert!(loaded.act_flags & (1 << 16) != 0);
        assert!(loaded.act_flags & (1 << 2) != 0);

        // Password path.
        assert!(db.verify_password(&name, "s3cretpw").await.unwrap());
        assert!(!db.verify_password(&name, "wrong").await.unwrap());
    }

    #[tokio::test]
    async fn save_is_idempotent_across_repeated_writes() {
        let Some(db) = test_db() else { return };
        db.init_tables().await.unwrap();
        let name = unique_name("IdemPotent");
        let mut ch = rich_player(&name);
        let idnum = db.create_player(&ch, "pw").await.unwrap();
        ch.idnum = idnum;
        db.save_player_with_host(&ch, "host-a").await.unwrap();
        let first = db.load_player(&name).await.unwrap();
        db.save_player(&ch).await.unwrap();
        let second = db.load_player(&name).await.unwrap();

        assert_eq!(first.points.gold, second.points.gold);
        assert_eq!(first.points.exp, second.points.exp);
        assert_eq!(first.quest_mob, second.quest_mob);
        assert_eq!(first.real_abils.str, second.real_abils.str);
        assert_eq!(first.real_abils.str_add, second.real_abils.str_add);
        assert_eq!(first.real_abils.wis, second.real_abils.wis);
    }

    #[tokio::test]
    async fn mutation_then_reload_reflects_the_new_state() {
        let Some(db) = test_db() else { return };
        db.init_tables().await.unwrap();
        let name = unique_name("Mutator");
        let mut ch = rich_player(&name);
        let idnum = db.create_player(&ch, "pw").await.unwrap();
        ch.idnum = idnum;
        ch.points.gold = 1;
        ch.player.level = 61;
        ch.quest_mob = 0;
        ch.quest_obj = 0;
        db.save_player(&ch).await.unwrap();
        let loaded = db.load_player(&name).await.unwrap();
        assert_eq!(loaded.player.level, 61);
        assert_eq!(loaded.points.gold, 1);
        assert_eq!(loaded.quest_mob, 0);
        assert_eq!(loaded.quest_obj, 0);
    }
}
