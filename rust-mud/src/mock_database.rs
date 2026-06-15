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
}

impl MockDatabase {
    pub fn new() -> Self {
        MockDatabase {
            players: Mutex::new(HashMap::new()),
            next_idnum: Mutex::new(1),
        }
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
        Ok(self.players.lock().unwrap().contains_key(&name.to_lowercase()))
    }

    async fn create_player(&self, character: &Character, password: &str) -> Result<i64> {
        let mut players = self.players.lock().unwrap();
        let mut idnum = self.next_idnum.lock().unwrap();
        let id = *idnum;
        *idnum += 1;
        let mut ch = character.clone();
        ch.idnum = id;
        players.insert(
            character.get_name().to_lowercase(),
            Stored { character: ch, password: password.to_string() },
        );
        Ok(id)
    }

    async fn load_player(&self, name: &str) -> Result<Character> {
        let players = self.players.lock().unwrap();
        players
            .get(&name.to_lowercase())
            .map(|s| s.character.clone())
            .ok_or_else(|| anyhow::anyhow!("Player not found"))
    }

    async fn save_player(&self, character: &Character) -> Result<()> {
        let mut players = self.players.lock().unwrap();
        if let Some(s) = players.get_mut(&character.get_name().to_lowercase()) {
            s.character = character.clone();
        }
        Ok(())
    }

    async fn verify_password(&self, name: &str, password: &str) -> Result<bool> {
        let players = self.players.lock().unwrap();
        match players.get(&name.to_lowercase()) {
            Some(s) => Ok(s.password == password),
            None => Ok(false),
        }
    }

    async fn list_players(&self) -> Result<Vec<crate::state::PlayerIndex>> {
        // Build index rows from the stored Character clones (the mock has no
        // host column — host is connection-derived, "" here; the live
        // descriptor's host is folded in later via update_player_index).
        let players = self.players.lock().unwrap();
        let out = players
            .values()
            .map(|s| crate::state::PlayerIndex {
                idnum: s.character.idnum,
                name: s.character.get_name().to_string(),
                level: s.character.player.level,
                last_logon: s.character.last_logon.timestamp(),
                host: String::new(),
            })
            .collect();
        Ok(out)
    }
}
