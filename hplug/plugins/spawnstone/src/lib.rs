//! Port of spawn-stone (Paper, 2520 lines): spawn <-> base fast travel with placeable "spawn stones".
//! One community stone (admin), one personal stone per player (owner or ONE membership), 100-block spacing,
//! pickup (10 blocks, 1 min cooldown) / relocate (7 days), members (removed members wait 2 days), eligibility =
//! spawn build access (`hplug_plugin_claims::has_spawn_access`) or op. Stones are `beehive[facing=west,honey_level=0]`
//! blocks, protected from breaking. Right click: 3 s in a void limbo (nausea, frozen), then arrival.
//! Decoy (06/11, was a gap): on departure a mannequin with the traveller's skin stays where they stood (plugin
//! `npcs`, Citizens replacement) for `decoy_secs` (10); if it is hit in that window the traveller is pulled back
//! to that spot and takes the hit's damage (`damage` command), as the Citizens decoy did. Config `decoy` (true).
//! NOT ported: the admin menu GUI, Citizens/claim-bypass listeners. A player who logs out in limbo is sent to the destination on join.
use hplug::{Ctx, Plugin, json, now_ms};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;

const MIN_DISTANCE: f64 = 100.0;
const PICKUP_MAX: f64 = 10.0;
const PICKUP_CD: u64 = 60_000;
const RELOCATE_CD: u64 = 7 * 86_400_000;
const REMOVED_CD: u64 = 2 * 86_400_000;
const LIMBO_TICKS: u64 = 60;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
struct Pos {
    dim: String,
    x: i32,
    y: i32,
    z: i32,
}
impl Pos {
    fn dist(&self, o: &Pos) -> f64 {
        if self.dim != o.dim {
            return f64::MAX;
        }
        (((self.x - o.x).pow(2) + (self.y - o.y).pow(2) + (self.z - o.z).pow(2)) as f64).sqrt()
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Stone {
    pos: Pos,
    #[serde(default)]
    members: Vec<String>,
    #[serde(default)]
    last_pickup: u64,
    #[serde(default)]
    last_relocate: u64,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct State {
    spawn: Option<Pos>,
    #[serde(default)]
    stones: HashMap<String, Stone>,
    /// removed members: uuid -> may be added again after (ms)
    #[serde(default)]
    cooldown: HashMap<String, u64>,
    /// players in limbo: uuid -> destination
    #[serde(default)]
    transit: HashMap<String, Pos>,
}
static STATE: Mutex<Option<State>> = Mutex::new(None);

fn k(u: u128) -> String {
    format!("{u:032x}")
}
fn with<R>(ctx: &Ctx, f: impl FnOnce(&mut State) -> R) -> R {
    let mut g = STATE.lock().unwrap_or_else(|e| e.into_inner());
    if g.is_none() {
        *g = Some(
            ctx.get("state")
                .and_then(|v| json::from_value(v).ok())
                .unwrap_or_default(),
        );
    }
    f(g.as_mut().unwrap())
}
fn save(ctx: &mut Ctx) {
    let v = STATE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .map(|s| json::to_value(s).unwrap_or_default());
    if let Some(v) = v {
        let _ = ctx.set("state", v);
    }
}
fn here(ctx: &Ctx) -> Pos {
    let p = ctx.player();
    Pos {
        dim: p.dimension.clone(),
        x: p.position[0].floor() as i32,
        y: p.position[1].floor() as i32,
        z: p.position[2].floor() as i32,
    }
}
fn block_cmd(ctx: &mut Ctx, p: &Pos, state: &str) {
    ctx.console(format!(
        "execute in {} run setblock {} {} {} {}",
        p.dim, p.x, p.y, p.z, state
    ));
}
fn put_stone(ctx: &mut Ctx, p: &Pos) {
    block_cmd(ctx, p, "minecraft:beehive[facing=west,honey_level=0]");
}
fn remove_stone(ctx: &mut Ctx, p: &Pos) {
    block_cmd(ctx, p, "minecraft:air");
}
fn eligible(ctx: &Ctx) -> bool {
    let p = ctx.player();
    ctx.has_perm(p.uuid, &p.name, "spawnstone.use") || hplug_plugin_claims::has_spawn_access(p.uuid)
}
/// Why `p` is too close to another stone, if it is; `except` is the owner moving their own stone.
fn spacing_problem(ctx: &Ctx, p: &Pos, except: Option<&str>) -> Option<String> {
    with(ctx, |s| {
        if s.spawn.as_ref().is_some_and(|sp| sp.dist(p) < MIN_DISTANCE) {
            return Some("That is within 100 blocks of the spawn stone.".to_owned());
        }
        s.stones
            .iter()
            .filter(|(o, _)| Some(o.as_str()) != except)
            .find(|(_, st)| st.pos.dist(p) < MIN_DISTANCE)
            .map(|_| "That is within 100 blocks of another player's stone.".to_owned())
    })
}
fn relation(ctx: &Ctx, me: &str) -> (Option<Stone>, Option<String>) {
    with(ctx, |s| {
        (
            s.stones.get(me).cloned(),
            s.stones
                .iter()
                .find(|(_, st)| st.members.iter().any(|m| m == me))
                .map(|(o, _)| o.clone()),
        )
    })
}
fn resolve(ctx: &Ctx, name: &str) -> Result<(u128, String), String> {
    ctx.find_online(name)
        .map(|p| (p.uuid, p.name))
        .or_else(|| ctx.uuid_for_name(name))
        .ok_or(format!("Unknown player {name}"))
}

/// Decoys out now: traveller uuid -> (origin dimension, position, look, expires at tick).
type Decoy = (u128, String, [f64; 3], [f32; 2], u64);
static DECOYS: Mutex<Vec<Decoy>> = Mutex::new(Vec::new());

fn spawn_decoy(ctx: &mut Ctx, uuid: u128, name: &str) {
    if !ctx.get("decoy").and_then(|v| v.as_bool()).unwrap_or(true) {
        return;
    }
    let Some(p) = ctx.find_online(name) else {
        return;
    };
    let secs = ctx.get("decoy_secs").and_then(|v| v.as_u64()).unwrap_or(10);
    let until = ctx.tick() + secs * 20;
    DECOYS.lock().unwrap_or_else(|e| e.into_inner()).push((
        uuid,
        p.dimension.clone(),
        p.position,
        [p.yaw, p.pitch],
        until,
    ));
    let (n, dim, pos, look) = (
        p.name.clone(),
        p.dimension.clone(),
        p.position,
        [p.yaw, p.pitch],
    );
    // the skin helper is refused inside a player's hook: next tick
    ctx.run_later(1, move |c| {
        let skin = c.host.player_textures(uuid);
        hplug_plugin_npcs::spawn_temp(&format!("decoy:{uuid:032x}"), &n, skin, &dim, pos, look);
    });
}

fn travel(ctx: &mut Ctx, uuid: u128, name: &str, from: &Pos, to: Pos) {
    spawn_decoy(ctx, uuid, name);
    let limbo = ctx
        .get("limbo")
        .and_then(|v| {
            v.as_array()
                .map(|a| a.iter().filter_map(|x| x.as_f64()).collect::<Vec<_>>())
        })
        .filter(|v| v.len() == 3)
        .unwrap_or(vec![0.5, 300.0, 0.5]);
    with(ctx, |s| {
        s.transit.insert(k(uuid), to.clone());
    });
    save(ctx);
    ctx.console(format!(
        "execute in {} run tp {name} {} {} {}",
        from.dim, limbo[0], limbo[1], limbo[2]
    ));
    for e in [
        "minecraft:slowness 8 255 true",
        "minecraft:jump_boost 8 128 true",
        "minecraft:nausea 6 0 true",
        "minecraft:slow_falling 8 0 true",
    ] {
        ctx.console(format!("effect give {name} {e}"));
    }
    ctx.tell(uuid, "§dThe stone hums... hold still.");
    let n = name.to_owned();
    ctx.run_later(LIMBO_TICKS, move |c| arrive(c, uuid, &n));
}
fn arrive(ctx: &mut Ctx, uuid: u128, name: &str) {
    let dest = with(ctx, |s| s.transit.remove(&k(uuid)));
    save(ctx);
    let Some(d) = dest else { return };
    ctx.console(format!(
        "execute in {} run tp {name} {}.5 {} {}.5",
        d.dim,
        d.x,
        d.y + 1,
        d.z
    ));
    ctx.console(format!("effect clear {name} minecraft:slowness"));
    ctx.console(format!("effect clear {name} minecraft:jump_boost"));
    ctx.console(format!("effect clear {name} minecraft:nausea"));
    ctx.console(format!("effect clear {name} minecraft:slow_falling"));
}

pub fn plugin() -> Plugin {
    Plugin::new("spawnstone")
        // decoys: expire, or pull the traveller back when hit
        .every(2, |ctx| {
            let now = ctx.tick();
            let hits = hplug_plugin_npcs::take_hits();
            let expired: Vec<u128> = {
                let mut d = DECOYS.lock().unwrap_or_else(|e| e.into_inner());
                let gone: Vec<u128> = d.iter().filter(|x| x.4 <= now).map(|x| x.0).collect();
                d.retain(|x| x.4 > now);
                gone
            };
            for u in expired {
                hplug_plugin_npcs::remove_temp(ctx, &format!("decoy:{u:032x}"));
            }
            for (key, dmg, _attacker) in hits {
                let Some(u) = key.strip_prefix("decoy:").and_then(|h| u128::from_str_radix(h, 16).ok()) else { continue };
                let hit = {
                    let mut d = DECOYS.lock().unwrap_or_else(|e| e.into_inner());
                    let i = d.iter().position(|x| x.0 == u);
                    i.map(|i| d.remove(i))
                };
                let Some((_, dim, pos, look, _)) = hit else { continue };
                hplug_plugin_npcs::remove_temp(ctx, &key);
                let Some(p) = ctx.online().into_iter().find(|o| o.uuid == u) else { continue };
                // cancel a pending arrival, then back to where the decoy stood, taking the hit
                with(ctx, |s| { s.transit.remove(&k(u)); });
                save(ctx);
                for e in ["slowness", "jump_boost", "nausea", "slow_falling"] {
                    ctx.console(format!("effect clear {} minecraft:{e}", p.name));
                }
                ctx.teleport_to(&p.name, &dim, pos, Some(look));
                ctx.console(format!("damage {} {:.1} minecraft:generic", p.name, dmg.max(0.5)));
                ctx.tell(u, "§cYour decoy was hit! You were pulled back.");
            }
        })
        .on_join(|ctx| {
            // restart/log-out mid-teleport: never strand a player in limbo
            let p = ctx.player().clone();
            if with(ctx, |s| s.transit.contains_key(&k(p.uuid))) { arrive(ctx, p.uuid, &p.name); }
        })
        .on_interact(|ctx, pos, _face| {
            let Some(p) = ctx.try_player().cloned() else { return false };
            let at = Pos { dim: p.dimension.clone(), x: pos[0], y: pos[1], z: pos[2] };
            // which stone, if any, is this
            let hit = with(ctx, |s| {
                if s.spawn.as_ref() == Some(&at) { return Some(None); }
                s.stones.iter().find(|(_, st)| st.pos == at).map(|(o, _)| Some(o.clone()))
            });
            let Some(owner) = hit else { return false };
            if !eligible(ctx) { ctx.tell(p.uuid, "§cYou need spawn build access to use stones (play 2+ hours)."); return true; }
            if with(ctx, |s| s.transit.contains_key(&k(p.uuid))) { return true; }
            let me = k(p.uuid);
            let (own, member_of) = relation(ctx, &me);
            match owner {
                // spawn stone: go to your own / your linked base
                None => {
                    let base = own.map(|st| st.pos).or_else(|| member_of.and_then(|o| with(ctx, |s| s.stones.get(&o).map(|st| st.pos.clone()))));
                    match base {
                        Some(b) => travel(ctx, p.uuid, &p.name, &at, b),
                        None => { ctx.tell(p.uuid, "§cYou have no stone yet. Place one with /spawnstone place."); }
                    }
                }
                // a personal stone: owner or member goes to spawn
                Some(o) => {
                    if o != me && !member_of.is_some_and(|m| m == o) { ctx.tell(p.uuid, "§cThis is not your stone."); return true; }
                    match with(ctx, |s| s.spawn.clone()) {
                        Some(sp) => travel(ctx, p.uuid, &p.name, &at, sp),
                        None => { ctx.tell(p.uuid, "§cNo spawn stone has been placed yet."); }
                    }
                }
            }
            true
        })
        .on_block_break(|ctx, pos| {
            let Some(p) = ctx.try_player().cloned() else { return false };
            let at = Pos { dim: p.dimension, x: pos[0], y: pos[1], z: pos[2] };
            let is_stone = with(ctx, |s| s.spawn.as_ref() == Some(&at) || s.stones.values().any(|st| st.pos == at));
            if is_stone { ctx.tell(p.uuid, "§cStones can't be broken. Use /spawnstone pickup."); }
            is_stone
        })
        .command("spawnstone <sub:word> [player:word]", 0, |ctx, args| {
            let p = ctx.player().clone();
            let me = k(p.uuid);
            let sub = args.get(0).map(|s| s.to_lowercase()).unwrap_or_default();
            let (own, member_of) = relation(ctx, &me);
            let now = now_ms();
            match sub.as_str() {
                "place" => {
                    if !eligible(ctx) { return Err("You need spawn build access (2+ hours playtime) to place a stone.".into()); }
                    if own.is_some() || member_of.is_some() { return Err("You already own a stone or are a member of one.".into()); }
                    let at = here(ctx);
                    if let Some(m) = spacing_problem(ctx, &at, None) { return Err(m); }
                    with(ctx, |s| { s.stones.insert(me.clone(), Stone { pos: at.clone(), ..Default::default() }); });
                    put_stone(ctx, &at);
                    save(ctx);
                    ctx.reply("§aYour spawn stone is placed. Right-click it to travel to spawn.");
                }
                "pickup" | "relocate" => {
                    let Some(st) = own else { return Err("You don't own a stone.".into()) };
                    let at = here(ctx);
                    let (cd, last, maxd) = if sub == "pickup" { (PICKUP_CD, st.last_pickup, PICKUP_MAX) } else { (RELOCATE_CD, st.last_relocate, f64::MAX) };
                    if now < last + cd { return Err(format!("Wait {} more seconds before that.", (last + cd - now) / 1000)); }
                    if st.pos.dist(&at) > maxd { return Err("A pickup moves the stone at most 10 blocks. Use /spawnstone relocate for more.".into()); }
                    if let Some(m) = spacing_problem(ctx, &at, Some(&me)) { return Err(m); }
                    remove_stone(ctx, &st.pos);
                    put_stone(ctx, &at);
                    with(ctx, |s| { if let Some(x) = s.stones.get_mut(&me) { x.pos = at.clone(); if sub == "pickup" { x.last_pickup = now } else { x.last_relocate = now } } });
                    save(ctx);
                    ctx.reply("§aStone moved.");
                }
                "addmember" => {
                    let Some(st) = own else { return Err("You don't own a stone.".into()) };
                    let (u, n) = resolve(ctx, args.get(1).map(String::as_str).unwrap_or(""))?;
                    let t = k(u);
                    let (t_own, t_mem) = relation(ctx, &t);
                    if t_own.is_some() || t_mem.is_some() { return Err(format!("{n} already owns a stone or is a member of one.")); }
                    if with(ctx, |s| s.cooldown.get(&t).is_some_and(|&c| c > now)) { return Err(format!("{n} was removed from a stone recently and can't be added for 2 days.")); }
                    let _ = st;
                    with(ctx, |s| { if let Some(x) = s.stones.get_mut(&me) { x.members.push(t.clone()); } });
                    save(ctx);
                    ctx.reply(format!("§a{n} is now a member of your stone."));
                }
                "removemember" => {
                    if own.is_none() { return Err("You don't own a stone.".into()); }
                    let (u, n) = resolve(ctx, args.get(1).map(String::as_str).unwrap_or(""))?;
                    let t = k(u);
                    let was = with(ctx, |s| { let st = s.stones.get_mut(&me).unwrap(); let b = st.members.len(); st.members.retain(|m| *m != t); let r = b != st.members.len(); if r { s.cooldown.insert(t.clone(), now + REMOVED_CD); } r });
                    if !was { return Err(format!("{n} is not a member of your stone.")); }
                    save(ctx);
                    ctx.reply(format!("§a{n} removed. They can't join any stone for 2 days."));
                }
                "info" => {
                    let msg = match (own, member_of) {
                        (Some(st), _) => format!("§eYour stone is at {} {} {} with {} member(s).", st.pos.x, st.pos.y, st.pos.z, st.members.len()),
                        (None, Some(o)) => format!("§eYou are a member of {}'s stone.", with(ctx, |s| s.stones.get(&o).map(|_| o.clone()).unwrap_or_default()).get(..8).unwrap_or("")),
                        _ => "§7You have no stone.".to_owned(),
                    };
                    ctx.reply(msg);
                }
                _ => return Err("Usage: /spawnstone place|pickup|relocate|addmember <p>|removemember <p>|info".into()),
            }
            Ok(())
        })
        .command("spawnstoneadmin <sub:word> [player:word]", 2, |ctx, args| {
            let sub = args.get(0).map(|s| s.to_lowercase()).unwrap_or_default();
            match sub.as_str() {
                "setspawn" => {
                    let at = here(ctx);
                    let old = with(ctx, |s| s.spawn.replace(at.clone()));
                    if let Some(o) = old { remove_stone(ctx, &o); }
                    put_stone(ctx, &at);
                    save(ctx);
                    ctx.reply("§aSpawn stone placed here.");
                }
                "setplayerstone" => {
                    let (u, n) = resolve(ctx, args.get(1).map(String::as_str).unwrap_or(""))?;
                    let at = here(ctx);
                    if let Some(m) = spacing_problem(ctx, &at, Some(&k(u))) { return Err(m); }
                    let old = with(ctx, |s| { let e = s.stones.entry(k(u)).or_default(); let o = e.pos.clone(); e.pos = at.clone(); o });
                    if old != Pos::default() { remove_stone(ctx, &old); }
                    put_stone(ctx, &at);
                    save(ctx);
                    ctx.reply(format!("§aStone of {n} placed here."));
                }
                "removeplayerstone" => {
                    let (u, n) = resolve(ctx, args.get(1).map(String::as_str).unwrap_or(""))?;
                    let removed = with(ctx, |s| s.stones.remove(&k(u)));
                    match removed {
                        Some(st) => { remove_stone(ctx, &st.pos); save(ctx); ctx.reply(format!("§aStone of {n} removed; its members are free.")); }
                        None => return Err(format!("{n} has no stone.")),
                    }
                }
                _ => return Err("Usage: /spawnstoneadmin setspawn|setplayerstone <p>|removeplayerstone <p>".into()),
            }
            Ok(())
        })
}
