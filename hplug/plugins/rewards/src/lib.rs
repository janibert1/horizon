//! Port of nl.jdries.rewards (HorizonsRewards):
//! Shards economy store, playtime rewards, reward packs, votes,
//! admin commands and cosmetics web shop RCON API.

use hplug::{json, Ctx, Plugin};
use std::collections::BTreeMap;
use std::sync::Mutex;

static LAST_ACTIVE: Mutex<BTreeMap<u128, u64>> = Mutex::new(BTreeMap::new());

pub fn mark_active(uuid: u128) {
    if let Ok(mut map) = LAST_ACTIVE.lock() {
        map.insert(uuid, hplug::now_ms());
    }
}

pub fn is_afk(uuid: u128, timeout_seconds: u64) -> bool {
    let now = hplug::now_ms();
    let Ok(map) = LAST_ACTIVE.lock() else { return false };
    let Some(&last) = map.get(&uuid) else { return false };
    (now.saturating_sub(last)) >= timeout_seconds * 1000
}

pub fn format_uuid(u: u128) -> String {
    format!(
        "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
        (u >> 96) as u32,
        (u >> 80) as u16,
        (u >> 64) as u16,
        (u >> 48) as u16,
        u & 0xFFFF_FFFF_FFFF
    )
}

pub fn parse_uuid(s: &str) -> Option<u128> {
    let clean = s.replace('-', "");
    if clean.len() != 32 {
        return None;
    }
    u128::from_str_radix(&clean, 16).ok()
}

/// Resolves player name or UUID string to (u128 uuid, String display_name).
pub fn resolve_player(ctx: &Ctx, name_or_uuid: &str) -> Option<(u128, String)> {
    if let Some(u) = parse_uuid(name_or_uuid) {
        // Try to find display name
        if let Some(p) = ctx.online().into_iter().find(|p| p.uuid == u) {
            return Some((u, p.name));
        }
        let names = uuid_names(ctx);
        let dashed = format_uuid(u);
        if let Some(n) = names.get(&dashed) {
            return Some((u, n.clone()));
        }
        return Some((u, dashed[..8].to_string()));
    }

    // Try online player by name
    if let Some(p) = ctx.find_online(name_or_uuid) {
        return Some((p.uuid, p.name));
    }

    // Try usercache/whitelist via ctx.uuid_for_name
    if let Some((u, n)) = ctx.uuid_for_name(name_or_uuid) {
        return Some((u, n));
    }

    // Try reading usercache/whitelist directly
    let names = uuid_names(ctx);
    for (u_str, name) in names {
        if name.eq_ignore_ascii_case(name_or_uuid)
            && let Some(u) = parse_uuid(&u_str) {
                return Some((u, name));
            }
    }

    None
}

/// Reads usercache.json then whitelist.json for dashed_uuid -> name mapping.
pub fn uuid_names(ctx: &Ctx) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Some(dir) = ctx.server_dir() else { return out };
    for f in ["usercache.json", "whitelist.json"] {
        let Ok(text) = std::fs::read_to_string(dir.join(f)) else { continue };
        let Ok(v) = json::from_str::<json::Value>(&text) else { continue };
        for e in v.as_array().into_iter().flatten() {
            if let (Some(u), Some(n)) = (
                e.get("uuid").and_then(|x| x.as_str()),
                e.get("name").and_then(|x| x.as_str()),
            ) {
                out.entry(u.to_owned()).or_insert_with(|| n.to_owned());
            }
        }
    }
    out
}

// --- Balances persistence ---

pub fn get_balance(ctx: &Ctx, uuid: u128) -> u64 {
    let key = format_uuid(uuid);
    ctx.get("balances")
        .and_then(|v| v.get(&key).and_then(|b| b.as_u64()))
        .unwrap_or(0)
}

pub fn set_balance(ctx: &mut Ctx, uuid: u128, amount: u64) -> Result<(), String> {
    let key = format_uuid(uuid);
    let mut map = ctx
        .get("balances")
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    map.insert(key, json::json!(amount));
    ctx.set("balances", json::Value::Object(map))
}

pub fn add_balance(ctx: &mut Ctx, uuid: u128, delta: i64) -> Result<u64, String> {
    let current = get_balance(ctx, uuid);
    let updated = if delta < 0 {
        current.saturating_sub((-delta) as u64)
    } else {
        current.saturating_add(delta as u64)
    };
    set_balance(ctx, uuid, updated)?;
    Ok(updated)
}

// --- History persistence ---

#[derive(Clone, Debug)]
pub struct HistoryEntry {
    pub r#type: String,
    pub amount: i64,
    pub detail: Option<String>,
    pub reason: Option<String>,
    pub actor: Option<String>,
    pub timestamp: u64,
}

pub fn log_history(
    ctx: &mut Ctx,
    uuid: u128,
    entry_type: &str,
    amount: i64,
    detail: Option<&str>,
    reason: Option<&str>,
    actor: Option<&str>,
) {
    let key = format_uuid(uuid);
    let mut map = ctx
        .get("history")
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    let mut entries: Vec<json::Value> = map
        .get(&key)
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default();

    entries.push(json::json!({
        "type": entry_type,
        "amount": amount,
        "detail": detail,
        "reason": reason,
        "actor": actor,
        "timestamp": hplug::now_ms(),
    }));

    if entries.len() > 50 {
        let drain_count = entries.len() - 50;
        entries.drain(0..drain_count);
    }

    map.insert(key, json::Value::Array(entries));
    let _ = ctx.set("history", json::Value::Object(map));
}

pub fn get_history(ctx: &Ctx, uuid: u128, limit: usize) -> Vec<HistoryEntry> {
    let key = format_uuid(uuid);
    let raw = ctx
        .get("history")
        .and_then(|v| v.get(&key).and_then(|a| a.as_array().cloned()))
        .unwrap_or_default();

    raw.into_iter()
        .rev()
        .take(limit)
        .filter_map(|v| {
            Some(HistoryEntry {
                r#type: v.get("type")?.as_str()?.to_string(),
                amount: v.get("amount")?.as_i64()?,
                detail: v.get("detail").and_then(|d| d.as_str()).map(str::to_string),
                reason: v.get("reason").and_then(|r| r.as_str()).map(str::to_string),
                actor: v.get("actor").and_then(|a| a.as_str()).map(str::to_string),
                timestamp: v.get("timestamp").and_then(|t| t.as_u64()).unwrap_or(0),
            })
        })
        .collect()
}

pub fn format_ts(ts_ms: u64) -> String {
    let s = ts_ms / 1000;
    let mut days = (s / 86400) as i64;
    let day_secs = (s % 86400) as u32;
    let hour = day_secs / 3600;
    let min = (day_secs % 3600) / 60;

    let mut year = 1970;
    loop {
        let leap = (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0);
        let days_in_year = if leap { 366 } else { 365 };
        if days < days_in_year {
            break;
        }
        days -= days_in_year;
        year += 1;
    }

    let leap = (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0);
    let days_in_months = [
        31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31,
    ];
    let month_names = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];

    let mut month = 0;
    for (m, &dim) in days_in_months.iter().enumerate() {
        if days < dim {
            month = m;
            break;
        }
        days -= dim;
    }
    let day = days + 1;
    format!("{} {} {:02}:{:02}", month_names[month], day, hour, min)
}

// --- Claims persistence ---

pub fn has_claim(ctx: &Ctx, uuid: u128, claim_id: &str) -> bool {
    let key = format_uuid(uuid);
    ctx.get("claims")
        .and_then(|v| v.get(&key).cloned())
        .and_then(|v| v.as_array().cloned())
        .map(|arr| arr.iter().any(|c| c.as_str() == Some(claim_id)))
        .unwrap_or(false)
}

pub fn record_claim(ctx: &mut Ctx, uuid: u128, claim_id: &str) {
    let key = format_uuid(uuid);
    let mut map = ctx
        .get("claims")
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    let mut claims: Vec<String> = map
        .get(&key)
        .and_then(|v| v.as_array().cloned())
        .map(|arr| {
            arr.into_iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    if !claims.iter().any(|c| c == claim_id) {
        claims.push(claim_id.to_string());
        map.insert(key, json::json!(claims));
        let _ = ctx.set("claims", json::Value::Object(map));
    }
}

pub fn list_web_claims(ctx: &Ctx, uuid: u128) -> Vec<String> {
    let key = format_uuid(uuid);
    let claims: Vec<String> = ctx
        .get("claims")
        .and_then(|v| v.get(&key).cloned())
        .and_then(|v| v.as_array().cloned())
        .map(|arr| {
            arr.into_iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    let mut out = Vec::new();
    for c in claims {
        if let Some(id) = c.strip_prefix("web_cosmetic:") {
            out.push(id.to_string());
        } else if let Some(tier) = c.strip_prefix("web_rank:") {
            out.push(format!("rank{tier}"));
        }
    }
    out
}

// --- Web links persistence ---

pub fn set_weblink(ctx: &mut Ctx, uuid: u128, code: &str) {
    let expires_at = hplug::now_ms() + 10 * 60 * 1000;
    let mut map = ctx
        .get("weblinks")
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    map.insert(
        code.to_string(),
        json::json!({
            "uuid": format_uuid(uuid),
            "expires_at": expires_at,
        }),
    );
    let _ = ctx.set("weblinks", json::Value::Object(map));
}

pub fn create_weblink(ctx: &mut Ctx, uuid: u128) -> String {
    const CHARS: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let mut seed = hplug::now_ms();
    let mut code = String::with_capacity(8);
    for _ in 0..8 {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let idx = ((seed >> 32) as usize) % CHARS.len();
        code.push(CHARS[idx] as char);
    }
    set_weblink(ctx, uuid, &code);
    code
}

pub fn consume_weblink(ctx: &mut Ctx, code: &str) -> Option<u128> {
    let mut map = ctx
        .get("weblinks")
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    let entry = map.remove(code);
    let _ = ctx.set("weblinks", json::Value::Object(map));

    let obj = entry?;
    let expires = obj.get("expires_at")?.as_u64()?;
    if expires < hplug::now_ms() {
        return None;
    }
    let u_str = obj.get("uuid")?.as_str()?;
    parse_uuid(u_str)
}

pub fn purge_weblinks(ctx: &mut Ctx) {
    let now = hplug::now_ms();
    let mut map = ctx
        .get("weblinks")
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    let before = map.len();
    map.retain(|_, v| {
        v.get("expires_at")
            .and_then(|e| e.as_u64())
            .map(|exp| exp >= now)
            .unwrap_or(false)
    });
    if map.len() != before {
        let _ = ctx.set("weblinks", json::Value::Object(map));
    }
}

// --- Playtime tracking ---

pub fn fmt_playtime(ticks: u64) -> String {
    let s = ticks / 20;
    let (d, h, m) = (s / 86400, (s % 86400) / 3600, (s % 3600) / 60);
    if d > 0 {
        format!("{d}d {h}h {m}m")
    } else {
        format!("{h}h {m}m")
    }
}

pub fn ticks_for(ctx: &Ctx, uuid_str: &str) -> u64 {
    let Some(dir) = ctx.host.world_dir() else { return 0 };
    let path = dir.join("players/stats").join(format!("{uuid_str}.json"));
    let Ok(text) = std::fs::read_to_string(path) else { return 0 };
    let Ok(v) = json::from_str::<json::Value>(&text) else { return 0 };
    v["stats"]["minecraft:custom"]["minecraft:play_time"]
        .as_u64()
        .or_else(|| v["stats"]["minecraft:custom"]["minecraft:play_one_minute"].as_u64())
        .unwrap_or(0)
}

pub fn playtime_leaderboard(ctx: &Ctx) -> Vec<(String, u64)> {
    let mut rows: Vec<(String, u64)> = uuid_names(ctx)
        .into_iter()
        .map(|(u, n)| {
            let t = ticks_for(ctx, &u);
            (n, t)
        })
        .filter(|(_, t)| *t > 0)
        .collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r.1));
    rows
}

pub fn accrue_playtime(ctx: &mut Ctx, uuid: u128, elapsed: u64, interval: u64) -> u32 {
    let key = format_uuid(uuid);
    let mut map = ctx
        .get("playtime")
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    let (mut so_far, mut total) = map
        .get(&key)
        .and_then(|v| {
            Some((
                v.get("so_far")?.as_u64()?,
                v.get("total")?.as_u64()?,
            ))
        })
        .unwrap_or((0, 0));

    so_far += elapsed;
    total += elapsed;
    let awards = (so_far / interval) as u32;
    so_far %= interval;

    map.insert(
        key,
        json::json!({
            "so_far": so_far,
            "total": total,
        }),
    );
    let _ = ctx.set("playtime", json::Value::Object(map));
    awards
}

// --- Packs definitions ---

#[derive(Clone, Debug)]
pub struct Pack {
    pub id: &'static str,
    pub display_name: &'static str,
    pub description: &'static str,
    pub access: &'static str,
    pub claim: &'static str,
    pub price: u64,
    pub shards: u64,
    pub items: &'static [(&'static str, u32)],
}

pub const DEFAULT_PACKS: &[Pack] = &[
    Pack {
        id: "welcome",
        display_name: "Welcome Pack",
        description: "Auto-granted the first time a player joins.",
        access: "ADMIN_ONLY",
        claim: "ONE_TIME",
        price: 0,
        shards: 300,
        items: &[],
    },
    Pack {
        id: "helper",
        display_name: "Helper's Thanks",
        description: "For players who've done something genuinely useful for the server.",
        access: "ADMIN_ONLY",
        claim: "REPEATABLE",
        price: 0,
        shards: 1000,
        items: &[("minecraft:diamond", 8)],
    },
    Pack {
        id: "supply-crate",
        display_name: "Supply Crate",
        description: "A basic purchasable bundle of useful materials.",
        access: "PURCHASABLE",
        claim: "REPEATABLE",
        price: 800,
        shards: 0,
        items: &[
            ("minecraft:iron_ingot", 16),
            ("minecraft:cooked_beef", 16),
        ],
    },
];

pub fn find_pack(id: &str) -> Option<&'static Pack> {
    DEFAULT_PACKS.iter().find(|p| p.id.eq_ignore_ascii_case(id))
}

pub fn normalize_material(m: &str) -> String {
    let lower = m.to_lowercase();
    if lower.contains(':') {
        lower
    } else {
        format!("minecraft:{lower}")
    }
}

/// `/playtime [player]` (also `/rewards playtime [player]`: the top-level name is shared with the playtime plugin,
/// whichever plugin registers it first owns it, as on Paper).
fn playtime_cmd(ctx: &mut hplug::Ctx, target: Option<String>) -> Result<(), String> {
    if ctx.is_player() {
        mark_active(ctx.player().uuid);
    }
    let target = match target {
        Some(t) => t,
        None if ctx.is_player() => ctx.sender_name(),
        None => return Err("Specify a player name.".into()),
    };
    let (target_uuid, target_name) = resolve_player(ctx, &target).ok_or(format!("Player not found: {target}"))?;
    let dashed = format_uuid(target_uuid);
    let ticks = ticks_for(ctx, &dashed);
    ctx.reply(format!("§6{target_name}§7's playtime: §e{}", fmt_playtime(ticks)));
    Ok(())
}

/// `/playtop [count]` (also `/rewards top [count]`), default 10, at most 20.
fn playtop_cmd(ctx: &mut hplug::Ctx, count: i64) -> Result<(), String> {
    if ctx.is_player() {
        mark_active(ctx.player().uuid);
    }
    let count = count.clamp(1, 20) as usize;
    let board = playtime_leaderboard(ctx);
    if board.is_empty() {
        ctx.reply("§7No playtime data yet.");
        return Ok(());
    }
    let n = count.min(board.len());
    ctx.reply(format!("§6Top {n} playtime:"));
    for (i, (name, t)) in board.iter().take(n).enumerate() {
        ctx.reply(format!("§7#{} §e{} §7- §f{}", i + 1, name, fmt_playtime(*t)));
    }
    Ok(())
}

pub fn plugin() -> Plugin {
    Plugin::new("rewards")
        // Player-facing /rewards (alias /wallet)
        .command("rewards [action:word] [arg1:word] [arg2:integer]", 0, |ctx, args| {
            if ctx.is_player() {
                mark_active(ctx.player().uuid);
            }

            let action = args.get(0).map(|s| s.to_lowercase()).unwrap_or_default();

            if action.is_empty() || action == "balance" {
                if !ctx.is_player() {
                    ctx.reply("Players only. Use /reward give|take|set for console.");
                    return Ok(());
                }
                let uuid = ctx.player().uuid;
                let balance = get_balance(ctx, uuid);
                ctx.reply(format!("§6You have §e{balance} Shards§6."));
                ctx.reply("§7Use /rewards history, /rewards packs, /rewards buy <pack>");
                return Ok(());
            }

            match action.as_str() {
                "playtime" => playtime_cmd(ctx, args.get(1).cloned()),
                "top" => playtop_cmd(ctx, args.get(1).and_then(|s| s.parse().ok()).unwrap_or(10)),
                "weblink" => {
                    if !ctx.is_player() {
                        ctx.reply("Players only.");
                        return Ok(());
                    }
                    let code = create_weblink(ctx, ctx.player().uuid);
                    ctx.reply(format!("§6Your website link code: §e§l{code}"));
                    ctx.reply("§7Enter this at the shop website within 10 minutes. It can only be used once.");
                    Ok(())
                }
                "history" => {
                    if !ctx.is_player() {
                        ctx.reply("Players only.");
                        return Ok(());
                    }
                    let entries = get_history(ctx, ctx.player().uuid, 10);
                    if entries.is_empty() {
                        ctx.reply("§7No reward history yet.");
                        return Ok(());
                    }
                    ctx.reply(format!("§6Your last {} reward events:", entries.len()));
                    for e in entries {
                        let sign = if e.amount >= 0 { "+" } else { "" };
                        let detail = e.detail.as_ref().map(|d| format!(" ({d})")).unwrap_or_default();
                        ctx.reply(format!(
                            "§7[{}] §f{sign}{} Shards{detail} §8- {}",
                            format_ts(e.timestamp),
                            e.amount,
                            e.r#type
                        ));
                    }
                    Ok(())
                }
                "packs" => {
                    let purchasable: Vec<_> = DEFAULT_PACKS
                        .iter()
                        .filter(|p| p.access == "PURCHASABLE")
                        .collect();
                    if purchasable.is_empty() {
                        ctx.reply("§7No purchasable packs available right now.");
                        return Ok(());
                    }
                    ctx.reply("§6Purchasable packs:");
                    for p in purchasable {
                        ctx.reply(format!(
                            "§e{} §7- {} Shards §8(/rewards buy {})",
                            p.display_name, p.price, p.id
                        ));
                        if !p.description.is_empty() {
                            ctx.reply(format!("  §7{}", p.description));
                        }
                    }
                    Ok(())
                }
                "buy" => {
                    if !ctx.is_player() {
                        ctx.reply("Players only.");
                        return Ok(());
                    }
                    let pack_id = args.get(1).ok_or("Usage: /rewards buy <pack>")?;
                    let pack = find_pack(pack_id)
                        .filter(|p| p.access == "PURCHASABLE")
                        .ok_or(format!("No purchasable pack named '{pack_id}'."))?;

                    let uuid = ctx.player().uuid;
                    if pack.claim == "ONE_TIME" && has_claim(ctx, uuid, pack.id) {
                        ctx.reply("§cYou've already claimed this pack.");
                        return Ok(());
                    }
                    let bal = get_balance(ctx, uuid);
                    if bal < pack.price {
                        ctx.reply("§cYou don't have enough Shards.");
                        return Ok(());
                    }

                    add_balance(ctx, uuid, -(pack.price as i64))?;
                    record_claim(ctx, uuid, pack.id);
                    if pack.shards > 0 {
                        add_balance(ctx, uuid, pack.shards as i64)?;
                    }
                    for &(item, amt) in pack.items {
                        ctx.give(item, amt);
                    }
                    log_history(
                        ctx,
                        uuid,
                        "PACK_PURCHASE",
                        -(pack.price as i64),
                        Some(pack.id),
                        Some("Player pack purchase"),
                        None,
                    );
                    ctx.reply(format!("§aPurchased {}!", pack.display_name));
                    Ok(())
                }
                "pay" => {
                    if !ctx.is_player() {
                        ctx.reply("Players only.");
                        return Ok(());
                    }
                    let sender_uuid = ctx.player().uuid;
                    let sender_name = ctx.sender_name();
                    let target_arg = args.get(1).ok_or("Usage: /rewards pay <player> <amount>")?;
                    let amount = args.int(2).map_err(|_| "Amount must be a whole positive number.")?;
                    if amount <= 0 {
                        return Err("Amount must be greater than 0.".into());
                    }

                    let (target_uuid, target_name) = resolve_player(ctx, target_arg)
                        .ok_or(format!("Unknown player '{target_arg}'."))?;

                    if target_uuid == sender_uuid {
                        return Err("You cannot pay yourself.".into());
                    }

                    let sender_bal = get_balance(ctx, sender_uuid);
                    if sender_bal < amount as u64 {
                        return Err("You don't have enough Shards.".into());
                    }

                    add_balance(ctx, sender_uuid, -amount)?;
                    add_balance(ctx, target_uuid, amount)?;

                    log_history(ctx, sender_uuid, "PAY_SEND", -amount, Some(&target_name), Some("Payment sent"), None);
                    log_history(ctx, target_uuid, "PAY_RECEIVE", amount, Some(&sender_name), Some("Payment received"), Some(&sender_name));

                    ctx.reply(format!("§aSent {amount} Shards to {target_name}."));
                    ctx.tell(target_uuid, format!("§6You received §e{amount} Shards §6from {sender_name}."));
                    Ok(())
                }
                _ => {
                    if !ctx.is_player() {
                        ctx.reply("Players only.");
                        return Ok(());
                    }
                    let balance = get_balance(ctx, ctx.player().uuid);
                    ctx.reply(format!("§6You have §e{balance} Shards§6."));
                    ctx.reply("§7Use /rewards history, /rewards packs, /rewards buy <pack>");
                    Ok(())
                }
            }
        })
        .alias("wallet")
        // Admin /reward command (op level 2)
        .command("reward [subcommand:word] [arg1:word] [arg2:word] [arg3:word] [arg4:text]", 2, |ctx, args| {
            if ctx.is_player() {
                mark_active(ctx.player().uuid);
            }

            let sub = args.get(0).map(|s| s.to_lowercase()).unwrap_or_default();
            if sub.is_empty() {
                return Err("Usage: /reward <give|take|set|balance|giveitem|pack|history|vote|weblink|web> ...".into());
            }

            match sub.as_str() {
                "give" => {
                    let target_arg = args.get(1).ok_or("Usage: /reward give <player> <amount> [reason...]")?;
                    let amount = args.get(2).and_then(|s| s.parse::<i64>().ok()).ok_or("Amount must be a whole number.")?;
                    let reason = if args.len() > 3 { Some(args.rest(3)) } else { None };

                    let (target_uuid, target_name) = resolve_player(ctx, target_arg)
                        .ok_or(format!("Unknown player '{target_arg}'."))?;

                    let actor = ctx.sender_name();
                    let new_bal = add_balance(ctx, target_uuid, amount)?;
                    log_history(ctx, target_uuid, "ADMIN_GIVE", amount, None, reason.as_deref(), Some(&actor));

                    ctx.reply(format!("§aGave {target_name} {amount} Shards (new balance: {new_bal})."));
                    let reason_str = reason.as_ref().map(|r| format!(" §6- {r}")).unwrap_or_default();
                    ctx.tell(target_uuid, format!("§6You received §e{amount} Shards{reason_str}"));
                    Ok(())
                }
                "take" => {
                    let target_arg = args.get(1).ok_or("Usage: /reward take <player> <amount> [reason...]")?;
                    let amount = args.get(2).and_then(|s| s.parse::<i64>().ok()).ok_or("Amount must be a whole number.")?;
                    let reason = if args.len() > 3 { Some(args.rest(3)) } else { None };

                    let (target_uuid, target_name) = resolve_player(ctx, target_arg)
                        .ok_or(format!("Unknown player '{target_arg}'."))?;

                    let actor = ctx.sender_name();
                    let new_bal = add_balance(ctx, target_uuid, -amount)?;
                    log_history(ctx, target_uuid, "ADMIN_TAKE", -amount, None, reason.as_deref(), Some(&actor));

                    ctx.reply(format!("§aTook {amount} Shards from {target_name} (new balance: {new_bal})."));
                    let reason_str = reason.as_ref().map(|r| format!(" §6- {r}")).unwrap_or_default();
                    ctx.tell(target_uuid, format!("§6You lost §e{amount} Shards{reason_str}"));
                    Ok(())
                }
                "set" => {
                    let target_arg = args.get(1).ok_or("Usage: /reward set <player> <amount> [reason...]")?;
                    let amount = args.get(2).and_then(|s| s.parse::<i64>().ok()).ok_or("Amount must be a whole number.")?;
                    if amount < 0 {
                        return Err("Balance cannot be negative.".into());
                    }
                    let reason = if args.len() > 3 { Some(args.rest(3)) } else { None };

                    let (target_uuid, target_name) = resolve_player(ctx, target_arg)
                        .ok_or(format!("Unknown player '{target_arg}'."))?;

                    let actor = ctx.sender_name();
                    set_balance(ctx, target_uuid, amount as u64)?;
                    log_history(ctx, target_uuid, "ADMIN_SET", amount, None, reason.as_deref(), Some(&actor));

                    ctx.reply(format!("§aSet {target_name}'s balance to {amount} Shards."));
                    Ok(())
                }
                "balance" => {
                    let target_arg = args.get(1).ok_or("Usage: /reward balance <player>")?;
                    let (target_uuid, target_name) = resolve_player(ctx, target_arg)
                        .ok_or(format!("Unknown player '{target_arg}'."))?;
                    let bal = get_balance(ctx, target_uuid);
                    ctx.reply(format!("§6{target_name} has §e{bal} Shards§6."));
                    Ok(())
                }
                "giveitem" => {
                    let target_arg = args.get(1).ok_or("Usage: /reward giveitem <player> <material> <amount> [reason...]")?;
                    let mat_arg = args.get(2).ok_or("Usage: /reward giveitem <player> <material> <amount> [reason...]")?;
                    let amount = args.get(3).and_then(|s| s.parse::<u32>().ok()).ok_or("Amount must be a whole number.")?;
                    let reason = if args.len() > 4 { Some(args.rest(4)) } else { None };

                    let online_player = ctx.find_online(target_arg);
                    let (target_uuid, target_name) = match online_player {
                        Some(ref p) => (p.uuid, p.name.clone()),
                        None => {
                            ctx.reply("§cTarget must be online to receive an item reward.");
                            return Ok(());
                        }
                    };

                    let material = normalize_material(mat_arg);
                    ctx.console(format!("give {target_name} {material} {amount}"));

                    let actor = ctx.sender_name();
                    log_history(
                        ctx,
                        target_uuid,
                        "ADMIN_ITEM",
                        0,
                        Some(&format!("{material} x{amount}")),
                        reason.as_deref(),
                        Some(&actor),
                    );

                    ctx.reply(format!("§aGave {target_name} {amount}x {material}."));
                    let reason_str = reason.as_ref().map(|r| format!(" §6- {r}")).unwrap_or_default();
                    ctx.tell(target_uuid, format!("§6You received §e{amount}x {material}{reason_str}"));
                    Ok(())
                }
                "pack" => {
                    if args.get(1).map(|s| s.as_str()) != Some("give") || args.len() < 4 {
                        return Err("Usage: /reward pack give <player> <packId>".into());
                    }
                    let target_arg = args.get(2).unwrap();
                    let pack_id = args.get(3).unwrap();

                    let online_player = ctx.find_online(target_arg);
                    let (target_uuid, target_name) = match online_player {
                        Some(ref p) => (p.uuid, p.name.clone()),
                        None => {
                            ctx.reply("§cTarget must be online to receive a pack.");
                            return Ok(());
                        }
                    };

                    let pack = find_pack(pack_id).ok_or(format!("No pack named '{pack_id}'."))?;
                    if pack.claim == "ONE_TIME" && has_claim(ctx, target_uuid, pack.id) {
                        ctx.reply("§cThat player already claimed this one-time pack.");
                        return Ok(());
                    }

                    if pack.shards > 0 {
                        add_balance(ctx, target_uuid, pack.shards as i64)?;
                    }
                    for &(item, amt) in pack.items {
                        ctx.console(format!("give {target_name} {item} {amt}"));
                    }
                    record_claim(ctx, target_uuid, pack.id);

                    let actor = ctx.sender_name();
                    log_history(
                        ctx,
                        target_uuid,
                        "PACK_ADMIN",
                        pack.shards as i64,
                        Some(pack.id),
                        Some("Admin pack grant"),
                        Some(&actor),
                    );

                    ctx.reply(format!("§aGranted pack '{}' to {target_name}.", pack.display_name));
                    ctx.tell(target_uuid, format!("§6You received the §e{}§6 pack!", pack.display_name));
                    Ok(())
                }
                "history" => {
                    let target_arg = args.get(1).ok_or("Usage: /reward history <player>")?;
                    let (target_uuid, target_name) = resolve_player(ctx, target_arg)
                        .ok_or(format!("Unknown player '{target_arg}'."))?;

                    let entries = get_history(ctx, target_uuid, 20);
                    if entries.is_empty() {
                        ctx.reply(format!("§7No reward history for {target_name}."));
                        return Ok(());
                    }

                    ctx.reply(format!("§6Reward history for {target_name}:"));
                    for e in entries {
                        let sign = if e.amount >= 0 { "+" } else { "" };
                        let detail = e.detail.as_ref().map(|d| format!(" ({d})")).unwrap_or_default();
                        let reason = e.reason.as_ref().map(|r| format!(" - {r}")).unwrap_or_default();
                        let actor = e.actor.as_ref().map(|a| format!(" [by {a}]")).unwrap_or_default();
                        ctx.reply(format!(
                            "§7[{}] §f{} {sign}{}{detail}{reason}{actor}",
                            format_ts(e.timestamp),
                            e.r#type,
                            e.amount
                        ));
                    }
                    Ok(())
                }
                "vote" => {
                    let target_arg = args.get(1).ok_or("Usage: /reward vote <player> [service]")?;
                    let service = args.get(2).map(|s| s.as_str()).unwrap_or("PlanetMinecraft");

                    let (target_uuid, target_name) = resolve_player(ctx, target_arg)
                        .ok_or(format!("Unknown player '{target_arg}'."))?;

                    let shards = 50i64;
                    let new_bal = add_balance(ctx, target_uuid, shards)?;
                    log_history(
                        ctx,
                        target_uuid,
                        "VOTE_REWARD",
                        shards,
                        Some(service),
                        Some(&format!("Voted on {service}")),
                        None,
                    );

                    ctx.reply(format!("§aGranted vote reward to {target_name}."));
                    ctx.tell(
                        target_uuid,
                        format!("§6Thanks for voting on §e{service}§6! You received §e{shards} Shards§6 (new balance: {new_bal})."),
                    );
                    Ok(())
                }
                "weblink" => {
                    let target_arg = args.get(1).ok_or("Usage: /reward weblink <player> <code>")?;
                    let code = args.get(2).ok_or("Usage: /reward weblink <player> <code>")?;
                    let (target_uuid, target_name) = resolve_player(ctx, target_arg)
                        .ok_or(format!("Unknown player '{target_arg}'."))?;
                    set_weblink(ctx, target_uuid, code);
                    ctx.reply(format!("§aSet link code {code} for {target_name}."));
                    Ok(())
                }
                "web" => {
                    if args.len() < 2 {
                        ctx.reply("ERROR usage");
                        return Ok(());
                    }
                    let web_sub = args.get(1).unwrap().to_lowercase();
                    match web_sub.as_str() {
                        "verify" => {
                            if args.len() < 3 {
                                ctx.reply("ERROR usage");
                                return Ok(());
                            }
                            let code = args.get(2).unwrap();
                            match consume_weblink(ctx, code) {
                                None => ctx.reply("INVALID"),
                                Some(uuid) => {
                                    let names = uuid_names(ctx);
                                    let dashed = format_uuid(uuid);
                                    let name = names.get(&dashed).cloned().unwrap_or_else(|| "unknown".into());
                                    ctx.reply(format!("OK {dashed} {name}"));
                                }
                            }
                        }
                        "balance" => {
                            if args.len() < 3 {
                                ctx.reply("ERROR usage");
                                return Ok(());
                            }
                            let uuid = match parse_uuid(args.get(2).unwrap()) {
                                Some(u) => u,
                                None => {
                                    ctx.reply("ERROR bad-uuid");
                                    return Ok(());
                                }
                            };
                            let bal = get_balance(ctx, uuid);
                            ctx.reply(format!("OK {bal}"));
                        }
                        "owned" => {
                            if args.len() < 3 {
                                ctx.reply("ERROR usage");
                                return Ok(());
                            }
                            let uuid = match parse_uuid(args.get(2).unwrap()) {
                                Some(u) => u,
                                None => {
                                    ctx.reply("ERROR bad-uuid");
                                    return Ok(());
                                }
                            };
                            let owned = list_web_claims(ctx, uuid);
                            ctx.reply(format!("OK {}", owned.join(",")));
                        }
                        "owns" => {
                            if args.len() < 4 {
                                ctx.reply("ERROR usage");
                                return Ok(());
                            }
                            let uuid = match parse_uuid(args.get(2).unwrap()) {
                                Some(u) => u,
                                None => {
                                    ctx.reply("ERROR bad-uuid");
                                    return Ok(());
                                }
                            };
                            let item_id = args.get(3).unwrap();
                            let owned = has_claim(ctx, uuid, &format!("web_cosmetic:{item_id}"));
                            ctx.reply(if owned { "OK true" } else { "OK false" });
                        }
                        "buy" => {
                            if args.len() < 5 {
                                ctx.reply("ERROR usage");
                                return Ok(());
                            }
                            let uuid = match parse_uuid(args.get(2).unwrap()) {
                                Some(u) => u,
                                None => {
                                    ctx.reply("ERROR bad-uuid");
                                    return Ok(());
                                }
                            };
                            let price = match args.get(3).unwrap().parse::<u64>() {
                                Ok(p) => p,
                                Err(_) => {
                                    ctx.reply("ERROR bad-price");
                                    return Ok(());
                                }
                            };
                            let cosmetic_id = args.get(4).unwrap();
                            let claim_key = format!("web_cosmetic:{cosmetic_id}");
                            if has_claim(ctx, uuid, &claim_key) {
                                ctx.reply("ALREADY_OWNED");
                                return Ok(());
                            }
                            let bal = get_balance(ctx, uuid);
                            if bal < price {
                                ctx.reply("INSUFFICIENT_FUNDS");
                                return Ok(());
                            }

                            add_balance(ctx, uuid, -(price as i64))?;
                            record_claim(ctx, uuid, &claim_key);
                            log_history(ctx, uuid, "COSMETIC_PURCHASE", -(price as i64), Some(cosmetic_id), Some("Web shop purchase"), None);
                            ctx.console(format!("lp user {} permission set hmccosmetics.{} true", format_uuid(uuid), cosmetic_id));
                            ctx.reply("OK");
                        }
                        "buypack" => {
                            if args.len() < 6 {
                                ctx.reply("ERROR usage");
                                return Ok(());
                            }
                            let uuid = match parse_uuid(args.get(2).unwrap()) {
                                Some(u) => u,
                                None => {
                                    ctx.reply("ERROR bad-uuid");
                                    return Ok(());
                                }
                            };
                            let pack_id = args.get(3).unwrap();
                            let price = match args.get(4).unwrap().parse::<u64>() {
                                Ok(p) => p,
                                Err(_) => {
                                    ctx.reply("ERROR bad-price");
                                    return Ok(());
                                }
                            };
                            let members_str = args.get(5).unwrap();
                            let claim_key = format!("web_pack:{pack_id}");
                            if has_claim(ctx, uuid, &claim_key) {
                                ctx.reply("ALREADY_OWNED");
                                return Ok(());
                            }
                            let bal = get_balance(ctx, uuid);
                            if bal < price {
                                ctx.reply("INSUFFICIENT_FUNDS");
                                return Ok(());
                            }

                            add_balance(ctx, uuid, -(price as i64))?;
                            record_claim(ctx, uuid, &claim_key);
                            for m in members_str.split(',') {
                                if !m.is_empty() {
                                    record_claim(ctx, uuid, &format!("web_cosmetic:{m}"));
                                    ctx.console(format!("lp user {} permission set hmccosmetics.{} true", format_uuid(uuid), m));
                                }
                            }
                            log_history(ctx, uuid, "PACK_PURCHASE", -(price as i64), Some(pack_id), Some("Web shop pack purchase"), None);
                            ctx.reply("OK");
                        }
                        "buyrank" => {
                            if args.len() < 5 {
                                ctx.reply("ERROR usage");
                                return Ok(());
                            }
                            let uuid = match parse_uuid(args.get(2).unwrap()) {
                                Some(u) => u,
                                None => {
                                    ctx.reply("ERROR bad-uuid");
                                    return Ok(());
                                }
                            };
                            let price = match args.get(3).unwrap().parse::<u64>() {
                                Ok(p) => p,
                                Err(_) => {
                                    ctx.reply("ERROR bad-price");
                                    return Ok(());
                                }
                            };
                            let tier = match args.get(4).unwrap().parse::<u32>() {
                                Ok(t) if (1..=5).contains(&t) => t,
                                _ => {
                                    ctx.reply("ERROR bad-tier");
                                    return Ok(());
                                }
                            };
                            let claim_key = format!("web_rank:{tier}");
                            if has_claim(ctx, uuid, &claim_key) {
                                ctx.reply("ALREADY_OWNED");
                                return Ok(());
                            }
                            let bal = get_balance(ctx, uuid);
                            if bal < price {
                                ctx.reply("INSUFFICIENT_FUNDS");
                                return Ok(());
                            }

                            add_balance(ctx, uuid, -(price as i64))?;
                            for i in 1..=tier {
                                record_claim(ctx, uuid, &format!("web_rank:{i}"));
                            }
                            log_history(ctx, uuid, "RANK_PURCHASE", -(price as i64), Some(&format!("cosmetics-tier{tier}")), Some("Web shop rank purchase"), None);
                            ctx.console(format!("lp user {} parent add cosmetics-tier{}", format_uuid(uuid), tier));
                            ctx.reply("OK");
                        }
                        _ => ctx.reply("ERROR unknown-subcommand"),
                    }
                    Ok(())
                }
                _ => Err("Usage: /reward <give|take|set|balance|giveitem|pack|history|vote|weblink|web> ...".into()),
            }
        })
        // /playtime and /playtop belong to the playtime plugin (decision 06/11, docs/READINESS.md); the same
        // numbers from the rewards side stay reachable as /rewards playtime [player] and /rewards top [n].
        // EntityTameEvent: count wolves tamed per player
        .on_tame(|ctx, kind, owner| {
            if kind != "minecraft:wolf" {
                return;
            }
            let Some(o) = owner else { return };
            mark_active(o);
            let key = format!("wolf_tames.{}", format_uuid(o));
            let n = ctx.get(&key).and_then(|v| v.as_u64()).unwrap_or(0) + 1;
            let _ = ctx.set(&key, json::json!(n));
        })
        // Activity tracking for AFK
        .on_action(|ctx, _action| {
            if let Some(p) = ctx.try_player() {
                mark_active(p.uuid);
            }
            false
        })
        .on_block_break(|ctx, _pos| {
            if let Some(p) = ctx.try_player() {
                mark_active(p.uuid);
            }
            false
        })
        .on_command_line(|ctx, _name, _line| {
            if let Some(p) = ctx.try_player() {
                mark_active(p.uuid);
            }
            None
        })
        // Passive playtime accrual every 30 seconds (600 ticks) + web link purging
        .every(600, |ctx| {
            purge_weblinks(ctx);
            let players = ctx.online();
            for p in players {
                if is_afk(p.uuid, 300) {
                    continue;
                }
                let awards = accrue_playtime(ctx, p.uuid, 30, 600);
                if awards > 0 {
                    let shards = awards as i64;
                    if let Ok(new_bal) = add_balance(ctx, p.uuid, shards) {
                        log_history(ctx, p.uuid, "PLAYTIME", shards, None, Some("Passive playtime accrual"), None);
                        ctx.tell(p.uuid, format!("§a+{shards} Shards (playtime) - balance {new_bal}"));
                    }
                }
            }
        })
}
