//! Port of nl.jdries.mute.MutePlugin (ChatMute): /mute <player>, /unmute <player>, muted players' chat is dropped.
//! Java source: /home/jan/mute-plugin/src/main/java/nl/jdries/mute/MutePlugin.java
//! Needs the `player_chat` hook (local fork patch, not upstream yet). Permission chatmute.use (default op) -> op level 2.
//! The chat hook has no Host, so the muted set lives in memory and is loaded from `muted` in
//! config/hplug/mute.json whenever any handler with a Ctx runs (join, commands): a player must have joined
//! to chat, so their mute is always loaded by then.
use hplug::{json, ChatVerdict, Ctx, Plugin};
use std::collections::HashSet;
use std::sync::Mutex;

static MUTED: Mutex<Option<HashSet<u128>>> = Mutex::new(None);

fn load(ctx: &Ctx) {
    let mut g = MUTED.lock().unwrap();
    if g.is_none() {
        let rows = ctx.get("muted").and_then(|v| v.as_array().cloned()).unwrap_or_default();
        *g = Some(rows.iter().filter_map(|r| u128::from_str_radix(r.as_str()?, 16).ok()).collect());
    }
}
fn save(ctx: &mut Ctx) -> Result<(), String> {
    let rows: Vec<String> = MUTED.lock().unwrap().as_ref().map(|s| s.iter().map(|u| format!("{u:032x}")).collect()).unwrap_or_default();
    ctx.set("muted", json::json!(rows))
}

pub fn plugin() -> Plugin {
    Plugin::new("mute")
        .command("mute <player:word>", 2, |ctx, args| {
            load(ctx);
            let name = args.get(0).ok_or("Usage: /mute <player>")?;
            let target = ctx.find_online(name).ok_or(format!("Player not found or not online: {name}"))?;
            let added = MUTED.lock().unwrap().as_mut().unwrap().insert(target.uuid);
            if added {
                save(ctx)?;
                ctx.reply(format!("§aMuted §f{}§a.", target.name));
                ctx.tell(target.uuid, "§cYou have been muted by a moderator.");
            } else {
                ctx.reply(format!("§e{} is already muted.", target.name));
            }
            Ok(())
        })
        .command("unmute <player:word>", 2, |ctx, args| {
            load(ctx);
            let name = args.get(0).ok_or("Usage: /unmute <player>")?;
            let online = ctx.find_online(name);
            let found = online.as_ref().map(|p| (p.uuid, p.name.clone())).or_else(|| ctx.uuid_for_name(name));
            let removed = found.as_ref().is_some_and(|(u, _)| MUTED.lock().unwrap().as_mut().unwrap().remove(u));
            let shown = found.as_ref().map(|f| f.1.clone()).unwrap_or_else(|| name.clone());
            if !removed {
                ctx.reply(format!("§e{shown} is not muted."));
                return Ok(());
            }
            save(ctx)?;
            ctx.reply(format!("§aUnmuted §f{shown}§a."));
            if let Some(p) = online {
                ctx.tell(p.uuid, "§aYou have been unmuted.");
            }
            Ok(())
        })
        .on_join(|ctx| load(ctx))
        .on_chat(|chat| {
            let muted = MUTED.lock().unwrap().as_ref().is_some_and(|s| s.contains(&chat.player));
            if muted { ChatVerdict::Cancel(Some("§cYou are muted and cannot send messages.".into())) } else { ChatVerdict::Allow }
        })
}
