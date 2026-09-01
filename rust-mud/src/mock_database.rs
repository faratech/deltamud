// In-memory database for tests / golden harness (no MySQL needed).
use crate::character::Character;
use anyhow::Result;
use std::collections::HashMap;
use std::sync::Mutex;

struct Stored {
    character: Character,
    password: String,
}

pub struct MockDatabase {
    players: Mutex<HashMap<String, Stored>>,
    next_idnum: Mutex<i64>,
    exists_delay: Mutex<Option<std::time::Duration>>,
    save_delay: Mutex<Option<std::time::Duration>>,
    rename_delay: Mutex<Option<std::time::Duration>>,
    rename_calls: Mutex<u64>,
    fail_next_save: Mutex<bool>,
    fail_next_rename: Mutex<bool>,
    fail_rename_on_call: Mutex<Option<u64>>,
}

impl MockDatabase {
    pub fn new() -> Self {
        MockDatabase {
            players: Mutex::new(HashMap::new()),
            next_idnum: Mutex::new(1),
            exists_delay: Mutex::new(None),
            save_delay: Mutex::new(None),
            rename_delay: Mutex::new(None),
            rename_calls: Mutex::new(0),
            fail_next_save: Mutex::new(false),
            fail_next_rename: Mutex::new(false),
            fail_rename_on_call: Mutex::new(None),
        }
    }

    pub fn set_save_delay(&self, delay: Option<std::time::Duration>) {
        *crate::lock_ok::lock(&self.save_delay) = delay;
    }

    pub fn set_exists_delay(&self, delay: Option<std::time::Duration>) {
        *crate::lock_ok::lock(&self.exists_delay) = delay;
    }

    pub fn fail_next_save(&self) {
        *crate::lock_ok::lock(&self.fail_next_save) = true;
    }

    pub fn set_rename_delay(&self, delay: Option<std::time::Duration>) {
        *crate::lock_ok::lock(&self.rename_delay) = delay;
    }

    pub fn fail_next_rename(&self) {
        *crate::lock_ok::lock(&self.fail_next_rename) = true;
    }

    pub fn fail_rename_on_call(&self, call: u64) {
        *crate::lock_ok::lock(&self.rename_calls) = 0;
        *crate::lock_ok::lock(&self.fail_rename_on_call) = Some(call);
    }
}

impl Default for MockDatabase {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl crate::DatabaseInterface for MockDatabase {
    async fn init_tables(&self) -> Result<()> {
        Ok(())
    }

    async fn player_exists(&self, name: &str) -> Result<bool> {
        let delay = *crate::lock_ok::lock(&self.exists_delay);
        if let Some(delay) = delay {
            tokio::time::sleep(delay).await;
        }
        Ok(self
            .players
            .lock()
            .unwrap()
            .contains_key(&name.to_lowercase()))
    }

    async fn create_player(&self, character: &Character, password: &str) -> Result<i64> {
        let mut players = crate::lock_ok::lock(&self.players);
        let mut idnum = crate::lock_ok::lock(&self.next_idnum);
        let id = *idnum;
        *idnum += 1;
        let mut ch = character.clone();
        ch.idnum = id;
        players.insert(
            character.get_name().to_lowercase(),
            Stored {
                character: ch,
                password: crate::password::hash_password(password),
            },
        );
        Ok(id)
    }

    async fn load_player(&self, name: &str) -> Result<Character> {
        let players = crate::lock_ok::lock(&self.players);
        players
            .get(&name.to_lowercase())
            .map(|s| s.character.clone())
            .ok_or_else(|| anyhow::anyhow!("Player not found"))
    }

    async fn save_player(&self, character: &Character) -> Result<()> {
        let delay = *crate::lock_ok::lock(&self.save_delay);
        if let Some(delay) = delay {
            tokio::time::sleep(delay).await;
        }
        if std::mem::take(&mut *crate::lock_ok::lock(&self.fail_next_save)) {
            anyhow::bail!("injected save failure");
        }
        let mut players = crate::lock_ok::lock(&self.players);
        if let Some(s) = players.get_mut(&character.get_name().to_lowercase()) {
            s.character = character.clone();
            if let Some(hash) = &character.pending_password_hash {
                s.password = hash.clone();
            }
        }
        Ok(())
    }

    async fn rename_player_if_current(
        &self,
        idnum: i64,
        expected_old_name: &str,
        new_name: &str,
    ) -> Result<bool> {
        let delay = *crate::lock_ok::lock(&self.rename_delay);
        if let Some(delay) = delay {
            tokio::time::sleep(delay).await;
        }
        let call = {
            let mut calls = crate::lock_ok::lock(&self.rename_calls);
            *calls = calls.saturating_add(1);
            *calls
        };
        let fail_this_call = {
            let mut configured = crate::lock_ok::lock(&self.fail_rename_on_call);
            if *configured == Some(call) {
                *configured = None;
                true
            } else {
                false
            }
        };
        if fail_this_call {
            anyhow::bail!("injected rename failure on call {call}");
        }
        if std::mem::take(&mut *crate::lock_ok::lock(&self.fail_next_rename)) {
            anyhow::bail!("injected rename failure");
        }

        let old_key = expected_old_name.to_lowercase();
        let new_key = new_name.to_lowercase();
        let mut players = crate::lock_ok::lock(&self.players);
        let current_matches = players
            .get(&old_key)
            .is_some_and(|stored| stored.character.idnum == idnum);
        if !current_matches {
            return Ok(false);
        }
        if players
            .get(&new_key)
            .is_some_and(|stored| stored.character.idnum != idnum)
        {
            return Ok(false);
        }

        let Some(mut stored) = players.remove(&old_key) else {
            return Ok(false);
        };
        stored.character.player.name = new_name.to_string();
        players.insert(new_key, stored);
        Ok(true)
    }

    async fn player_name_by_id(&self, idnum: i64) -> Result<Option<String>> {
        let players = crate::lock_ok::lock(&self.players);
        Ok(players
            .values()
            .find(|stored| stored.character.idnum == idnum)
            .map(|stored| stored.character.get_name().to_string()))
    }

    async fn verify_password(&self, name: &str, password: &str) -> Result<bool> {
        let players = crate::lock_ok::lock(&self.players);
        match players.get(&name.to_lowercase()) {
            Some(s) => Ok(crate::password::check_password(&s.password, password)),
            None => Ok(false),
        }
    }

    async fn get_password_hash(&self, name: &str) -> Result<Option<String>> {
        let players = crate::lock_ok::lock(&self.players);
        Ok(players
            .get(&name.to_lowercase())
            .map(|s| s.password.clone()))
    }

    async fn delete_deleted_players(&self) -> Result<u64> {
        let mut players = crate::lock_ok::lock(&self.players);
        let before = players.len();
        players.retain(|_, s| (s.character.act_flags & crate::flags::PLR_DELETED) == 0);
        Ok((before - players.len()) as u64)
    }

    async fn delete_deleted_players_by_idnums(&self, idnums: Vec<i64>) -> Result<u64> {
        let ids: std::collections::HashSet<i64> = idnums.into_iter().collect();
        let mut players = crate::lock_ok::lock(&self.players);
        let before = players.len();
        players.retain(|_, stored| {
            !ids.contains(&stored.character.idnum)
                || stored.character.act_flags & crate::flags::PLR_DELETED == 0
        });
        Ok((before - players.len()) as u64)
    }

    async fn clan_destroy_fixup(&self, destroyed: i32) -> Result<()> {
        let mut players = crate::lock_ok::lock(&self.players);
        for stored in players.values_mut() {
            let c = &mut stored.character;
            if c.clan == destroyed {
                c.clan = -1;
                c.clan_rank = -1;
            } else if c.clan > destroyed {
                c.clan -= 1;
            }
        }
        Ok(())
    }

    async fn clan_lower_ranks(&self, clan: i32) -> Result<()> {
        let mut players = crate::lock_ok::lock(&self.players);
        for stored in players.values_mut() {
            let c = &mut stored.character;
            if c.clan == clan && c.clan_rank != -1 {
                c.clan_rank = 1;
            }
        }
        Ok(())
    }

    async fn clan_member_counts(&self) -> Result<Vec<(i32, i32)>> {
        let players = crate::lock_ok::lock(&self.players);
        let mut counts: HashMap<i32, i32> = HashMap::new();
        for stored in players.values() {
            if stored.character.clan >= 0 && stored.character.clan_rank != -1 {
                *counts.entry(stored.character.clan).or_insert(0) += 1;
            }
        }
        let mut out: Vec<_> = counts.into_iter().collect();
        out.sort_by_key(|(clan, _)| *clan);
        Ok(out)
    }

    async fn list_players(&self) -> Result<Vec<crate::state::PlayerIndex>> {
        // Build index rows from the stored Character clones (the mock has no
        // host column — host is connection-derived, "" here; the live
        // descriptor's host is folded in later via update_player_index).
        let players = crate::lock_ok::lock(&self.players);
        let out = players
            .values()
            .map(|s| crate::state::PlayerIndex {
                idnum: s.character.idnum,
                name: s.character.get_name().to_string(),
                level: s.character.player.level,
                class: s.character.player.class,
                last_logon: s.character.last_logon.timestamp(),
                host: String::new(),
                act_flags: s.character.act_flags,
                clan: s.character.clan,
                clan_rank: s.character.clan_rank,
            })
            .collect();
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DatabaseInterface;

    #[tokio::test]
    async fn save_player_persists_pending_password_hash() {
        let db = MockDatabase::new();
        let ch = Character::new_player(
            "Mort".to_string(),
            crate::types::Class::Warrior,
            crate::types::Race::Human,
        );
        db.create_player(&ch, "oldpass").await.unwrap();
        assert!(db.verify_password("Mort", "oldpass").await.unwrap());

        let mut loaded = db.load_player("Mort").await.unwrap();
        loaded.pending_password_hash = Some(crate::password::hash_password("newpass"));
        db.save_player(&loaded).await.unwrap();

        assert!(!db.verify_password("Mort", "oldpass").await.unwrap());
        assert!(db.verify_password("Mort", "newpass").await.unwrap());
    }

    #[tokio::test]
    async fn delete_deleted_players_removes_flagged_records() {
        let db = MockDatabase::new();
        let keep = Character::new_player(
            "Keep".to_string(),
            crate::types::Class::Warrior,
            crate::types::Race::Human,
        );
        let mut delete = Character::new_player(
            "Delete".to_string(),
            crate::types::Class::Warrior,
            crate::types::Race::Human,
        );
        delete.act_flags |= crate::flags::PLR_DELETED;

        db.create_player(&keep, "keep-pass").await.unwrap();
        db.create_player(&delete, "delete-pass").await.unwrap();

        assert_eq!(db.delete_deleted_players().await.unwrap(), 1);
        assert!(db.player_exists("Keep").await.unwrap());
        assert!(!db.player_exists("Delete").await.unwrap());

        let mut names: Vec<_> = db
            .list_players()
            .await
            .unwrap()
            .into_iter()
            .map(|p| p.name)
            .collect();
        names.sort();
        assert_eq!(names, vec!["Keep".to_string()]);
    }

    #[tokio::test]
    async fn clan_member_counts_aggregates_non_applicant_members() {
        let db = MockDatabase::new();
        let mut first = Character::new_player(
            "First".to_string(),
            crate::types::Class::Warrior,
            crate::types::Race::Human,
        );
        first.clan = 2;
        first.clan_rank = 1;
        let mut second = Character::new_player(
            "Second".to_string(),
            crate::types::Class::Warrior,
            crate::types::Race::Human,
        );
        second.clan = 2;
        second.clan_rank = 0;
        let mut applicant = Character::new_player(
            "Applicant".to_string(),
            crate::types::Class::Warrior,
            crate::types::Race::Human,
        );
        applicant.clan = 2;
        applicant.clan_rank = -1;
        let mut noclan = Character::new_player(
            "Noclan".to_string(),
            crate::types::Class::Warrior,
            crate::types::Race::Human,
        );
        noclan.clan = -1;
        noclan.clan_rank = 1;

        db.create_player(&first, "pass").await.unwrap();
        db.create_player(&second, "pass").await.unwrap();
        db.create_player(&applicant, "pass").await.unwrap();
        db.create_player(&noclan, "pass").await.unwrap();

        assert_eq!(db.clan_member_counts().await.unwrap(), vec![(2, 2)]);
    }

    #[tokio::test]
    async fn conditional_rename_requires_the_exact_id_old_name_and_free_destination() {
        let db = MockDatabase::new();
        let old = Character::new_player(
            "Oldname".to_string(),
            crate::types::Class::Warrior,
            crate::types::Race::Human,
        );
        let occupied = Character::new_player(
            "Occupied".to_string(),
            crate::types::Class::Cleric,
            crate::types::Race::Elf,
        );
        let idnum = db.create_player(&old, "old-pass").await.unwrap();
        db.create_player(&occupied, "occupied-pass").await.unwrap();

        assert!(
            !db.rename_player_if_current(idnum + 100, "Oldname", "Newname")
                .await
                .unwrap()
        );
        assert!(
            !db.rename_player_if_current(idnum, "Wrongname", "Newname")
                .await
                .unwrap()
        );
        assert!(
            !db.rename_player_if_current(idnum, "Oldname", "Occupied")
                .await
                .unwrap()
        );
        assert!(db.load_player("Oldname").await.is_ok());

        assert!(
            db.rename_player_if_current(idnum, "Oldname", "Newname")
                .await
                .unwrap()
        );
        assert!(db.load_player("Oldname").await.is_err());
        assert_eq!(db.load_player("Newname").await.unwrap().idnum, idnum);
        assert!(db.verify_password("Newname", "old-pass").await.unwrap());
    }
}
