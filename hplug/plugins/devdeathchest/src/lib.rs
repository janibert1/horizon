//! Port of the DeathChest plugin by devcyntrix (`/home/jan/deathchest-src`):
//! - Core death chest placement (finds replaceable spot around death origin, 1 or 2 chests)
//! - Items transferred to chest, overflow drops normally
//! - Configurable expiration with removal (optionally dropping remaining items or destroying them)
//! - Owner/thief protection (only owner/bypass may open or break for N seconds, then public)
//! - Item blacklist (items excluded from chest storage, left as ground drops or destroyed)
//! - Holograms above chest (using `text_display` entities tagged `ddc_<id>`)
//! - Blast protection against explosions
//! - Admin commands (`/deathchest reload|deleteinworld|blacklist|report|list`)
//! - Report/audit store
//! - Coexistence toggle with simple `deathchest` plugin
#![allow(clippy::too_many_arguments)] // plugin-local helpers with many parts
use hplug::{Ctx, Direction, ItemStack, NativeBlockEdit, NativeBlockRun, Plugin, json};
use serde::{Deserialize, Serialize};

const SLOTS_PER_CHEST: usize = 27;

/// Candidate offsets around the death origin block, nearest first.
const OFFSETS: [[i32; 3]; 24] = [
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
    [0, -1, 0],
    [1, -1, 0],
    [-1, -1, 0],
    [0, -1, 1],
    [0, -1, -1],
];

fn default_true() -> bool {
    true
}
fn default_expiration() -> i64 {
    600
}
fn default_thief_perm() -> String {
    "deathchest.thiefprotected".into()
}
fn default_bypass_perm() -> String {
    "deathchest.thiefprotected.bypass".into()
}
fn default_thief_msg() -> String {
    "§cYou are not permitted to open this chest".into()
}
fn default_player_notif_msg() -> String {
    "§7You died. Your items were put into a chest at §e${x} ${y} ${z}§7 which disappears after §c${duration}§7!".into()
}
fn default_global_notif_msg() -> String {
    "§7${player_name} died at ${x} ${y} ${z} in ${world}".into()
}
fn default_holo_height() -> f64 {
    1.2
}
fn default_holo_lines() -> Vec<String> {
    vec![
        "§7§lR.I.P".into(),
        "${player_name}".into(),
        "§3-§6-§3-§6-§3-§6-§3-".into(),
        "${duration}".into(),
    ]
}
fn default_blacklist_action() -> String {
    "drop".into()
}
fn default_max_chests() -> usize {
    2
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DevDeathChestConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_expiration")]
    pub expiration_secs: i64,
    #[serde(default)]
    pub drop_items_after_expiration: bool,
    #[serde(default)]
    pub blast_protection: bool,
    #[serde(default = "default_true")]
    pub thief_protection_enabled: bool,
    #[serde(default = "default_thief_perm")]
    pub thief_protection_permission: String,
    #[serde(default = "default_bypass_perm")]
    pub thief_bypass_permission: String,
    #[serde(default)]
    pub thief_protection_duration_secs: u64,
    #[serde(default = "default_thief_msg")]
    pub thief_message: String,
    #[serde(default = "default_true")]
    pub player_notification_enabled: bool,
    #[serde(default = "default_player_notif_msg")]
    pub player_notification_msg: String,
    #[serde(default)]
    pub global_notification_enabled: bool,
    #[serde(default = "default_global_notif_msg")]
    pub global_notification_msg: String,
    #[serde(default = "default_true")]
    pub hologram_enabled: bool,
    #[serde(default = "default_holo_height")]
    pub hologram_height: f64,
    #[serde(default = "default_holo_lines")]
    pub hologram_lines: Vec<String>,
    #[serde(default)]
    pub world_filter_type: String,
    #[serde(default)]
    pub world_filter_list: Vec<String>,
    #[serde(default)]
    pub item_blacklist: Vec<String>,
    #[serde(default = "default_blacklist_action")]
    pub blacklist_action: String,
    #[serde(default = "default_max_chests")]
    pub max_chests: usize,
}

impl Default for DevDeathChestConfig {
    fn default() -> Self {
        Self {
            enabled: default_true(),
            expiration_secs: default_expiration(),
            drop_items_after_expiration: false,
            blast_protection: false,
            thief_protection_enabled: default_true(),
            thief_protection_permission: default_thief_perm(),
            thief_bypass_permission: default_bypass_perm(),
            thief_protection_duration_secs: 0,
            thief_message: default_thief_msg(),
            player_notification_enabled: default_true(),
            player_notification_msg: default_player_notif_msg(),
            global_notification_enabled: false,
            global_notification_msg: default_global_notif_msg(),
            hologram_enabled: default_true(),
            hologram_height: default_holo_height(),
            hologram_lines: default_holo_lines(),
            world_filter_type: "blacklist".into(),
            world_filter_list: vec!["disabled_world".into()],
            item_blacklist: vec![],
            blacklist_action: default_blacklist_action(),
            max_chests: default_max_chests(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChestRecord {
    pub id: u64,
    pub owner_uuid: String, // 32 hex digits: serde_json cannot hold a u128 (the record was silently never saved)
    pub owner_name: String,
    pub dimension: String,
    pub positions: Vec<[i32; 3]>,
    pub created_at: u64,
    pub expires_at: i64,
    pub protected_until: i64,
    pub thief_protected: bool,
    pub items_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportRecord {
    pub id: u64,
    pub timestamp_ms: u64,
    pub date_str: String,
    pub chest_count: usize,
    pub summary: String,
}

fn load_config(ctx: &Ctx) -> DevDeathChestConfig {
    ctx.get("config")
        .and_then(|v| json::from_value(v).ok())
        .unwrap_or_default()
}

fn save_config(ctx: &mut Ctx, cfg: &DevDeathChestConfig) {
    if let Ok(v) = json::to_value(cfg) {
        let _ = ctx.set("config", v);
    }
}

fn load_chests(ctx: &Ctx) -> Vec<ChestRecord> {
    match ctx.get("chests") {
        None => Vec::new(),
        Some(v) => match json::from_value(v) {
            Ok(c) => c,
            Err(e) => {
                ctx.log(format!(
                    "[devdeathchest] stored chest list is unreadable: {e}"
                ));
                Vec::new()
            }
        },
    }
}

fn save_chests(ctx: &mut Ctx, chests: &[ChestRecord]) {
    match json::to_value(chests) {
        Ok(v) => {
            if let Err(e) = ctx.set("chests", v) {
                ctx.log(format!(
                    "[devdeathchest] could not save {} chest record(s): {e}",
                    chests.len()
                ));
            }
        }
        Err(e) => ctx.log(format!(
            "[devdeathchest] could not serialize the chest list: {e}"
        )),
    }
}

fn load_reports(ctx: &Ctx) -> Vec<ReportRecord> {
    ctx.get("reports")
        .and_then(|v| json::from_value(v).ok())
        .unwrap_or_default()
}

fn save_reports(ctx: &mut Ctx, reports: &[ReportRecord]) {
    if let Ok(v) = json::to_value(reports) {
        let _ = ctx.set("reports", v);
    }
}

fn next_chest_id(ctx: &mut Ctx) -> u64 {
    let next = ctx.get("next_id").and_then(|v| v.as_u64()).unwrap_or(1);
    let _ = ctx.set("next_id", json::json!(next + 1));
    next
}

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

fn format_duration(rem_ms: i64) -> String {
    if rem_ms <= 0 {
        return "00:00".into();
    }
    let total_secs = rem_ms / 1000;
    let mins = total_secs / 60;
    let secs = total_secs % 60;
    if mins >= 60 {
        let hrs = mins / 60;
        let rem_m = mins % 60;
        format!("{hrs}h {rem_m}m")
    } else {
        format!("{mins:02}:{secs:02}")
    }
}

fn substitute(
    template: &str,
    player_name: &str,
    world: &str,
    x: i32,
    y: i32,
    z: i32,
    chest_x: i32,
    chest_y: i32,
    chest_z: i32,
    duration: &str,
) -> String {
    template
        .replace("${player_name}", player_name)
        .replace("${player_displayname}", player_name)
        .replace("${world}", world)
        .replace("${world_alias}", world)
        .replace("${x}", &x.to_string())
        .replace("${y}", &y.to_string())
        .replace("${z}", &z.to_string())
        .replace("${chest_x}", &chest_x.to_string())
        .replace("${chest_y}", &chest_y.to_string())
        .replace("${chest_z}", &chest_z.to_string())
        .replace("${duration}", duration)
}

fn despawn_hologram(ctx: &mut Ctx, dim: &str, id: u64) {
    ctx.console(format!(
        "execute in {dim} run kill @e[type=text_display,tag=ddc_{id}]"
    ));
}

fn spawn_hologram(
    ctx: &mut Ctx,
    cfg: &DevDeathChestConfig,
    dim: &str,
    id: u64,
    pos: [i32; 3],
    owner_name: &str,
    expires_at: i64,
) {
    if !cfg.hologram_enabled {
        return;
    }
    let hx = pos[0] as f64 + 0.5;
    let hy = pos[1] as f64 + cfg.hologram_height;
    let hz = pos[2] as f64 + 0.5;

    let now = hplug::now_ms() as i64;
    let dur_str = if expires_at < 0 {
        "never".to_string()
    } else {
        format_duration(expires_at - now)
    };

    let lines: Vec<String> = cfg
        .hologram_lines
        .iter()
        .map(|line| {
            substitute(
                line, owner_name, dim, pos[0], pos[1], pos[2], pos[0], pos[1], pos[2], &dur_str,
            )
        })
        .collect();
    let text = lines.join("\n");
    let escaped = text.replace('\\', "\\\\").replace('"', "\\\"");
    ctx.console(format!(
        "execute in {dim} run summon text_display {hx:.2} {hy:.2} {hz:.2} {{Tags:[\"ddc_{id}\"],billboard:\"center\",text:'{{\"text\":\"{escaped}\"}}'}}"
    ));
}

fn is_blacklisted(item: &str, blacklist: &[String]) -> bool {
    blacklist.iter().any(|b| {
        let b = b.trim();
        if b.starts_with("minecraft:") {
            b.eq_ignore_ascii_case(item)
        } else {
            let full = format!("minecraft:{b}");
            full.eq_ignore_ascii_case(item) || b.eq_ignore_ascii_case(item)
        }
    })
}

fn world_allowed(dim: &str, cfg: &DevDeathChestConfig) -> bool {
    let mode = cfg.world_filter_type.to_lowercase();
    let contains = cfg
        .world_filter_list
        .iter()
        .any(|w| w.eq_ignore_ascii_case(dim) || dim.ends_with(w));
    if mode == "whitelist" {
        contains
    } else {
        !contains
    }
}

/// Exactly one death-chest plugin stores an inventory (decision 06/11): the simple graves (`deathchest`) by
/// default; this one only when they are switched off (`/graves off`, `{"enabled": false}` in deathchest.json).
fn graves_off(ctx: &Ctx) -> bool {
    ctx.host
        .config_dir()
        .ok()
        .and_then(|d| std::fs::read_to_string(d.join("hplug").join("deathchest.json")).ok())
        .and_then(|t| json::from_str::<json::Value>(&t).ok())
        .and_then(|v| v.get("enabled").and_then(|e| e.as_bool()))
        == Some(false)
}

pub fn plugin() -> Plugin {
    Plugin::new("devdeathchest")
        // Core death hook: transfers inventory into chest(s)
        .on_death_drops(|ctx, dimension, origin, drops| {
            if drops.is_empty() {
                return;
            }
            let cfg = load_config(ctx);
            if !cfg.enabled || !graves_off(ctx) {
                return;
            }
            if !world_allowed(dimension, &cfg) {
                return;
            }

            let Some(player) = ctx.try_player().cloned() else {
                return;
            };
            let uuid = player.uuid;
            let player_name = player.name.clone();

            // Handle blacklist
            let mut blacklisted = Vec::new();
            let mut valid_drops = Vec::new();
            for item in drops.drain(..) {
                if is_blacklisted(&item.item, &cfg.item_blacklist) {
                    blacklisted.push(item);
                } else {
                    valid_drops.push(item);
                }
            }

            if cfg.blacklist_action == "drop" {
                drops.extend(blacklisted);
            }

            if valid_drops.is_empty() {
                return;
            }

            let base = [
                origin[0].floor() as i32,
                origin[1].floor() as i32,
                origin[2].floor() as i32,
            ];
            let needed_chests = valid_drops.len().div_ceil(SLOTS_PER_CHEST);
            let wanted = needed_chests.min(cfg.max_chests.clamp(1, 2)).max(1);

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

            if spots.is_empty() {
                // No room for any chest: restore remaining items to drops
                drops.extend(valid_drops);
                return;
            }

            let mut placed: Vec<[i32; 3]> = Vec::new();
            let mut items_stored: usize = 0;
            for pos in spots {
                let edit = NativeBlockEdit {
                    palette: vec!["minecraft:chest[facing=south]".into()],
                    filters: vec![],
            block_entities: vec![],
            neighbour_updates: false,
                    runs: vec![NativeBlockRun {
                        x: pos[0],
                        z: pos[2],
                        y: pos[1],
                        height: 1,
                        surface: None,
                        block: 0,
                        only: None,
                    }],
                };
                match ctx.host.edit_blocks(dimension, &edit) {
                    Ok(n) if n > 0 => {
                        let take = valid_drops.len().min(SLOTS_PER_CHEST);
                        let batch: Vec<ItemStack> = valid_drops.drain(..take).collect();
                        for stack in batch {
                            items_stored += 1;
                            if let Some(rest) = ctx.host.insert_item(pos, Direction::Up, stack) {
                                drops.push(rest);
                                items_stored = items_stored.saturating_sub(1);
                            }
                        }
                        placed.push(pos);
                    }
                    Ok(_) => {}
                    Err(e) => ctx.log(format!("[devdeathchest] failed to place chest at {pos:?}: {e}")),
                }
            }

            if placed.is_empty() {
                drops.extend(valid_drops);
                return;
            }

            // Stacks that still did not fit spill on the ground
            drops.extend(valid_drops);

            let now = hplug::now_ms();
            let stays_forever = ctx.has_perm(uuid, &player_name, "deathchest.stays-forever");
            let expires_at = if stays_forever || cfg.expiration_secs < 0 {
                -1
            } else {
                (now + (cfg.expiration_secs as u64) * 1000) as i64
            };

            let is_protected = cfg.thief_protection_enabled
                && ctx.has_perm(uuid, &player_name, &cfg.thief_protection_permission);
            let protected_until = if !is_protected {
                0
            } else if cfg.thief_protection_duration_secs > 0 {
                (now + cfg.thief_protection_duration_secs * 1000) as i64
            } else {
                expires_at
            };

            let chest_id = next_chest_id(ctx);
            let first_pos = placed[0];

            let record = ChestRecord {
                id: chest_id,
                owner_uuid: format!("{uuid:032x}"),
                owner_name: player_name.clone(),
                dimension: dimension.to_string(),
                positions: placed.clone(),
                created_at: now,
                expires_at,
                protected_until,
                thief_protected: is_protected,
                items_count: items_stored,
            };

            let mut chests = load_chests(ctx);
            chests.push(record);
            save_chests(ctx, &chests);

            // Spawn hologram
            spawn_hologram(ctx, &cfg, dimension, chest_id, first_pos, &player_name, expires_at);

            // Player notification
            let dur_str = if expires_at < 0 {
                "never".to_string()
            } else {
                format_duration(expires_at - now as i64)
            };
            if cfg.player_notification_enabled {
                let msg = substitute(
                    &cfg.player_notification_msg,
                    &player_name,
                    dimension,
                    base[0],
                    base[1],
                    base[2],
                    first_pos[0],
                    first_pos[1],
                    first_pos[2],
                    &dur_str,
                );
                ctx.tell(uuid, msg);
            }

            // Global broadcast
            if cfg.global_notification_enabled {
                let gmsg = substitute(
                    &cfg.global_notification_msg,
                    &player_name,
                    dimension,
                    base[0],
                    base[1],
                    base[2],
                    first_pos[0],
                    first_pos[1],
                    first_pos[2],
                    &dur_str,
                );
                ctx.broadcast(gmsg);
            }
        })
        // Owner/thief protection on container open
        .on_action(|ctx, a| {
            if let hplug::Action::ContainerOpen { .. } = a {
                let Some(player) = ctx.try_player().cloned() else {
                    return false;
                };
                let uuid = player.uuid;
                let cfg = load_config(ctx);
                if !cfg.thief_protection_enabled {
                    return false;
                }
                if ctx.is_op(uuid) || ctx.has_perm(uuid, &player.name, &cfg.thief_bypass_permission) {
                    return false;
                }

                let chests = load_chests(ctx);
                let now = hplug::now_ms() as i64;
                for c in &chests {
                    if !c.thief_protected {
                        continue;
                    }
                    if c.protected_until > 0 && now >= c.protected_until {
                        continue;
                    }
                    if c.owner_uuid == format!("{uuid:032x}") {
                        continue;
                    }
                    if c.dimension != player.dimension {
                        continue;
                    }
                    for pos in &c.positions {
                        let dx = player.position[0] - (pos[0] as f64 + 0.5);
                        let dy = player.position[1] - (pos[1] as f64 + 0.5);
                        let dz = player.position[2] - (pos[2] as f64 + 0.5);
                        let dist_sq = dx * dx + dy * dy + dz * dz;
                        if dist_sq <= 36.0 {
                            ctx.tell(uuid, &cfg.thief_message);
                            return true; // Cancel ContainerOpen
                        }
                    }
                }
            }
            false
        })
        // Owner/thief protection on right-click chest interact
        .on_interact(|ctx, pos, _face| {
            let Some(player) = ctx.try_player().cloned() else {
                return false;
            };
            let uuid = player.uuid;
            let cfg = load_config(ctx);
            if !cfg.thief_protection_enabled {
                return false;
            }
            if ctx.is_op(uuid) || ctx.has_perm(uuid, &player.name, &cfg.thief_bypass_permission) {
                return false;
            }

            let chests = load_chests(ctx);
            let now = hplug::now_ms() as i64;
            for c in &chests {
                if !c.thief_protected {
                    continue;
                }
                if c.protected_until > 0 && now >= c.protected_until {
                    continue;
                }
                if c.owner_uuid == format!("{uuid:032x}") {
                    continue;
                }
                if c.dimension != player.dimension {
                    continue;
                }
                if c.positions.contains(&pos) {
                    ctx.tell(uuid, &cfg.thief_message);
                    return true; // Cancel interaction
                }
            }
            false
        })
        // Block break protection & destruction handling
        .on_block_break(|ctx, pos| {
            let Some(player) = ctx.try_player().cloned() else {
                return false;
            };
            let uuid = player.uuid;
            let cfg = load_config(ctx);
            let chests = load_chests(ctx);
            let now = hplug::now_ms() as i64;

            let mut matched_id = None;
            for c in &chests {
                if c.dimension != player.dimension {
                    continue;
                }
                if c.positions.contains(&pos) {
                    let is_protected = c.thief_protected && (c.protected_until <= 0 || now < c.protected_until);
                    let can_access = !is_protected
                        || c.owner_uuid == format!("{uuid:032x}")
                        || ctx.is_op(uuid)
                        || ctx.has_perm(uuid, &player.name, &cfg.thief_bypass_permission);
                    if !can_access {
                        ctx.tell(uuid, &cfg.thief_message);
                        return true; // Cancel break
                    }
                    matched_id = Some((c.id, c.dimension.clone()));
                    break;
                }
            }

            if let Some((id, dim)) = matched_id {
                despawn_hologram(ctx, &dim, id);
                let updated: Vec<_> = chests.into_iter().filter(|c| c.id != id).collect();
                save_chests(ctx, &updated);
            }
            false
        })
        // Explosion blast protection
        .on_explosion(|ctx, dim, _center, blocks| {
            let cfg = load_config(ctx);
            if !cfg.blast_protection {
                return;
            }
            let chests = load_chests(ctx);
            blocks.retain(|b| {
                !chests
                    .iter()
                    .any(|c| c.dimension == dim && c.positions.iter().any(|p| p == b))
            });
        })
        // Periodic sweeper: expiry cleanup and hologram duration sync
        .every(20, |ctx| {
            let chests = load_chests(ctx);
            if chests.is_empty() {
                return;
            }
            let cfg = load_config(ctx);
            let now = hplug::now_ms() as i64;

            let (due, mut keep): (Vec<_>, Vec<_>) = chests
                .into_iter()
                .partition(|c| c.expires_at > 0 && now >= c.expires_at);

            for c in due {
                for pos in &c.positions {
                    let (x, y, z) = (pos[0], pos[1], pos[2]);
                    let dim = &c.dimension;
                    if cfg.drop_items_after_expiration {
                        ctx.console(format!("execute in {dim} run setblock {x} {y} {z} minecraft:air destroy"));
                    } else {
                        ctx.console(format!("execute in {dim} run setblock {x} {y} {z} minecraft:air"));
                    }
                }
                despawn_hologram(ctx, &c.dimension, c.id);
            }

            // Sync thief protection state
            for c in &mut keep {
                if c.thief_protected && c.protected_until > 0 && now >= c.protected_until {
                    c.thief_protected = false;
                }
            }

            save_chests(ctx, &keep);
        })
        // Admin commands: /deathchest ...
        .command(
            "deathchest [sub:word] [arg1:word] [arg2:word] [arg3:word]",
            2,
            |ctx, args| {
                let sub = args.get(0).map(|s| s.as_str()).unwrap_or("help");
                match sub {
                    "reload" => {
                        let cfg = load_config(ctx);
                        save_config(ctx, &cfg);
                        ctx.reply("§a[DeathChest] Configuration reloaded successfully.");
                        Ok(())
                    }
                    "deleteinworld" => {
                        let target_world = args.get(1).map(|s| s.as_str());
                        let chests = load_chests(ctx);
                        let mut deleted = 0;
                        let mut keep = Vec::new();

                        for c in chests {
                            let matches = match target_world {
                                Some("all") | None => true,
                                Some(w) => c.dimension.eq_ignore_ascii_case(w) || c.dimension.ends_with(w),
                            };
                            if matches {
                                for pos in &c.positions {
                                    let (x, y, z) = (pos[0], pos[1], pos[2]);
                                    let dim = &c.dimension;
                                    ctx.console(format!("execute in {dim} run setblock {x} {y} {z} minecraft:air"));
                                }
                                despawn_hologram(ctx, &c.dimension, c.id);
                                deleted += 1;
                            } else {
                                keep.push(c);
                            }
                        }
                        save_chests(ctx, &keep);
                        let world_name = match target_world {
                            Some("all") | None => "all worlds",
                            Some(w) => w,
                        };
                        ctx.reply(format!("§a[DeathChest] A total of {deleted} chests were deleted in {world_name}."));
                        Ok(())
                    }
                    "blacklist" => {
                        let action = args.get(1).map(|s| s.as_str()).unwrap_or("list");
                        let mut cfg = load_config(ctx);
                        match action {
                            "add" => {
                                let Some(item) = args.get(2) else {
                                    return Err("Usage: /deathchest blacklist add <item>".into());
                                };
                                if !cfg.item_blacklist.iter().any(|b| b.eq_ignore_ascii_case(item)) {
                                    cfg.item_blacklist.push(item.to_string());
                                    save_config(ctx, &cfg);
                                }
                                ctx.reply(format!("§a[DeathChest] Added {item} to blacklist."));
                                Ok(())
                            }
                            "remove" => {
                                let Some(item) = args.get(2) else {
                                    return Err("Usage: /deathchest blacklist remove <item>".into());
                                };
                                cfg.item_blacklist.retain(|b| !b.eq_ignore_ascii_case(item));
                                save_config(ctx, &cfg);
                                ctx.reply(format!("§a[DeathChest] Removed {item} from blacklist."));
                                Ok(())
                            }
                            "list" => {
                                if cfg.item_blacklist.is_empty() {
                                    ctx.reply("§7[DeathChest] Blacklist is empty.");
                                } else {
                                    ctx.reply(format!(
                                        "§6[DeathChest] Blacklisted items ({}): §f{}",
                                        cfg.item_blacklist.len(),
                                        cfg.item_blacklist.join(", ")
                                    ));
                                }
                                Ok(())
                            }
                            other => Err(format!("Unknown blacklist action '{other}'. Use add, remove, or list.")),
                        }
                    }
                    "report" => {
                        let action = args.get(1).map(|s| s.as_str()).unwrap_or("list");
                        let mut reports = load_reports(ctx);
                        match action {
                            "create" => {
                                let now = hplug::now_ms();
                                let next_id = reports.iter().map(|r| r.id).max().unwrap_or(0) + 1;
                                let chests = load_chests(ctx);
                                let rep = ReportRecord {
                                    id: next_id,
                                    timestamp_ms: now,
                                    date_str: format!("Report #{next_id} at {now}"),
                                    chest_count: chests.len(),
                                    summary: format!(
                                        "Chests: {}, Expiry: {}s, ThiefProt: {}",
                                        chests.len(),
                                        load_config(ctx).expiration_secs,
                                        load_config(ctx).thief_protection_enabled
                                    ),
                                };
                                reports.push(rep);
                                save_reports(ctx, &reports);
                                ctx.reply(format!("§a[DeathChest] Report #{next_id} created successfully."));
                                Ok(())
                            }
                            "list" => {
                                if reports.is_empty() {
                                    ctx.reply("§7[DeathChest] No reports available.");
                                } else {
                                    ctx.reply(format!("§6[DeathChest] Reports ({}):", reports.len()));
                                    for r in &reports {
                                        ctx.reply(format!("§e  #{} §7- {} ({} chests)", r.id, r.summary, r.chest_count));
                                    }
                                }
                                Ok(())
                            }
                            "latest" => {
                                if let Some(r) = reports.last() {
                                    ctx.reply(format!("§6[DeathChest] Latest Report #{}:", r.id));
                                    ctx.reply(format!("§7  {}", r.summary));
                                } else {
                                    ctx.reply("§7[DeathChest] No reports available.");
                                }
                                Ok(())
                            }
                            "delete" => {
                                let Some(id_str) = args.get(2) else {
                                    return Err("Usage: /deathchest report delete <id>".into());
                                };
                                let Ok(id) = id_str.parse::<u64>() else {
                                    return Err("Report ID must be a number".into());
                                };
                                let before = reports.len();
                                reports.retain(|r| r.id != id);
                                if reports.len() < before {
                                    save_reports(ctx, &reports);
                                    ctx.reply(format!("§a[DeathChest] Deleted report #{id}."));
                                } else {
                                    ctx.reply(format!("§c[DeathChest] Report #{id} not found."));
                                }
                                Ok(())
                            }
                            "deleteall" => {
                                let count = reports.len();
                                reports.clear();
                                save_reports(ctx, &reports);
                                ctx.reply(format!("§a[DeathChest] Deleted all {count} reports."));
                                Ok(())
                            }
                            other => Err(format!("Unknown report action '{other}'. Use create, list, latest, delete, or deleteall.")),
                        }
                    }
                    "list" => {
                        let chests = load_chests(ctx);
                        if chests.is_empty() {
                            ctx.reply("§7[DeathChest] No active death chests.");
                        } else {
                            let now = hplug::now_ms() as i64;
                            ctx.reply(format!("§6[DeathChest] Active chests ({}):", chests.len()));
                            for c in &chests {
                                let pos_str = c
                                    .positions
                                    .iter()
                                    .map(|p| format!("({},{},{})", p[0], p[1], p[2]))
                                    .collect::<Vec<_>>()
                                    .join(" + ");
                                let rem = if c.expires_at < 0 {
                                    "permanent".into()
                                } else {
                                    format_duration(c.expires_at - now)
                                };
                                ctx.reply(format!(
                                    "§e  #{} §f{} §7at {} in {} (expires: {})",
                                    c.id, c.owner_name, pos_str, c.dimension, rem
                                ));
                            }
                        }
                        Ok(())
                    }
                    _ => {
                        ctx.reply("§6--- DeathChest Commands ---");
                        ctx.reply("§e/deathchest reload §7- Reload configuration");
                        ctx.reply("§e/deathchest deleteinworld [world] §7- Delete chests in world");
                        ctx.reply("§e/deathchest blacklist <add|remove|list> §7- Manage item blacklist");
                        ctx.reply("§e/deathchest report <create|list|latest|delete|deleteall> §7- Audit reports");
                        ctx.reply("§e/deathchest list §7- List active death chests");
                        Ok(())
                    }
                }
            },
        )
        .alias("dc")
}
