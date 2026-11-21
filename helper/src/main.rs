//! omessaged — background helper for the Omessage Omarchy plugin.
//!
//! Usage:
//!   omessaged daemon                    run the service (systemd --user)
//!   omessaged watch                     stream JSON snapshots (used by the panel)
//!   omessaged snapshot                  print one JSON snapshot
//!   omessaged send <who>                send a NIP-17 private message read from stdin
//!   omessaged rpc                       read one JSON request from stdin and send it
//!                                       to the service (used by the panel, so message
//!                                       text and contacts never appear in argv)
//!   omessaged retry <message-id>        send a failed message again
//!   omessaged add <who> [name]          add a contact (npub, hex or NIP-05)
//!   omessaged rename <who> <name>       set a contact nickname
//!   omessaged read <who>                mark a conversation read
//!   omessaged delete <who>              delete a conversation locally
//!   omessaged accept <who>              accept a message request
//!   omessaged block <who>               block a sender and drop their messages
//!   omessaged unblock <who>             unblock a sender
//!   omessaged id                        print this device's npub
//!   omessaged export-key [--out PATH]   write the secret key (nsec) to a new 0600 file
//!   omessaged export-key --stdout       print the nsec (explicit opt-in)
//!   omessaged import-key                read an nsec from stdin (no existing identity)

mod config;
mod daemon;
mod identity;
mod paths;
mod store;

use std::io::Write;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

fn usage() -> ! {
    eprintln!(
        "omessaged {}\nusage: omessaged daemon|watch|snapshot|rpc|send <who> (message on stdin)|retry <id>|add <who> [name]|rename <who> <name>|read <who>|delete <who>|accept <who>|block <who>|unblock <who>|id|export-key [--out PATH|--stdout]|import-key",
        env!("CARGO_PKG_VERSION")
    );
    std::process::exit(2)
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("");
    let code = match run(cmd, &args[1.min(args.len())..]).await {
        Ok(c) => c,
        Err(e) => {
            println!("{}", json!({"ok": false, "error": e.to_string()}));
            1
        }
    };
    std::process::exit(code);
}

async fn run(cmd: &str, rest: &[String]) -> anyhow::Result<i32> {
    match cmd {
        "daemon" => {
            daemon::run().await?;
            Ok(0)
        }
        "watch" => watch().await,
        "snapshot" | "status" => request(json!({"cmd": "snapshot"})).await,
        "send" => {
            // The text comes from stdin, never argv: other local users can
            // read every process's arguments.
            let Some(to) = rest.first() else { usage() };
            if rest.len() > 1 {
                anyhow::bail!("pass the message on stdin, not as an argument: echo 'hi' | omessaged send <who>");
            }
            let text = read_stdin(MAX_STDIN_BYTES, false).await?;
            request(json!({"cmd": "send", "to": to, "text": text})).await
        }
        "rpc" => {
            let line = read_stdin(MAX_STDIN_BYTES, true).await?;
            request(parse_rpc(&line)?).await
        }
        "retry" => {
            let Some(id) = rest.first() else { usage() };
            request(json!({"cmd": "retry", "id": id})).await
        }
        "add" => {
            let Some(who) = rest.first() else { usage() };
            let name = rest.get(1..).map(|r| r.join(" ")).filter(|s| !s.is_empty());
            request(json!({"cmd": "add", "who": who, "name": name})).await
        }
        "rename" => {
            if rest.len() < 2 {
                usage()
            }
            request(json!({"cmd": "rename", "who": rest[0], "name": rest[1..].join(" ")})).await
        }
        "read" | "delete" | "accept" | "block" | "unblock" => {
            let Some(who) = rest.first() else { usage() };
            request(json!({"cmd": cmd, "who": who})).await
        }
        "id" => {
            let keys =
                identity::load().map_err(|_| anyhow::anyhow!("no identity yet; start the omessaged service once"))?;
            println!("{}", identity::npub(&keys.public_key()));
            Ok(0)
        }
        "export-key" => export_key(rest),
        "import-key" => {
            let mut s = String::new();
            std::io::stdin().read_line(&mut s)?;
            let keys = identity::import(&s)?;
            println!("{}", json!({"ok": true, "npub": identity::npub(&keys.public_key())}));
            Ok(0)
        }
        "version" | "--version" => {
            println!("omessaged {}", env!("CARGO_PKG_VERSION"));
            Ok(0)
        }
        _ => usage(),
    }
}

fn export_key(rest: &[String]) -> anyhow::Result<i32> {
    let keys = identity::load().map_err(|_| anyhow::anyhow!("no identity to export"))?;
    if rest.iter().any(|a| a == "--stdout") {
        println!("{}", identity::nsec(&keys));
        return Ok(0);
    }
    let npub = identity::npub(&keys.public_key());
    let path = match rest.iter().position(|a| a == "--out") {
        Some(i) => std::path::PathBuf::from(rest.get(i + 1).ok_or_else(|| anyhow::anyhow!("--out needs a path"))?),
        None => paths::home().join(format!("omessage-key-backup-{}.txt", &npub[5..13])),
    };
    identity::write_secret(&path, &identity::nsec(&keys), false)
        .map_err(|e| anyhow::anyhow!("could not write {}: {e}", path.display()))?;
    println!(
        "{}",
        json!({"ok": true, "path": path.display().to_string(), "npub": npub})
    );
    Ok(0)
}

/// Largest stdin payload `send` and `rpc` accept (the service caps
/// messages at 16 KiB; JSON escaping can roughly double that).
const MAX_STDIN_BYTES: u64 = 64 * 1024;

/// Read stdin up to `max` bytes: the first line if `one_line`, else all of
/// it. Gives up after 10 s, so a caller that never writes can't hang us.
async fn read_stdin(max: u64, one_line: bool) -> anyhow::Result<String> {
    use tokio::io::AsyncReadExt;
    let mut input = tokio::io::stdin().take(max + 1);
    let read = async {
        let mut buf = Vec::new();
        if one_line {
            BufReader::new(&mut input).read_until(b'\n', &mut buf).await?;
        } else {
            input.read_to_end(&mut buf).await?;
        }
        anyhow::Ok(buf)
    };
    let buf = tokio::time::timeout(Duration::from_secs(10), read)
        .await
        .map_err(|_| anyhow::anyhow!("timed out reading stdin"))??;
    if buf.len() as u64 > max {
        anyhow::bail!("input is too long");
    }
    Ok(String::from_utf8(buf)?)
}

/// Check one panel request: a JSON object with a `cmd` the service runs
/// once (not `watch`, which streams).
fn parse_rpc(line: &str) -> anyhow::Result<Value> {
    let req: Value = serde_json::from_str(line.trim()).map_err(|e| anyhow::anyhow!("bad request: {e}"))?;
    match req["cmd"].as_str() {
        Some("watch") | None => anyhow::bail!("bad request: missing or unsupported cmd"),
        Some(_) if req.is_object() => Ok(req),
        Some(_) => anyhow::bail!("bad request"),
    }
}

async fn request(req: Value) -> anyhow::Result<i32> {
    let stream = UnixStream::connect(paths::socket_path())
        .await
        .map_err(|_| anyhow::anyhow!("omessaged is not running (systemctl --user start omessaged)"))?;
    let (r, mut w) = stream.into_split();
    w.write_all(format!("{req}\n").as_bytes()).await?;
    let mut lines = BufReader::new(r).lines();
    let line = tokio::time::timeout(Duration::from_secs(45), lines.next_line())
        .await
        .map_err(|_| anyhow::anyhow!("timed out waiting for omessaged"))??
        .unwrap_or_default();
    println!("{line}");
    let v: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
    Ok(if v["ok"] == false { 1 } else { 0 })
}

/// Stream snapshots forever, reconnecting when the service restarts. Each
/// line is one JSON object; `{"type":"offline"}` while the service is down.
async fn watch() -> anyhow::Result<i32> {
    let mut stdout = std::io::stdout();
    loop {
        match UnixStream::connect(paths::socket_path()).await {
            Ok(stream) => {
                let (r, mut w) = stream.into_split();
                if w.write_all(b"{\"cmd\":\"watch\"}\n").await.is_ok() {
                    let mut lines = BufReader::new(r).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        if writeln!(stdout, "{line}").and_then(|_| stdout.flush()).is_err() {
                            return Ok(0); // reader went away
                        }
                    }
                }
            }
            Err(_) => {
                let npub = identity::load().ok().map(|k| identity::npub(&k.public_key()));
                let line = json!({"type": "offline", "npub": npub, "identity": identity::exists()});
                if writeln!(stdout, "{line}").and_then(|_| stdout.flush()).is_err() {
                    return Ok(0);
                }
            }
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rpc_accepts_one_shot_commands_only() {
        let req = parse_rpc("{\"cmd\":\"send\",\"to\":\"npub1x\",\"text\":\"hi\"}\n").unwrap();
        assert_eq!(req["text"], "hi");
        assert!(parse_rpc("{\"cmd\":\"watch\"}").is_err());
        assert!(parse_rpc("{\"to\":\"npub1x\"}").is_err());
        assert!(parse_rpc("[\"send\"]").is_err());
        assert!(parse_rpc("not json").is_err());
    }
}
