//! Vanish (replaces Vanish++ / Essentials' vanish): `/vanish [player]` (alias `/v`). A vanished player is hidden
//! from players below `see_level` (config, default 2 = ops see vanished staff) in the world, the Tab list, the
//! server-list sample and join/leave messages (`Host::hide_player`). Kept across restarts and re-applied on join.
//! Permission `vanish.use` (ops always), `vanish.others` for `/vanish <player>`.
//! Vanished players do not pick up items or experience orbs (Vanish++ default; config `allow_pickup: true` to allow).
//! NOT (no engine hooks): mobs still target the player, container opening still plays its sound/animation. Use
//! spectator mode for silent chest looking.
use hplug::{json, Args, CmdResult, Ctx, Plugin};

fn list(ctx: &Ctx) -> Vec<String> {
    ctx.get("vanished").and_then(|v| v.as_array().cloned()).unwrap_or_default().iter().filter_map(|x| x.as_str().map(str::to_owned)).collect()
}
fn level(ctx: &Ctx) -> u8 {
    ctx.get("see_level").and_then(|v| v.as_u64()).unwrap_or(2).min(4) as u8
}
/// Hide or show now (the Host call is refused inside a player's hook: next tick).
fn apply(ctx: &mut Ctx, uuid: u128, hidden: bool) {
    let lvl = level(ctx);
    ctx.run_later(1, move |c| {
        c.host.hide_player(uuid, if hidden { Some(lvl) } else { None });
    });
}
/// Whether a player is vanished (for other plugins: tab counts, join messages, cosmetics). Reads
/// `config/hplug/vanish.json` itself, so it is right whichever plugin's `Ctx` calls it.
pub fn is_vanished(ctx: &Ctx, uuid: u128) -> bool {
    if ctx.plugin_id() == "vanish" {
        return list(ctx).iter().any(|k| *k == hplug::uuid_dashed(uuid));
    }
    let key = hplug::uuid_dashed(uuid);
    ctx.host
        .config_dir()
        .ok()
        .and_then(|d| std::fs::read_to_string(d.join("hplug").join("vanish.json")).ok())
        .and_then(|t| json::from_str::<json::Value>(&t).ok())
        .and_then(|v| v.get("vanished").and_then(|a| a.as_array().cloned()))
        .is_some_and(|a| a.iter().any(|x| x.as_str() == Some(&key)))
}

fn vanish(ctx: &mut Ctx, args: &Args) -> CmdResult {
    let actor = ctx.try_player().cloned();
    let may = |ctx: &Ctx, node: &str| actor.as_ref().is_none_or(|p| ctx.has_perm(p.uuid, &p.name, node));
    if !may(ctx, "vanish.use") {
        return Err("§cYou do not have permission to vanish.".into());
    }
    let target = match args.get(0) {
        Some(n) => {
            if !may(ctx, "vanish.others") {
                return Err("§cYou do not have permission to vanish others.".into());
            }
            ctx.find_online(n).ok_or_else(|| format!("§cPlayer {n} is not online."))?
        }
        None => actor.clone().ok_or("§cUsage: vanish <player>")?,
    };
    let k = hplug::uuid_dashed(target.uuid);
    let mut l = list(ctx);
    let now_hidden = if l.contains(&k) { l.retain(|x| *x != k); false } else { l.push(k); true };
    ctx.set("vanished", json::json!(l))?;
    apply(ctx, target.uuid, now_hidden);
    ctx.tell(target.uuid, if now_hidden { "§aYou are now vanished." } else { "§eYou are visible again." });
    if actor.as_ref().is_none_or(|a| a.uuid != target.uuid) {
        ctx.reply(format!("§a{} is now {}.", target.name, if now_hidden { "vanished" } else { "visible" }));
    }
    Ok(())
}

pub fn plugin() -> Plugin {
    Plugin::new("vanish")
        .command("vanish [player:word]", 0, vanish)
        .alias("v")
        .command("vanishlist", 2, |ctx, _| {
            let online = ctx.online();
            let names: Vec<String> = online.iter().filter(|p| is_vanished(ctx, p.uuid)).map(|p| p.name.clone()).collect();
            ctx.reply(format!("§7Vanished online: §f{}", if names.is_empty() { "-".into() } else { names.join(", ") }));
            Ok(())
        })
        // before the player is placed, so even the join message is hidden from those below `see_level`
        .on_login(|ctx, uuid, _name| {
            if is_vanished(ctx, uuid) {
                let lvl = level(ctx);
                ctx.host.hide_player(uuid, Some(lvl));
            }
            None
        })
        .on_pickup(|ctx, _item, _count| {
            let Some(u) = ctx.player_uuid() else { return false };
            !ctx.get("allow_pickup").and_then(|v| v.as_bool()).unwrap_or(false) && is_vanished(ctx, u)
        })
        .on_join(|ctx| {
            let u = ctx.player().uuid;
            if is_vanished(ctx, u) {
                apply(ctx, u, true);
                ctx.tell(u, "§7You joined vanished.");
            }
        })
}
