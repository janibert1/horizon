//! Port of mod-spec-plugin (Paper, 179 lines): `/spec` saves the moderator's place and switches to
//! spectator, `/norm` returns them there in survival. Needs the node `modspec.use` (ops pass; others via
//! config/hplug/permissions.json). Return points persist (config/hplug/modspec.json).
//! Teleport guard (06/11, engine hook `player_teleport`): a moderator (`modspec.use`) may only teleport (`/tp`,
//! the spectator menu) while in spectator mode, as the Paper plugin's PlayerTeleportEvent guard did. Players of op
//! level `guard_exempt_level` (3) and up are exempt; `teleport_guard: false` in config/hplug/modspec.json turns it off.
//! `/norm`'s own return teleport runs before the switch back to survival, so it is allowed.
use hplug::{Plugin, json};

pub fn plugin() -> Plugin {
    Plugin::new("modspec")
        .command("spec", 0, |ctx, _| {
            let p = ctx.player().clone();
            if !ctx.has_perm(p.uuid, &p.name, "modspec.use") {
                return Err("You do not have permission to use /spec.".into());
            }
            let key = format!("ret.{:032x}", p.uuid);
            if ctx.get(&key).is_some() {
                ctx.reply("You already have a stored return point. Use /norm first.");
                return Ok(());
            }
            ctx.set(&key, json::json!({"dim": p.dimension, "pos": p.position, "yaw": p.yaw, "pitch": p.pitch}))?;
            ctx.console(format!("gamemode spectator {}", p.name));
            ctx.reply("Spectator mode enabled. Your return point has been saved.");
            Ok(())
        })
        .on_teleport(|ctx, _t| {
            if !ctx.get("teleport_guard").and_then(|v| v.as_bool()).unwrap_or(true) {
                return false;
            }
            let Some(p) = ctx.try_player().cloned() else { return false };
            let exempt = ctx.get("guard_exempt_level").and_then(|v| v.as_u64()).unwrap_or(3) as u8;
            if p.game_mode == 3 || p.permission >= exempt || !ctx.has_perm(p.uuid, &p.name, "modspec.use") {
                return false;
            }
            ctx.tell(p.uuid, "§cModerators can only teleport in spectator mode: use /spec first.");
            true
        })
        .command("norm", 0, |ctx, _| {
            let p = ctx.player().clone();
            if !ctx.has_perm(p.uuid, &p.name, "modspec.use") {
                return Err("You do not have permission to use /norm.".into());
            }
            let key = format!("ret.{:032x}", p.uuid);
            let Some(v) = ctx.get(&key) else {
                ctx.reply("No stored return point was found.");
                return Ok(());
            };
            let dim = v["dim"].as_str().unwrap_or("minecraft:overworld").to_owned();
            let pos: Vec<f64> = v["pos"].as_array().map(|a| a.iter().filter_map(|x| x.as_f64()).collect()).unwrap_or_default();
            if pos.len() != 3 {
                return Err("The stored return point is damaged.".into());
            }
            let (yaw, pitch) = (v["yaw"].as_f64().unwrap_or(0.0), v["pitch"].as_f64().unwrap_or(0.0));
            ctx.remove(&key)?;
            ctx.console(format!("execute in {dim} run tp {} {:.2} {:.2} {:.2} {yaw:.1} {pitch:.1}", p.name, pos[0], pos[1], pos[2]));
            ctx.console(format!("gamemode survival {}", p.name));
            ctx.reply(format!("Returned to {}, {}, {} and switched back to survival mode.", pos[0].floor(), pos[1].floor(), pos[2].floor()));
            Ok(())
        })
}
