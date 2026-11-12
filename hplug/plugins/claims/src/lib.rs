//! `claims`: native replacement for the land protection Jan's SMP builds from WorldGuard (the `spawn` region)
//! + GriefPrevention (personal base claims) + the SpawnClaimGuard plugin's rules. Policy as on the live server:
//!   a claim protects ONLY breaking and placing blocks (and explosions); chests, doors, buttons are open to all.
//!  - Personal claims: golden-shovel two-corner (right click the corners), full height, 5000 start blocks +
//!    100 per played hour, max 50000, max 3 claims, trust list, /abandonclaim.
//!  - Spawn region: nobody but members/ops builds; members come from `/claimadmin spawn add` or automatically
//!    after 2 h of non-AFK time (SpawnClaimGuard's rule).
//!  - Ops are never blocked. Everything is in `config/hplug/claims.json` (the plugin store).
//!
//! Not done (gaps): pistons, fire/liquid flow, bucket placing, entity damage in claims, claim visualisation,
//!   resize by shovel, the SpawnClaimGuard container sweep + boats + respawn-at-spawn (no spawn API).
#![allow(clippy::type_complexity)] // plugin-local helpers with many parts
use hplug::{Action, Ctx, Plugin, json};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
struct Claim {
    id: u32,
    owner: String,
    owner_name: String,
    dim: String,
    x1: i32,
    z1: i32,
    x2: i32,
    z2: i32,
    #[serde(default)]
    trusted: Vec<String>,
}
impl Claim {
    fn contains(&self, dim: &str, x: i32, z: i32) -> bool {
        self.dim == dim && x >= self.x1 && x <= self.x2 && z >= self.z1 && z <= self.z2
    }
    fn area(&self) -> i64 {
        (self.x2 - self.x1 + 1) as i64 * (self.z2 - self.z1 + 1) as i64
    }
    fn overlaps(&self, o: &Claim) -> bool {
        self.dim == o.dim && self.x1 <= o.x2 && o.x1 <= self.x2 && self.z1 <= o.z2 && o.z1 <= self.z2
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Spawn {
    dim: String,
    x1: i32,
    z1: i32,
    x2: i32,
    z2: i32,
    #[serde(default)]
    members: Vec<String>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct State {
    #[serde(default)]
    claims: Vec<Claim>,
    spawn: Option<Spawn>,
    #[serde(default)]
    next_id: u32,
    /// non-AFK seconds per player, and who already got spawn access that way
    #[serde(default)]
    afk_seconds: HashMap<String, u64>,
    #[serde(default)]
    granted: Vec<String>,
}
static STATE: Mutex<Option<State>> = Mutex::new(None);
static CORNERS: Mutex<Option<HashMap<u128, (String, i32, i32)>>> = Mutex::new(None);
static LAST_POS: Mutex<Option<HashMap<u128, [f64; 3]>>> = Mutex::new(None);

/// For other plugins (SpawnStone): may this player build at spawn (member of the spawn region, or an op)?
/// False while the claims state is not loaded yet.
pub fn has_spawn_access(uuid: u128) -> bool {
    let k = format!("{uuid:032x}");
    STATE.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|s| s.spawn.as_ref()).is_some_and(|sp| sp.members.contains(&k))
}

/// For other plugins: the spawn region `(dimension, x1, z1, x2, z2)` once claims state is loaded and a region is set.
pub fn spawn_box() -> Option<(String, i32, i32, i32, i32)> {
    STATE.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|s| s.spawn.as_ref()).map(|sp| (sp.dim.clone(), sp.x1, sp.z1, sp.x2, sp.z2))
}

fn key(uuid: u128) -> String {
    format!("{uuid:032x}")
}
fn with<R>(ctx: &Ctx, f: impl FnOnce(&mut State) -> R) -> R {
    let mut g = STATE.lock().unwrap();
    if g.is_none() {
        *g = Some(ctx.get("state").and_then(|v| json::from_value(v).ok()).unwrap_or_default());
    }
    f(g.as_mut().unwrap())
}
fn save(ctx: &mut Ctx) {
    let v = STATE.lock().unwrap().as_ref().map(|s| json::to_value(s).unwrap_or_default());
    if let Some(v) = v {
        let _ = ctx.set("state", v);
    }
}
fn num(ctx: &Ctx, k: &str, d: u64) -> u64 {
    ctx.get(k).and_then(|v| v.as_u64()).unwrap_or(d)
}

/// None = may build; Some(reason) = refused.
fn deny_reason(ctx: &Ctx, uuid: u128, dim: &str, x: i32, z: i32) -> Option<String> {
    if ctx.is_op(uuid) || ctx.try_player().is_some_and(|p| p.uuid == uuid && p.permission >= 2) {
        return None;
    }
    let me = key(uuid);
    with(ctx, |s| {
        if let Some(sp) = &s.spawn
            && sp.dim == dim && x >= sp.x1 && x <= sp.x2 && z >= sp.z1 && z <= sp.z2
        {
            return if sp.members.contains(&me) { None } else { Some("§c[Spawn] Only members can build at spawn (play 2 hours to unlock).".to_owned()) };
        }
        s.claims.iter().find(|c| c.contains(dim, x, z)).and_then(|c| {
            if c.owner == me || c.trusted.contains(&me) { None } else { Some(format!("§c[Claims] This land is claimed by {}.", c.owner_name)) }
        })
    })
}
fn protected_at(ctx: &Ctx, dim: &str, x: i32, z: i32) -> bool {
    with(ctx, |s| s.spawn.as_ref().is_some_and(|sp| sp.dim == dim && x >= sp.x1 && x <= sp.x2 && z >= sp.z1 && z <= sp.z2) || s.claims.iter().any(|c| c.contains(dim, x, z)))
}
fn total_blocks(ctx: &Ctx, owner: &str) -> i64 {
    let hours = playtime_hours(ctx, owner) as i64;
    let bonus = ctx.get("bonus").and_then(|b| b.get(owner).and_then(|v| v.as_i64())).unwrap_or(0); // imported GriefPrevention bonus blocks
    (num(ctx, "initial_blocks", 5000) as i64 + hours * num(ctx, "blocks_per_hour", 100) as i64).min(num(ctx, "max_blocks", 50000) as i64) + bonus
}
fn used_blocks(ctx: &Ctx, owner: &str) -> i64 {
    with(ctx, |s| s.claims.iter().filter(|c| c.owner == owner).map(Claim::area).sum())
}
fn playtime_hours(ctx: &Ctx, uuid32: &str) -> u64 {
    let dashed = format!("{}-{}-{}-{}-{}", &uuid32[..8], &uuid32[8..12], &uuid32[12..16], &uuid32[16..20], &uuid32[20..]);
    let Some(dir) = ctx.host.world_dir() else { return 0 };
    let Ok(t) = std::fs::read_to_string(dir.join("players/stats").join(format!("{dashed}.json"))) else { return 0 };
    json::from_str::<json::Value>(&t).ok().and_then(|v| v["stats"]["minecraft:custom"]["minecraft:play_time"].as_u64()).unwrap_or(0) / 72_000
}
fn interactable(block: &str) -> bool {
    ["door", "button", "lever", "chest", "barrel", "gate", "furnace", "smoker", "crafting_table", "shulker_box", "anvil", "bed", "hopper", "dispenser", "dropper", "lectern", "bell", "enchanting_table", "brewing_stand", "loom", "cartography_table", "grindstone", "smithing_table", "stonecutter", "note_block", "jukebox", "cauldron", "composter", "repeater", "comparator", "daylight_detector", "beacon"]
        .iter()
        .any(|k| block.contains(k))
}
fn resolve_target(ctx: &Ctx, name: &str) -> Result<(u128, String), String> {
    ctx.find_online(name).map(|p| (p.uuid, p.name)).or_else(|| ctx.uuid_for_name(name)).ok_or(format!("Unknown player {name}"))
}

pub fn plugin() -> Plugin {
    Plugin::new("claims")
        .on_block_break(|ctx, pos| {
            let Some(p) = ctx.try_player().cloned() else { return false };
            match deny_reason(ctx, p.uuid, &p.dimension, pos[0], pos[2]) {
                Some(r) => { ctx.tell(p.uuid, r); true }
                None => false,
            }
        })
        .on_action(|ctx, a| {
            let Some(p) = ctx.try_player().cloned() else { return false };
            // starting to dig (survival) is refused too, so the block does not even crack
            if let Action::LeftClickBlock { pos } = a {
                return match deny_reason(ctx, p.uuid, &p.dimension, pos[0], pos[2]) {
                    Some(r) => { ctx.tell(p.uuid, r); true }
                    None => false,
                };
            }
            let Action::UseItemOn { pos, face, item } = a else { return false };
            // golden shovel = claim tool (never digs a path)
            if item == "minecraft:golden_shovel" {
                shovel(ctx, &p, pos[0], pos[2]);
                return true;
            }
            if item == "minecraft:air" || ctx.host.block(*pos).is_empty() {
                return false;
            }
            // right click on something that opens/toggles is open to everybody (policy); anything else with an item may place
            if interactable(&ctx.host.block(*pos)) {
                return false;
            }
            let target = [pos[0] + face[0], pos[1] + face[1], pos[2] + face[2]];
            match deny_reason(ctx, p.uuid, &p.dimension, target[0], target[2]) {
                Some(r) => { ctx.tell(p.uuid, r); true }
                None => false,
            }
        })
        .on_explosion(|ctx, dim, _c, blocks| {
            blocks.retain(|b| !protected_at(ctx, dim, b[0], b[2]));
        })
        // 2 h of non-AFK time unlocks building at spawn (SpawnClaimGuard)
        .every(20, |ctx| {
            let need = num(ctx, "afk_threshold_secs", 7200);
            let mut newly = vec![];
            for p in ctx.online() {
                let moved = {
                    let mut g = LAST_POS.lock().unwrap();
                    let m = g.get_or_insert_with(Default::default);
                    let old = m.insert(p.uuid, p.position);
                    old.is_none_or(|o| (o[0] - p.position[0]).powi(2) + (o[1] - p.position[1]).powi(2) + (o[2] - p.position[2]).powi(2) > 0.01)
                };
                if !moved { continue; }
                let k = key(p.uuid);
                with(ctx, |s| {
                    let t = s.afk_seconds.entry(k.clone()).or_default();
                    *t += 1;
                    if *t >= need && !s.granted.contains(&k) {
                        s.granted.push(k.clone());
                        if let Some(sp) = &mut s.spawn && !sp.members.contains(&k) { sp.members.push(k.clone()); }
                        newly.push((p.uuid, p.name.clone()));
                    }
                });
            }
            for (u, n) in newly {
                ctx.tell(u, "§a[Claims] You've been playing for 2+ hours - you now have build access at spawn!");
                ctx.log(format!("{n} crossed the non-AFK threshold: spawn build access granted"));
                save(ctx);
            }
            if ctx.tick() % 1200 == 0 { save(ctx); }
        })
        .on_quit(|ctx| {
            if let Some(u) = ctx.player_uuid() {
                CORNERS.lock().unwrap().get_or_insert_with(Default::default).remove(&u);
                LAST_POS.lock().unwrap().get_or_insert_with(Default::default).remove(&u);
            }
            save(ctx);
        })
        .command("trust <player:word>", 0, |ctx, a| trust(ctx, a.get(0).cloned().unwrap_or_default(), true))
        .command("untrust <player:word>", 0, |ctx, a| trust(ctx, a.get(0).cloned().unwrap_or_default(), false))
        .command("abandonclaim", 0, |ctx, _| {
            let p = ctx.player().clone();
            let me = key(p.uuid);
            let at = (p.position[0].floor() as i32, p.position[2].floor() as i32);
            let removed = with(ctx, |s| {
                let i = s.claims.iter().position(|c| c.contains(&p.dimension, at.0, at.1))?;
                if s.claims[i].owner != me { return Some(Err("That claim is not yours.".to_owned())); }
                Some(Ok(s.claims.remove(i)))
            });
            match removed {
                None => Err("You are not standing in a claim.".into()),
                Some(Err(e)) => Err(e),
                Some(Ok(c)) => { save(ctx); ctx.reply(format!("§aClaim #{} abandoned. {} claim blocks left.", c.id, total_blocks(ctx, &me) - used_blocks(ctx, &me))); Ok(()) }
            }
        })
        .command("claiminfo", 0, |ctx, _| {
            let p = ctx.player().clone();
            let at = (p.position[0].floor() as i32, p.position[2].floor() as i32);
            let msg = with(ctx, |s| {
                if s.spawn.as_ref().is_some_and(|sp| sp.dim == p.dimension && at.0 >= sp.x1 && at.0 <= sp.x2 && at.1 >= sp.z1 && at.1 <= sp.z2) { return "§eYou are in the spawn region.".to_owned(); }
                match s.claims.iter().find(|c| c.contains(&p.dimension, at.0, at.1)) {
                    Some(c) => format!("§eClaim #{} owned by {} ({} blocks, {} trusted).", c.id, c.owner_name, c.area(), c.trusted.len()),
                    None => "§7Unclaimed land.".to_owned(),
                }
            });
            ctx.reply(msg);
            Ok(())
        })
        .command("claimblocks", 0, |ctx, _| {
            let me = key(ctx.player().uuid);
            let (t, u) = (total_blocks(ctx, &me), used_blocks(ctx, &me));
            ctx.reply(format!("§eClaim blocks: {u} used of {t} ({} left). Claims: {}/{}.", t - u, with(ctx, |s| s.claims.iter().filter(|c| c.owner == me).count()), num(ctx, "max_claims", 3)));
            Ok(())
        })
        // WorldGuard-compatible bridge for Ori's spawn-build approval: `rg addmember -w world spawn <name>`
        // (also `removemember`; `-w <world>` optional). Only the region `spawn` exists; the reply is WorldGuard's.
        .command("rg <sub:word> [a:word] [b:word] [c:word] [d:word]", 2, |ctx, args| {
            let sub = args.get(0).map(|s| s.to_lowercase()).unwrap_or_default();
            let add = match sub.as_str() {
                "addmember" | "addmem" | "am" => true,
                "removemember" | "removemem" | "remmember" | "rmm" => false,
                _ => return Err("Usage: /rg addmember|removemember [-w world] spawn <player>".into()),
            };
            let mut rest: Vec<String> = Vec::new();
            let mut i = 1;
            while let Some(a) = args.get(i) {
                if a == "-w" { i += 2; continue; } // the world flag and its value
                rest.push(a.clone());
                i += 1;
            }
            let (Some(region), Some(name)) = (rest.first(), rest.get(1)) else {
                return Err("Usage: /rg addmember|removemember [-w world] spawn <player>".into());
            };
            if !region.eq_ignore_ascii_case("spawn") {
                return Err(format!("Could not find a region by that ID: {region}"));
            }
            let n = spawn_member(ctx, add, name)?;
            ctx.reply(if add { format!("Added player {n} to region 'spawn'.") } else { format!("Removed player {n} from region 'spawn'.") });
            Ok(())
        })
        .command("claimadmin <sub:word> [a:word] [b:word] [c:word] [d:word] [e:word]", 2, |ctx, args| {
            let sub = args.get(0).map(|s| s.to_lowercase()).unwrap_or_default();
            match (sub.as_str(), args.get(1).map(|s| s.to_lowercase()).as_deref()) {
                ("spawn", Some("set")) => {
                    let n: Vec<i32> = (2..6).filter_map(|i| args.get(i)?.parse().ok()).collect();
                    if n.len() != 4 { return Err("Usage: /claimadmin spawn set <x1> <z1> <x2> <z2> [dimension]".into()); }
                    let dim = args.get(6).cloned().unwrap_or_else(|| ctx.try_player().map(|p| p.dimension.clone()).unwrap_or_else(|| "minecraft:overworld".into()));
                    with(ctx, |s| {
                        let members = s.spawn.take().map(|x| x.members).unwrap_or_default();
                        s.spawn = Some(Spawn { dim: dim.clone(), x1: n[0].min(n[2]), z1: n[1].min(n[3]), x2: n[0].max(n[2]), z2: n[1].max(n[3]), members });
                    });
                    save(ctx);
                    ctx.reply(format!("§aSpawn region set in {dim}."));
                }
                ("spawn", Some(op @ ("add" | "remove"))) => {
                    let n = spawn_member(ctx, op == "add", args.get(2).map(String::as_str).unwrap_or(""))?;
                    ctx.reply(format!("§a{n} {} spawn builders.", if op == "add" { "is now one of the" } else { "is no longer one of the" }));
                }
                ("delete", _) => {
                    let id: u32 = args.get(1).and_then(|s| s.parse().ok()).ok_or("Usage: /claimadmin delete <claim id>")?;
                    let n = with(ctx, |s| { let b = s.claims.len(); s.claims.retain(|c| c.id != id); b - s.claims.len() });
                    save(ctx);
                    ctx.reply(if n > 0 { format!("§aClaim #{id} deleted.") } else { format!("§cNo claim #{id}.") });
                }
                ("list", _) => {
                    let lines: Vec<String> = with(ctx, |s| s.claims.iter().map(|c| format!("#{} {} {} [{},{}]-[{},{}] trusted {}", c.id, c.owner_name, c.dim, c.x1, c.z1, c.x2, c.z2, c.trusted.len())).collect());
                    ctx.reply(if lines.is_empty() { "No claims.".into() } else { lines.join("\n") });
                }
                _ => return Err("Usage: /claimadmin spawn set|add|remove ... | delete <id> | list".into()),
            }
            Ok(())
        })
}

/// Add/remove a player to the spawn region's builders (shared by /claimadmin spawn add|remove and /rg); the player's name.
fn spawn_member(ctx: &mut Ctx, add: bool, who: &str) -> Result<String, String> {
    let (u, n) = resolve_target(ctx, who)?;
    let k = key(u);
    let ok = with(ctx, |s| s.spawn.as_mut().map(|sp| { sp.members.retain(|m| *m != k); if add { sp.members.push(k.clone()); } }).is_some());
    if !ok { return Err("No spawn region: /claimadmin spawn set first.".into()); }
    save(ctx);
    Ok(n)
}

fn trust(ctx: &mut Ctx, name: String, add: bool) -> Result<(), String> {
    let me = key(ctx.player().uuid);
    let (u, n) = resolve_target(ctx, &name)?;
    let k = key(u);
    let changed = with(ctx, |s| {
        let mut any = false;
        for c in s.claims.iter_mut().filter(|c| c.owner == me) {
            any = true;
            c.trusted.retain(|t| *t != k);
            if add { c.trusted.push(k.clone()); }
        }
        any
    });
    if !changed { return Err("You have no claims.".into()); }
    save(ctx);
    ctx.reply(format!("§a{n} {} trusted in all your claims.", if add { "is now" } else { "is no longer" }));
    Ok(())
}

fn shovel(ctx: &mut Ctx, p: &hplug::Player, x: i32, z: i32) {
    let me = key(p.uuid);
    let first = CORNERS.lock().unwrap().get_or_insert_with(Default::default).remove(&p.uuid);
    let Some((dim, x0, z0)) = first.filter(|(d, ..)| *d == p.dimension) else {
        CORNERS.lock().unwrap().get_or_insert_with(Default::default).insert(p.uuid, (p.dimension.clone(), x, z));
        ctx.tell(p.uuid, format!("§eFirst corner set at {x}, {z}. Right-click the opposite corner with the golden shovel."));
        return;
    };
    let new = Claim { id: 0, owner: me.clone(), owner_name: p.name.clone(), dim, x1: x0.min(x), z1: z0.min(z), x2: x0.max(x), z2: z0.max(z), trusted: vec![] };
    let min = num(ctx, "min_size", 5) as i32;
    if new.x2 - new.x1 + 1 < min || new.z2 - new.z1 + 1 < min {
        ctx.tell(p.uuid, format!("§cA claim must be at least {min}x{min}."));
        return;
    }
    let (total, used) = (total_blocks(ctx, &me), used_blocks(ctx, &me));
    let count = with(ctx, |s| s.claims.iter().filter(|c| c.owner == me).count()) as u64;
    if count >= num(ctx, "max_claims", 3) && !ctx.is_op(p.uuid) {
        ctx.tell(p.uuid, "§cYou have reached the maximum number of claims. /abandonclaim one first.");
        return;
    }
    if new.area() > total - used && !ctx.is_op(p.uuid) {
        ctx.tell(p.uuid, format!("§cThat claim needs {} blocks but you only have {} left.", new.area(), total - used));
        return;
    }
    let clash = with(ctx, |s| {
        s.claims.iter().find(|c| c.overlaps(&new)).map(|c| format!("§cThat overlaps {}'s claim.", c.owner_name))
            .or_else(|| s.spawn.as_ref().filter(|sp| sp.dim == new.dim && new.x1 <= sp.x2 && sp.x1 <= new.x2 && new.z1 <= sp.z2 && sp.z1 <= new.z2).map(|_| "§cThat overlaps the spawn region.".to_owned()))
    });
    if let Some(m) = clash {
        ctx.tell(p.uuid, m);
        return;
    }
    let area = new.area();
    with(ctx, |s| { s.next_id += 1; let mut c = new; c.id = s.next_id; s.claims.push(c); });
    save(ctx);
    ctx.tell(p.uuid, format!("§aClaim created: {area} blocks. {} claim blocks left. /trust <player> lets others build.", total - used - area));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The file `tools/import_claims.py` writes for tools/testdata/gp must load as this plugin's state.
    #[test]
    fn imported_griefprevention_data_loads() {
        let store: json::Value = json::from_str(include_str!("../testdata/imported.json")).unwrap();
        let state: State = json::from_value(store["state"].clone()).unwrap();
        assert_eq!(state.claims.len(), 1);
        let c = &state.claims[0];
        assert!(c.contains("minecraft:overworld", 140, 320));
        assert!(!c.contains("minecraft:overworld", 119, 320));
        assert_eq!(c.area(), 41 * 41);
        assert_eq!(c.trusted, vec!["99999999222233334444555555555555".to_owned()]);
        let sp = state.spawn.unwrap();
        assert_eq!((sp.x1, sp.z1, sp.x2, sp.z2), (-100, -80, 100, 90));
        assert_eq!(sp.members.len(), 2);
        assert_eq!(store["bonus"]["11111111222233334444555555555555"], 1500);
    }
}
