//! Citizens replacement (the parts the SMP used): server-side NPCs that look like players, built on vanilla 26.x's
//! `minecraft:mannequin` entity (a player-model entity with a profile/skin, immovable). Horizon supports it fully on
//! the server (`entity_nbt.rs`, `lan.rs` mannequin data), so no engine change is needed.
//!
//! `/npc create <name> [skin]`, `/npc remove <id>`, `/npc list`, `/npc tphere <id>`, `/npc tp <id>`,
//! `/npc skin <id> <player>`, `/npc rename <id> <name>`, `/npc lookclose <id> [on|off]`,
//! `/npc cmd <id> add <console|player> <command...>` (`{player}` = the clicker), `/npc cmd <id> clear`,
//! `/npc nameplate <id> [on|off]` (op level 2). Right-clicking runs its commands; NPCs take no damage and cannot be
//! pushed. Skins resolve from the profile name on the client (vanilla ResolvableProfile), or from given textures.
//!
//! The NPCs are respawned by this plugin, not kept by the world: each one is summoned with tags `hpnpc`,
//! `hpnpc_<id>` and this session's `hpnpcs_<n>`; leftovers of earlier sessions (saved in chunks) are removed when
//! seen. Other plugins use [`spawn_temp`] / [`remove_temp`] / [`take_hits`] (spawn-stone's decoy).
#![allow(clippy::type_complexity)] // plugin-local helpers with many parts
use horizon_world::nbt::{Compound, JavaString, Tag};
use hplug::{json, Args, CmdResult, Ctx, Damage, Plugin};
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Clone, Debug)]
struct Npc {
    name: String,
    skin: Option<String>,
    dim: String,
    pos: [f64; 3],
    look: [f32; 2],
    commands: Vec<String>,
    lookclose: bool,
    nameplate: bool,
}
impl Npc {
    fn to_json(&self) -> json::Value {
        json::json!({"name": self.name, "skin": self.skin, "world": self.dim, "x": self.pos[0], "y": self.pos[1], "z": self.pos[2],
            "yaw": self.look[0], "pitch": self.look[1], "commands": self.commands, "lookclose": self.lookclose, "nameplate": self.nameplate})
    }
    fn from_json(v: &json::Value) -> Option<Self> {
        Some(Npc {
            name: v.get("name")?.as_str()?.to_owned(),
            skin: v.get("skin").and_then(|s| s.as_str()).map(str::to_owned),
            dim: v.get("world")?.as_str()?.to_owned(),
            pos: [v.get("x")?.as_f64()?, v.get("y")?.as_f64()?, v.get("z")?.as_f64()?],
            look: [v.get("yaw").and_then(|x| x.as_f64()).unwrap_or(0.0) as f32, v.get("pitch").and_then(|x| x.as_f64()).unwrap_or(0.0) as f32],
            commands: v.get("commands").and_then(|c| c.as_array()).into_iter().flatten().filter_map(|c| c.as_str().map(str::to_owned)).collect(),
            lookclose: v.get("lookclose").and_then(|b| b.as_bool()).unwrap_or(false),
            nameplate: v.get("nameplate").and_then(|b| b.as_bool()).unwrap_or(true),
        })
    }
}

/// A temporary NPC another plugin asked for (not saved).
struct Temp {
    dim: String,
    entity: Option<u32>,
    spec: (String, Option<(String, Option<String>)>, [f64; 3], [f32; 2]),
}

#[derive(Default)]
struct Run {
    session: u64,
    entities: HashMap<u32, String>, // entity id -> npc id ("t:<key>" for temps)
    spawned: HashMap<String, u32>,  // npc id -> entity id
    temps: HashMap<String, Temp>,
    hits: Vec<(String, f32, Option<u128>)>, // temp key, damage, attacker player
}
static RUN: Mutex<Option<Run>> = Mutex::new(None);
fn run<R>(f: impl FnOnce(&mut Run) -> R) -> R {
    let mut g = RUN.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(|| Run { session: hplug::now_ms() % 1_000_000_007, ..Default::default() }))
}

fn s(v: &str) -> Tag {
    Tag::String(JavaString::from(v))
}
fn nbt(name: &str, profile: Tag, look: [f32; 2], tags: &[String], nameplate: bool) -> Compound {
    let mut c = Compound::new();
    c.insert(JavaString::from("profile"), profile);
    c.insert(JavaString::from("immovable"), Tag::Byte(1));
    c.insert(JavaString::from("hide_description"), Tag::Byte(1));
    c.insert(JavaString::from("Silent"), Tag::Byte(1));
    c.insert(JavaString::from("PersistenceRequired"), Tag::Byte(1));
    c.insert(JavaString::from("CustomName"), Tag::compound([("text", s(name))]));
    c.insert(JavaString::from("CustomNameVisible"), Tag::Byte(nameplate as i8));
    c.insert(JavaString::from("Rotation"), Tag::List(vec![Tag::Float(look[0].to_bits()), Tag::Float(look[1].to_bits())]));
    c.insert(JavaString::from("Tags"), Tag::List(tags.iter().map(|t| s(t)).collect()));
    c
}
/// A profile by name (the client resolves the skin) or with explicit textures.
fn profile(name_or_textures: &Option<(String, Option<String>)>, fallback: &str) -> Tag {
    match name_or_textures {
        Some((v, sig)) if v.len() > 40 => {
            let mut p = vec![("name", s("textures")), ("value", s(v))];
            if let Some(sig) = sig { p.push(("signature", s(sig))); }
            Tag::compound([("name", s(&fallback.chars().take(16).collect::<String>())), ("properties", Tag::List(vec![Tag::compound(p)]))])
        }
        Some((name, _)) => s(name),
        None => s(&fallback.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_').take(16).collect::<String>()),
    }
}

fn load(ctx: &Ctx) -> HashMap<String, Npc> {
    ctx.get("npcs").and_then(|v| v.as_object().cloned()).unwrap_or_default().iter().filter_map(|(k, v)| Some((k.clone(), Npc::from_json(v)?))).collect()
}
fn store(ctx: &mut Ctx, all: &HashMap<String, Npc>) -> Result<(), String> {
    let m: json::Map<String, json::Value> = all.iter().map(|(k, n)| (k.clone(), n.to_json())).collect();
    ctx.set("npcs", json::Value::Object(m))
}
fn despawn(ctx: &mut Ctx, id: &str, dim: &str) {
    if let Some(e) = run(|r| r.spawned.remove(id)) {
        run(|r| r.entities.remove(&e));
        ctx.host.remove_entity(dim, e);
    }
    // and by tag: an entity the table lost (or a copy from a chunk saved earlier) goes too
    ctx.console(format!("execute in {dim} run kill @e[type=minecraft:mannequin,tag=hpnpc_{id}]"));
}

/// Summon every saved NPC that is not out yet and has a player within 96 blocks (its chunk is loaded).
fn ensure(ctx: &mut Ctx) {
    let session = run(|r| r.session);
    let online = ctx.online();
    // leftovers of earlier sessions come back with their chunks: sweep the dimensions players are in
    let mut dims: Vec<String> = online.iter().map(|o| o.dimension.clone()).collect();
    dims.sort();
    dims.dedup();
    for d in dims {
        ctx.console(format!("execute in {d} run kill @e[type=minecraft:mannequin,tag=hpnpc,tag=!hpnpcs_{session}]"));
    }
    let near = |dim: &str, p: [f64; 3]| online.iter().any(|o| o.dimension == dim && (0..3).map(|i| (o.position[i] - p[i]).powi(2)).sum::<f64>() < 96.0 * 96.0);
    for (id, n) in load(ctx) {
        let alive = run(|r| r.spawned.get(&id).copied()).is_some_and(|e| ctx.host.entity(e).is_some());
        if alive || !near(&n.dim, n.pos) {
            if !alive { run(|r| { if let Some(e) = r.spawned.remove(&id) { r.entities.remove(&e); } }); }
            continue;
        }
        let tags = vec!["hpnpc".to_owned(), format!("hpnpc_{id}"), format!("hpnpcs_{session}")];
        let prof = profile(&n.skin.clone().map(|s| (s, None)), &n.name);
        if let Some(e) = ctx.host.summon(&n.dim, "minecraft:mannequin", n.pos, nbt(&n.name.replace('&', "§"), prof, n.look, &tags, n.nameplate)) {
            run(|r| { r.spawned.insert(id.clone(), e); r.entities.insert(e, id); });
        }
    }
    ensure_temps(ctx);
}

/// A temporary NPC (not saved, gone on restart): `key` names it for [`remove_temp`] and [`take_hits`]. `textures`
/// is a skin `(value, signature)` (e.g. `Host::player_skin` of a player) or None for the skin of `name`.
/// Summoned at the next tick.
pub fn spawn_temp(key: &str, name: &str, textures: Option<(String, Option<String>)>, dim: &str, pos: [f64; 3], look: [f32; 2]) {
    run(|r| { r.temps.insert(key.to_owned(), Temp { dim: dim.to_owned(), entity: None, spec: (name.to_owned(), textures, pos, look) }); });
}
/// Remove a temporary NPC (no-op when gone).
pub fn remove_temp(ctx: &mut Ctx, key: &str) {
    if let Some(t) = run(|r| r.temps.remove(key))
        && let Some(e) = t.entity {
            run(|r| r.entities.remove(&e));
            let dim = t.dim.clone();
            ctx.run_later(1, move |c| { c.host.remove_entity(&dim, e); });
        }
}
/// Hits on temporary NPCs since the last call: `(key, damage, attacking player)`.
pub fn take_hits() -> Vec<(String, f32, Option<u128>)> {
    run(|r| std::mem::take(&mut r.hits))
}

fn cmd(ctx: &mut Ctx, args: &Args) -> CmdResult {
    let mut all = load(ctx);
    let sub = args.get(0).map(|s| s.to_lowercase()).unwrap_or_else(|| "list".into());
    let id_arg = |i: usize| -> Result<String, String> {
        let id = args.get(i).cloned().ok_or("§cUsage: /npc <sub> <id> ...")?;
        if all.contains_key(&id) { Ok(id) } else { Err(format!("§cNo NPC with id {id} (/npc list).")) }
    };
    match sub.as_str() {
        "create" => {
            let p = ctx.try_player().cloned().ok_or("§cPlayers only.")?;
            let name = args.get(1).cloned().ok_or("§cUsage: /npc create <name> [skin]")?;
            let next = ctx.get("next").and_then(|v| v.as_u64()).unwrap_or(1);
            ctx.set("next", json::json!(next + 1))?;
            let id = next.to_string();
            all.insert(id.clone(), Npc { name: name.clone(), skin: args.get(2).cloned(), dim: p.dimension.clone(), pos: p.position, look: [p.yaw, p.pitch], commands: vec![], lookclose: false, nameplate: true });
            store(ctx, &all)?;
            ctx.reply(format!("§aCreated NPC §f{name}§a (id {id})."));
        }
        "remove" | "delete" => {
            let id = id_arg(1)?;
            let n = all.remove(&id).unwrap();
            store(ctx, &all)?;
            let dim = n.dim.clone();
            ctx.run_later(1, move |c| despawn(c, &id, &dim));
            ctx.reply(format!("§aRemoved NPC §f{}§a.", n.name));
        }
        "list" => {
            if all.is_empty() { ctx.reply("§7No NPCs."); }
            let mut ids: Vec<&String> = all.keys().collect();
            ids.sort_by_key(|k| k.parse::<u64>().unwrap_or(0));
            for id in ids {
                let n = &all[id];
                ctx.reply(format!("§7#{id} §f{} §7{} {:.0} {:.0} {:.0} cmds:{}", n.name, n.dim.trim_start_matches("minecraft:"), n.pos[0], n.pos[1], n.pos[2], n.commands.len()));
            }
        }
        "tphere" | "move" => {
            let id = id_arg(1)?;
            let p = ctx.try_player().cloned().ok_or("§cPlayers only.")?;
            let n = all.get_mut(&id).unwrap();
            let old = n.dim.clone();
            n.dim = p.dimension.clone();
            n.pos = p.position;
            n.look = [p.yaw, p.pitch];
            store(ctx, &all)?;
            ctx.run_later(1, move |c| despawn(c, &id, &old));
            ctx.reply("§aNPC moved to you.");
        }
        "tp" => {
            let id = id_arg(1)?;
            let p = ctx.try_player().cloned().ok_or("§cPlayers only.")?;
            let n = &all[&id];
            ctx.teleport_to(&p.name, &n.dim, n.pos, Some(n.look));
        }
        "skin" | "rename" | "nameplate" | "lookclose" => {
            let id = id_arg(1)?;
            let n = all.get_mut(&id).unwrap();
            match sub.as_str() {
                "skin" => n.skin = args.get(2).cloned(),
                "rename" => n.name = args.rest(2),
                "nameplate" => n.nameplate = args.get(2).map(|v| v != "off").unwrap_or(!n.nameplate),
                _ => n.lookclose = args.get(2).map(|v| v != "off").unwrap_or(!n.lookclose),
            }
            let dim = n.dim.clone();
            store(ctx, &all)?;
            ctx.run_later(1, move |c| despawn(c, &id, &dim));
            ctx.reply("§aNPC updated.");
        }
        "cmd" | "command" => {
            let id = id_arg(1)?;
            let n = all.get_mut(&id).unwrap();
            match args.get(2).map(|s| s.to_lowercase()).as_deref() {
                Some("add") => {
                    let mode = args.get(3).map(|s| s.to_lowercase()).filter(|m| m == "console" || m == "player").ok_or("§cUsage: /npc cmd <id> add <console|player> <command>")?;
                    let line = args.rest(4);
                    if line.is_empty() { return Err("§cUsage: /npc cmd <id> add <console|player> <command>".into()); }
                    n.commands.push(format!("{mode}:{}", line.trim_start_matches('/')));
                }
                Some("clear") => n.commands.clear(),
                _ => {
                    for (i, c) in n.commands.iter().enumerate() { ctx.reply(format!("§7{i}: §f{c}")); }
                    return Ok(());
                }
            }
            store(ctx, &all)?;
            ctx.reply("§aNPC commands updated.");
        }
        _ => ctx.reply("§7/npc create|remove|list|tphere|tp|skin|rename|nameplate|lookclose|cmd"),
    }
    Ok(())
}

fn npc_of(entity: i32) -> Option<String> {
    run(|r| r.entities.get(&(entity as u32)).cloned())
}

pub fn plugin() -> Plugin {
    Plugin::new("npcs")
        .command("npc [sub:word] [a:word] [b:word] [c:word] [rest:text]", 2, cmd)
        .on_action(|ctx, a| {
            let hplug::Action::InteractEntity { entity, .. } = a else { return false };
            let Some(id) = npc_of(*entity) else { return false };
            if id.starts_with("t:") { return true; }
            let Some(p) = ctx.try_player().cloned() else { return true };
            let Some(n) = load(ctx).remove(&id) else { return true };
            for c in n.commands {
                let line = c.replace("{player}", &p.name);
                match line.split_once(':') {
                    Some(("player", rest)) => ctx.console(format!("execute as {} at @s run {rest}", p.name)),
                    Some((_, rest)) => ctx.console(rest.to_owned()),
                    None => {}
                }
            }
            true
        })
        .on_attack(|_ctx, entity, _kind| npc_of(entity).is_some())
        // no damage to NPCs; a hit on a temporary one is reported to its owner plugin
        .on_damage(|d: &Damage| {
            let Some(id) = npc_of(d.entity) else { return false };
            if let Some(key) = id.strip_prefix("t:") {
                let key = key.to_owned();
                run(|r| r.hits.push((key, d.amount, d.attacker_player)));
            }
            true
        })
        .every(40, |ctx| {
            ensure(ctx);
            // look close: turn to the nearest player within 8 blocks
            let online = ctx.online();
            for (id, n) in load(ctx).into_iter().filter(|(_, n)| n.lookclose) {
                let Some(e) = run(|r| r.spawned.get(&id).copied()) else { continue };
                let Some(p) = online.iter().filter(|o| o.dimension == n.dim).min_by(|a, b| {
                    let d = |o: &hplug::Player| (0..3).map(|i| (o.position[i] - n.pos[i]).powi(2)).sum::<f64>();
                    d(a).total_cmp(&d(b))
                }) else { continue };
                let (dx, dz) = (p.position[0] - n.pos[0], p.position[2] - n.pos[2]);
                if dx * dx + dz * dz > 64.0 { continue; }
                let yaw = (-dx.atan2(dz)).to_degrees() as f32;
                let dy = p.position[1] - n.pos[1];
                let pitch = (-(dy).atan2((dx * dx + dz * dz).sqrt())).to_degrees() as f32;
                ctx.host.move_entity(&n.dim, e, n.pos, yaw, pitch);
            }
        })
        .every(2, |ctx| {
            // temps should appear quickly
            if run(|r| r.temps.values().any(|t| t.entity.is_none())) { ensure_temps(ctx); }
        })
}

fn ensure_temps(ctx: &mut Ctx) {
    let session = run(|r| r.session);
    let pending: Vec<(String, String, (String, Option<(String, Option<String>)>, [f64; 3], [f32; 2]))> =
        run(|r| r.temps.iter().filter(|(_, t)| t.entity.is_none()).map(|(k, t)| (k.clone(), t.dim.clone(), t.spec.clone())).collect());
    for (key, dim, (name, prof, pos, look)) in pending {
        let tags = vec!["hpnpc".to_owned(), format!("hpnpcs_{session}"), "hpnpc_temp".to_owned()];
        if let Some(e) = ctx.host.summon(&dim, "minecraft:mannequin", pos, nbt(&name, profile(&prof, &name), look, &tags, true)) {
            run(|r| {
                if let Some(t) = r.temps.get_mut(&key) { t.entity = Some(e); }
                r.entities.insert(e, format!("t:{key}"));
            });
        }
    }
}
