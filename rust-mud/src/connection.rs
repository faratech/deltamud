// Connection edge: async socket I/O over channels, plus the per-connection
// Descriptor that lives inside GameState. Commands never touch sockets —
// they append to Descriptor::outbuf, which the Game task flushes to the
// writer task after each command/pulse (mirrors C process_input/process_output).

use crate::types::{CharId, ConnId};
use anyhow::Result;
use std::net::SocketAddr;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

/// Connection / login state machine. Editor/OLC states will extend this via
/// the nested-input stack (Batch 1 Pillar D groundwork: `editor`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConState {
    GetName,
    GetOldPassword,
    ConfirmName, // "Did I get that right (Y/N)?"
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

/// A nested input context (string editor, OLC editor). Tier-0 stub; the
/// stack lets a Playing descriptor push an editor without a giant enum.
#[derive(Debug, Clone)]
pub enum InputContext {
    StringEdit { buffer: String, max_len: usize },
}

pub struct Descriptor {
    pub id: ConnId,
    pub host: String,
    pub state: ConState,
    /// Stack of nested input contexts; empty == normal command/menu input.
    pub editors: Vec<InputContext>,
    pub character: Option<CharId>,
    pub original: Option<CharId>, // for `switch`
    /// Output accumulated this pulse; flushed by the Game task.
    pub outbuf: String,
    /// True when a fresh prompt should be sent after flushing.
    pub need_prompt: bool,
    /// Command-lag counter (C `d->wait`): the heartbeat decrements it each pulse
    /// and only pulls the next queued command when it reaches <= 0. WAIT_STATE
    /// sets it from combat skills/casting to impose command lag.
    pub wait: i32,
    /// Queued raw input lines awaiting the wait gate (C `d->input`).
    pub input_queue: std::collections::VecDeque<String>,
    // Scratch during login / char creation.
    pub temp_name: Option<String>,
    pub temp_password: Option<String>,
}

impl Descriptor {
    pub fn new(id: ConnId, host: String) -> Self {
        Descriptor {
            id,
            host,
            state: ConState::GetName,
            editors: Vec::new(),
            character: None,
            original: None,
            outbuf: String::new(),
            need_prompt: true,
            wait: 1,
            input_queue: std::collections::VecDeque::new(),
            temp_name: None,
            temp_password: None,
        }
    }

    pub fn write(&mut self, msg: &str) {
        self.outbuf.push_str(msg);
    }
}

// ANSI color: DeltaMUD `&x` codes. (Render path; the strip path for
// color-off players is added with the act() engine.)
pub const COLOR_CODES: &[(&str, &str)] = &[
    ("&n", "\x1b[0m"),
    ("&r", "\x1b[0;31m"),
    ("&g", "\x1b[0;32m"),
    ("&y", "\x1b[0;33m"),
    ("&b", "\x1b[0;34m"),
    ("&m", "\x1b[0;35m"),
    ("&c", "\x1b[0;36m"),
    ("&w", "\x1b[0;37m"),
    ("&R", "\x1b[1;31m"),
    ("&G", "\x1b[1;32m"),
    ("&Y", "\x1b[1;33m"),
    ("&B", "\x1b[1;34m"),
    ("&M", "\x1b[1;35m"),
    ("&C", "\x1b[1;36m"),
    ("&W", "\x1b[1;37m"),
];

pub fn render_color(text: &str) -> String {
    let mut result = text.to_string();
    for (code, ansi) in COLOR_CODES {
        result = result.replace(code, ansi);
    }
    result
}

/// Strip color codes (for players with color off / for logs).
pub fn strip_color(text: &str) -> String {
    let mut result = text.to_string();
    for (code, _) in COLOR_CODES {
        result = result.replace(code, "");
    }
    result
}

// Messages from connection tasks to the single Game task.
#[derive(Debug)]
pub enum GameMessage {
    NewConnection {
        id: ConnId,
        host: String,
        output_tx: mpsc::Sender<String>,
    },
    Input {
        conn_id: ConnId,
        input: String,
    },
    Disconnect {
        conn_id: ConnId,
    },
}

/// Per-connection task: split the stream, register with the Game, pump input
/// lines into the game channel, and run a writer task draining output.
pub async fn handle_client(
    stream: TcpStream,
    addr: SocketAddr,
    conn_id: ConnId,
    game_tx: mpsc::Sender<GameMessage>,
) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    let (output_tx, mut output_rx) = mpsc::channel::<String>(256);

    game_tx
        .send(GameMessage::NewConnection {
            id: conn_id,
            host: addr.ip().to_string(),
            output_tx: output_tx.clone(),
        })
        .await?;

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

    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) => break, // EOF
            Ok(_) => {
                let input = line.trim_end_matches(['\r', '\n']).to_string();
                game_tx
                    .send(GameMessage::Input { conn_id, input })
                    .await?;
            }
            Err(_) => break,
        }
    }

    let _ = game_tx.send(GameMessage::Disconnect { conn_id }).await;
    write_handle.abort();
    Ok(())
}
