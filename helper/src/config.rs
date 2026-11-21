//! ~/.config/omessage/config.json

use serde::{Deserialize, Serialize};

use crate::paths;

pub const DEFAULT_RELAYS: &[&str] = &["wss://relay.damus.io", "wss://nos.lol", "wss://relay.primal.net"];
pub const DEFAULT_INBOX_RELAYS: &[&str] = &["wss://relay.damus.io", "wss://nos.lol"];
pub const DEFAULT_DISCOVERY_RELAYS: &[&str] = &["wss://purplepag.es", "wss://relay.nos.social"];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Relays the helper stays connected to (reads and general writes).
    pub relays: Vec<String>,
    /// Relays advertised in the kind 10050 DM inbox list; others send our
    /// gift-wrapped DMs here. Put a personal always-on relay first.
    pub inbox_relays: Vec<String>,
    /// Indexer relays used to look up other people's inbox lists and to
    /// publish our own.
    pub discovery_relays: Vec<String>,
    /// Desktop notifications for incoming messages.
    pub notifications: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            relays: DEFAULT_RELAYS.iter().map(|s| s.to_string()).collect(),
            inbox_relays: DEFAULT_INBOX_RELAYS.iter().map(|s| s.to_string()).collect(),
            discovery_relays: DEFAULT_DISCOVERY_RELAYS.iter().map(|s| s.to_string()).collect(),
            notifications: true,
        }
    }
}

impl Config {
    /// Load the config, writing the defaults the first time.
    pub fn load() -> anyhow::Result<Self> {
        let path = paths::config_path();
        if let Ok(raw) = std::fs::read_to_string(&path) {
            let cfg: Config = serde_json::from_str(&raw).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
            return Ok(cfg);
        }
        let cfg = Config::default();
        std::fs::create_dir_all(paths::config_dir())?;
        std::fs::write(&path, serde_json::to_string_pretty(&cfg)? + "\n")?;
        Ok(cfg)
    }

    /// Every relay the client keeps in its pool.
    pub fn all_relays(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for r in self
            .relays
            .iter()
            .chain(&self.inbox_relays)
            .chain(&self.discovery_relays)
        {
            if !out.contains(r) {
                out.push(r.clone());
            }
        }
        out
    }
}
