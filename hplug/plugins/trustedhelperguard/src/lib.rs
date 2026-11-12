//! Port of trusted-helper-guard (Paper, 585 lines): deny-by-default command guard for "trusted helper" accounts
//! (config `trusted_players`, names). Every command of a trusted player is blocked, or allowed AND logged with what is
//! needed to reverse it (`/thg reverse <id>`, console/RCON or op). Records live in the plugin store (`rec.<id>`).
//! Reversible and allowed: gamemode, tp (previous position of the moved player), give (removes the items again with
//! `clear`), kick (nothing to restore), ban (pardon), effect (clears effects), mute/unmute, cosmetic title/tellraw/
//! playsound/particle/bossbar. Always blocked: op deop stop reload whitelist pardon ban-ip debug datapack... and, because
//! Horizon has no inventory/xp/time snapshot API, `clear`, `xp`/`experience`, `time`, `weather` (the Paper plugin kept
//! snapshots for these). WorldEdit commands do not exist on Horizon.
use hplug::{json, now_ms, Ctx, Plugin};

const BLOCKED: &[&str] = &["op", "deop", "stop", "restart", "reload", "whitelist", "ban-ip", "banip", "pardon", "pardon-ip", "unban", "unbanip", "debug", "jfr", "publish", "datapack", "function", "execute", "summon", "setblock", "fill", "clone", "kill", "data", "scoreboard", "gamerule", "difficulty", "setworldspawn", "defaultgamemode", "save-all", "save-off", "forceload", "transfer", "rcon"];
const NO_SNAPSHOT: &[&str] = &["clear", "xp", "experience", "time", "weather"];
const COSMETIC: &[&str] = &["title", "tellraw", "playsound", "particle", "bossbar"];

struct Decision {
    kind: &'static str,
    threat: &'static str,
    target: Option<String>,
    data: json::Value,
    note: String,
}

fn is_trusted(ctx: &Ctx, name: &str) -> bool {
    ctx.get("trusted_players").and_then(|v| v.as_array().map(|a| a.iter().any(|x| x.as_str().is_some_and(|s| s.eq_ignore_ascii_case(name))))).unwrap_or(false)
}
fn gamemode(s: &str) -> Option<&'static str> {
    Some(match s { "survival" | "s" | "0" => "survival", "creative" | "c" | "1" => "creative", "adventure" | "a" | "2" => "adventure", "spectator" | "sp" | "3" => "spectator", _ => return None })
}
fn mode_name(i: i32) -> &'static str { ["survival", "creative", "adventure", "spectator"].get(i as usize).copied().unwrap_or("survival") }
fn literal(s: &str) -> bool {
    !s.is_empty() && s.len() <= 16 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
}
fn num(s: &str) -> bool { !s.contains('~') && !s.contains('^') && s.parse::<f64>().is_ok() }

fn classify(ctx: &Ctx, me: &hplug::Player, label: &str, p: &[&str]) -> Option<Decision> {
    let online = |n: &str| if literal(n) { ctx.find_online(n) } else { None };
    match label {
        "gamemode" => {
            let mode = gamemode(&p.get(1)?.to_lowercase())?;
            let t = match p.get(2) { Some(n) => online(n)?, None => me.clone() };
            Some(Decision { kind: "GAMEMODE", threat: if t.uuid == me.uuid { "LOW" } else { "MED" }, target: Some(t.name.clone()), data: json::json!({"prev": mode_name(t.game_mode)}), note: format!("gamemode -> {mode} for {}", t.name) })
        }
        "tp" | "teleport" => {
            let pos = |t: &hplug::Player| json::json!({"dim": t.dimension, "pos": t.position, "yaw": t.yaw, "pitch": t.pitch});
            match p.len() {
                2 => { let dest = online(p[1])?; Some(Decision { kind: "TELEPORT", threat: "LOW", target: Some(me.name.clone()), data: pos(me), note: format!("{} -> {}", me.name, dest.name) }) }
                3 => { let a = online(p[1])?; online(p[2])?; Some(Decision { kind: "TELEPORT", threat: "MED", target: Some(a.name.clone()), data: pos(&a), note: format!("{} -> {}", a.name, p[2]) }) }
                4 if p[1..].iter().all(|x| num(x)) => Some(Decision { kind: "TELEPORT", threat: "LOW", target: Some(me.name.clone()), data: pos(me), note: "self -> coords".into() }),
                5 if p[2..].iter().all(|x| num(x)) => { let a = online(p[1])?; Some(Decision { kind: "TELEPORT", threat: "MED", target: Some(a.name.clone()), data: pos(&a), note: format!("{} -> coords", a.name) }) }
                _ => None,
            }
        }
        "kick" => { let t = online(p.get(1)?)?; Some(Decision { kind: "KICK", threat: "MED", target: Some(t.name.clone()), data: json::json!({}), note: format!("kicked {} (nothing persistent to restore)", t.name) }) }
        "ban" => { let t = *p.get(1)?; if !literal(t) { return None; } Some(Decision { kind: "BAN", threat: "HIGH", target: Some(t.to_owned()), data: json::json!({}), note: format!("banned {t} (reversal pardons)") }) }
        "give" => {
            let t = online(p.get(1)?)?;
            let item = p.get(2)?.trim_start_matches("minecraft:");
            if item.is_empty() || !item.chars().all(|c| c.is_ascii_lowercase() || c == '_' || c.is_ascii_digit()) { return None; }
            let amount: u32 = match p.get(3) { Some(a) => a.parse().ok()?, None => 1 };
            let valuable = ["diamond", "netherite", "emerald", "elytra", "totem", "enchanted_golden_apple", "nether_star", "dragon_egg", "beacon", "trident", "shulker_box", "experience_bottle"].iter().any(|v| item.contains(v));
            Some(Decision { kind: "GIVE", threat: if valuable { "HIGH" } else { "MED" }, target: Some(t.name.clone()), data: json::json!({"item": item, "amount": amount}), note: format!("gave {amount}x {item} to {}", t.name) })
        }
        "effect" => {
            let sub = p.get(1)?.to_lowercase();
            if sub != "give" && sub != "clear" { return None; }
            let t = online(p.get(2)?)?;
            Some(Decision { kind: "EFFECT", threat: "LOW", target: Some(t.name.clone()), data: json::json!({}), note: format!("effect {sub} on {}", t.name) })
        }
        "mute" | "unmute" => { let t = *p.get(1)?; if !literal(t) { return None; } Some(Decision { kind: "MUTE", threat: "LOW", target: Some(t.to_owned()), data: json::json!({"action": label}), note: format!("{label} {t}") }) }
        l if COSMETIC.contains(&l) => Some(Decision { kind: "COSMETIC_NOOP", threat: "LOW", target: None, data: json::json!({}), note: format!("cosmetic: {l}") }),
        _ => None,
    }
}

fn record(ctx: &mut Ctx, who: &str, raw: &str, d: Option<&Decision>, blocked: bool, note: &str) -> u64 {
    let id = ctx.get("last_id").and_then(|v| v.as_u64()).unwrap_or(0) + 1;
    let _ = ctx.set("last_id", json::json!(id));
    let rec = json::json!({"id": id, "ts": now_ms(), "player": who, "raw": raw, "blocked": blocked, "reversed": false, "note": note,
        "kind": d.map(|d| d.kind).unwrap_or("BLOCKED"), "threat": d.map(|d| d.threat).unwrap_or("LOW"),
        "target": d.and_then(|d| d.target.clone()), "data": d.map(|d| d.data.clone()).unwrap_or(json::json!({}))});
    let _ = ctx.set(&format!("rec.{id}"), rec);
    ctx.log(format!("CMD id={id} threat={} player={who} {} cmd=\"{raw}\"", d.map(|d| d.threat).unwrap_or("LOW"), if blocked { "BLOCKED" } else { "allowed" }));
    id
}

fn reverse(ctx: &mut Ctx, rec: &json::Value) -> String {
    let t = rec["target"].as_str().unwrap_or("").to_owned();
    let d = &rec["data"];
    match rec["kind"].as_str().unwrap_or("") {
        "GAMEMODE" => { ctx.console(format!("gamemode {} {t}", d["prev"].as_str().unwrap_or("survival"))); format!("restored {t}'s gamemode to {}", d["prev"].as_str().unwrap_or("survival")) }
        "TELEPORT" => {
            let p: Vec<f64> = d["pos"].as_array().map(|a| a.iter().filter_map(|x| x.as_f64()).collect()).unwrap_or_default();
            if p.len() != 3 { return "damaged record".into(); }
            ctx.console(format!("execute in {} run tp {t} {:.2} {:.2} {:.2} {:.1} {:.1}", d["dim"].as_str().unwrap_or("minecraft:overworld"), p[0], p[1], p[2], d["yaw"].as_f64().unwrap_or(0.0), d["pitch"].as_f64().unwrap_or(0.0)));
            format!("teleported {t} back to their previous location")
        }
        "GIVE" => { ctx.console(format!("clear {t} minecraft:{} {}", d["item"].as_str().unwrap_or("air"), d["amount"].as_u64().unwrap_or(1))); format!("removed up to {}x {} from {t} (what was already used or moved stays gone)", d["amount"], d["item"].as_str().unwrap_or("?")) }
        "BAN" => { ctx.console(format!("pardon {t}")); format!("pardoned {t}") }
        "EFFECT" => { ctx.console(format!("effect clear {t}")); format!("cleared {t}'s effects (earlier effects are not restored)") }
        "MUTE" => { let undo = if d["action"] == "mute" { "unmute" } else { "mute" }; ctx.console(format!("{undo} {t}")); format!("{undo} {t}") }
        "KICK" | "COSMETIC_NOOP" => "nothing persistent to restore".into(),
        k => format!("no reversal for {k}"),
    }
}

pub fn plugin() -> Plugin {
    Plugin::new("trustedhelperguard")
        .on_command_line(|ctx, _name, line| {
            let me = ctx.try_player()?.clone();
            if !is_trusted(ctx, &me.name) { return None; }
            let words: Vec<&str> = line.split_whitespace().collect();
            let first = words.first()?.trim_start_matches('/').to_lowercase();
            let label = first.rsplit(':').next().unwrap_or(&first).to_owned();
            if label == "thg" { return Some("§c[TrustedHelperGuard] That command isn't available to you.".into()); }
            let guarded = BLOCKED.contains(&label.as_str()) || NO_SNAPSHOT.contains(&label.as_str()) || ["gamemode", "tp", "teleport", "kick", "ban", "give", "effect", "mute", "unmute"].contains(&label.as_str()) || COSMETIC.contains(&label.as_str());
            if !guarded { return None; } // ordinary command, no elevated capability
            let mut p = words.clone();
            p[0] = &label;
            let decision = if BLOCKED.contains(&label.as_str()) || NO_SNAPSHOT.contains(&label.as_str()) { None } else { classify(ctx, &me, &label, &p) };
            match decision {
                None => {
                    record(ctx, &me.name, line, None, true, "Blocked (not on the reversible whitelist / unparseable / no snapshot API)");
                    Some("§c[TrustedHelperGuard] That command isn't available to you.".into())
                }
                Some(d) => {
                    let id = record(ctx, &me.name, line, Some(&d), false, &d.note);
                    ctx.tell(me.uuid, format!("§7[TrustedHelperGuard] Logged as #{id} ({} threat) -- reversible.", d.threat));
                    None
                }
            }
        })
        .command("thg <sub:word> <id:integer>", 2, |ctx, args| {
            if args.get(0).map(String::as_str) != Some("reverse") { return Err("Usage: /thg reverse <id>".into()); }
            let id = args.int(1)?;
            let Some(rec) = ctx.get(&format!("rec.{id}")) else { return Err(format!("No record #{id}")) };
            if rec["reversed"] == true { return Err(format!("#{id} was already reversed.")); }
            if rec["blocked"] == true { return Err(format!("#{id} was a blocked attempt -- nothing executed, nothing to reverse.")); }
            let r = reverse(ctx, &rec);
            let mut rec = rec;
            rec["reversed"] = json::json!(true);
            let _ = ctx.set(&format!("rec.{id}"), rec);
            ctx.reply(format!("Reversed #{id}: {r}"));
            Ok(())
        })
}
