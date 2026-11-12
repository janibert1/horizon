//! Port of the death-chest ("grave") feature of load-guard-plugin (Paper): when a player dies (keepInventory
//! off) their inventory goes into up to `MAX_CHESTS` chests next to the death point instead of being scattered;
//! stacks that do not fit are dropped normally; the chests vanish after `EXPIRY_HOURS`. The player is told where.
//! Java source: /home/jan/load-guard-plugin (README "Death chests"). Needs the engine change of
//! horizon-rust/horizon#8919 (`native_loot_drops` for a dying player) -> `Plugin::on_death_drops`.
use hplug::{Direction, ItemStack, NativeBlockEdit, NativeBlockRun, Plugin, json};

const MAX_CHESTS: usize = 8; // death-chest.max-chests
const EXPIRY_HOURS: u64 = 168; // death-chest.expiry-hours
const SLOTS: usize = 27;
const SWEEP_TICKS: u64 = 1200;

/// Candidate offsets around the death block, nearest first (the Java used a fixed list too).
const OFFSETS: [[i32; 3]; 19] = [
    [0, 0, 0],
    [1, 0, 0],
    [-1, 0, 0],
    [0, 0, 1],
    [0, 0, -1],
    [1, 0, 1],
    [-1, 0, -1],
    [1, 0, -1],
    [-1, 0, 1],
    [0, 1, 0],
    [2, 0, 0],
    [-2, 0, 0],
    [0, 0, 2],
    [0, 0, -2],
    [0, 2, 0],
    [1, 1, 0],
    [-1, 1, 0],
    [0, 1, 1],
    [0, 1, -1],
];

fn replaceable(block: &str) -> bool {
    matches!(
        block,
        "minecraft:air"
            | "minecraft:cave_air"
            | "minecraft:void_air"
            | "minecraft:water"
            | "minecraft:short_grass"
            | "minecraft:tall_grass"
            | "minecraft:fern"
            | "minecraft:large_fern"
            | "minecraft:snow"
    )
}

pub fn plugin() -> Plugin {
    Plugin::new("deathchest")
        // `/graves on|off|status` (op): which death-chest plugin takes the drops. Off = DeathChest by devcyntrix.
        .command("graves [state:word]", 2, |ctx, args| {
            match args.get(0).map(|s| s.to_lowercase()).as_deref() {
                Some(s @ ("on" | "off")) => {
                    ctx.set("enabled", json::json!(s == "on"))?;
                    ctx.reply(format!("§aSimple graves {}.", if s == "on" { "ON (they take the drops first)" } else { "OFF (devdeathchest takes the drops)" }));
                }
                _ => {
                    let on = ctx.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
                    ctx.reply(format!("§7Simple graves: {}", if on { "ON" } else { "OFF" }));
                }
            }
            Ok(())
        })
        .on_death_drops(|ctx, dimension, origin, drops| {
            // Only one death-chest plugin may store an inventory (both are linked, Jan 04/11 "do both"):
            // `{"enabled": false}` in config/hplug/deathchest.json hands every death to `devdeathchest`.
            if drops.is_empty() || !ctx.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true) {
                return;
            }
            let base = [origin[0].floor() as i32, origin[1].floor() as i32, origin[2].floor() as i32];
            let wanted = drops.len().div_ceil(SLOTS).min(MAX_CHESTS);
            let mut spots: Vec<[i32; 3]> = Vec::new();
            for o in OFFSETS {
                let pos = [base[0] + o[0], base[1] + o[1], base[2] + o[2]];
                if replaceable(&ctx.host.block(pos)) {
                    spots.push(pos);
                    if spots.len() == wanted {
                        break;
                    }
                }
            }
            let mut placed: Vec<[i32; 3]> = Vec::new();
            for pos in spots {
                let edit = NativeBlockEdit {
                    palette: vec!["minecraft:chest[facing=south]".into()],
                    filters: vec![],
            block_entities: vec![],
            neighbour_updates: false,
                    runs: vec![NativeBlockRun { x: pos[0], z: pos[2], y: pos[1], height: 1, surface: None, block: 0, only: None }],
                };
                match ctx.host.edit_blocks(dimension, &edit) {
                    Ok(n) if n > 0 => {
                        let take = drops.len().min(SLOTS);
                        let batch: Vec<ItemStack> = drops.drain(..take).collect();
                        for stack in batch {
                            // a chest has room for 27 stacks; anything left over goes back to the normal drops
                            if let Some(rest) = ctx.host.insert_item(pos, Direction::Up, stack) {
                                drops.push(rest);
                            }
                        }
                        placed.push(pos);
                    }
                    Ok(_) => {}
                    Err(e) => ctx.log(format!("[deathchest] could not place a chest at {pos:?}: {e}")),
                }
            }
            let Some(uuid) = ctx.player_uuid() else { return };
            if placed.is_empty() {
                return; // nothing placed: the items drop as usual
            }
            let expires = hplug::now_ms() + EXPIRY_HOURS * 3_600_000;
            let mut graves = ctx.get("graves").and_then(|v| v.as_array().cloned()).unwrap_or_default();
            for pos in &placed {
                graves.push(json::json!({ "dim": dimension, "x": pos[0], "y": pos[1], "z": pos[2], "expires": expires }));
            }
            if let Err(e) = ctx.set("graves", json::Value::Array(graves)) {
                ctx.log(format!("[deathchest] could not save the grave list: {e}"));
            }
            let first = placed[0];
            ctx.tell(
                uuid,
                format!(
                    "Your items are in {} chest(s) at {} {} {} ({}). They stay for {} hours.",
                    placed.len(), first[0], first[1], first[2], dimension, EXPIRY_HOURS
                ),
            );
            let overflow = drops.len();
            if overflow > 0 {
                ctx.tell(uuid, format!("{overflow} stack(s) did not fit and were dropped on the ground."));
            }
        })
        // periodic sweep: graves past their expiry are replaced by air (the chest and what is left in it are gone)
        .every(SWEEP_TICKS, |ctx| {
            let Some(graves) = ctx.get("graves").and_then(|v| v.as_array().cloned()) else { return };
            let now = hplug::now_ms();
            let (due, keep): (Vec<_>, Vec<_>) =
                graves.into_iter().partition(|g| g.get("expires").and_then(|e| e.as_u64()).is_some_and(|e| e <= now));
            if due.is_empty() {
                return;
            }
            for g in &due {
                let (dim, x, y, z) = (
                    g.get("dim").and_then(|v| v.as_str()).unwrap_or("minecraft:overworld"),
                    g.get("x").and_then(|v| v.as_i64()).unwrap_or(0),
                    g.get("y").and_then(|v| v.as_i64()).unwrap_or(0),
                    g.get("z").and_then(|v| v.as_i64()).unwrap_or(0),
                );
                ctx.console(format!("execute in {dim} run setblock {x} {y} {z} minecraft:air"));
            }
            if let Err(e) = ctx.set("graves", json::Value::Array(keep)) {
                ctx.log(format!("[deathchest] could not save the grave list: {e}"));
            }
        })
}
