// In-memory database for tests / golden harness (no MySQL needed).
use crate::character::Character;
use anyhow::Result;
use std::collections::HashMap;
use std::sync::Mutex;

struct Stored {
    character: Character,
    password: String,
}

fn authority_state(character: &Character) -> crate::PlayerAuthorityState {
    crate::PlayerAuthorityState {
        level: character.player.level,
        trust: character.trust,
        exp: character.points.exp,
        godcmds1: character.godcmds1,
        godcmds2: character.godcmds2,
        godcmds3: character.godcmds3,
        godcmds4: character.godcmds4,
    }
}

fn validate_authority_state(label: &str, state: crate::PlayerAuthorityState) -> Result<()> {
    crate::database_compat::validated_player_level(i32::from(state.level))
        .map_err(|error| anyhow::anyhow!("invalid {label} player authority level: {error}"))?;
    crate::database_compat::validated_player_trust(state.trust)
        .map_err(|error| anyhow::anyhow!("invalid {label} player command trust: {error}"))?;
    Ok(())
}

pub struct MockDatabase {
    players: Mutex<HashMap<String, Stored>>,
    next_idnum: Mutex<i64>,
    exists_delay: Mutex<Option<std::time::Duration>>,
    list_delay: Mutex<Option<std::time::Duration>>,
    save_delay: Mutex<Option<std::time::Duration>>,
    rename_delay: Mutex<Option<std::time::Duration>>,
    rename_calls: Mutex<u64>,
    fail_next_save: Mutex<bool>,
    fail_next_exists: Mutex<bool>,
    fail_next_password_update: Mutex<bool>,
    fail_next_authority_read: Mutex<bool>,
    fail_next_authority_update: Mutex<bool>,
    fail_next_authority_update_after_commit: Mutex<bool>,
    fail_next_rename: Mutex<bool>,
    fail_rename_on_call: Mutex<Option<u64>>,
}

impl MockDatabase {
    pub fn new() -> Self {
        MockDatabase {
            players: Mutex::new(HashMap::new()),
            next_idnum: Mutex::new(1),
            exists_delay: Mutex::new(None),
            list_delay: Mutex::new(None),
            save_delay: Mutex::new(None),
            rename_delay: Mutex::new(None),
            rename_calls: Mutex::new(0),
            fail_next_save: Mutex::new(false),
            fail_next_exists: Mutex::new(false),
            fail_next_password_update: Mutex::new(false),
            fail_next_authority_read: Mutex::new(false),
            fail_next_authority_update: Mutex::new(false),
            fail_next_authority_update_after_commit: Mutex::new(false),
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

    #[cfg(test)]
    pub fn set_list_delay(&self, delay: Option<std::time::Duration>) {
        *crate::lock_ok::lock(&self.list_delay) = delay;
    }

    pub fn fail_next_save(&self) {
        *crate::lock_ok::lock(&self.fail_next_save) = true;
    }

    pub fn fail_next_exists(&self) {
        *crate::lock_ok::lock(&self.fail_next_exists) = true;
    }

    pub fn fail_next_password_update(&self) {
        *crate::lock_ok::lock(&self.fail_next_password_update) = true;
    }

    pub fn fail_next_authority_update(&self) {
        *crate::lock_ok::lock(&self.fail_next_authority_update) = true;
    }

    pub fn fail_next_authority_read(&self) {
        *crate::lock_ok::lock(&self.fail_next_authority_read) = true;
    }

    pub fn fail_next_authority_update_after_commit(&self) {
        *crate::lock_ok::lock(&self.fail_next_authority_update_after_commit) = true;
    }

    #[cfg(test)]
    pub fn set_password_hash_for_test(&self, name: &str, password_hash: &str) {
        if let Some(stored) = crate::lock_ok::lock(&self.players).get_mut(&name.to_lowercase()) {
            stored.password = password_hash.to_string();
        }
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

    async fn verify_schema(&self) -> Result<()> {
        Ok(())
    }

    async fn player_exists(&self, name: &str) -> Result<bool> {
        let delay = *crate::lock_ok::lock(&self.exists_delay);
        if let Some(delay) = delay {
            tokio::time::sleep(delay).await;
        }
        if std::mem::take(&mut *crate::lock_ok::lock(&self.fail_next_exists)) {
            anyhow::bail!("injected player_exists failure");
        }
        Ok(self
            .players
            .lock()
            .unwrap()
            .contains_key(&name.to_lowercase()))
    }

    async fn create_player(&self, character: &Character, password: &str) -> Result<i64> {
        let password_hash = crate::password::hash_password_async(password.to_owned())
            .await
            .ok_or_else(|| anyhow::anyhow!("password hashing worker failed"))?;
        self.create_player_with_password_hash(character, &password_hash)
            .await
    }

    async fn create_player_with_password_hash(
        &self,
        character: &Character,
        password_hash: &str,
    ) -> Result<i64> {
        if crate::password::password_needs_upgrade(password_hash) {
            anyhow::bail!("new player credential is not a current bounded Argon2id hash");
        }
        let key = character.get_name().to_lowercase();
        let mut players = crate::lock_ok::lock(&self.players);
        if players.contains_key(&key) {
            anyhow::bail!("player name {} already exists", character.get_name());
        }
        let mut idnum = crate::lock_ok::lock(&self.next_idnum);
        let id = *idnum;
        *idnum = id
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("player idnum space is exhausted"))?;
        let mut ch = character.clone();
        ch.idnum = id;
        ch.pending_password_hash = None;
        players.insert(
            key,
            Stored {
                character: ch,
                password: password_hash.to_string(),
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
            let mut snapshot = character.clone();
            snapshot.pending_password_hash = None;
            s.character = snapshot;
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

    async fn player_authority_by_id(
        &self,
        idnum: i64,
    ) -> Result<Option<(String, crate::PlayerAuthorityState)>> {
        if idnum <= 0 {
            anyhow::bail!("player authority idnum must be positive");
        }
        if std::mem::take(&mut *crate::lock_ok::lock(&self.fail_next_authority_read)) {
            anyhow::bail!("injected authority read failure");
        }
        let players = crate::lock_ok::lock(&self.players);
        let Some(stored) = players
            .values()
            .find(|stored| stored.character.idnum == idnum)
        else {
            return Ok(None);
        };
        let name = stored.character.get_name().to_string();
        crate::database_compat::validate_persisted_player_name(&name)
            .map_err(|error| anyhow::anyhow!("invalid durable player authority name: {error}"))?;
        let state = authority_state(&stored.character);
        validate_authority_state("durable", state)?;
        Ok(Some((name, state)))
    }

    async fn update_authority_if_current(
        &self,
        idnum: i64,
        expected_name: &str,
        expected: crate::PlayerAuthorityState,
        replacement: crate::PlayerAuthorityState,
    ) -> Result<crate::AuthorityUpdateOutcome> {
        if idnum <= 0 {
            anyhow::bail!("player authority idnum must be positive");
        }
        crate::database_compat::validate_persisted_player_name(expected_name)
            .map_err(|error| anyhow::anyhow!("invalid expected player authority name: {error}"))?;
        validate_authority_state("expected", expected)?;
        validate_authority_state("replacement", replacement)?;
        if std::mem::take(&mut *crate::lock_ok::lock(&self.fail_next_authority_update)) {
            anyhow::bail!("injected authority update failure");
        }

        let mut players = crate::lock_ok::lock(&self.players);
        let Some(stored) = players
            .values_mut()
            .find(|stored| stored.character.idnum == idnum)
        else {
            return Ok(crate::AuthorityUpdateOutcome::PreconditionsChanged);
        };
        let current_name = stored.character.get_name().to_string();
        crate::database_compat::validate_persisted_player_name(&current_name)
            .map_err(|error| anyhow::anyhow!("invalid durable player authority name: {error}"))?;
        let current = authority_state(&stored.character);
        validate_authority_state("durable", current)?;

        if current_name == expected_name && current == expected {
            stored.character.player.level = replacement.level;
            stored.character.trust = replacement.trust;
            stored.character.points.exp = replacement.exp;
            stored.character.godcmds1 = replacement.godcmds1;
            stored.character.godcmds2 = replacement.godcmds2;
            stored.character.godcmds3 = replacement.godcmds3;
            stored.character.godcmds4 = replacement.godcmds4;
            if std::mem::take(&mut *crate::lock_ok::lock(
                &self.fail_next_authority_update_after_commit,
            )) {
                anyhow::bail!("injected authority update failure after commit");
            }
            return Ok(crate::AuthorityUpdateOutcome::Updated);
        }
        if current_name == expected_name && current == replacement {
            Ok(crate::AuthorityUpdateOutcome::Updated)
        } else {
            Ok(crate::AuthorityUpdateOutcome::PreconditionsChanged)
        }
    }

    async fn update_password_hash(
        &self,
        idnum: i64,
        expected_name: &str,
        expected_current_hash: Option<&str>,
        password_hash: &str,
    ) -> Result<crate::PasswordHashUpdateOutcome> {
        if std::mem::take(&mut *crate::lock_ok::lock(&self.fail_next_password_update)) {
            anyhow::bail!("injected password update failure");
        }
        if idnum <= 0 || expected_name.is_empty() {
            return Ok(crate::PasswordHashUpdateOutcome::IdentityMismatch);
        }
        if password_hash.is_empty() {
            anyhow::bail!("refusing to persist an empty password hash");
        }

        let mut players = crate::lock_ok::lock(&self.players);
        let Some(stored) = players.get_mut(&expected_name.to_lowercase()) else {
            return Ok(crate::PasswordHashUpdateOutcome::IdentityMismatch);
        };
        if stored.character.idnum != idnum {
            return Ok(crate::PasswordHashUpdateOutcome::IdentityMismatch);
        }
        if expected_current_hash.is_some_and(|expected| stored.password != expected) {
            return Ok(crate::PasswordHashUpdateOutcome::CurrentHashMismatch);
        }
        stored.password = password_hash.to_string();
        Ok(crate::PasswordHashUpdateOutcome::Updated)
    }

    async fn verify_password(&self, name: &str, password: &str) -> Result<bool> {
        let stored = crate::lock_ok::lock(&self.players)
            .get(&name.to_lowercase())
            .map(|stored| stored.password.clone());
        let Some(stored) = stored else {
            return Ok(false);
        };
        Ok(crate::password::check_password_async(stored, password.to_owned()).await)
    }

    async fn get_password_hash(&self, name: &str) -> Result<Option<String>> {
        let players = crate::lock_ok::lock(&self.players);
        Ok(players
            .get(&name.to_lowercase())
            .map(|s| s.password.clone()))
    }

    async fn bootstrap_implementor(
        &self,
        name: &str,
    ) -> Result<crate::ImplementorBootstrapOutcome> {
        // One mutex covers both the existing-admin predicate and the targeted
        // mutation, mirroring the real database's advisory-lock critical
        // section and making concurrent mock calls deterministic and atomic.
        let mut players = crate::lock_ok::lock(&self.players);
        if let Some(existing) = players.values().find(|stored| {
            stored.character.trust >= i32::from(crate::types::LVL_IMPL)
                && stored.character.act_flags & crate::flags::PLR_DELETED == 0
        }) {
            return Ok(crate::ImplementorBootstrapOutcome::AlreadyExists(
                existing.character.get_name().to_string(),
            ));
        }

        let Some(stored) = players.get_mut(&name.to_lowercase()) else {
            return Ok(crate::ImplementorBootstrapOutcome::TargetNotFound);
        };
        if stored.character.idnum <= 0
            || stored.character.is_npc
            || stored.character.act_flags & crate::flags::PLR_DELETED != 0
        {
            return Ok(crate::ImplementorBootstrapOutcome::TargetNotFound);
        }
        let (godcmds1, godcmds2, godcmds3, godcmds4) = crate::implementor_command_grants();
        stored.character.player.level = crate::types::LVL_IMPL;
        stored.character.trust = i32::from(crate::types::LVL_IMPL);
        stored.character.player.title = Some("the Implementor".to_string());
        stored.character.godcmds1 = godcmds1;
        stored.character.godcmds2 = godcmds2;
        stored.character.godcmds3 = godcmds3;
        stored.character.godcmds4 = godcmds4;
        Ok(crate::ImplementorBootstrapOutcome::Promoted)
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
        let delay = *crate::lock_ok::lock(&self.list_delay);
        if let Some(delay) = delay {
            tokio::time::sleep(delay).await;
        }
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
                trust: s.character.trust,
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
    use std::sync::Arc;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_creation_has_one_same_name_winner_and_distinct_ids_for_different_names() {
        let same_name_db = Arc::new(MockDatabase::new());
        let same_character = Character::new_player(
            "Samecreator".to_string(),
            crate::types::Class::Warrior,
            crate::types::Race::Human,
        );
        let first = {
            let db = same_name_db.clone();
            let character = same_character.clone();
            tokio::spawn(async move { db.create_player(&character, "first-pass").await })
        };
        let second = {
            let db = same_name_db.clone();
            let character = same_character.clone();
            tokio::spawn(async move { db.create_player(&character, "second-pass").await })
        };
        let outcomes = [first.await.unwrap(), second.await.unwrap()];
        assert_eq!(outcomes.iter().filter(|outcome| outcome.is_ok()).count(), 1);
        assert_eq!(
            outcomes.iter().filter(|outcome| outcome.is_err()).count(),
            1
        );
        assert_eq!(same_name_db.list_players().await.unwrap().len(), 1);

        let different_name_db = Arc::new(MockDatabase::new());
        let first_character = Character::new_player(
            "Firstcreator".to_string(),
            crate::types::Class::Warrior,
            crate::types::Race::Human,
        );
        let second_character = Character::new_player(
            "Secondcreator".to_string(),
            crate::types::Class::Cleric,
            crate::types::Race::Elf,
        );
        let first = {
            let db = different_name_db.clone();
            tokio::spawn(async move { db.create_player(&first_character, "first-pass").await })
        };
        let second = {
            let db = different_name_db.clone();
            tokio::spawn(async move { db.create_player(&second_character, "second-pass").await })
        };
        let (first_id, second_id) = (
            first.await.unwrap().unwrap(),
            second.await.unwrap().unwrap(),
        );
        assert_ne!(first_id, second_id);
        assert_eq!(different_name_db.list_players().await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn generic_save_never_persists_pending_password_hash() {
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

        assert!(db.verify_password("Mort", "oldpass").await.unwrap());
        assert!(!db.verify_password("Mort", "newpass").await.unwrap());
        assert!(
            db.load_player("Mort")
                .await
                .unwrap()
                .pending_password_hash
                .is_none()
        );
    }

    #[tokio::test]
    async fn targeted_password_update_checks_identity_and_optional_current_hash() {
        let db = MockDatabase::new();
        let ch = Character::new_player(
            "Credential".to_string(),
            crate::types::Class::Warrior,
            crate::types::Race::Human,
        );
        let idnum = db.create_player(&ch, "oldpass").await.unwrap();
        let old_hash = db.get_password_hash("Credential").await.unwrap().unwrap();
        let concurrent_hash = crate::password::hash_password("concurrent");
        let requested_hash = crate::password::hash_password("requested");

        assert_eq!(
            db.update_password_hash(idnum + 1, "Credential", None, &requested_hash)
                .await
                .unwrap(),
            crate::PasswordHashUpdateOutcome::IdentityMismatch
        );
        assert_eq!(
            db.update_password_hash(idnum, "Credential", Some(&old_hash), &concurrent_hash)
                .await
                .unwrap(),
            crate::PasswordHashUpdateOutcome::Updated
        );
        assert_eq!(
            db.update_password_hash(idnum, "Credential", Some(&old_hash), &requested_hash)
                .await
                .unwrap(),
            crate::PasswordHashUpdateOutcome::CurrentHashMismatch
        );
        assert!(
            db.verify_password("Credential", "concurrent")
                .await
                .unwrap()
        );
        assert!(!db.verify_password("Credential", "requested").await.unwrap());

        db.fail_next_password_update();
        assert!(
            db.update_password_hash(idnum, "Credential", None, &requested_hash)
                .await
                .is_err()
        );
        assert!(
            db.verify_password("Credential", "concurrent")
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn targeted_authority_update_is_exact_narrow_and_idempotent() {
        let db = MockDatabase::new();
        let mut ch = Character::new_player(
            "Authority".to_string(),
            crate::types::Class::Warrior,
            crate::types::Race::Human,
        );
        ch.points.gold = 777;
        // Negative XP is reachable through legacy gameplay paths and remains a
        // valid signed persistence value; it must not prevent a demotion CAS.
        ch.points.exp = -500;
        let idnum = db.create_player(&ch, "oldpass").await.unwrap();
        let (_, expected) = db.player_authority_by_id(idnum).await.unwrap().unwrap();
        let replacement = crate::PlayerAuthorityState {
            level: crate::types::LVL_IMMORT,
            trust: i32::from(crate::types::LVL_IMMORT),
            exp: 4_242,
            godcmds1: 11,
            godcmds2: 22,
            godcmds3: 33,
            godcmds4: 44,
        };

        let stale_states = [
            crate::PlayerAuthorityState {
                level: 2,
                ..expected
            },
            crate::PlayerAuthorityState {
                trust: 2,
                ..expected
            },
            crate::PlayerAuthorityState {
                exp: expected.exp + 1,
                ..expected
            },
            crate::PlayerAuthorityState {
                godcmds1: 1,
                ..expected
            },
            crate::PlayerAuthorityState {
                godcmds2: 1,
                ..expected
            },
            crate::PlayerAuthorityState {
                godcmds3: 1,
                ..expected
            },
            crate::PlayerAuthorityState {
                godcmds4: 1,
                ..expected
            },
        ];
        for stale in stale_states {
            assert_eq!(
                db.update_authority_if_current(idnum, "Authority", stale, replacement)
                    .await
                    .unwrap(),
                crate::AuthorityUpdateOutcome::PreconditionsChanged
            );
        }
        assert_eq!(
            db.update_authority_if_current(idnum, "authority", expected, replacement)
                .await
                .unwrap(),
            crate::AuthorityUpdateOutcome::PreconditionsChanged,
            "durable identity comparison must preserve exact name case"
        );
        db.fail_next_authority_update();
        assert!(
            db.update_authority_if_current(idnum, "Authority", expected, replacement)
                .await
                .is_err()
        );
        assert_eq!(
            db.player_authority_by_id(idnum).await.unwrap(),
            Some(("Authority".to_string(), expected)),
            "a pre-commit failure must leave the durable tuple unchanged"
        );

        db.fail_next_authority_update_after_commit();
        assert!(
            db.update_authority_if_current(idnum, "Authority", expected, replacement)
                .await
                .is_err(),
            "the injected post-commit error must hide an applied replacement"
        );
        db.fail_next_authority_read();
        assert!(db.player_authority_by_id(idnum).await.is_err());
        assert_eq!(
            db.player_authority_by_id(idnum).await.unwrap(),
            Some(("Authority".to_string(), replacement))
        );
        assert_eq!(db.load_player("Authority").await.unwrap().points.gold, 777);
        assert_eq!(
            db.update_authority_if_current(idnum, "Authority", expected, replacement)
                .await
                .unwrap(),
            crate::AuthorityUpdateOutcome::Updated,
            "an exact replacement readback makes a retry idempotent"
        );

        let invalid = crate::PlayerAuthorityState {
            trust: i32::from(crate::types::LVL_IMPL) + 1,
            ..replacement
        };
        assert!(
            db.update_authority_if_current(idnum, "Authority", replacement, invalid)
                .await
                .is_err()
        );
        let invalid_level = crate::PlayerAuthorityState {
            level: crate::types::LVL_IMPL + 1,
            ..replacement
        };
        assert!(
            db.update_authority_if_current(idnum, "Authority", replacement, invalid_level)
                .await
                .is_err()
        );
        assert!(
            db.update_authority_if_current(0, "Authority", replacement, replacement)
                .await
                .is_err()
        );
        assert!(
            db.update_authority_if_current(idnum, "bad1", replacement, replacement)
                .await
                .is_err()
        );
        assert_eq!(
            db.player_authority_by_id(idnum).await.unwrap(),
            Some(("Authority".to_string(), replacement))
        );
    }

    #[tokio::test]
    async fn delayed_generic_save_cannot_resurrect_a_stale_password() {
        let db = MockDatabase::new();
        let ch = Character::new_player(
            "SaveRace".to_string(),
            crate::types::Class::Warrior,
            crate::types::Race::Human,
        );
        let idnum = db.create_player(&ch, "oldpass").await.unwrap();
        let snapshot = db.load_player("SaveRace").await.unwrap();
        let replacement_hash = crate::password::hash_password("newpass");
        db.set_save_delay(Some(std::time::Duration::from_millis(25)));

        let (save_result, update_result) = tokio::join!(db.save_player(&snapshot), async {
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
            db.update_password_hash(idnum, "SaveRace", None, &replacement_hash)
                .await
        });

        save_result.unwrap();
        assert_eq!(
            update_result.unwrap(),
            crate::PasswordHashUpdateOutcome::Updated
        );
        assert!(!db.verify_password("SaveRace", "oldpass").await.unwrap());
        assert!(db.verify_password("SaveRace", "newpass").await.unwrap());
    }

    #[tokio::test]
    async fn concurrent_implementor_bootstraps_promote_exactly_one_player() {
        let db = MockDatabase::new();
        let first = Character::new_player(
            "Firstadmin".to_string(),
            crate::types::Class::Warrior,
            crate::types::Race::Human,
        );
        let second = Character::new_player(
            "Secondadmin".to_string(),
            crate::types::Class::Cleric,
            crate::types::Race::Elf,
        );
        db.create_player(&first, "first-pass").await.unwrap();
        db.create_player(&second, "second-pass").await.unwrap();

        let (first_outcome, second_outcome) = tokio::join!(
            db.bootstrap_implementor("Firstadmin"),
            db.bootstrap_implementor("Secondadmin")
        );
        let outcomes = [first_outcome.unwrap(), second_outcome.unwrap()];
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| **outcome == crate::ImplementorBootstrapOutcome::Promoted)
                .count(),
            1
        );
        assert_eq!(
            db.list_players()
                .await
                .unwrap()
                .iter()
                .filter(|player| player.level >= crate::types::LVL_IMPL)
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn implementor_bootstrap_ignores_tombstones_and_never_promotes_one() {
        let db = MockDatabase::new();
        let tombstone = Character::new_player(
            "Oldadmin".to_string(),
            crate::types::Class::Warrior,
            crate::types::Race::Human,
        );
        db.create_player(&tombstone, "old-pass").await.unwrap();
        let mut tombstone = db.load_player("Oldadmin").await.unwrap();
        tombstone.player.level = crate::types::LVL_IMPL;
        tombstone.trust = i32::from(crate::types::LVL_IMPL);
        tombstone.act_flags |= crate::flags::PLR_DELETED;
        db.save_player(&tombstone).await.unwrap();

        let candidate = Character::new_player(
            "Newadmin".to_string(),
            crate::types::Class::Cleric,
            crate::types::Race::Elf,
        );
        db.create_player(&candidate, "new-pass").await.unwrap();
        assert_eq!(
            db.bootstrap_implementor("Newadmin").await.unwrap(),
            crate::ImplementorBootstrapOutcome::Promoted
        );

        let separate = MockDatabase::new();
        let deleted_target = Character::new_player(
            "Goneadmin".to_string(),
            crate::types::Class::Warrior,
            crate::types::Race::Human,
        );
        separate
            .create_player(&deleted_target, "gone-pass")
            .await
            .unwrap();
        let mut deleted_target = separate.load_player("Goneadmin").await.unwrap();
        deleted_target.act_flags |= crate::flags::PLR_DELETED;
        separate.save_player(&deleted_target).await.unwrap();
        assert_eq!(
            separate.bootstrap_implementor("Goneadmin").await.unwrap(),
            crate::ImplementorBootstrapOutcome::TargetNotFound
        );
    }

    #[tokio::test]
    async fn implementor_bootstrap_uses_effective_trust_not_display_level() {
        let db = MockDatabase::new();
        let level_only = Character::new_player(
            "Levelonly".to_string(),
            crate::types::Class::Warrior,
            crate::types::Race::Human,
        );
        db.create_player(&level_only, "level-pass").await.unwrap();
        let mut level_only = db.load_player("Levelonly").await.unwrap();
        level_only.player.level = crate::types::LVL_IMPL;
        level_only.trust = 1;
        db.save_player(&level_only).await.unwrap();
        let indexed_level_only = db
            .list_players()
            .await
            .unwrap()
            .into_iter()
            .find(|player| player.name == "Levelonly")
            .unwrap();
        assert_eq!(indexed_level_only.level, crate::types::LVL_IMPL);
        assert_eq!(indexed_level_only.trust, 1);

        let candidate = Character::new_player(
            "Candidate".to_string(),
            crate::types::Class::Cleric,
            crate::types::Race::Elf,
        );
        db.create_player(&candidate, "candidate-pass")
            .await
            .unwrap();
        assert_eq!(
            db.bootstrap_implementor("Candidate").await.unwrap(),
            crate::ImplementorBootstrapOutcome::Promoted,
            "level without Implementor trust must not block the one-time bootstrap"
        );

        let separate = MockDatabase::new();
        let trust_only = Character::new_player(
            "Trustadmin".to_string(),
            crate::types::Class::Warrior,
            crate::types::Race::Human,
        );
        separate
            .create_player(&trust_only, "trust-pass")
            .await
            .unwrap();
        let mut trust_only = separate.load_player("Trustadmin").await.unwrap();
        trust_only.player.level = 1;
        trust_only.trust = i32::from(crate::types::LVL_IMPL);
        separate.save_player(&trust_only).await.unwrap();
        let indexed_trust_only = separate
            .list_players()
            .await
            .unwrap()
            .into_iter()
            .find(|player| player.name == "Trustadmin")
            .unwrap();
        assert_eq!(indexed_trust_only.level, 1);
        assert_eq!(indexed_trust_only.trust, i32::from(crate::types::LVL_IMPL));

        let other = Character::new_player(
            "Otheradmin".to_string(),
            crate::types::Class::Cleric,
            crate::types::Race::Elf,
        );
        separate.create_player(&other, "other-pass").await.unwrap();
        assert_eq!(
            separate.bootstrap_implementor("Otheradmin").await.unwrap(),
            crate::ImplementorBootstrapOutcome::AlreadyExists("Trustadmin".to_string()),
            "persisted Implementor trust is authoritative even when level is lower"
        );
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
