//! Port of nl.jdries.playtime.PlaytimePlugin: /playtime [player] (alias /pt), /playtop [count] (alias /ptop),
//! wolf-tame counter. Reads vanilla stats files `<server>/world/players/stats/<uuid>.json`.
//! Java source: /home/jan/playtime-plugin/src/main/java/nl/jdries/playtime/PlaytimePlugin.java
use hplug::{json, Ctx, Plugin};
use std::collections::BTreeMap;

fn fmt(ticks: u64) -> String {
    let s = ticks / 20;
    let (d, h, m) = (s / 86400, (s % 86400) / 3600, (s % 3600) / 60);
    if d > 0 { format!("{d}d {h}h {m}m") } else if h > 0 { format!("{h}h {m}m") } else { format!("{m}m") }
}

/// uuid (dashed) -> name, from usercache.json then whitelist.json (first wins), as the Java did.
fn uuid_names(ctx: &Ctx) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Some(dir) = ctx.server_dir() else { return out };
    for f in ["usercache.json", "whitelist.json"] {
        let Ok(text) = std::fs::read_to_string(dir.join(f)) else { continue };
        let Ok(v) = json::from_str::<json::Value>(&text) else { continue };
        for e in v.as_array().into_iter().flatten() {
            if let (Some(u), Some(n)) = (e.get("uuid").and_then(|x| x.as_str()), e.get("name").and_then(|x| x.as_str())) {
                out.entry(u.to_owned()).or_insert_with(|| n.to_owned());
            }
        }
    }
    out
}

fn ticks_for(ctx: &Ctx, uuid: &str) -> u64 {
    let Some(dir) = ctx.host.world_dir() else { return 0 };
    let Ok(text) = std::fs::read_to_string(dir.join("players/stats").join(format!("{uuid}.json"))) else { return 0 };
    let Ok(v) = json::from_str::<json::Value>(&text) else { return 0 };
    v["stats"]["minecraft:custom"]["minecraft:play_time"].as_u64().unwrap_or(0)
}

fn leaderboard(ctx: &Ctx) -> Vec<(String, u64)> {
    let mut rows: Vec<(String, u64)> = uuid_names(ctx)
        .into_iter()
        .map(|(u, n)| { let t = ticks_for(ctx, &u); (n, t) })
        .filter(|(_, t)| *t > 0)
        .collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r.1));
    rows
}

pub fn plugin() -> Plugin {
    Plugin::new("playtime")
        .command("playtime [player:word]", 0, |ctx, args| {
            let target = match args.get(0) {
                Some(t) => t.clone(),
                None if ctx.is_player() => ctx.sender_name(),
                None => return Err("Specify a player name.".into()),
            };
            let names = uuid_names(ctx);
            let uuid = names.iter().find(|(_, n)| n.eq_ignore_ascii_case(&target)).map(|(u, _)| u.clone())
                .ok_or(format!("Player not found: {target}"))?;
            let ticks = ticks_for(ctx, &uuid);
            let who = if ctx.sender_name().eq_ignore_ascii_case(&target) { "Your".to_owned() } else { format!("{target}'s") };
            ctx.reply(format!("§6⏱ §e{who} playtime: §f{}", fmt(ticks)));
            Ok(())
        })
        .alias("pt")
        .command("playtop [count:integer]", 0, |ctx, args| {
            let count = args.int(0).unwrap_or(10).clamp(1, 20) as usize;
            let board = leaderboard(ctx);
            if board.is_empty() {
                ctx.reply("§7No playtime data yet.");
                return Ok(());
            }
            let n = count.min(board.len());
            ctx.reply(format!("§6─── ⏱ Playtime Top {n} ───"));
            let medals = ["🥇", "🥈", "🥉"];
            for (i, (name, t)) in board.iter().take(n).enumerate() {
                let rank = if i < 3 { medals[i].to_owned() } else { format!("#{}", i + 1) };
                ctx.reply(format!("§e{rank} §f{name}§7 — {}", fmt(*t)));
            }
            Ok(())
        })
        .alias("ptop")
        // EntityTameEvent: count wolves tamed per player (the Java kept it in wolf_tames.json)
        .on_tame(|ctx, kind, owner| {
            if kind != "minecraft:wolf" { return; }
            let Some(o) = owner else { return };
            let key = format!("tames.{o:032x}");
            let n = ctx.get(&key).and_then(|v| v.as_u64()).unwrap_or(0) + 1;
            let _ = ctx.set(&key, json::json!(n));
        })
}
