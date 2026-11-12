//! HMCCosmetics / UltraCosmetics replacement (the parts the SMP sold through the web shop): wearable cosmetics
//! drawn with server-side `item_display` entities that follow the wearer, and particle trails.
//!
//! * Types: `hat` (on the head, turns with the head; hidden from the wearer so it never blocks the first-person
//!   view), `backpack` (on the back, turns with the player), `balloon` (floats above a shoulder, bobbing),
//!   `particle` (a trail around the player every `particle_ticks`, 5).
//! * Ownership = the permission nodes the shop already grants: `hmccosmetics.<id>` (rewards' web shop runs
//!   `lp user <uuid> permission set hmccosmetics.<id> true`, kept by the `perms` plugin), or `cosmetics.<id>`,
//!   `cosmetics.*`; the shop's `cosmetics-tier<N>` groups work when permissions.json gives them those nodes.
//!   `require_permission: false` lets everyone wear everything (events).
//! * Commands (`/cosmetics`, aliases `/cosmetic`, `/wardrobe`): `list`, `equip <id>`, `unequip [type|all]`,
//!   `info`; staff (op 2): `define <id> <hat|backpack|balloon|particle> <item or particle> [item_model]`,
//!   `undefine <id>`, `give <player> <id>`, `take <player> <id>`, `clearall <player>`.
//! * Config `config/hplug/cosmetics.json`: `cosmetics{id: {type, item, item_model?, custom_model_data?, name?,
//!   particle?, count?, scale?, offset?: [x,y,z]}}`, `equipped{dashed-uuid: {type: id}}`.
//!   Migration of the HMCCosmetics definitions: tools/migrate_hmccosmetics.py.
//! * The models come from the server resource pack, as on Paper: serve the same pack (`resource-pack` in
//!   server.toml [properties]) or the items show as their base item.
//!
//! DIFFERENCES from HMCCosmetics (no engine API for per-viewer fake equipment): a hat is a separate entity, not the
//! helmet slot, so it sits over a real helmet and lags a tick behind fast head turns (`teleport_duration` 1
//! smooths it); the wearer does not see their own hat (also in F5); no wardrobe GUI/preview mannequin, no emotes,
//! no dyeable cosmetics, no offhand cosmetics, no balloon lead. Hidden while the wearer is a spectator, vanished
//! (`vanish` plugin) or dead. Entities are this session's (tags `hpcos`, `hpcoss_<n>`): leftovers of an earlier
//! run that come back with their chunks are removed.
use horizon_world::nbt::{Compound, JavaString, Tag};
use hplug::{Args, CmdResult, Ctx, Plugin, json};
use std::collections::HashMap;
use std::sync::Mutex;

const TYPES: [&str; 4] = ["hat", "backpack", "balloon", "particle"];

#[derive(Clone, Debug)]
struct Cosmetic {
    kind: String,
    item: String,
    item_model: Option<String>,
    custom_model_data: Option<f32>,
    name: String,
    count: i32,
    scale: f32,
    offset: [f64; 3],
}

fn cosmetic(id: &str, v: &json::Value) -> Option<Cosmetic> {
    let kind = v.get("type")?.as_str()?.to_lowercase();
    if !TYPES.contains(&kind.as_str()) {
        return None;
    }
    let default_scale = match kind.as_str() {
        "hat" => 0.625,
        "backpack" => 0.6,
        _ => 0.8,
    };
    Some(Cosmetic {
        item: v
            .get("item")
            .or_else(|| v.get("particle"))
            .and_then(|x| x.as_str())
            .unwrap_or("minecraft:paper")
            .to_owned(),
        item_model: v
            .get("item_model")
            .and_then(|x| x.as_str())
            .map(str::to_owned),
        custom_model_data: v
            .get("custom_model_data")
            .and_then(|x| x.as_f64())
            .map(|f| f as f32),
        name: v
            .get("name")
            .and_then(|x| x.as_str())
            .unwrap_or(id)
            .to_owned(),
        count: v.get("count").and_then(|x| x.as_i64()).unwrap_or(3) as i32,
        scale: v
            .get("scale")
            .and_then(|x| x.as_f64())
            .map(|f| f as f32)
            .unwrap_or(default_scale),
        offset: v
            .get("offset")
            .and_then(|o| o.as_array())
            .filter(|a| a.len() == 3)
            .map(|a| {
                [
                    a[0].as_f64().unwrap_or(0.0),
                    a[1].as_f64().unwrap_or(0.0),
                    a[2].as_f64().unwrap_or(0.0),
                ]
            })
            .unwrap_or([0.0; 3]),
        kind,
    })
}

fn all(ctx: &Ctx) -> HashMap<String, Cosmetic> {
    ctx.get("cosmetics")
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|(k, v)| cosmetic(k, v).map(|c| (k.to_lowercase(), c)))
        .collect()
}
fn equipped(ctx: &Ctx, uuid: u128) -> HashMap<String, String> {
    ctx.get("equipped")
        .and_then(|e| {
            e.get(hplug::uuid_dashed(uuid))
                .and_then(|x| x.as_object().cloned())
        })
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(k, v)| v.as_str().map(|s| (k, s.to_owned())))
        .collect()
}
fn set_equipped(ctx: &mut Ctx, uuid: u128, slot: &str, id: Option<&str>) -> Result<(), String> {
    let mut e = ctx
        .get("equipped")
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    let key = hplug::uuid_dashed(uuid);
    let mut mine = e
        .get(&key)
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    match id {
        Some(i) => {
            mine.insert(slot.to_owned(), json::json!(i));
        }
        None => {
            mine.remove(slot);
        }
    }
    if mine.is_empty() {
        e.remove(&key);
    } else {
        e.insert(key, json::Value::Object(mine));
    }
    ctx.set("equipped", json::Value::Object(e))
}
fn owns(ctx: &Ctx, uuid: u128, name: &str, id: &str) -> bool {
    !ctx.get("require_permission")
        .and_then(|v| v.as_bool())
        .unwrap_or(true)
        || ctx.has_perm(uuid, name, &format!("hmccosmetics.{id}"))
        || ctx.has_perm(uuid, name, &format!("cosmetics.{id}"))
}

/// Shown entities: (wearer, type) -> (dimension, entity id, cosmetic id).
#[derive(Default)]
struct Run {
    session: u64,
    shown: HashMap<(u128, String), (String, u32, String)>,
    /// Vanished players (vanish plugin), read once a second.
    vanished: Vec<u128>,
}
static RUN: Mutex<Option<Run>> = Mutex::new(None);
fn run<R>(f: impl FnOnce(&mut Run) -> R) -> R {
    let mut g = RUN.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(|| Run {
        session: hplug::now_ms() % 1_000_000_007,
        ..Default::default()
    }))
}

fn s(v: &str) -> Tag {
    Tag::String(JavaString::from(v))
}
fn f(v: f32) -> Tag {
    Tag::Float(v.to_bits())
}

fn display_nbt(c: &Cosmetic, look: [f32; 2], tags: &[String]) -> Compound {
    let mut components = Compound::new();
    if let Some(m) = &c.item_model {
        components.insert(JavaString::from("minecraft:item_model"), s(m));
    }
    if let Some(d) = c.custom_model_data {
        components.insert(
            JavaString::from("minecraft:custom_model_data"),
            Tag::compound([("floats", Tag::List(vec![f(d)]))]),
        );
    }
    let mut item = Compound::new();
    item.insert(JavaString::from("id"), s(&c.item));
    item.insert(JavaString::from("count"), Tag::Int(1));
    if !components.is_empty() {
        item.insert(JavaString::from("components"), Tag::Compound(components));
    }
    let mut n = Compound::new();
    n.insert(JavaString::from("item"), Tag::Compound(item));
    n.insert(
        JavaString::from("item_display"),
        s(if c.kind == "hat" { "head" } else { "fixed" }),
    );
    n.insert(JavaString::from("teleport_duration"), Tag::Int(1));
    n.insert(JavaString::from("view_range"), f(0.6));
    n.insert(
        JavaString::from("Rotation"),
        Tag::List(vec![f(look[0]), f(look[1])]),
    );
    n.insert(
        JavaString::from("transformation"),
        Tag::compound([
            (
                "left_rotation",
                Tag::List(vec![f(0.0), f(0.0), f(0.0), f(1.0)]),
            ),
            (
                "right_rotation",
                Tag::List(vec![f(0.0), f(0.0), f(0.0), f(1.0)]),
            ),
            ("translation", Tag::List(vec![f(0.0), f(0.0), f(0.0)])),
            ("scale", Tag::List(vec![f(c.scale), f(c.scale), f(c.scale)])),
        ]),
    );
    n.insert(
        JavaString::from("Tags"),
        Tag::List(tags.iter().map(|t| s(t)).collect()),
    );
    n
}

/// Where a cosmetic sits for a player at `feet` looking `look`, and the look it gets.
fn placement(c: &Cosmetic, feet: [f64; 3], look: [f32; 2], tick: u64) -> ([f64; 3], [f32; 2]) {
    let yaw = (look[0] as f64).to_radians();
    let fwd = [-yaw.sin(), yaw.cos()];
    let right = [-fwd[1], fwd[0]];
    let o = c.offset;
    let (base, l) = match c.kind.as_str() {
        // eye height of a standing player; the "head" display transform puts the model around that point
        "hat" => ([feet[0], feet[1] + 1.62, feet[2]], look),
        "backpack" => (
            [
                feet[0] - fwd[0] * 0.3,
                feet[1] + 1.05,
                feet[2] - fwd[1] * 0.3,
            ],
            [look[0] + 180.0, 0.0],
        ),
        _ => {
            let bob = ((tick % 80) as f64 / 80.0 * std::f64::consts::TAU).sin() * 0.08;
            (
                [
                    feet[0] + right[0] * 0.7 - fwd[0] * 0.3,
                    feet[1] + 2.6 + bob,
                    feet[2] + right[1] * 0.7 - fwd[1] * 0.3,
                ],
                [look[0], 0.0],
            )
        }
    };
    // offset is in the player's frame: x = right, y = up, z = forward
    (
        [
            base[0] + right[0] * o[0] + fwd[0] * o[2],
            base[1] + o[1],
            base[2] + right[1] * o[0] + fwd[1] * o[2],
        ],
        l,
    )
}

fn despawn(ctx: &mut Ctx, uuid: u128, slot: Option<&str>) {
    let gone: Vec<(String, u32)> = run(|r| {
        let keys: Vec<_> = r
            .shown
            .keys()
            .filter(|(u, s)| *u == uuid && slot.is_none_or(|x| x == s))
            .cloned()
            .collect();
        keys.into_iter()
            .filter_map(|k| r.shown.remove(&k))
            .map(|(d, e, _)| (d, e))
            .collect()
    });
    for (d, e) in gone {
        ctx.host.remove_entity(&d, e);
    }
}

/// One tick: spawn, move or remove every wearer's displays.
fn tick(ctx: &mut Ctx) {
    let defs = all(ctx);
    let t = ctx.tick();
    let session = run(|r| r.session);
    let online = ctx.online();
    // players who left: their entities go
    let gone: Vec<u128> = run(|r| {
        r.shown
            .keys()
            .map(|k| k.0)
            .filter(|u| !online.iter().any(|p| p.uuid == *u))
            .collect()
    });
    for u in gone {
        despawn(ctx, u, None);
    }
    if t.is_multiple_of(20) {
        let v: Vec<u128> = online
            .iter()
            .filter(|p| hplug_plugin_vanish::is_vanished(ctx, p.uuid))
            .map(|p| p.uuid)
            .collect();
        run(|r| r.vanished = v);
    }
    for p in &online {
        let hidden = p.game_mode == 3 || p.health <= 0.0 || run(|r| r.vanished.contains(&p.uuid));
        let wear = equipped(ctx, p.uuid);
        for kind in ["hat", "backpack", "balloon"] {
            let want = if hidden {
                None
            } else {
                wear.get(kind)
                    .and_then(|id| defs.get(id).map(|c| (id.clone(), c)))
            };
            let have = run(|r| r.shown.get(&(p.uuid, kind.to_owned())).cloned());
            match (want, have) {
                (None, Some(_)) => despawn(ctx, p.uuid, Some(kind)),
                (Some((id, c)), have) => {
                    let (pos, look) = placement(c, p.position, [p.yaw, p.pitch], t);
                    if let Some((d, e, hid)) = &have {
                        if *d == p.dimension && *hid == id {
                            ctx.host.move_entity(d, *e, pos, look[0], look[1]);
                            continue;
                        }
                        despawn(ctx, p.uuid, Some(kind));
                    }
                    let tags = vec!["hpcos".to_owned(), format!("hpcoss_{session}")];
                    if let Some(e) = ctx.host.summon(
                        &p.dimension,
                        "minecraft:item_display",
                        pos,
                        display_nbt(c, look, &tags),
                    ) {
                        if kind == "hat" {
                            ctx.host.hide_entity_from(p.uuid, e, true);
                        }
                        run(|r| {
                            r.shown.insert(
                                (p.uuid, kind.to_owned()),
                                (p.dimension.clone(), e, id.clone()),
                            )
                        });
                    }
                }
                (None, None) => {}
            }
        }
        // particle trail
        let every = ctx
            .get("particle_ticks")
            .and_then(|v| v.as_u64())
            .unwrap_or(5)
            .max(1);
        if !hidden
            && t.is_multiple_of(every)
            && let Some(c) = wear.get("particle").and_then(|id| defs.get(id))
        {
            let (x, y, z) = (
                p.position[0] + c.offset[0],
                p.position[1] + 0.2 + c.offset[1],
                p.position[2] + c.offset[2],
            );
            ctx.console(format!(
                "execute in {} run particle {} {x:.2} {y:.2} {z:.2} 0.3 0.1 0.3 0.01 {} normal",
                p.dimension,
                c.item,
                c.count.clamp(1, 50)
            ));
        }
    }
    // leftovers of earlier runs (saved in chunks) come back with their chunks
    if t.is_multiple_of(200) {
        let mut dims: Vec<String> = online.iter().map(|p| p.dimension.clone()).collect();
        dims.sort();
        dims.dedup();
        for d in dims {
            ctx.console(format!("execute in {d} run kill @e[type=minecraft:item_display,tag=hpcos,tag=!hpcoss_{session}]"));
        }
    }
}

fn cmd(ctx: &mut Ctx, args: &Args) -> CmdResult {
    let defs = all(ctx);
    let me = ctx.try_player().cloned();
    let sub = args
        .get(0)
        .map(|s| s.to_lowercase())
        .unwrap_or_else(|| "list".into());
    let staff = |ctx: &Ctx| ctx.op_level() >= 2;
    match sub.as_str() {
        "list" => {
            let p = me.ok_or("§cPlayers only (staff: /cosmetics info).")?;
            let mut ids: Vec<_> = defs
                .iter()
                .filter(|(id, _)| owns(ctx, p.uuid, &p.name, id))
                .collect();
            ids.sort_by(|a, b| a.0.cmp(b.0));
            if ids.is_empty() {
                ctx.reply("§7You own no cosmetics yet (web shop: /shop).");
            } else {
                let wear = equipped(ctx, p.uuid);
                ctx.reply(format!("§6Your cosmetics ({}):", ids.len()));
                for (id, c) in ids {
                    let on = if wear.get(&c.kind) == Some(id) {
                        " §a(worn)"
                    } else {
                        ""
                    };
                    ctx.reply(format!(
                        "§e{id} §7{} - {}{on}",
                        c.kind,
                        c.name.replace('&', "§")
                    ));
                }
            }
        }
        "equip" | "wear" => {
            let p = me.ok_or("§cPlayers only.")?;
            let id = args
                .get(1)
                .map(|s| s.to_lowercase())
                .ok_or("§cUsage: /cosmetics equip <id>")?;
            let c = defs
                .get(&id)
                .ok_or_else(|| format!("§cUnknown cosmetic '{id}'."))?;
            if !owns(ctx, p.uuid, &p.name, &id) {
                return Err(format!("§cYou do not own '{id}'."));
            }
            set_equipped(ctx, p.uuid, &c.kind.clone(), Some(&id))?;
            ctx.reply(format!(
                "§aNow wearing {} ({}).",
                c.name.replace('&', "§"),
                c.kind
            ));
        }
        "unequip" | "remove" => {
            let p = me.ok_or("§cPlayers only.")?;
            let what = args
                .get(1)
                .map(|s| s.to_lowercase())
                .unwrap_or_else(|| "all".into());
            let slots: Vec<&str> = if what == "all" {
                TYPES.to_vec()
            } else {
                TYPES.iter().copied().filter(|t| *t == what).collect()
            };
            if slots.is_empty() {
                return Err(
                    "§cUsage: /cosmetics unequip [hat|backpack|balloon|particle|all]".into(),
                );
            }
            for s in slots {
                set_equipped(ctx, p.uuid, s, None)?;
            }
            ctx.reply(format!("§aTook off: {what}."));
        }
        "info" => {
            let shown = run(|r| r.shown.len());
            ctx.reply(format!(
                "§6Cosmetics: §f{} defined, {shown} shown now.",
                defs.len()
            ));
        }
        "define" if staff(ctx) => {
            let (Some(id), Some(kind), Some(item)) = (args.get(1), args.get(2), args.get(3)) else {
                return Err("§cUsage: /cosmetics define <id> <hat|backpack|balloon|particle> <item or particle> [item_model]".into());
            };
            let kind = kind.to_lowercase();
            if !TYPES.contains(&kind.as_str()) {
                return Err(format!("§cType must be one of {}.", TYPES.join(", ")));
            }
            let mut m = ctx
                .get("cosmetics")
                .and_then(|v| v.as_object().cloned())
                .unwrap_or_default();
            let mut e = json::json!({"type": kind, "item": item});
            if let Some(model) = args.get(4) {
                e["item_model"] = json::json!(model);
            }
            m.insert(id.to_lowercase(), e);
            ctx.set("cosmetics", json::Value::Object(m))?;
            ctx.reply(format!(
                "§aDefined cosmetic {} ({kind}).",
                id.to_lowercase()
            ));
        }
        "undefine" if staff(ctx) => {
            let id = args
                .get(1)
                .map(|s| s.to_lowercase())
                .ok_or("§cUsage: /cosmetics undefine <id>")?;
            let mut m = ctx
                .get("cosmetics")
                .and_then(|v| v.as_object().cloned())
                .unwrap_or_default();
            if m.remove(&id).is_none() {
                return Err(format!("§cUnknown cosmetic '{id}'."));
            }
            ctx.set("cosmetics", json::Value::Object(m))?;
            ctx.reply(format!("§aRemoved cosmetic {id}."));
        }
        "give" | "take" if staff(ctx) => {
            let (Some(who), Some(id)) = (args.get(1), args.get(2)) else {
                return Err(format!("§cUsage: /cosmetics {sub} <player> <id>"));
            };
            let id = id.to_lowercase();
            if !defs.contains_key(&id) {
                return Err(format!("§cUnknown cosmetic '{id}'."));
            }
            // through the perms plugin, as the web shop does
            let line = if sub == "give" {
                format!("lp user {who} permission set hmccosmetics.{id} true")
            } else {
                format!("lp user {who} permission unset hmccosmetics.{id}")
            };
            ctx.console(line);
            ctx.reply(format!(
                "§a{} {id} {} {who}.",
                if sub == "give" { "Gave" } else { "Took" },
                if sub == "give" { "to" } else { "from" }
            ));
        }
        "clearall" if staff(ctx) => {
            let who = args.get(1).ok_or("§cUsage: /cosmetics clearall <player>")?;
            let p = ctx
                .online()
                .into_iter()
                .find(|p| p.name.eq_ignore_ascii_case(who))
                .ok_or("§cNot online.")?;
            for s in TYPES {
                set_equipped(ctx, p.uuid, s, None)?;
            }
            ctx.reply(format!("§aTook every cosmetic off {}.", p.name));
        }
        _ => {
            ctx.reply("§7/cosmetics list | equip <id> | unequip [type|all] | info");
            if staff(ctx) {
                ctx.reply("§7staff: define <id> <type> <item|particle> [item_model] | undefine <id> | give|take <player> <id> | clearall <player>");
            }
        }
    }
    Ok(())
}

pub fn plugin() -> Plugin {
    Plugin::new("cosmetics")
        .command(
            "cosmetics [sub:word] [a:word] [b:word] [c:word] [d:word]",
            0,
            cmd,
        )
        .alias("cosmetic")
        .alias("wardrobe")
        .on_quit(|ctx| {
            if let Some(u) = ctx.player_uuid() {
                // removed at the next tick (Host calls are refused inside a player's hook)
                ctx.run_later(1, move |c| despawn(c, u, None));
            }
        })
        .every(1, tick)
}
