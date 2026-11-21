//! Per-device Nostr identity, stored as a hex secret in
//! ~/.local/share/omessage/identity.key with mode 0600.

use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use nostr::prelude::*;

use crate::paths;

pub fn exists() -> bool {
    paths::key_path().exists()
}

pub fn load() -> anyhow::Result<Keys> {
    let path = paths::key_path();
    let meta = std::fs::metadata(&path)?;
    if meta.permissions().mode() & 0o077 != 0 {
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    let raw = std::fs::read_to_string(&path)?;
    Ok(Keys::parse(raw.trim())?)
}

/// Load the identity, creating one the first time.
pub fn load_or_create() -> anyhow::Result<(Keys, bool)> {
    if exists() {
        return Ok((load()?, false));
    }
    let keys = Keys::generate();
    write_secret(&paths::key_path(), &keys.secret_key().to_secret_hex(), false)?;
    Ok((keys, true))
}

pub fn import(secret: &str) -> anyhow::Result<Keys> {
    let keys = Keys::parse(secret.trim())?;
    if exists() {
        anyhow::bail!(
            "an identity already exists at {}; move it away first",
            paths::key_path().display()
        );
    }
    write_secret(&paths::key_path(), &keys.secret_key().to_secret_hex(), false)?;
    Ok(keys)
}

/// Write a secret to a new 0600 file. Refuses to overwrite unless asked.
pub fn write_secret(path: &Path, secret: &str, overwrite: bool) -> anyhow::Result<()> {
    if let Some(parent) = path.parent()
        && parent == paths::data_dir()
    {
        paths::private_dir(&parent.to_path_buf())?;
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).mode(0o600);
    if overwrite {
        opts.create(true).truncate(true);
    } else {
        opts.create_new(true);
    }
    let mut f = opts.open(path)?;
    f.write_all(secret.as_bytes())?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

pub fn npub(pk: &PublicKey) -> String {
    pk.to_bech32().unwrap_or_else(|_| pk.to_hex())
}

pub fn nsec(keys: &Keys) -> String {
    keys.secret_key().to_bech32().expect("bech32")
}
