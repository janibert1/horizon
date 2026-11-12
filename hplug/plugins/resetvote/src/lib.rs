//! Port of reset-vote (Paper, 474 lines): `/vote reset <yes|no|status>`, `/resetvoteadmin newseason|forcecheck`
//! and the console/RCON API `resetvoteadmin webstatus|webvote <player> <yes|no>` used by the Discord bot.
//! Season-reset vote: yes > no must hold `countdown_days` straight days; alt accounts count once.
//! Config / state in `config/hplug/resetvote.json`: min_playtime_hours (4), countdown_days (7),
//! discord_bot_token, discord_channel_id (milestone posts; sent with `curl` from a thread),
//! votes {uuid: bool}, vote_ts {uuid: ms}, sustained_start_ms, due_announced.
//! Alt groups: `config/hplug/resetvote_alt_accounts.json`, `[["name1","name2"],["name3"]]` (written by the bot).
//! RCON: output of a console command goes to the RCON reply through `Host::feedback`.
use hplug::{json, now_ms, Ctx, Plugin};
use std::collections::{BTreeMap, HashSet};

type Votes = BTreeMap<String, (bool, u64)>; // uuid (32 hex) -> (yes, ts)

fn load_votes(ctx: &Ctx) -> Votes {
    let v = ctx.get("votes").unwrap_or_default();
    let ts = ctx.get("vote_ts").unwrap_or_default();
    v.as_object().into_iter().flatten().filter_map(|(k, b)| Some((k.clone(), (b.as_bool()?, ts.get(k).and_then(|t| t.as_u64()).unwrap_or(0))))).collect()
}
fn save_votes(ctx: &mut Ctx, votes: &Votes) -> Result<(), String> {
    ctx.set("votes", json::Value::Object(votes.iter().map(|(k, (b, _))| (k.clone(), json::json!(b))).collect()))?;
    ctx.set("vote_ts", json::Value::Object(votes.iter().map(|(k, (_, t))| (k.clone(), json::json!(t))).collect()))
}
fn num(ctx: &Ctx, key: &str, default: u64) -> u64 {
    ctx.get(key).and_then(|v| v.as_u64()).unwrap_or(default)
}

fn names(ctx: &Ctx) -> BTreeMap<String, String> {
    // uuid (32 hex) -> name, from usercache.json
    let mut out = BTreeMap::new();
    let Some(dir) = ctx.server_dir() else { return out };
    if let Ok(t) = std::fs::read_to_string(dir.join("usercache.json"))
        && let Ok(v) = json::from_str::<json::Value>(&t)
    {
        for e in v.as_array().into_iter().flatten() {
            if let (Some(u), Some(n)) = (e["uuid"].as_str(), e["name"].as_str()) {
                out.insert(u.replace('-', "").to_lowercase(), n.to_owned());
            }
        }
    }
    out
}
fn playtime_hours(ctx: &Ctx, uuid32: &str) -> u64 {
    let dashed = format!("{}-{}-{}-{}-{}", &uuid32[..8], &uuid32[8..12], &uuid32[12..16], &uuid32[16..20], &uuid32[20..]);
    let Some(dir) = ctx.host.world_dir() else { return 0 };
    let Ok(t) = std::fs::read_to_string(dir.join("players/stats").join(format!("{dashed}.json"))) else { return 0 };
    let Ok(v) = json::from_str::<json::Value>(&t) else { return 0 };
    v["stats"]["minecraft:custom"]["minecraft:play_time"].as_u64().unwrap_or(0) / 20 / 60 / 60
}
fn alt_groups(ctx: &Ctx) -> Vec<HashSet<String>> {
    let Ok(dir) = ctx.host.config_dir() else { return vec![] };
    let Ok(t) = std::fs::read_to_string(dir.join("hplug/resetvote_alt_accounts.json")) else { return vec![] };
    let Ok(v) = json::from_str::<Vec<Vec<String>>>(&t) else { return vec![] };
    v.into_iter().map(|g| g.into_iter().map(|n| n.to_lowercase()).collect()).collect()
}

/// (yes, no) with alt groups counted once (their most recent vote).
fn tally(ctx: &Ctx, votes: &Votes) -> (u64, u64) {
    let names = names(ctx);
    let mut grouped: HashSet<&String> = HashSet::new();
    let (mut yes, mut no) = (0, 0);
    for group in alt_groups(ctx) {
        let members: Vec<&String> = votes.keys().filter(|u| names.get(*u).is_some_and(|n| group.contains(&n.to_lowercase()))).collect();
        if members.len() > 1 {
            let latest = members.iter().max_by_key(|u| votes[**u].1).unwrap();
            if votes[*latest].0 { yes += 1 } else { no += 1 }
            grouped.extend(members);
        }
    }
    for (u, (b, _)) in votes {
        if grouped.contains(u) { continue; }
        if *b { yes += 1 } else { no += 1 }
    }
    (yes, no)
}

fn discord(ctx: &Ctx, text: &str) {
    let (Some(tok), Some(ch)) = (ctx.get("discord_bot_token"), ctx.get("discord_channel_id")) else { return };
    let (Some(tok), Some(ch)) = (tok.as_str().map(str::to_owned), ch.as_str().map(str::to_owned)) else { return };
    if tok.is_empty() || ch.is_empty() { return; }
    let body = json::json!({"content": text}).to_string();
    std::thread::spawn(move || {
        use std::io::Write;
        let child = std::process::Command::new("curl")
            .args(["-s", "-m", "10", "-o", "/dev/null", "-w", "%{http_code}", "-X", "POST", &format!("https://discord.com/api/v10/channels/{ch}/messages"),
                   "-H", &format!("Authorization: Bot {tok}"), "-H", "Content-Type: application/json", "-H", "User-Agent: DiscordBot (horizons-plugin, 1.0)", "--data-binary", "@-"])
            .stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).spawn();
        match child {
            Ok(mut c) => {
                let _ = c.stdin.take().map(|mut s| s.write_all(body.as_bytes()));
                match c.wait_with_output() {
                    Ok(o) if !o.stdout.starts_with(b"2") => eprintln!("[resetvote] Discord post got HTTP {}", String::from_utf8_lossy(&o.stdout)),
                    Err(e) => eprintln!("[resetvote] Discord post failed: {e}"),
                    _ => {}
                }
            }
            Err(e) => eprintln!("[resetvote] Discord post failed (curl missing?): {e}"),
        }
    });
}

fn describe(ctx: &Ctx) -> String {
    let votes = load_votes(ctx);
    let (yes, no) = tally(ctx, &votes);
    let (hours, days) = (num(ctx, "min_playtime_hours", 4), num(ctx, "countdown_days", 7));
    let mut s = format!("Reset vote: {yes} yes / {no} no (need {hours}h+ playtime to count, alts count once). ");
    let start = num(ctx, "sustained_start_ms", 0);
    if ctx.get("due_announced").and_then(|v| v.as_bool()).unwrap_or(false) {
        s += "Reset is DUE - awaiting staff.";
    } else if start == 0 {
        s += &format!("No pro-reset majority right now - needs {days} straight days of yes > no to trigger a reset.");
    } else {
        let held = (now_ms() - start) / 86_400_000;
        s += &format!("Pro-reset majority holding for {held} day(s), {} more needed. Still fully votable - a flip back to no resets this.", days.saturating_sub(held));
    }
    s
}

fn check(ctx: &mut Ctx) {
    let votes = load_votes(ctx);
    let (yes, no) = tally(ctx, &votes);
    let days = num(ctx, "countdown_days", 7);
    let start = num(ctx, "sustained_start_ms", 0);
    let due = ctx.get("due_announced").and_then(|v| v.as_bool()).unwrap_or(false);
    if yes > no && yes + no > 0 {
        if start == 0 {
            let _ = ctx.set("sustained_start_ms", json::json!(now_ms()));
            ctx.broadcast(format!("§6The vote has flipped in favor of resetting! If this majority holds for {days} straight days, the season resets. Anyone can still vote against it any time."));
            discord(ctx, &format!("🗳️ **Reset vote is currently pro-reset** ({yes} yes / {no} no) - needs to hold for {days} straight days to trigger a reset. Votes stay live the whole time."));
        } else if !due && (now_ms() - start) / 86_400_000 >= days {
            let _ = ctx.set("due_announced", json::json!(true));
            ctx.log(format!("RESET DUE: pro-reset majority held for {days} straight days"));
            ctx.broadcast(format!("§cThe pro-reset vote has held for {days} straight days - the season reset is now due. Staff will be performing it soon."));
            discord(ctx, &format!("⏰ **Reset is due!** The pro-reset majority held for {days} straight days - staff need to perform the season reset now."));
        }
    } else if start != 0 && !due {
        let _ = ctx.set("sustained_start_ms", json::json!(0));
        ctx.broadcast(format!("§eThe reset vote has flipped back - the countdown reset. A fresh {days} straight days of pro-reset majority is needed to trigger a reset."));
        discord(ctx, &format!("↩️ **Reset vote flipped back** ({yes} yes / {no} no) - countdown reset, needs a fresh {days} straight days of pro-reset majority."));
    }
}

/// Cast a vote for `uuid` (32 hex); Err(text) for the refusal.
fn cast(ctx: &mut Ctx, uuid: u128, yes: bool) -> Result<bool, u64> {
    let hours = playtime_hours(ctx, &format!("{uuid:032x}"));
    if hours < num(ctx, "min_playtime_hours", 4) {
        return Err(hours);
    }
    let mut votes = load_votes(ctx);
    let changed = votes.insert(format!("{uuid:032x}"), (yes, now_ms())).is_some_and(|(b, _)| b != yes);
    let _ = save_votes(ctx, &votes);
    Ok(changed)
}

pub fn plugin() -> Plugin {
    Plugin::new("resetvote")
        .command("vote <what:word> [choice:word]", 0, |ctx, args| {
            if args.get(0).map(String::as_str) != Some("reset") {
                return Err("Usage: /vote reset <yes|no|status>".into());
            }
            let choice = args.get(1).map(|s| s.to_lowercase()).unwrap_or_else(|| "status".into());
            if choice == "status" {
                let d = describe(ctx);
                ctx.reply(d);
                return Ok(());
            }
            if choice != "yes" && choice != "no" {
                return Err("Usage: /vote reset <yes|no|status>".into());
            }
            if !ctx.is_player() {
                return Err("Only players can cast a vote.".into());
            }
            let u = ctx.player().uuid;
            let need = num(ctx, "min_playtime_hours", 4);
            match cast(ctx, u, choice == "yes") {
                Err(h) => Err(format!("You need {need}h playtime this season to vote - you're at {h}h so far.")),
                Ok(true) => { ctx.reply(format!("§eVote changed to {}.", choice.to_uppercase())); Ok(()) }
                Ok(false) => { ctx.reply(format!("§aVote recorded: {}. You can change it anytime with /vote reset yes|no.", choice.to_uppercase())); Ok(()) }
            }
        })
        .command("resetvoteadmin <sub:word> [a:word] [b:word]", 2, |ctx, args| {
            match args.get(0).map(|s| s.to_lowercase()).as_deref() {
                Some("newseason") => {
                    let _ = ctx.set("votes", json::json!({}));
                    let _ = ctx.set("vote_ts", json::json!({}));
                    let _ = ctx.set("sustained_start_ms", json::json!(0));
                    let _ = ctx.set("due_announced", json::json!(false));
                    ctx.log(format!("Reset vote state cleared for new season by {}", ctx.sender_name()));
                    ctx.reply("ResetVote: cleared all votes and countdown for the new season.");
                }
                Some("forcecheck") => {
                    check(ctx);
                    let d = describe(ctx);
                    ctx.reply(format!("ResetVote: {d}"));
                }
                // Console/RCON API for the Discord bot
                Some("webstatus") => {
                    let votes = load_votes(ctx);
                    let (yes, no) = tally(ctx, &votes);
                    let start = num(ctx, "sustained_start_ms", 0);
                    let held = if start == 0 { 0 } else { (now_ms() - start) / 86_400_000 };
                    let due = ctx.get("due_announced").and_then(|v| v.as_bool()).unwrap_or(false);
                    ctx.reply(format!("OK {yes} {no} {} {} {held} {due}", num(ctx, "min_playtime_hours", 4), num(ctx, "countdown_days", 7)));
                }
                Some("webvote") => {
                    let (Some(name), Some(choice)) = (args.get(1), args.get(2).map(|s| s.to_lowercase())) else { return Err("ERROR usage".into()) };
                    if choice != "yes" && choice != "no" { return Err("ERROR usage".into()); }
                    let Some((u, _)) = ctx.uuid_for_name(name) else { ctx.reply("UNKNOWN"); return Ok(()) };
                    match cast(ctx, u, choice == "yes") {
                        Err(h) => ctx.reply(format!("INELIGIBLE {h}")),
                        Ok(_) => ctx.reply(format!("OK {choice}")),
                    }
                }
                // EXTENSION (not in the Java source on this machine; Ori's vote-reaction removal sends it):
                // `resetvoteadmin webunvote <name>` -> `OK removed` | `OK novote` | `UNKNOWN`
                Some("webunvote") => {
                    let Some(name) = args.get(1) else { return Err("ERROR usage".into()) };
                    let Some((u, _)) = ctx.uuid_for_name(name) else { ctx.reply("UNKNOWN"); return Ok(()) };
                    let mut votes = load_votes(ctx);
                    if votes.remove(&format!("{u:032x}")).is_some() {
                        save_votes(ctx, &votes)?;
                        ctx.reply("OK removed");
                    } else {
                        ctx.reply("OK novote");
                    }
                }
                _ => return Err("Usage: /resetvoteadmin <newseason|forcecheck>".into()),
            }
            Ok(())
        })
        .every(6000, check)
}
