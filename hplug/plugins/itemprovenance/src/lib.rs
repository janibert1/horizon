//! Item Provenance Tracker (`itemprovenance`)
//! Port of Paper's item-provenance-tracker anti-duplication system for Horizon.
//!
//! Tracks high-value items via a persistent provenance tag in `minecraft:custom_data`.
//! Guarantees the invariant: an ID can exist in at most one place in the world at a time.
//! If seen twice, it is direct proof of item duplication.

use hplug::{json, Args, CmdResult, Ctx, Plugin};
use horizon_items::items::ItemStack as HItemStack;
use horizon_world::nbt::{Compound, JavaString, Tag};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

static COUNTER: AtomicU64 = AtomicU64::new(1);

fn generate_id(prefix: &str) -> String {
    let now = hplug::now_ms();
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}-{now:x}-{seq:x}")
}

// =============================================================================
// IdTag: reads and writes provenance ID list on an ItemStack
// Stored in `minecraft:custom_data` under key "provenance_ids".
// =============================================================================

pub mod id_tag {
    use super::*;

    const CUSTOM_DATA_KEY: &str = "minecraft:custom_data";
    const PROV_KEY: &str = "provenance_ids";
    const DELIMITER: &str = ",";

    /// The IDs on a stack: Horizon's `custom_data.provenance_ids`, else (an item tagged on Paper and carried over in
    /// the world) the Bukkit PersistentDataContainer copy, `custom_data.PublicBukkitValues."<plugin>:<key>"` whose key
    /// names provenance ids. The next write moves them to Horizon's key.
    pub fn read(stack: &HItemStack) -> Vec<String> {
        let Some(data) = stack.data() else { return Vec::new() };
        let Some(Tag::Compound(map)) = data.other(CUSTOM_DATA_KEY) else { return Vec::new() };
        let pdc = || -> Option<&Tag> {
            let Some(Tag::Compound(bukkit)) = map.get(&JavaString::from("PublicBukkitValues")) else { return None };
            bukkit.iter().find(|(k, _)| {
                let k = k.to_string_lossy().to_lowercase();
                k.contains("provenance") && (k.ends_with("ids") || k.ends_with("id") || k.ends_with("_ids"))
            }).map(|(_, v)| v)
        };
        let Some(Tag::String(s)) = map.get(&JavaString::from(PROV_KEY)).or_else(pdc) else { return Vec::new() };
        let val = s.to_string_lossy();
        if val.is_empty() {
            return Vec::new();
        }
        val.split(DELIMITER)
            .filter(|p| !p.trim().is_empty())
            .map(|p| p.trim().to_owned())
            .collect()
    }

    pub fn write(stack: &mut HItemStack, ids: &[String]) {
        let mut map = match stack.data().and_then(|d| d.other(CUSTOM_DATA_KEY)) {
            Some(Tag::Compound(m)) => m.clone(),
            _ => Compound::new(),
        };

        if ids.is_empty() {
            map.remove(&JavaString::from(PROV_KEY));
            if map.is_empty() {
                stack.data_mut().set_other(CUSTOM_DATA_KEY, None);
            } else {
                stack.data_mut().set_other(CUSTOM_DATA_KEY, Some(Tag::Compound(map)));
            }
        } else {
            let joined = ids.join(DELIMITER);
            map.insert(JavaString::from(PROV_KEY), Tag::String(JavaString::from(joined.as_str())));
            stack.data_mut().set_other(CUSTOM_DATA_KEY, Some(Tag::Compound(map)));
        }
    }

    pub fn mint(stack: &mut HItemStack, count: u32, genesis_type: &str) -> Vec<String> {
        let mut ids = Vec::with_capacity(count as usize);
        for _ in 0..count {
            ids.push(generate_id(genesis_type));
        }
        write(stack, &ids);
        ids
    }

    /// The ingredient IDs a crafted stack came from (at most 32 kept), in `custom_data.provenance_parents`.
    pub fn write_parents(stack: &mut HItemStack, parents: &[String]) {
        let mut map = match stack.data().and_then(|d| d.other(CUSTOM_DATA_KEY)) {
            Some(Tag::Compound(m)) => m.clone(),
            _ => Compound::new(),
        };
        let kept: Vec<&str> = parents.iter().take(32).map(String::as_str).collect();
        map.insert(JavaString::from("provenance_parents"), Tag::String(JavaString::from(kept.join(DELIMITER).as_str())));
        stack.data_mut().set_other(CUSTOM_DATA_KEY, Some(Tag::Compound(map)));
    }

    pub fn is_consistent(ids: &[String], amount: u32) -> bool {
        if ids.is_empty() {
            true // untagged is outside system, not violation
        } else {
            ids.len() == amount as usize
        }
    }
}

// =============================================================================
// Config
// =============================================================================

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    pub testing_mode: bool,
    pub scan_interval_ticks: u64,
    pub tracked_materials: HashSet<String>,
    pub genesis_admin_given: bool,
    /// Mint IDs on tracked drops of blocks a player broke (Java: MINED). Off by default: a tagged stack does
    /// not stack with an untagged or differently tagged one, so this changes how mined diamonds stack.
    #[serde(default)]
    pub genesis_mined: bool,
    /// Mint IDs on tracked drops of mobs a player killed (Java: MOB_KILL; nether stars, shulker shells).
    #[serde(default)]
    pub genesis_mob_kill: bool,
    /// Mint IDs on tracked items generated into loot chests / archaeology / fishing (Java: LOOT).
    #[serde(default)]
    pub genesis_loot: bool,
    /// Crafting propagation (Java: CraftListener): a tracked result crafted from at least one tagged ingredient
    /// gets fresh CRAFTED IDs (one per item) and its parents' IDs in `provenance_parents`. Results from untagged
    /// ingredients stay untagged (outside the system, as on Paper). Needs the engine hook `item_craft_result`.
    #[serde(default = "default_true")]
    pub propagate_crafting: bool,
}

fn default_true() -> bool {
    true
}

impl Default for Config {
    fn default() -> Self {
        let tracked = [
            "minecraft:diamond",
            "minecraft:emerald",
            "minecraft:netherite_scrap",
            "minecraft:gold_ingot",
            "minecraft:netherite_ingot",
            "minecraft:netherite_block",
            "minecraft:diamond_block",
            "minecraft:emerald_block",
            "minecraft:gold_block",
            "minecraft:nether_star",
            "minecraft:beacon",
            "minecraft:shulker_shell",
            "minecraft:shulker_box",
            "minecraft:elytra",
            "minecraft:enchanted_golden_apple",
            "minecraft:diamond_sword",
            "minecraft:diamond_pickaxe",
            "minecraft:diamond_axe",
            "minecraft:diamond_shovel",
            "minecraft:diamond_hoe",
            "minecraft:diamond_helmet",
            "minecraft:diamond_chestplate",
            "minecraft:diamond_leggings",
            "minecraft:diamond_boots",
            "minecraft:netherite_sword",
            "minecraft:netherite_pickaxe",
            "minecraft:netherite_axe",
            "minecraft:netherite_shovel",
            "minecraft:netherite_hoe",
            "minecraft:netherite_helmet",
            "minecraft:netherite_chestplate",
            "minecraft:netherite_leggings",
            "minecraft:netherite_boots",
        ]
        .into_iter()
        .map(String::from)
        .collect();

        Self {
            testing_mode: true,
            scan_interval_ticks: 1200,
            tracked_materials: tracked,
            genesis_admin_given: true,
            genesis_mined: false,
            genesis_mob_kill: false,
            genesis_loot: false,
            propagate_crafting: true,
        }
    }
}

impl Config {
    pub fn is_tracked(&self, item: &str) -> bool {
        let name = if item.contains(':') {
            item.to_owned()
        } else {
            format!("minecraft:{item}")
        };
        self.tracked_materials.contains(&name)
    }
}

// =============================================================================
// Scanner & Duplicate Detection
// =============================================================================

#[derive(Clone, Debug)]
pub struct Sighting {
    pub id: String,
    pub player_name: String,
    pub player_uuid: u128,
    pub slot: usize,
    pub material: String,
}

#[derive(Default, Debug)]
pub struct ScanReport {
    pub items_scanned: usize,
    pub ids_scanned: usize,
    pub repaired_stacks: usize,
    pub duplicates: HashMap<String, Vec<Sighting>>,
}

fn get_player_inventory(ctx: &mut Ctx, uuid: u128) -> Vec<(usize, HItemStack)> {
    let mut out = Vec::new();
    if let Some(stacks) = ctx.host.player_stacks(uuid) {
        for (i, opt) in stacks.into_iter().enumerate() {
            if let Some(s) = opt {
                out.push((i, s));
            }
        }
    } else {
        // Acting player has swapped state; carried_stack accesses their slots
        for i in 0..36 {
            if let Some(s) = ctx.host.carried_stack(i) {
                out.push((i as usize, s.clone()));
            }
        }
    }
    out
}

pub fn scan_online_players(ctx: &mut Ctx, config: &Config) -> ScanReport {
    let players = ctx.online();
    let mut report = ScanReport::default();
    let mut all_sightings: Vec<Sighting> = Vec::new();

    for p in players {
        let inv = get_player_inventory(ctx, p.uuid);
        for (slot, mut stack) in inv {
            let item_id = &*stack.item;
            if !config.is_tracked(item_id) {
                continue;
            }
            report.items_scanned += 1;
            let ids = id_tag::read(&stack);

            if !id_tag::is_consistent(&ids, stack.count) {
                // Inconsistent stack (e.g. split without tag recalculation)
                // Untag it to prevent false positive dupe alarm (per Java ConsistencyGuard contract)
                id_tag::write(&mut stack, &[]);
                report.repaired_stacks += 1;
                continue;
            }

            report.ids_scanned += ids.len();
            for id in ids {
                all_sightings.push(Sighting {
                    id,
                    player_name: p.name.clone(),
                    player_uuid: p.uuid,
                    slot,
                    material: item_id.to_owned(),
                });
            }
        }
    }

    // Group sightings by id
    let mut grouped: HashMap<String, Vec<Sighting>> = HashMap::new();
    for s in all_sightings {
        grouped.entry(s.id.clone()).or_default().push(s);
    }

    for (id, sightings) in grouped {
        if sightings.len() > 1 {
            report.duplicates.insert(id, sightings);
        }
    }

    report
}

fn handle_violations(ctx: &mut Ctx, config: &Config, report: &ScanReport) {
    if report.duplicates.is_empty() {
        return;
    }

    ctx.log(format!(
        "[ItemProvenance] {} duplicate ID(s) detected across online players (testing_mode={})",
        report.duplicates.len(),
        config.testing_mode
    ));

    for (id, sightings) in &report.duplicates {
        let mat = &sightings[0].material;
        let holders: Vec<String> = sightings
            .iter()
            .map(|s| format!("{}(slot {})", s.player_name, s.slot))
            .collect();
        ctx.log(format!(
            "  DUPLICATE: {} id={} x{} in {}",
            mat,
            id,
            sightings.len(),
            holders.join(", ")
        ));

        // Record violation in persistent store
        let mut violations = ctx
            .get("violations")
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default();
        violations.push(json::json!({
            "id": id,
            "material": mat,
            "count": sightings.len(),
            "holders": holders,
            "detected_at": hplug::now_ms(),
        }));
        let _ = ctx.set("violations", json::Value::Array(violations));

        // Live enforcement if testing_mode == false
        if !config.testing_mode {
            // Remove the duplicate copy from player inventories (keep first copy, confiscate others)
            for dup in sightings.iter().skip(1) {
                ctx.console(format!(
                    "item replace entity {} container.{} with minecraft:air",
                    dup.player_name, dup.slot
                ));
                ctx.tell(
                    dup.player_uuid,
                    format!("§c[Provenance] Confiscated duplicated item {} (ID: {})", dup.material, id),
                );
            }
        }
    }
}

// =============================================================================
// Plugin entry point
// =============================================================================

pub fn plugin() -> Plugin {
    Plugin::new("itemprovenance")
        .command("provenance [action:word] [a:word] [b:word] [c:word]", 0, |ctx: &mut Ctx, args: &Args| -> CmdResult {
            let action = args.get(0).map(|s| s.to_lowercase()).unwrap_or_else(|| "help".into());
            let config: Config = ctx.get("config").and_then(|v| json::from_value(v).ok()).unwrap_or_default();

            match action.as_str() {
                "scan" => {
                    let report = scan_online_players(ctx, &config);
                    ctx.reply(format!(
                        "§6[Provenance] Scanned {} tracked item stack(s) / {} ID(s) across online players.",
                        report.items_scanned, report.ids_scanned
                    ));
                    if report.repaired_stacks > 0 {
                        ctx.reply(format!(
                            "§7  ({} stack(s) had inconsistent ID counts - repaired/untagged)",
                            report.repaired_stacks
                        ));
                    }
                    if report.duplicates.is_empty() {
                        ctx.reply(format!(
                            "§aNo duplicate provenance IDs found (testing_mode={}).",
                            config.testing_mode
                        ));
                    } else {
                        ctx.reply(format!("§c{} duplicate ID(s) found:", report.duplicates.len()));
                        for (id, sightings) in &report.duplicates {
                            let locs: Vec<String> = sightings.iter().map(|s| s.player_name.clone()).collect();
                            ctx.reply(format!(
                                "§c  {} id={}... x{} -> {}",
                                sightings[0].material,
                                &id[..id.len().min(10)],
                                sightings.len(),
                                locs.join(", ")
                            ));
                        }
                        handle_violations(ctx, &config, &report);
                    }
                }
                "stats" => {
                    let minted_count = ctx.get("minted_count").and_then(|v| v.as_u64()).unwrap_or(0);
                    let violations = ctx.get("violations").and_then(|v| v.as_array().map(|a| a.len())).unwrap_or(0);
                    ctx.reply(format!("§6[Provenance] testing_mode: {}", config.testing_mode));
                    ctx.reply(format!("§6  IDs minted (all time): {}", minted_count));
                    ctx.reply(format!("§6  Violations logged: {}", violations));
                    ctx.reply(format!("§6  Tracked materials: {}", config.tracked_materials.len()));
                }
                "mode" => {
                    let target = args.get(1).map(|s| s.trim().to_lowercase());
                    let mut new_conf = config.clone();
                    match target.as_deref() {
                        Some("test") => {
                            new_conf.testing_mode = true;
                            let _ = ctx.set("config", json::to_value(&new_conf).unwrap());
                            ctx.reply("§6[Provenance] Set testing_mode = true (log-only).");
                        }
                        Some("enforce") => {
                            new_conf.testing_mode = false;
                            let _ = ctx.set("config", json::to_value(&new_conf).unwrap());
                            ctx.reply("§6[Provenance] Set testing_mode = false (live confiscation).");
                        }
                        _ => {
                            ctx.reply(format!("§6[Provenance] Current testing_mode: {}. Use '/provenance mode test' or '/provenance mode enforce'", config.testing_mode));
                        }
                    }
                }
                "genesis" => {
                    let mut new_conf = config.clone();
                    let on = match args.get(2).map(|s| s.to_lowercase()).as_deref() {
                        Some("on") => Some(true),
                        Some("off") => Some(false),
                        _ => None,
                    };
                    match (args.get(1).map(|s| s.to_lowercase()).as_deref(), on) {
                        (Some("mined"), Some(v)) => new_conf.genesis_mined = v,
                        (Some("mobkill"), Some(v)) => new_conf.genesis_mob_kill = v,
                        (Some("loot"), Some(v)) => new_conf.genesis_loot = v,
                        _ => {
                            ctx.reply(format!(
                                "§6[Provenance] genesis mined={} mobkill={} loot={}. Use '/provenance genesis <mined|mobkill|loot> <on|off>'",
                                config.genesis_mined, config.genesis_mob_kill, config.genesis_loot
                            ));
                            return Ok(());
                        }
                    }
                    let _ = ctx.set("config", json::to_value(&new_conf).unwrap());
                    ctx.reply(format!(
                        "§6[Provenance] genesis mined={} mobkill={} loot={}",
                        new_conf.genesis_mined, new_conf.genesis_mob_kill, new_conf.genesis_loot
                    ));
                }
                "give" => {
                    let target_name = args.get(1).ok_or("Usage: /provenance give <player> <material> [amount]")?;
                    let mat_name = args.get(2).ok_or("Missing material name")?;
                    let count: u32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(1);

                    let full_mat = if mat_name.contains(':') {
                        mat_name.to_owned()
                    } else {
                        format!("minecraft:{mat_name}")
                    };

                    let (target_uuid, target_display) = if target_name == "@s" || ctx.try_player().is_some_and(|p| p.name.eq_ignore_ascii_case(target_name)) {
                        (ctx.player_uuid().ok_or("no player uuid")?, ctx.sender_name())
                    } else if let Some(p) = ctx.find_online(target_name) {
                        (p.uuid, p.name)
                    } else if let Some((u, n)) = ctx.uuid_for_name(target_name) {
                        (u, n)
                    } else {
                        return Err(format!("Player '{target_name}' not found"));
                    };

                    let mut stack = HItemStack {
                        item: Arc::from(full_mat.as_str()),
                        count,
                        damage: 0,
                        extra: None,
                    };

                    let ids = if config.is_tracked(&full_mat) {
                        let minted = id_tag::mint(&mut stack, count, "ADMIN_GIVEN");
                        // increment minted count
                        let curr = ctx.get("minted_count").and_then(|v| v.as_u64()).unwrap_or(0);
                        let _ = ctx.set("minted_count", json::json!(curr + count as u64));
                        minted
                    } else {
                        Vec::new()
                    };

                    let leftover = ctx.host.give_stack(target_uuid, stack);
                    if leftover.is_some() {
                        ctx.reply(format!("§eGave items to {}, but inventory was partially full.", target_display));
                    } else {
                        ctx.reply(format!(
                            "§aGave {} {}x {} {}",
                            target_display,
                            count,
                            full_mat,
                            if ids.is_empty() { "(untracked)" } else { "(tagged with provenance IDs)" }
                        ));
                    }
                }
                "lookup" => {
                    let id_prefix = args.get(1).ok_or("Usage: /provenance lookup <id>")?;
                    let violations = ctx.get("violations").and_then(|v| v.as_array().cloned()).unwrap_or_default();
                    let found: Vec<_> = violations
                        .iter()
                        .filter(|v| v.get("id").and_then(|i| i.as_str()).is_some_and(|s| s.contains(id_prefix)))
                        .collect();
                    if found.is_empty() {
                        ctx.reply(format!("§7No recorded violation matching ID '{id_prefix}'."));
                    } else {
                        ctx.reply(format!("§6Found {} violation record(s):", found.len()));
                        for item in found {
                            ctx.reply(format!("§7  {}", item));
                        }
                    }
                }
                _ => {
                    ctx.reply("§6--- Item Provenance Tracker ---");
                    ctx.reply("§e/provenance scan §7- Scan online players for duplicate IDs");
                    ctx.reply("§e/provenance stats §7- Show ledger stats");
                    ctx.reply("§e/provenance mode [test|enforce] §7- Set mode");
                    ctx.reply("§e/provenance genesis <mined|mobkill|loot> <on|off> §7- Mint IDs at drops");
                    ctx.reply("§e/provenance give <player> <item> <count> §7- Give tagged item");
                    ctx.reply("§e/provenance lookup <id> §7- Query recorded violations");
                }
            }
            Ok(())
        })
        .alias("prov")
        .alias("ipt")
        // Genesis at the drop (Java: GenesisListener on BlockDropItemEvent / EntityDeathEvent / LootGenerateEvent).
        // A dying player's inventory is never a genesis: its items already carry their IDs.
        .on_loot(|ctx, loot, drops| {
            let config: Config = ctx.get("config").and_then(|v| json::from_value(v).ok()).unwrap_or_default();
            let genesis = match (&loot.block, &loot.entity) {
                (_, Some((_, kind))) if kind == "minecraft:player" => return,
                (Some(_), _) if loot.player.is_some() && config.genesis_mined => "MINED",
                (None, Some(_)) if loot.player.is_some() && config.genesis_mob_kill => "MOB_KILL",
                (None, None) if !loot.table.is_empty() && config.genesis_loot => "LOOT",
                _ => return,
            };
            let mut minted = 0usize;
            for stack in drops.iter_mut() {
                if stack.count == 0 || !config.is_tracked(&stack.item) || !id_tag::read(stack).is_empty() {
                    continue;
                }
                let n = stack.count;
                minted += id_tag::mint(stack, n, genesis).len();
            }
            if minted > 0 {
                let all = ctx.get("minted_count").and_then(|v| v.as_u64()).unwrap_or(0);
                let _ = ctx.set("minted_count", json::json!(all + minted as u64));
                ctx.log(format!("genesis {genesis}: {minted} id(s) from {} at {:?}", if loot.table.is_empty() { "-" } else { &loot.table }, loot.origin));
            }
        })
        // Crafting propagation (Java: PrepareItemCraftEvent / CraftItemEvent). The preview is tagged, so the
        // IDs a player sees on the result are the ones they take; each grid change makes a new preview.
        .on_craft(|ctx, _pos, matrix, result| {
            let config: Config = ctx.get("config").and_then(|v| json::from_value(v).ok()).unwrap_or_default();
            if !config.propagate_crafting {
                return;
            }
            let Some(out) = result.as_mut() else { return };
            if out.count == 0 || !config.is_tracked(&out.item) || !id_tag::read(out).is_empty() {
                return; // untracked, or the recipe copied a tagged input's components (the IDs move with it)
            }
            let parents: Vec<String> = matrix.iter().flatten().flat_map(id_tag::read).collect();
            if parents.is_empty() {
                return;
            }
            let n = out.count;
            id_tag::mint(out, n, "CRAFTED");
            id_tag::write_parents(out, &parents);
        })
        .on_action(|ctx, action| {
            if let hplug::Action::ContainerOpen { .. } = action {
                let config: Config = ctx.get("config").and_then(|v| json::from_value(v).ok()).unwrap_or_default();
                let report = scan_online_players(ctx, &config);
                handle_violations(ctx, &config, &report);
            }
            false
        })
        .every(1200, |ctx| {
            let config: Config = ctx.get("config").and_then(|v| json::from_value(v).ok()).unwrap_or_default();
            let report = scan_online_players(ctx, &config);
            handle_violations(ctx, &config, &report);
        })
}
