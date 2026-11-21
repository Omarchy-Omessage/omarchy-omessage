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
    /// Load the config. Only a missing file counts as a first run (the
    /// defaults are written then). Any other read or parse error is fatal:
    /// the file is never overwritten, so private relay settings can't be
    /// silently replaced with public defaults and then published.
    pub fn load() -> anyhow::Result<Self> {
        Self::load_from(&paths::config_path())
    }

    pub fn load_from(path: &std::path::Path) -> anyhow::Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(raw) => serde_json::from_str(&raw)
                .map_err(|e| anyhow::anyhow!("{} is not valid JSON ({e}); fix or remove it", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let cfg = Config::default();
                if let Some(dir) = path.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                // create_new: never replace a file that appeared meanwhile.
                let mut f = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(path)
                    .map_err(|e| anyhow::anyhow!("could not create {}: {e}", path.display()))?;
                std::io::Write::write_all(&mut f, (serde_json::to_string_pretty(&cfg)? + "\n").as_bytes())?;
                Ok(cfg)
            }
            Err(e) => anyhow::bail!("could not read {}: {e}", path.display()),
        }
    }

    /// Your own relays, where the helper reads messages addressed to you
    /// (the only connections that authenticate with your key).
    pub fn reader_relays(&self) -> Vec<String> {
        union(&[&self.inbox_relays, &self.relays])
    }

    /// Relays used for lookups (profiles, other people's relay lists).
    /// These connections never authenticate and never ask for your messages.
    pub fn lookup_relays(&self) -> Vec<String> {
        union(&[&self.discovery_relays, &self.relays])
    }

    /// Where the kind 10050 inbox list is published.
    pub fn all_relays(&self) -> Vec<String> {
        union(&[&self.relays, &self.inbox_relays, &self.discovery_relays])
    }
}

fn union(lists: &[&Vec<String>]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for r in lists.iter().flat_map(|l| l.iter()) {
        if !out.contains(r) {
            out.push(r.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("omessage-cfg-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn missing_file_writes_defaults() {
        let p = tmp("missing").join("sub/config.json");
        let cfg = Config::load_from(&p).unwrap();
        assert_eq!(cfg.inbox_relays, DEFAULT_INBOX_RELAYS);
        assert!(p.exists());
    }

    #[test]
    fn invalid_json_fails_and_keeps_file() {
        let p = tmp("invalid").join("config.json");
        std::fs::write(&p, "{ not json").unwrap();
        assert!(Config::load_from(&p).is_err());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "{ not json");
    }

    #[test]
    fn unreadable_file_fails_and_keeps_file() {
        // A directory where the file should be: read_to_string fails with
        // something other than NotFound, and nothing may be written.
        let p = tmp("unreadable").join("config.json");
        std::fs::create_dir(&p).unwrap();
        assert!(Config::load_from(&p).is_err());
        assert!(p.is_dir());
    }

    #[test]
    fn existing_settings_are_kept() {
        let p = tmp("existing").join("config.json");
        std::fs::write(&p, r#"{"inbox_relays":["wss://my.example"]}"#).unwrap();
        let cfg = Config::load_from(&p).unwrap();
        assert_eq!(cfg.inbox_relays, vec!["wss://my.example".to_string()]);
        assert_eq!(cfg.relays, DEFAULT_RELAYS);
    }
}
