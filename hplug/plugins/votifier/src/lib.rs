//! NuVotifier replacement: a vote listener on its own TCP port (`port`, default 8192, `host` default 0.0.0.0) that
//! speaks both NuVotifier protocols, so the vote sites keep their settings:
//! * v1: 256 bytes RSA/PKCS#1 v1.5 with the server's key ("VOTE\nservice\nusername\naddress\ntimestamp\n").
//!   Keys: `config/hplug/votifier/rsa/private.key` + `public.key` (base64 PKCS#8 / X.509 DER, NuVotifier's own
//!   format): COPY `plugins/Votifier/rsa/` from Paper so the public key the sites hold stays valid. Missing = v1 off
//!   (a key is NOT generated here: generate with NuVotifier or `openssl` and paste it to the vote sites).
//! * v2: `0x733A`, length, JSON `{"payload": "...", "signature": base64(HMAC-SHA256(payload, token))}` with the
//!   greeting's challenge; tokens per service in config `tokens` (`{"default": "..."}`, copy from NuVotifier's
//!   config.yml `tokens:`).
//!   A vote runs `vote_command` on the console (default `reward vote {player} {service}`: the rewards plugin's vote
//!   entry, the Paper VotifierEvent listener's job). Votes for players who never joined are still passed on (the
//!   command decides). Status: `/votifier status` (op), `/votifier test <player> [service]` simulates a vote.
use base64::Engine;
use hmac::{Hmac, Mac};
use hplug::{json, Ctx, Plugin};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Mutex;
use std::time::Duration;

#[derive(Clone, Debug)]
struct Vote {
    service: String,
    username: String,
    address: String,
}

struct Shared {
    started: bool,
    queue: Vec<Vote>,
    log: Vec<String>,
    received: u64,
}
static SHARED: Mutex<Shared> = Mutex::new(Shared { started: false, queue: Vec::new(), log: Vec::new(), received: 0 });
fn shared<R>(f: impl FnOnce(&mut Shared) -> R) -> R {
    f(&mut SHARED.lock().unwrap_or_else(|e| e.into_inner()))
}
fn note(s: String) {
    eprintln!("[votifier] {s}");
    shared(|x| {
        x.log.push(s);
        if x.log.len() > 20 {
            x.log.remove(0);
        }
    });
}

struct Config {
    host: String,
    port: u16,
    tokens: json::Map<String, json::Value>,
    key: Option<rsa::RsaPrivateKey>,
}

fn load_key(dir: &std::path::Path) -> Option<rsa::RsaPrivateKey> {
    use rsa::pkcs8::DecodePrivateKey;
    let text = std::fs::read_to_string(dir.join("votifier/rsa/private.key")).ok()?;
    let der = base64::engine::general_purpose::STANDARD.decode(text.split_whitespace().collect::<String>()).ok()?;
    match rsa::RsaPrivateKey::from_pkcs8_der(&der) {
        Ok(k) => Some(k),
        Err(e) => {
            note(format!("rsa/private.key unreadable ({e}); v1 votes are off"));
            None
        }
    }
}

fn challenge() -> String {
    // std's RandomState is seeded from the OS's randomness: two fresh hashers give 128 unpredictable bits
    use std::hash::{BuildHasher, Hasher};
    let mut out = String::new();
    for i in 0..2u64 {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(hplug::now_ms() ^ i);
        out.push_str(&format!("{:016x}", h.finish()));
    }
    out
}

fn handle(mut s: TcpStream, cfg: &Config) -> Result<Vote, String> {
    s.set_read_timeout(Some(Duration::from_secs(5))).ok();
    let ch = challenge();
    s.write_all(format!("VOTIFIER 2 {ch}\n").as_bytes()).map_err(|e| e.to_string())?;
    let mut head = [0u8; 2];
    s.read_exact(&mut head).map_err(|e| format!("read: {e}"))?;
    if head == [0x73, 0x3A] {
        // v2
        let mut len = [0u8; 2];
        s.read_exact(&mut len).map_err(|e| e.to_string())?;
        let mut body = vec![0u8; u16::from_be_bytes(len) as usize];
        s.read_exact(&mut body).map_err(|e| e.to_string())?;
        let r = v2(&body, &ch, cfg);
        let reply = match &r {
            Ok(_) => json::json!({"status": "ok"}),
            Err(e) => json::json!({"status": "error", "cause": "CorruptedFrameException", "error": e}),
        };
        let _ = s.write_all(format!("{reply}\r\n").as_bytes());
        r
    } else {
        // v1: the rest of the 256-byte block
        let mut block = vec![0u8; 256];
        block[..2].copy_from_slice(&head);
        s.read_exact(&mut block[2..]).map_err(|e| format!("v1 block: {e}"))?;
        let key = cfg.key.as_ref().ok_or("v1 vote but no rsa/private.key")?;
        let plain = key.decrypt(rsa::Pkcs1v15Encrypt, &block).map_err(|e| format!("v1 decrypt: {e}"))?;
        let text = String::from_utf8_lossy(&plain);
        let mut l = text.split('\n');
        if l.next() != Some("VOTE") {
            return Err("v1: not a VOTE block".into());
        }
        let (service, username, address) = (l.next().unwrap_or("").trim(), l.next().unwrap_or("").trim(), l.next().unwrap_or("").trim());
        Ok(Vote { service: service.into(), username: username.into(), address: address.into() })
    }
}

fn v2(body: &[u8], ch: &str, cfg: &Config) -> Result<Vote, String> {
    let msg: json::Value = json::from_slice(body).map_err(|e| format!("bad json: {e}"))?;
    let payload_s = msg.get("payload").and_then(|p| p.as_str()).ok_or("no payload")?;
    let sig = msg.get("signature").and_then(|p| p.as_str()).ok_or("no signature")?;
    let payload: json::Value = json::from_str(payload_s).map_err(|e| format!("bad payload: {e}"))?;
    let service = payload.get("serviceName").and_then(|v| v.as_str()).unwrap_or("").to_owned();
    let token = cfg.tokens.get(&service).or_else(|| cfg.tokens.get("default")).and_then(|t| t.as_str()).ok_or("unknown service and no default token")?;
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(token.as_bytes()).map_err(|e| e.to_string())?;
    mac.update(payload_s.as_bytes());
    let want = base64::engine::general_purpose::STANDARD.decode(sig).map_err(|_| "signature not base64")?;
    mac.verify_slice(&want).map_err(|_| "signature mismatch")?;
    if payload.get("challenge").and_then(|c| c.as_str()) != Some(ch) {
        return Err("challenge mismatch".into());
    }
    Ok(Vote {
        service,
        username: payload.get("username").and_then(|v| v.as_str()).unwrap_or("").trim().to_owned(),
        address: payload.get("address").and_then(|v| v.as_str()).unwrap_or("").to_owned(),
    })
}

fn start(ctx: &Ctx) {
    if shared(|s| std::mem::replace(&mut s.started, true)) {
        return;
    }
    if !ctx.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true) {
        note("disabled in config".into());
        return;
    }
    let Ok(dir) = ctx.host.config_dir().map(|d| d.join("hplug")) else { return };
    let cfg = Config {
        host: ctx.get("host").and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_else(|| "0.0.0.0".into()),
        port: ctx.get("port").and_then(|v| v.as_u64()).unwrap_or(8192) as u16,
        tokens: ctx.get("tokens").and_then(|v| v.as_object().cloned()).unwrap_or_default(),
        key: load_key(&dir),
    };
    if cfg.key.is_none() && cfg.tokens.is_empty() {
        note("no rsa/private.key and no tokens: nothing could authenticate a vote, listener not started".into());
        return;
    }
    std::thread::Builder::new()
        .name("votifier".into())
        .spawn(move || {
            let listener = match TcpListener::bind((cfg.host.as_str(), cfg.port)) {
                Ok(l) => l,
                Err(e) => return note(format!("cannot listen on {}:{}: {e}", cfg.host, cfg.port)),
            };
            note(format!("listening on {}:{} (v1 {}, v2 tokens {})", cfg.host, cfg.port, if cfg.key.is_some() { "on" } else { "off" }, cfg.tokens.len()));
            for conn in listener.incoming().flatten() {
                let peer = conn.peer_addr().map(|a| a.to_string()).unwrap_or_default();
                match handle(conn, &cfg) {
                    Ok(v) if !v.username.is_empty() && v.username.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.') && v.username.len() <= 17 => {
                        note(format!("vote from {} for {} ({peer})", v.service, v.username));
                        shared(|s| {
                            s.received += 1;
                            s.queue.push(v);
                        });
                    }
                    Ok(v) => note(format!("vote with a bad username {:?} from {peer} ignored", v.username)),
                    Err(e) => note(format!("rejected connection from {peer}: {e}")),
                }
            }
        })
        .ok();
}

fn run_vote(ctx: &mut Ctx, v: &Vote) {
    let template = ctx.get("vote_command").and_then(|c| c.as_str().map(str::to_owned)).unwrap_or_else(|| "reward vote {player} {service}".into());
    let service: String = v.service.chars().filter(|c| !c.is_whitespace()).collect();
    let line = template.replace("{player}", &v.username).replace("{service}", if service.is_empty() { "unknown" } else { &service }).replace("{address}", &v.address);
    ctx.console(line);
}

pub fn plugin() -> Plugin {
    Plugin::new("votifier")
        .command("votifier [sub:word] [player:word] [service:word]", 2, |ctx, args| {
            match args.get(0).map(|s| s.as_str()) {
                Some("test") => {
                    let p = args.get(1).ok_or("Usage: /votifier test <player> [service]")?;
                    let v = Vote { service: args.get(2).cloned().unwrap_or_else(|| "test".into()), username: p.clone(), address: "127.0.0.1".into() };
                    run_vote(ctx, &v);
                    ctx.reply(format!("§aTest vote for {p} queued."));
                }
                _ => {
                    let (started, received, log) = shared(|s| (s.started, s.received, s.log.clone()));
                    ctx.reply(format!("§7Votifier: {} §7votes received since start: §f{received}", if started { "§aSTARTED" } else { "§cNOT STARTED" }));
                    for l in log.iter().rev().take(5) {
                        ctx.reply(format!("§8{l}"));
                    }
                }
            }
            Ok(())
        })
        .every(20, |ctx| {
            start(ctx);
            let votes = shared(|s| std::mem::take(&mut s.queue));
            for v in votes {
                run_vote(ctx, &v);
            }
        })
}
