use tokio::net::TcpStream;
use tokio::io::{AsyncWriteExt, BufReader, AsyncBufReadExt};
use tokio::sync::mpsc;
use std::sync::Arc;
use parking_lot::RwLock;
use crate::character::Character;
use std::net::SocketAddr;
use anyhow::Result;

// Connection states
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    GetName,
    GetOldPassword,
    GetNewName,
    Confirm,
    GetNewPassword,
    ConfirmPassword,
    GetSex,
    GetClass,
    GetRace,
    ReadMotd,
    Menu,
    Playing,
    Close,
}

// Color codes
pub const COLOR_CODES: &[(&str, &str)] = &[
    ("&n", "\x1b[0m"),     // Normal
    ("&r", "\x1b[0;31m"),  // Red
    ("&g", "\x1b[0;32m"),  // Green
    ("&y", "\x1b[0;33m"),  // Yellow
    ("&b", "\x1b[0;34m"),  // Blue
    ("&m", "\x1b[0;35m"),  // Magenta
    ("&c", "\x1b[0;36m"),  // Cyan
    ("&w", "\x1b[0;37m"),  // White
    ("&R", "\x1b[1;31m"),  // Bright Red
    ("&G", "\x1b[1;32m"),  // Bright Green
    ("&Y", "\x1b[1;33m"),  // Bright Yellow
    ("&B", "\x1b[1;34m"),  // Bright Blue
    ("&M", "\x1b[1;35m"),  // Bright Magenta
    ("&C", "\x1b[1;36m"),  // Bright Cyan
    ("&W", "\x1b[1;37m"),  // Bright White
];

pub struct Connection {
    pub id: u64,
    pub addr: SocketAddr,
    pub state: ConnectionState,
    pub character: Option<Arc<RwLock<Character>>>,
    pub original: Option<Arc<RwLock<Character>>>,  // For switch command
    
    // I/O channels
    pub output_tx: mpsc::Sender<String>,
    pub input_rx: Option<mpsc::Receiver<String>>,
    
    // Temporary data during character creation
    pub temp_name: Option<String>,
    pub temp_password: Option<String>,
}

impl Connection {
    pub fn new(id: u64, addr: SocketAddr, output_tx: mpsc::Sender<String>) -> Self {
        Connection {
            id,
            addr,
            state: ConnectionState::GetName,
            character: None,
            original: None,
            output_tx,
            input_rx: None,
            temp_name: None,
            temp_password: None,
        }
    }
    
    pub async fn send(&self, message: &str) -> Result<()> {
        let processed = self.process_color_codes(message);
        self.output_tx.send(processed).await?;
        Ok(())
    }
    
    pub async fn send_line(&self, message: &str) -> Result<()> {
        self.send(&format!("{}\r\n", message)).await
    }
    
    pub async fn send_prompt(&self) -> Result<()> {
        match self.state {
            ConnectionState::Playing => {
                if let Some(ch) = &self.character {
                    let (hit, mana, mv) = {
                        let ch = ch.read();
                        (ch.points.hit, ch.points.mana, ch.points.move_points)
                    };
                    let prompt = format!("&g{}H &c{}M &y{}V&n> ", hit, mana, mv);
                    self.send(&prompt).await?;
                }
            }
            ConnectionState::GetName => {
                self.send("By what name do you wish to be known? ").await?;
            }
            ConnectionState::GetOldPassword => {
                self.send("Password: ").await?;
            }
            ConnectionState::GetNewPassword => {
                self.send("Give me a password for your character: ").await?;
            }
            ConnectionState::ConfirmPassword => {
                self.send("Please retype password: ").await?;
            }
            ConnectionState::GetSex => {
                self.send("What is your sex (M/F)? ").await?;
            }
            ConnectionState::GetClass => {
                self.send_line("\r\nSelect a class:").await?;
                self.send_line("  &YW&n) &YW&narrior").await?;
                self.send_line("  &YC&n) &YC&nleric").await?;
                self.send_line("  &YT&n) &YT&nhief").await?;
                self.send_line("  &YM&n) &YM&nagic User").await?;
                self.send_line("  &YA&n) &YA&nrtisan").await?;
                self.send("\r\nClass: ").await?;
            }
            ConnectionState::GetRace => {
                self.send_line("\r\nSelect a race:").await?;
                self.send_line("  &YH&n) &YH&numan").await?;
                self.send_line("  &YE&n) &YE&nlf").await?;
                self.send_line("  &YD&n) &YD&nwarf").await?;
                self.send_line("  &YG&n) &YG&nnome").await?;
                self.send("\r\nRace: ").await?;
            }
            ConnectionState::Menu => {
                self.send_line("\r\n&YWelcome to DeltaMUD!&n").await?;
                self.send_line("\r\n&g0&n) Exit from DeltaMUD.").await?;
                self.send_line("&g1&n) Enter the game.").await?;
                self.send_line("&g2&n) Enter description.").await?;
                self.send_line("&g3&n) Read the background story.").await?;
                self.send_line("&g4&n) Change password.").await?;
                self.send_line("&g5&n) Delete this character.").await?;
                self.send("\r\n   Make your choice: ").await?;
            }
            _ => {}
        }
        Ok(())
    }
    
    fn process_color_codes(&self, text: &str) -> String {
        let mut result = text.to_string();
        for (code, ansi) in COLOR_CODES {
            result = result.replace(code, ansi);
        }
        result
    }
    
    pub fn close(&mut self) {
        self.state = ConnectionState::Close;
    }
}

// Handles individual client connections
pub async fn handle_client(
    stream: TcpStream,
    addr: SocketAddr,
    conn_id: u64,
    game_tx: mpsc::Sender<GameMessage>,
) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    
    // Create output channel
    let (output_tx, mut output_rx) = mpsc::channel(100);
    
    // Notify game of new connection
    game_tx.send(GameMessage::NewConnection {
        id: conn_id,
        addr,
        output_tx: output_tx.clone(),
    }).await?;
    
    // Send welcome message
    let welcome = "\r\n&YWelcome to DeltaMUD!&n\r\n\r\n";
    output_tx.send(welcome.to_string()).await?;
    
    // Spawn task to handle output
    let write_handle = tokio::spawn(async move {
        while let Some(msg) = output_rx.recv().await {
            if writer.write_all(msg.as_bytes()).await.is_err() {
                break;
            }
            if writer.flush().await.is_err() {
                break;
            }
        }
    });
    
    // Read input
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) => break, // EOF
            Ok(_) => {
                let input = line.trim().to_string();
                if !input.is_empty() {
                    game_tx.send(GameMessage::Input {
                        conn_id,
                        input,
                    }).await?;
                }
            }
            Err(_) => break,
        }
    }
    
    // Notify disconnection
    game_tx.send(GameMessage::Disconnect { conn_id }).await?;
    
    // Cleanup
    write_handle.abort();
    Ok(())
}

// Messages sent to the main game loop
#[derive(Debug)]
pub enum GameMessage {
    NewConnection {
        id: u64,
        addr: SocketAddr,
        output_tx: mpsc::Sender<String>,
    },
    Input {
        conn_id: u64,
        input: String,
    },
    Disconnect {
        conn_id: u64,
    },
}