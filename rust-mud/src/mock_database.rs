// Mock database for testing without MySQL
use crate::character::Character;
use anyhow::Result;
use std::collections::HashMap;
use std::sync::Mutex;

pub struct MockDatabase {
    players: Mutex<HashMap<String, Character>>,
}

impl MockDatabase {
    pub fn new() -> Self {
        MockDatabase {
            players: Mutex::new(HashMap::new()),
        }
    }
}

#[async_trait::async_trait]
impl crate::DatabaseInterface for MockDatabase {
    async fn init_tables(&self) -> Result<()> {
        // Mock implementation - always succeeds
        Ok(())
    }
    
    async fn player_exists(&self, name: &str) -> Result<bool> {
        let players = self.players.lock().unwrap();
        Ok(players.contains_key(name))
    }
    
    async fn create_player(&self, character: &Character, _password: &str) -> Result<u64> {
        let mut players = self.players.lock().unwrap();
        let id = (players.len() + 1) as u64;
        let mut ch = character.clone_for_save();
        ch.id = id;
        players.insert(character.get_name().to_string(), ch);
        Ok(id)
    }
    
    async fn load_player(&self, name: &str) -> Result<Character> {
        let players = self.players.lock().unwrap();
        if let Some(character) = players.get(name) {
            Ok(character.clone_for_save())
        } else {
            Err(anyhow::anyhow!("Player not found"))
        }
    }
    
    async fn save_player(&self, character: &Character) -> Result<()> {
        let mut players = self.players.lock().unwrap();
        players.insert(character.get_name().to_string(), character.clone_for_save());
        Ok(())
    }
    
    async fn verify_password(&self, name: &str, password: &str) -> Result<bool> {
        // For testing, accept any password for existing players
        // or password "test" for new players
        let players = self.players.lock().unwrap();
        Ok(players.contains_key(name) || password == "test")
    }
}