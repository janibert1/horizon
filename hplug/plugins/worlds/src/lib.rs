//! Multiverse-Core replacement: extra worlds on one Horizon server, built on the engine's extra levels
//! (`Host::create_level` / `Host::levels`, engine patch `horizon_client::extra_levels`).
//!
//! `/mv create <name> [void|flat|normal] [seed]` (op 2): a new world `<namespace>:<name>` (namespace from config,
//! default `worlds`), saved by the engine in `<world>/horizon_levels.json`, so it is there after a restart.
//! `/mv list`, `/mv info [world]`, `/mv tp <world> [player]`, `/mv spawn` (this world's spawn),
//! `/mv setspawn` (op 2), `/mv remove <name>` (op 3: forgotten at the next restart; the folder stays on disk),
//! `/mv who [world]`. Aliases: `/mvtp <world>`, `/worlds`.
//! Names: `world`, `world_nether`, `world_the_end` and the dimension ids are vanilla's levels; any other name is
//! looked up as `<namespace>:<name>`, a full id (`horizons:duel_arena`) works too.
//! Permissions: `worlds.tp` (default open: config `open_tp`), `worlds.tp.<name>` when config `per_world_tp` is
//! on, staff commands by op level. Per-world settings in config/hplug/worlds.json `worlds{id: {spawn, respawn_here}}`:
//! `respawn_here: true` brings a player who died in that world back to its spawn after the respawn (Multiverse's
//! `respawnWorld`); otherwise they respawn as vanilla (bed / anchor / world spawn).
//! Void worlds get a 3x3 stone platform under their spawn the first time someone arrives (and slow falling
//! for 5 s on every teleport into a void world, in case the platform's chunk is still loading).
//!
//! What extra worlds are (engine limits, see engine-patches/README.md): of the Overworld's dimension type (no
//! Nether- or End-type worlds), no portals out of them, one shared game time/weather/game rules (as vanilla's
//! levels share the Overworld's), not unloadable at runtime. Multiverse-Portals / -Inventories / -NetherPortals:
//! not ported (inventories are shared across worlds, as in vanilla's own levels).
use hplug::{json, Args, CmdResult, Ctx, Plugin};
use json::Value;
use std::sync::Mutex;

const VANILLA: [(&str, &str); 3] = [
    ("world", "minecraft:overworld"),
    ("world_nether", "minecraft:the_nether"),
    ("world_the_end", "minecraft:the_end"),
];

#[derive(Default)]
struct Run {
    /// Players who died in a `respawn_here` world: (uuid, world) to send back once alive again.
    respawn: Vec<(u128, String)>,
    /// Void worlds whose platform is to be placed when someone is there: (world, x, y, z).
    platforms: Vec<(String, i32, i32, i32)>,
}
static RUN: Mutex<Option<Run>> = Mutex::new(None);
fn run<R>(f: impl FnOnce(&mut Run) -> R) -> R {
    let mut g = RUN.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(Run::default))
}

fn namespace(ctx: &Ctx) -> String {
    ctx.get("namespace").and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_else(|| "worlds".into())
}
/// A world name or id -> level id (None when no such level exists).
fn resolve(ctx: &mut Ctx, name: &str) -> Option<String> {
    let levels = ctx.host.levels();
    let lower = name.to_lowercase();
    if let Some((_, id)) = VANILLA.iter().find(|(n, _)| *n == lower) {
        return Some((*id).to_owned());
    }
    if levels.contains(&lower) {
        return Some(lower);
    }
    let id = format!("{}:{lower}", namespace(ctx));
    levels.contains(&id).then_some(id)
}
/// The short name players type.
fn short(ctx: &Ctx, id: &str) -> String {
    if let Some((n, _)) = VANILLA.iter().find(|(_, i)| *i == id) {
        return (*n).to_owned();
    }
    let ns = format!("{}:", namespace(ctx));
    id.strip_prefix(&ns).unwrap_or(id).to_owned()
}
fn meta(ctx: &Ctx, id: &str) -> Value {
    ctx.get("worlds").and_then(|w| w.get(id).cloned()).unwrap_or(Value::Null)
}
fn set_meta(ctx: &mut Ctx, id: &str, key: &str, v: Value) -> Result<(), String> {
    let mut all = ctx.get("worlds").and_then(|w| w.as_object().cloned()).unwrap_or_default();
    let mut m = all.get(id).and_then(|x| x.as_object().cloned()).unwrap_or_default();
    m.insert(key.to_owned(), v);
    all.insert(id.to_owned(), Value::Object(m));
    ctx.set("worlds", Value::Object(all))
}
fn spawn_of(ctx: &Ctx, id: &str) -> [f64; 5] {
    let m = meta(ctx, id);
    let s = &m["spawn"];
    let default_y = if m["generator"].as_str() == Some("normal") { 100.0 } else { 65.0 };
    [
        s["x"].as_f64().unwrap_or(0.5),
        s["y"].as_f64().unwrap_or(default_y),
        s["z"].as_f64().unwrap_or(0.5),
        s["yaw"].as_f64().unwrap_or(0.0),
        s["pitch"].as_f64().unwrap_or(0.0),
    ]
}

/// Teleport `player` to `id`'s spawn (vanilla levels: `/execute in .. run spawnpoint`-less: their stored spawn or 0 100 0).
fn send(ctx: &mut Ctx, player: &str, id: &str) {
    let [x, y, z, yaw, pitch] = spawn_of(ctx, id);
    let void = meta(ctx, id)["generator"].as_str() == Some("void");
    if void {
        ctx.console(format!("effect give {player} minecraft:slow_falling 5 0 true"));
        if !meta(ctx, id)["platform"].as_bool().unwrap_or(false) {
            run(|r| r.platforms.push((id.to_owned(), x.floor() as i32, y.floor() as i32 - 1, z.floor() as i32)));
        }
    }
    ctx.console(format!("execute in {id} run tp {player} {x} {y} {z} {yaw} {pitch}"));
}

fn staff(ctx: &Ctx, level: u8) -> Result<(), String> {
    if ctx.op_level() >= level { Ok(()) } else { Err("§cYou may not do that.".into()) }
}

fn mv(ctx: &mut Ctx, args: &Args) -> CmdResult {
    let sub = args.get(0).map(|s| s.to_lowercase()).unwrap_or_else(|| "list".into());
    let me = ctx.try_player().cloned();
    match sub.as_str() {
        "list" => {
            let online = ctx.online();
            ctx.reply("§6Worlds:");
            for id in ctx.host.levels() {
                let n = online.iter().filter(|p| p.dimension == id).count();
                let kind = meta(ctx, &id)["generator"].as_str().map(|g| format!(" §8({g})")).unwrap_or_default();
                let name = short(ctx, &id);
                ctx.reply(format!("§e{name} §7{id}{kind} §f{n} online"));
            }
        }
        "create" => {
            staff(ctx, 2)?;
            let name = args.get(1).map(|s| s.to_lowercase()).ok_or("§cUsage: /mv create <name> [normal|nether|the_end] [void|flat|normal] [seed]")?;
            if VANILLA.iter().any(|(n, _)| *n == name) {
                return Err("§cThat is a vanilla level.".into());
            }
            let id = if name.contains(':') { name.clone() } else { format!("{}:{name}", namespace(ctx)) };
            // Multiverse: `/mv create <name> <NORMAL|NETHER|THE_END> [type] [seed]`; the environment may be left out
            let mut rest: Vec<String> = (2..6).filter_map(|i| args.get(i).map(|s| s.to_lowercase())).collect();
            let env = match rest.first().map(String::as_str) {
                Some("nether" | "the_nether") => {
                    rest.remove(0);
                    "nether"
                }
                Some("the_end" | "end") => {
                    rest.remove(0);
                    "the_end"
                }
                Some("overworld") => {
                    rest.remove(0);
                    "normal"
                }
                Some("normal") if rest.len() > 1 && ["void", "empty", "flat", "normal", "default"].contains(&rest[1].as_str()) => {
                    rest.remove(0);
                    "normal"
                }
                _ => "normal",
            };
            let kind = rest.first().cloned().unwrap_or_else(|| "normal".into());
            let seed_arg = rest.get(1).cloned();
            let mut generator = match kind.as_str() {
                "void" | "empty" => json::json!({"generator": "void"}),
                "flat" => json::json!({"generator": "flat"}),
                "normal" | "default" => match seed_arg.as_ref() {
                    Some(s) => {
                        // Multiverse: a number, or any text hashed like Java's String.hashCode
                        let seed = s.parse::<i64>().unwrap_or_else(|_| s.chars().fold(0i32, |h, c| h.wrapping_mul(31).wrapping_add(c as i32)) as i64);
                        json::json!({"generator": "normal", "seed": seed})
                    }
                    None => json::json!({"generator": "normal"}),
                },
                other => return Err(format!("§cUnknown type {other}: void, flat or normal.")),
            };
            generator["environment"] = json::json!(env);
            ctx.host.create_level(&id, &generator.to_string()).map_err(|e| format!("§c{e}"))?;
            set_meta(ctx, &id, "generator", json::json!(generator["generator"]))?;
            set_meta(ctx, &id, "environment", json::json!(env))?;
            ctx.reply(format!("§aWorld {name} ({id}, {env}, {kind}) created. §7/mv tp {name}"));
        }
        "remove" | "delete" => {
            staff(ctx, 3)?;
            let name = args.get(1).ok_or("§cUsage: /mv remove <name>")?;
            let id = resolve(ctx, name).ok_or("§cNo such world.")?;
            if VANILLA.iter().any(|(_, i)| *i == id) {
                return Err("§cVanilla levels stay.".into());
            }
            // the engine's table lives for the process: drop it from horizon_levels.json for the next start
            let path = ctx.host.world_dir().map(|d| d.join("horizon_levels.json"));
            let Some(path) = path.filter(|p| p.is_file()) else {
                return Err("§chorizon_levels.json not found beside the world.".into());
            };
            let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
            let mut v: Value = json::from_str(&text).map_err(|e| e.to_string())?;
            if let Some(list) = v.get_mut("levels").and_then(|l| l.as_array_mut()) {
                list.retain(|l| l["id"].as_str() != Some(id.as_str()));
            }
            std::fs::write(&path, json::to_string_pretty(&v).unwrap_or_default()).map_err(|e| e.to_string())?;
            ctx.reply(format!("§aWorld {id} will be gone after the next restart (its folder stays: dimensions/{}).", id.replace(':', "/")));
        }
        "tp" => {
            let name = args.get(1).ok_or("§cUsage: /mv tp <world> [player]")?;
            let id = resolve(ctx, name).ok_or_else(|| format!("§cNo world {name} (/mv list)."))?;
            let target = match args.get(2) {
                Some(t) => {
                    staff(ctx, 2)?;
                    t.clone()
                }
                None => me.as_ref().map(|p| p.name.clone()).ok_or("§cUsage from the console: /mv tp <world> <player>")?,
            };
            if let Some(p) = &me {
                let open = ctx.get("open_tp").and_then(|v| v.as_bool()).unwrap_or(true);
                let per = ctx.get("per_world_tp").and_then(|v| v.as_bool()).unwrap_or(false);
                let may = ctx.op_level() >= 2
                    || ((open || ctx.has_perm(p.uuid, &p.name, "worlds.tp"))
                        && (!per || ctx.has_perm(p.uuid, &p.name, &format!("worlds.tp.{}", short(ctx, &id)))));
                if !may {
                    return Err("§cYou may not go there.".into());
                }
            }
            send(ctx, &target, &id);
            ctx.reply(format!("§aTeleporting {target} to {}.", short(ctx, &id)));
        }
        "spawn" => {
            let p = me.ok_or("§cPlayers only.")?;
            let id = p.dimension.clone();
            send(ctx, &p.name, &id);
        }
        "setspawn" => {
            staff(ctx, 2)?;
            let p = me.ok_or("§cPlayers only.")?;
            set_meta(ctx, &p.dimension, "spawn", json::json!({"x": p.position[0], "y": p.position[1], "z": p.position[2], "yaw": p.yaw, "pitch": p.pitch}))?;
            set_meta(ctx, &p.dimension, "platform", json::json!(true))?;
            ctx.reply(format!("§aSpawn of {} set here.", short(ctx, &p.dimension)));
        }
        "respawnhere" => {
            staff(ctx, 2)?;
            let name = args.get(1).ok_or("§cUsage: /mv respawnhere <world> <on|off>")?;
            let id = resolve(ctx, name).ok_or("§cNo such world.")?;
            let on = args.get(2).map(|s| s == "on").unwrap_or(true);
            set_meta(ctx, &id, "respawn_here", json::json!(on))?;
            ctx.reply(format!("§aRespawn in {}: {}", short(ctx, &id), if on { "on" } else { "off" }));
        }
        "info" | "who" => {
            let id = match args.get(1) {
                Some(n) => resolve(ctx, n).ok_or("§cNo such world.")?,
                None => me.map(|p| p.dimension).ok_or("§cUsage: /mv info <world>")?,
            };
            let names: Vec<String> = ctx.online().into_iter().filter(|p| p.dimension == id).map(|p| p.name).collect();
            let m = meta(ctx, &id);
            let [x, y, z, ..] = spawn_of(ctx, &id);
            ctx.reply(format!("§6{} §7({id})", short(ctx, &id)));
            ctx.reply(format!("§7Type: §f{} §7environment: §f{}", m["generator"].as_str().unwrap_or("vanilla"), m["environment"].as_str().unwrap_or("normal")));
            ctx.reply(format!("§7Spawn: §f{x:.1} {y:.1} {z:.1} §7respawn here: §f{}", m["respawn_here"].as_bool().unwrap_or(false)));
            ctx.reply(format!("§7Players ({}): §f{}", names.len(), if names.is_empty() { "-".into() } else { names.join(", ") }));
        }
        _ => ctx.reply("§7/mv list | create <name> [normal|nether|the_end] [void|flat|normal] [seed] | tp <world> [player] | spawn | setspawn | respawnhere <world> on|off | info [world] | remove <name>"),
    }
    Ok(())
}

pub fn plugin() -> Plugin {
    Plugin::new("worlds")
        .command("mv [sub:word] [a:word] [b:word] [c:word]", 0, mv)
        .alias("multiverse")
        .alias("worlds")
        .command("mvtp <world:word> [player:word]", 0, |ctx, args| {
            let mut a = vec!["tp".to_owned()];
            a.extend((0..args.len()).filter_map(|i| args.get(i).cloned()));
            mv(ctx, &Args(a))
        })
        .on_death(|ctx, _cause| {
            let Some(p) = ctx.try_player().cloned() else { return };
            if meta(ctx, &p.dimension)["respawn_here"].as_bool().unwrap_or(false) {
                run(|r| r.respawn.push((p.uuid, p.dimension.clone())));
            }
        })
        .every(10, |ctx| {
            let online = ctx.online();
            // back to their world once alive again (respawned)
            let due: Vec<(u128, String)> = run(|r| std::mem::take(&mut r.respawn));
            let mut keep = Vec::new();
            for (u, id) in due {
                match online.iter().find(|p| p.uuid == u) {
                    Some(p) if p.health > 0.0 => {
                        if p.dimension != id {
                            let name = p.name.clone();
                            send(ctx, &name, &id);
                        }
                    }
                    Some(_) => keep.push((u, id)),
                    None => {}
                }
            }
            run(|r| r.respawn.extend(keep));
            // void-world platforms, once a player is in that world (its spawn chunk is loaded then)
            let pending: Vec<(String, i32, i32, i32)> = run(|r| std::mem::take(&mut r.platforms));
            let mut later = Vec::new();
            for (id, x, y, z) in pending {
                if online.iter().any(|p| p.dimension == id) {
                    ctx.console(format!("execute in {id} run fill {} {y} {} {} {y} {} minecraft:stone keep", x - 1, z - 1, x + 1, z + 1));
                    let _ = set_meta(ctx, &id, "platform", json::json!(true));
                } else {
                    later.push((id, x, y, z));
                }
            }
            run(|r| {
                for l in later {
                    if !r.platforms.contains(&l) {
                        r.platforms.push(l);
                    }
                }
            });
        })
}
