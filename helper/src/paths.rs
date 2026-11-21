//! XDG locations used by the helper. Nothing outside these directories is
//! ever written.

use std::fs;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::PathBuf;

fn env_dir(var: &str, fallback: &str) -> PathBuf {
    match std::env::var_os(var) {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => home().join(fallback),
    }
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// ~/.local/share/omessage — identity key and message database (0700).
pub fn data_dir() -> PathBuf {
    env_dir("XDG_DATA_HOME", ".local/share").join("omessage")
}

/// ~/.config/omessage — relay configuration.
pub fn config_dir() -> PathBuf {
    env_dir("XDG_CONFIG_HOME", ".config").join("omessage")
}

/// $XDG_RUNTIME_DIR/omessage — the control socket (0700).
pub fn runtime_dir() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(v) if !v.is_empty() => PathBuf::from(v).join("omessage"),
        _ => data_dir().join("run"),
    }
}

pub fn socket_path() -> PathBuf {
    runtime_dir().join("omessaged.sock")
}

pub fn key_path() -> PathBuf {
    data_dir().join("identity.key")
}

pub fn db_path() -> PathBuf {
    data_dir().join("messages.db")
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.json")
}

/// Create a directory (and parents) restricted to the owner.
pub fn private_dir(p: &PathBuf) -> std::io::Result<()> {
    fs::DirBuilder::new().recursive(true).mode(0o700).create(p)?;
    fs::set_permissions(p, fs::Permissions::from_mode(0o700))
}
