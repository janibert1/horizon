//! Port of HorizonsAuth (Paper, 897 lines): password login for cracked (offline-uuid) Java players.
//! Premium (version-4 UUID) and Floodgate/Bedrock players are never asked. A player that must authenticate is
//! frozen (slowness 255 + jump boost 128 effects, cleared after login), cannot move on, interact, break/place,
//! open containers, attack, chat, take damage or run commands except /login /register /changepassword, and is
//! kicked after 60 s or 5 wrong passwords. A 7-day IP session skips the login for a registered player that comes
//! back from the same IP (`NativeServerPlayer::address`, patch `player-address`; behind Velocity the forwarded IP).
//! Differences from the Paper plugin (on purpose / by gap):
//!  - passwords are stored salted + hashed (`$h1$salt$sha256^20000`), NOT plaintext. A legacy plaintext `auth.json`
//!    entry still works and is upgraded to a hash on the first successful login.
//!  - NOT ported: moving a pending player to the world spawn and back (no world-spawn API; set `auth_spawn` =
//!    [x,y,z] in the config for a fixed hub if wanted), drop/pickup blocking (no hooks), the Bukkit
//!    `PlayerAuthenticatedEvent` (other plugins call `horizonsauth::is_authenticated(uuid)`).
//!
//! Files: `config/hplug/horizonsauth/{auth.json,ip_sessions.json}` (or the dir in config key `shared_data_path`).
use hplug::{is_floodgate, is_premium, json, now_ms, Action, ChatVerdict, Ctx, Plugin};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Mutex;

const MIN_PASSWORD: usize = 6;
const MAX_ATTEMPTS: u32 = 5;
const IP_SESSION_MS: u64 = 7 * 24 * 3600 * 1000;

struct Pending {
    since: u64,
    attempts: u32,
    name: String,
    registered: bool,
}
static PENDING: Mutex<Option<HashMap<u128, Pending>>> = Mutex::new(None);
static AUTHED: Mutex<Option<HashSet<u128>>> = Mutex::new(None);

/// For other plugins in the same server: true unless the player still has to log in.
pub fn is_authenticated(uuid: u128) -> bool {
    !needs_auth(uuid)
}
fn needs_auth(uuid: u128) -> bool {
    PENDING.lock().unwrap().as_ref().is_some_and(|m| m.contains_key(&uuid))
}
fn prompt(uuid: u128) -> String {
    let registered = PENDING.lock().unwrap().as_ref().and_then(|m| m.get(&uuid)).is_some_and(|p| p.registered);
    if registered { "/login <password>".into() } else { "/register <password> <password>".into() }
}

// ---- password hashing ----
fn hash(password: &str, salt: &[u8]) -> String {
    let mut h = Sha256::new_with_prefix(salt);
    h.update(password.as_bytes());
    let mut d: [u8; 32] = h.finalize().into();
    for _ in 0..20_000 {
        let mut n = Sha256::new_with_prefix(salt);
        n.update(d);
        n.update(password.as_bytes());
        d = n.finalize().into();
    }
    format!("$h1${}${}", hex(salt), hex(&d))
}
fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
fn unhex(s: &str) -> Vec<u8> {
    (0..s.len() / 2).filter_map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok()).collect()
}
fn random_salt() -> [u8; 16] {
    use std::hash::{BuildHasher, Hasher};
    let mut out = [0u8; 16];
    for chunk in out.chunks_mut(8) {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(now_ms());
        chunk.copy_from_slice(&h.finish().to_le_bytes());
    }
    out
}
fn make_hash(password: &str) -> String {
    hash(password, &random_salt())
}
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}
/// Some(true) = correct. Legacy plaintext entries compare directly.
fn verify(stored: &str, password: &str) -> bool {
    if let Some(rest) = stored.strip_prefix("$h1$") {
        let Some((salt, _)) = rest.split_once('$') else { return false };
        same(&hash(password, &unhex(salt)), stored)
    } else {
        same(stored, password)
    }
}

// ---- storage ----
fn dir(ctx: &Ctx) -> Result<PathBuf, String> {
    let d = match ctx.get("shared_data_path").and_then(|v| v.as_str().map(PathBuf::from)).filter(|p| !p.as_os_str().is_empty()) {
        Some(p) => p,
        None => ctx.host.config_dir()?.join("hplug/horizonsauth"),
    };
    std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
    Ok(d)
}
fn read_map(ctx: &Ctx, file: &str) -> BTreeMap<String, json::Value> {
    let Ok(d) = dir(ctx) else { return BTreeMap::new() };
    std::fs::read_to_string(d.join(file)).ok().and_then(|t| json::from_str(&t).ok()).unwrap_or_default()
}
fn write_map(ctx: &Ctx, file: &str, m: &BTreeMap<String, json::Value>) {
    let Ok(d) = dir(ctx) else { return };
    let tmp = d.join(format!("{file}.tmp"));
    if std::fs::write(&tmp, json::to_string_pretty(m).unwrap_or_default()).is_ok() {
        let _ = std::fs::rename(&tmp, d.join(file));
    }
}
fn stored(ctx: &Ctx, name: &str) -> Option<String> {
    read_map(ctx, "auth.json").get(&name.to_lowercase()).and_then(|v| v.as_str().map(str::to_owned))
}
fn set_stored(ctx: &Ctx, name: &str, value: Option<String>) -> bool {
    let mut m = read_map(ctx, "auth.json");
    let existed = match value {
        Some(v) => m.insert(name.to_lowercase(), json::json!(v)).is_some(),
        None => m.remove(&name.to_lowercase()).is_some(),
    };
    write_map(ctx, "auth.json", &m);
    existed
}
fn record_ip(ctx: &Ctx, uuid: u128, ip: &str) {
    if ip.is_empty() { return; }
    let mut m = read_map(ctx, "ip_sessions.json");
    m.insert(format!("{uuid:032x}"), json::json!({"ip": ip, "ts": now_ms()}));
    write_map(ctx, "ip_sessions.json", &m);
}
fn clear_ip(ctx: &Ctx, uuid: u128) {
    let mut m = read_map(ctx, "ip_sessions.json");
    if m.remove(&format!("{uuid:032x}")).is_some() {
        write_map(ctx, "ip_sessions.json", &m);
    }
}
fn valid_ip_session(ctx: &Ctx, uuid: u128, ip: &str) -> bool {
    let m = read_map(ctx, "ip_sessions.json");
    let Some(s) = m.get(&format!("{uuid:032x}")) else { return false };
    s["ts"].as_u64().is_some_and(|t| now_ms().saturating_sub(t) <= IP_SESSION_MS) && s["ip"].as_str() == Some(ip)
}

fn freeze(ctx: &mut Ctx, name: &str) {
    ctx.console(format!("effect give {name} minecraft:slowness 120 255 true"));
    ctx.console(format!("effect give {name} minecraft:jump_boost 120 128 true"));
}
fn unfreeze(ctx: &mut Ctx, name: &str) {
    ctx.console(format!("effect clear {name} minecraft:slowness"));
    ctx.console(format!("effect clear {name} minecraft:jump_boost"));
    ctx.console(format!("title {name} clear"));
}
fn title(ctx: &mut Ctx, name: &str, registered: bool) {
    let (t, s) = if registered { ("Authentication Required", "Type  /login <password>") } else { ("Welcome to Horizons SMP!", "Type  /register <password> <password>") };
    ctx.title(name, t, s, 100);
}
fn finish(ctx: &mut Ctx, uuid: u128, name: &str, welcome: &str) {
    PENDING.lock().unwrap().get_or_insert_with(Default::default).remove(&uuid);
    AUTHED.lock().unwrap().get_or_insert_with(Default::default).insert(uuid);
    if let Some(ip) = ctx.try_player().map(|p| p.address.clone()) {
        record_ip(ctx, uuid, &ip);
    }
    unfreeze(ctx, name);
    ctx.reply(welcome);
}
fn bad_password(ctx: &mut Ctx, uuid: u128, name: &str) {
    let attempts = {
        let mut g = PENDING.lock().unwrap();
        let p = g.get_or_insert_with(Default::default).get_mut(&uuid);
        p.map(|p| { p.attempts += 1; p.attempts }).unwrap_or(1)
    };
    if attempts >= MAX_ATTEMPTS {
        PENDING.lock().unwrap().get_or_insert_with(Default::default).remove(&uuid);
        ctx.kick(name, "Too many failed login attempts. Reconnect to try again.");
    } else {
        ctx.reply(format!("§c✗ Incorrect password. {} attempt(s) left.", MAX_ATTEMPTS - attempts));
    }
}

pub fn plugin() -> Plugin {
    Plugin::new("horizonsauth")
        .on_join(|ctx| {
            let p = ctx.player().clone();
            if ctx.get("enabled").and_then(|v| v.as_bool()) == Some(false) {
                return; // switched off (config `enabled: false`; `hp test` does this for the other plugins' tests)
            }
            if is_floodgate(p.uuid) || is_premium(p.uuid) || ctx.has_perm(p.uuid, &p.name, "horizonsauth.bypass") {
                AUTHED.lock().unwrap().get_or_insert_with(Default::default).insert(p.uuid);
                return;
            }
            let registered = stored(ctx, &p.name).is_some();
            if registered && valid_ip_session(ctx, p.uuid, &p.address) {
                AUTHED.lock().unwrap().get_or_insert_with(Default::default).insert(p.uuid);
                ctx.tell(p.uuid, "§aWelcome back! (same IP as your last login, no password needed)");
                return;
            }
            PENDING.lock().unwrap().get_or_insert_with(Default::default).insert(p.uuid, Pending { since: now_ms(), attempts: 0, name: p.name.clone(), registered });
            freeze(ctx, &p.name);
            title(ctx, &p.name, registered);
        })
        .on_quit(|ctx| {
            if let Some(u) = ctx.player_uuid() {
                PENDING.lock().unwrap().get_or_insert_with(Default::default).remove(&u);
                AUTHED.lock().unwrap().get_or_insert_with(Default::default).remove(&u);
            }
        })
        // once a second: countdown in the action bar, title every 15 s, timeout kick
        .every(20, |ctx| {
            let now = now_ms();
            let list: Vec<(u128, String, u64, bool)> = PENDING.lock().unwrap().as_ref().map(|m| m.iter().map(|(u, p)| (*u, p.name.clone(), p.since, p.registered)).collect()).unwrap_or_default();
            let online: HashSet<u128> = ctx.online().into_iter().map(|p| p.uuid).collect();
            for (u, name, since, registered) in list {
                if !online.contains(&u) {
                    PENDING.lock().unwrap().get_or_insert_with(Default::default).remove(&u);
                    continue;
                }
                let remaining = (ctx.get("timeout_secs").and_then(|v| v.as_u64()).unwrap_or(60) as i64 * 1000 - (now - since) as i64) / 1000;
                if remaining <= 0 {
                    PENDING.lock().unwrap().get_or_insert_with(Default::default).remove(&u);
                    ctx.kick(&name, "Login timeout. Reconnect and /login <password>.");
                    continue;
                }
                let cmd = if registered { "/login <password>" } else { "/register <pass> <pass>" };
                ctx.action_bar(u, format!("§e⏱ {remaining}s  |  {cmd}"));
                if remaining % 15 == 0 {
                    title(ctx, &name, registered);
                    freeze(ctx, &name); // the effects last 120 s; refresh with the title
                }
            }
        })
        .on_chat(|c| {
            if needs_auth(c.player) { ChatVerdict::Cancel(Some(format!("§c✗ Authenticate first: {}", prompt(c.player)))) } else { ChatVerdict::Allow }
        })
        .on_command_line(|ctx, _name, line| {
            let u = ctx.player_uuid()?;
            if !needs_auth(u) { return None; }
            let first = line.split_whitespace().next().unwrap_or("").to_lowercase();
            if matches!(first.as_str(), "login" | "register" | "changepassword") { None } else { Some(format!("§c✗ Authenticate first: {}", prompt(u))) }
        })
        .on_action(|ctx, _a: &Action| ctx.player_uuid().is_some_and(needs_auth))
        .on_block_break(|ctx, _| ctx.player_uuid().is_some_and(needs_auth))
        .on_interact(|ctx, _, _| ctx.player_uuid().is_some_and(needs_auth))
        .on_attack(|ctx, _, _| ctx.player_uuid().is_some_and(needs_auth))
        .on_damage(|d| d.victim_player.is_some_and(needs_auth))
        .command("login <password:word>", 0, |ctx, args| {
            let (u, name) = (ctx.player().uuid, ctx.player().name.clone());
            if !needs_auth(u) { ctx.reply("§7Already logged in."); return Ok(()); }
            let Some(st) = stored(ctx, &name) else { return Err("Account not registered. Use: /register <password> <confirm>".into()) };
            let pw = args.get(0).cloned().unwrap_or_default();
            if !verify(&st, &pw) { bad_password(ctx, u, &name); return Ok(()); }
            if !st.starts_with("$h1$") { set_stored(ctx, &name, Some(make_hash(&pw))); } // upgrade a legacy plaintext entry
            finish(ctx, u, &name, &format!("§aLogged in. Welcome back, {name}!"));
            Ok(())
        })
        .command("register <password:word> <confirm:word>", 0, |ctx, args| {
            let (u, name) = (ctx.player().uuid, ctx.player().name.clone());
            if !needs_auth(u) { ctx.reply("§7Already authenticated. Use /changepassword to change your password."); return Ok(()); }
            if stored(ctx, &name).is_some() { return Err("Account already registered. Use /login <password>".into()); }
            let (a, b) = (args.get(0).cloned().unwrap_or_default(), args.get(1).cloned().unwrap_or_default());
            if a != b { return Err("✗ Passwords don't match.".into()); }
            if a.len() < MIN_PASSWORD { return Err("✗ Password must be at least 6 characters.".into()); }
            set_stored(ctx, &name, Some(make_hash(&a)));
            finish(ctx, u, &name, &format!("§aRegistered and logged in. Welcome, {name}!"));
            Ok(())
        })
        .command("changepassword <old:word> <new:word> <confirm:word>", 0, |ctx, args| {
            let (u, name) = (ctx.player().uuid, ctx.player().name.clone());
            if needs_auth(u) { return Err("You must be logged in to change your password.".into()); }
            let (old, new, conf) = (args.get(0).cloned().unwrap_or_default(), args.get(1).cloned().unwrap_or_default(), args.get(2).cloned().unwrap_or_default());
            if new != conf { return Err("✗ New passwords don't match.".into()); }
            if new.len() < MIN_PASSWORD { return Err("✗ Password must be at least 6 characters.".into()); }
            match stored(ctx, &name) {
                Some(st) if verify(&st, &old) => {
                    set_stored(ctx, &name, Some(make_hash(&new)));
                    let ip = ctx.player().address.clone();
                    record_ip(ctx, u, &ip);
                    ctx.reply("§aPassword changed.");
                    Ok(())
                }
                _ => Err("✗ Incorrect old password.".into()),
            }
        })
        .command("horizonsauth <sub:word> [player:word] [value:word]", 2, |ctx, args| {
            let sub = args.get(0).map(|s| s.to_lowercase()).unwrap_or_default();
            let target = args.get(1).cloned();
            let need = |t: &Option<String>| t.clone().ok_or(format!("Usage: /horizonsauth {sub} <player>"));
            match sub.as_str() {
                "reset" => {
                    let t = need(&target)?;
                    if set_stored(ctx, &t, None) {
                        if let Some((u, _)) = ctx.uuid_for_name(&t) { clear_ip(ctx, u); AUTHED.lock().unwrap().get_or_insert_with(Default::default).remove(&u); }
                        ctx.reply(format!("§aPassword and session cleared for {t}."));
                    } else {
                        ctx.reply(format!("§cNo registered password for {t}."));
                    }
                }
                "info" => {
                    let t = need(&target)?;
                    let uuid = ctx.find_online(&t).map(|p| p.uuid).or_else(|| ctx.uuid_for_name(&t).map(|x| x.0));
                    let online = ctx.find_online(&t).is_some();
                    let registered = stored(ctx, &t).is_some();
                    let authenticated = uuid.is_some_and(|u| online && !needs_auth(u));
                    let premium = uuid.is_some_and(is_premium);
                    let session = uuid.zip(ctx.find_online(&t)).is_some_and(|(u, p)| valid_ip_session(ctx, u, &p.address));
                    ctx.reply(format!("§e{t} - online={online}  registered={registered}  authenticated={authenticated}  premium={premium}  ip-session={session}"));
                }
                "deauth" => {
                    let t = need(&target)?;
                    if let Some((u, _)) = ctx.uuid_for_name(&t) { clear_ip(ctx, u); AUTHED.lock().unwrap().get_or_insert_with(Default::default).remove(&u); }
                    ctx.reply(format!("§aSession cleared for {t}. They must log in again."));
                }
                "setpassword" => {
                    let t = need(&target)?;
                    let v = args.get(2).cloned().ok_or("Usage: /horizonsauth setpassword <player> <password>")?;
                    set_stored(ctx, &t, Some(make_hash(&v)));
                    if let Some((u, _)) = ctx.uuid_for_name(&t) { clear_ip(ctx, u); }
                    ctx.reply(format!("§aPassword set for {t}."));
                }
                "reload" => ctx.reply("§a✓ Auth data is read from disk on every use; nothing to reload."),
                _ => return Err("Unknown subcommand. Options: reset, info, deauth, setpassword, reload".into()),
            }
            Ok(())
        })
}
