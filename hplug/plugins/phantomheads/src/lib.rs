//! Port of PhantomHeads (PacketEvents, 1743 LOC)
//! Virtual floating skull NPCs with holograms, particles, and click interactions.
//!
//! Because Horizon has no packet-level fake entity injection API, heads are rendered using
//! real server display entities (`item_display` for skulls, `text_display` for holograms, and
//! `interaction` for click hitboxes), tagged with `ph_<id>` and managed via console commands.
use hplug::{Action, Args, CmdResult, Ctx, Plugin, json};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;

const PFX: &str = "§7[§dPhantomHeads§7] ";
const LINE_GAP: f64 = 0.28;
const HOLO_BASE_OFFSET: f64 = 0.65;
const BOB_AMPLITUDE: f64 = 0.08;
const BOB_SPEED: f64 = 0.05;
const ROT_SPEED: f32 = 3.5;

fn default_true() -> bool {
    true
}
fn default_speed() -> f64 {
    1.0
}
fn default_cooldown() -> u32 {
    3
}
fn default_particle_type() -> String {
    "FLAME".into()
}
fn default_particle_density() -> f64 {
    1.0
}
fn default_ambient_effect() -> String {
    "circle".into()
}
fn default_click_effect() -> String {
    "burst".into()
}
fn default_animation() -> String {
    "FLOATING".into()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HeadData {
    pub id: String,
    pub world: String,
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub yaw: f32,
    #[serde(default)]
    pub texture: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_true")]
    pub hologram_enabled: bool,
    #[serde(default)]
    pub lines: Vec<String>,
    #[serde(default = "default_animation")]
    pub animation_style: String,
    #[serde(default = "default_speed")]
    pub speed_multiplier: f64,
    #[serde(default = "default_particle_type")]
    pub particle_type: String,
    #[serde(default = "default_particle_density")]
    pub particle_density: f64,
    #[serde(default = "default_ambient_effect")]
    pub ambient_effect: String,
    #[serde(default = "default_click_effect")]
    pub click_effect: String,
    #[serde(default)]
    pub player_commands: Vec<String>,
    #[serde(default)]
    pub console_commands: Vec<String>,
    #[serde(default)]
    pub messages: Vec<String>,
    #[serde(default = "default_cooldown")]
    pub cooldown_seconds: u32,
}

impl HeadData {
    fn new(
        id: String,
        world: String,
        x: f64,
        y: f64,
        z: f64,
        yaw: f32,
        texture: Option<String>,
    ) -> Self {
        Self {
            id,
            world,
            x,
            y,
            z,
            yaw,
            texture,
            enabled: true,
            hologram_enabled: true,
            lines: Vec::new(),
            animation_style: "FLOATING".into(),
            speed_multiplier: 1.0,
            particle_type: "FLAME".into(),
            particle_density: 1.0,
            ambient_effect: "circle".into(),
            click_effect: "burst".into(),
            player_commands: Vec::new(),
            console_commands: Vec::new(),
            messages: Vec::new(),
            cooldown_seconds: 3,
        }
    }
}

#[derive(Default)]
struct State {
    heads: HashMap<String, HeadData>,
    cooldowns: HashMap<(String, u128), u64>,
    anim_ticks: HashMap<String, f64>,
    initialized: bool,
}

static STATE: std::sync::LazyLock<Mutex<State>> = std::sync::LazyLock::new(|| {
    Mutex::new(State {
        heads: HashMap::new(),
        cooldowns: HashMap::new(),
        anim_ticks: HashMap::new(),
        initialized: false,
    })
});

fn fmt_f(v: f64) -> String {
    format!("{:.1}", v)
}

fn particle_name(raw: &str) -> &'static str {
    match raw.to_ascii_uppercase().as_str() {
        "FLAME" => "flame",
        "SOUL_FIRE_FLAME" | "SOUL_FLAME" => "soul_fire_flame",
        "HEART" => "heart",
        "PORTAL" => "portal",
        "SMOKE" => "smoke",
        "HAPPY_VILLAGER" | "VILLAGER_HAPPY" => "happy_villager",
        "ENCHANT" | "ENCHANTMENT_TABLE" => "enchant",
        "END_ROD" => "end_rod",
        "CRIT" => "crit",
        "FIREWORK" => "firework",
        "WITCH" => "witch",
        "CLOUD" => "cloud",
        _ => "flame",
    }
}

fn spawn_head_entities(ctx: &mut Ctx, head: &HeadData) {
    let dim = &head.world;
    let id = &head.id;

    // Despawn old entities first to avoid duplicates
    ctx.console(format!("execute in {dim} run kill @e[tag=ph_{id}]"));
    PERSONAL
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|x| x.0 != *id);

    if !head.enabled {
        return;
    }

    // 1. Skull item display
    let comp = match &head.texture {
        Some(tex) if !tex.is_empty() && !tex.eq_ignore_ascii_case("%player_name%") => {
            if tex.len() > 32 {
                format!(
                    ",components:{{\"minecraft:profile\":{{\"properties\":[{{\"name\":\"textures\",\"value\":\"{tex}\"}}]}}}}"
                )
            } else {
                format!(",components:{{\"minecraft:profile\":{{\"name\":\"{tex}\"}}}}")
            }
        }
        _ => String::new(),
    };
    // `%player_name%`: no shared skull; every viewer gets a personal one (sync_personal)
    let personal = head
        .texture
        .as_deref()
        .is_some_and(|t| t.eq_ignore_ascii_case("%player_name%"));
    if !personal {
        ctx.console(format!(
            "execute in {dim} run summon item_display {:.2} {:.2} {:.2} {{Tags:[\"ph_entity\",\"ph_{id}\",\"ph_{id}_head\"],item:{{id:\"minecraft:player_head\",count:1{comp}}}}}",
            head.x, head.y, head.z
        ));
    }

    // 2. Hologram text displays
    if head.hologram_enabled {
        for (i, line) in head.lines.iter().enumerate() {
            let line_y = head.y + HOLO_BASE_OFFSET + (i as f64 * LINE_GAP);
            let escaped = line.replace('\\', "\\\\").replace('"', "\\\"");
            ctx.console(format!(
                "execute in {dim} run summon text_display {:.2} {:.2} {:.2} {{Tags:[\"ph_entity\",\"ph_{id}\",\"ph_{id}_line\",\"ph_line_{i}\"],text:'\"{escaped}\"',billboard:\"center\"}}",
                head.x, line_y, head.z
            ));
        }
    }

    // 3. Click interaction hitbox
    ctx.console(format!(
        "execute in {dim} run summon interaction {:.2} {:.2} {:.2} {{Tags:[\"ph_entity\",\"ph_{id}\",\"ph_{id}_click\"],width:0.8f,height:0.8f}}",
        head.x, head.y - 0.2, head.z
    ));
}

fn despawn_head_entities(ctx: &mut Ctx, head_id: &str, dim: &str) {
    clear_personal(ctx, head_id);
    ctx.console(format!("execute in {dim} run kill @e[tag=ph_{head_id}]"));
}

fn respawn_lines_only(ctx: &mut Ctx, head: &HeadData) {
    let dim = &head.world;
    let id = &head.id;
    ctx.console(format!("execute in {dim} run kill @e[tag=ph_{id}_line]"));

    if head.enabled && head.hologram_enabled {
        for (i, line) in head.lines.iter().enumerate() {
            let line_y = head.y + HOLO_BASE_OFFSET + (i as f64 * LINE_GAP);
            let escaped = line.replace('\\', "\\\\").replace('"', "\\\"");
            ctx.console(format!(
                "execute in {dim} run summon text_display {:.2} {:.2} {:.2} {{Tags:[\"ph_entity\",\"ph_{id}\",\"ph_{id}_line\",\"ph_line_{i}\"],text:'\"{escaped}\"',billboard:\"center\"}}",
                head.x, line_y, head.z
            ));
        }
    }
}

fn save_state(ctx: &mut Ctx) {
    let state = STATE.lock().unwrap();
    let heads_list: Vec<&HeadData> = state.heads.values().collect();
    if let Ok(v) = json::to_value(&heads_list) {
        let _ = ctx.set("heads", v);
    }
}

fn load_state(ctx: &mut Ctx) {
    let mut state = STATE.lock().unwrap();
    if state.initialized {
        return;
    }
    state.initialized = true;

    if let Some(val) = ctx.get("heads")
        && let Ok(loaded) = json::from_value::<Vec<HeadData>>(val)
    {
        for h in loaded {
            state.heads.insert(h.id.clone(), h);
        }
    }
    let heads_to_spawn: Vec<HeadData> = state.heads.values().cloned().collect();
    drop(state);

    // Initial world entity cleanup and spawn
    ctx.console("kill @e[tag=ph_entity]");
    for h in &heads_to_spawn {
        if h.enabled {
            spawn_head_entities(ctx, h);
        }
    }
}

fn spawn_click_particles(ctx: &mut Ctx, head: &HeadData) {
    let dim = &head.world;
    let p = particle_name(&head.particle_type);
    let (x, y, z) = (head.x, head.y, head.z);

    match head.click_effect.to_ascii_lowercase().as_str() {
        "firework" => {
            ctx.console(format!(
                "execute in {dim} run particle {p} {x:.2} {:.2} {z:.2} 0.4 0.4 0.4 0.1 40",
                y + 0.5
            ));
        }
        "shockwave" => {
            ctx.console(format!(
                "execute in {dim} run particle {p} {x:.2} {:.2} {z:.2} 1.0 0.1 1.0 0.05 32",
                y + 0.1
            ));
        }
        "fountain" => {
            ctx.console(format!(
                "execute in {dim} run particle {p} {x:.2} {:.2} {z:.2} 0.2 0.5 0.2 0.1 30",
                y + 0.3
            ));
        }
        "heart" => {
            ctx.console(format!(
                "execute in {dim} run particle heart {x:.2} {:.2} {z:.2} 0.3 0.3 0.3 0.05 10",
                y + 0.5
            ));
        }
        _ => {
            // "burst" and default
            ctx.console(format!(
                "execute in {dim} run particle {p} {x:.2} {:.2} {z:.2} 0.5 0.5 0.5 0.15 60",
                y + 0.5
            ));
        }
    }
}

fn trigger_head_interaction(ctx: &mut Ctx, head_id: &str) {
    let Some(player) = ctx.try_player().cloned() else {
        return;
    };
    let uuid = player.uuid;
    let name = player.name.clone();

    // Permission check (default: true)
    if !ctx.has_perm(uuid, &name, "phantomheads.use") {
        ctx.tell(uuid, "§cYou don't have permission to interact with this.");
        return;
    }

    let head = {
        let state = STATE.lock().unwrap();
        state.heads.get(head_id).cloned()
    };
    let Some(head) = head else { return };
    if !head.enabled {
        return;
    }

    // Cooldown check
    let now = hplug::now_ms();
    {
        let mut state = STATE.lock().unwrap();
        if let Some(&last_ms) = state.cooldowns.get(&(head.id.clone(), uuid))
            && now.saturating_sub(last_ms) < (head.cooldown_seconds as u64) * 1000
        {
            ctx.tell(uuid, "§ePlease wait before clicking that again.");
            return;
        }
        state.cooldowns.insert((head.id.clone(), uuid), now);
    }

    spawn_click_particles(ctx, &head);

    for cmd in &head.player_commands {
        let resolved = cmd.replace("%player_name%", &name);
        ctx.console(format!("execute as {name} at {name} run {resolved}"));
    }
    for cmd in &head.console_commands {
        let resolved = cmd.replace("%player_name%", &name);
        ctx.console(resolved);
    }
    for msg in &head.messages {
        let resolved = msg.replace("%player_name%", &name);
        ctx.tell(uuid, resolved);
    }
}

fn find_head_by_entity_id(ctx: &Ctx, entity_id: i32) -> Option<String> {
    if let Some(eref) = ctx.host.entity(entity_id as u32) {
        if let Some(tags) = ctx.host.entity_tags(eref) {
            for tag in tags {
                if let Some(id) = tag.strip_prefix("ph_") {
                    if id != "entity"
                        && id != "head"
                        && id != "line"
                        && id != "click"
                        && !id.starts_with("line_")
                        && !id.ends_with("_head")
                        && !id.ends_with("_line")
                        && !id.ends_with("_click")
                    {
                        return Some(id.to_string());
                    }
                    if let Some(base) = id
                        .strip_suffix("_click")
                        .or_else(|| id.strip_suffix("_head"))
                    {
                        return Some(base.to_string());
                    }
                }
            }
        }
        if let Some(pos) = ctx.host.entity_position(eref) {
            let state = STATE.lock().unwrap();
            for head in state.heads.values() {
                if head.enabled {
                    let dx = head.x - pos[0];
                    let dy = head.y - pos[1];
                    let dz = head.z - pos[2];
                    if dx * dx + dy * dy + dz * dz <= 2.25 {
                        return Some(head.id.clone());
                    }
                }
            }
        }
    }
    // Fallback: check proximity to acting player
    if let Some(player) = ctx.try_player() {
        let state = STATE.lock().unwrap();
        for head in state.heads.values() {
            if head.enabled && head.world == player.dimension {
                let dx = head.x - player.position[0];
                let dy = head.y - player.position[1];
                let dz = head.z - player.position[2];
                if dx * dx + dy * dy + dz * dz <= 16.0 {
                    return Some(head.id.clone());
                }
            }
        }
    }
    None
}

// -----------------------------------------------------------------------------
// Commands
// -----------------------------------------------------------------------------

fn send_help(ctx: &mut Ctx) {
    ctx.reply(format!("{PFX}§ePhantomHeads Commands §7(/ph)"));
    ctx.reply(format!(
        "{PFX}  §fcreate <id> [texture]  §7— spawn a head at your location"
    ));
    ctx.reply(format!(
        "{PFX}  §fdelete|info|enable|disable|toggle|movehere|teleport <id>"
    ));
    ctx.reply(format!(
        "{PFX}  §fclone <id> <new_id>  §7|  §fhologram <enable|disable> <id>"
    ));
    ctx.reply(format!(
        "{PFX}  §fline <add|set|remove|swap|list> <id> [...]"
    ));
    ctx.reply(format!(
        "{PFX}  §faction <id> <add|remove|list> <player|console|message> [value]"
    ));
    ctx.reply(format!(
        "{PFX}  §flist  §7|  §fnear [radius]  §7|  §freload"
    ));
}

fn handle_ph(ctx: &mut Ctx, args: &Args) -> CmdResult {
    load_state(ctx);

    if args.is_empty() {
        send_help(ctx);
        return Ok(());
    }

    let sub = args
        .get(0)
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    match sub.as_str() {
        "create" => {
            let Some(player) = ctx.try_player().cloned() else {
                return Err(format!("{PFX}§cThis command requires a player."));
            };
            let id = match args.get(1) {
                Some(s) if !s.is_empty() => s.clone(),
                _ => return Err(format!("{PFX}§eUsage: /ph create <id> [texture]")),
            };

            let texture = args.get(2).cloned();
            let mut state = STATE.lock().unwrap();
            if state.heads.contains_key(&id) {
                return Err(format!("{PFX}§cA head with ID §f{id}§c already exists."));
            }

            let head = HeadData::new(
                id.clone(),
                player.dimension.clone(),
                player.position[0],
                player.position[1],
                player.position[2],
                player.yaw,
                texture,
            );
            state.heads.insert(id.clone(), head.clone());
            drop(state);

            spawn_head_entities(ctx, &head);
            save_state(ctx);
            ctx.reply(format!(
                "{PFX}§aPhantom head §f{id}§a created at your location."
            ));
        }
        "delete" => {
            let id = match args.get(1) {
                Some(s) if !s.is_empty() => s.clone(),
                _ => return Err(format!("{PFX}§eUsage: /ph delete <id>")),
            };
            let removed = {
                let mut state = STATE.lock().unwrap();
                state.heads.remove(&id)
            };
            let Some(head) = removed else {
                return Err(format!("{PFX}§cNo head found with ID: §f{id}§c."));
            };
            despawn_head_entities(ctx, &id, &head.world);
            save_state(ctx);
            ctx.reply(format!("{PFX}§cHead §f{id}§c deleted."));
        }
        "clone" => {
            let Some(player) = ctx.try_player().cloned() else {
                return Err(format!("{PFX}§cThis command requires a player."));
            };
            let src = match args.get(1) {
                Some(s) => s.clone(),
                None => return Err(format!("{PFX}§eUsage: /ph clone <id> <new_id>")),
            };
            let dst = match args.get(2) {
                Some(s) => s.clone(),
                None => return Err(format!("{PFX}§eUsage: /ph clone <id> <new_id>")),
            };

            let mut state = STATE.lock().unwrap();
            if state.heads.contains_key(&dst) {
                return Err(format!("{PFX}§cID §f{dst}§c is already in use."));
            }
            let Some(source) = state.heads.get(&src).cloned() else {
                return Err(format!("{PFX}§cNo head found with ID: §f{src}§c."));
            };

            let mut copy = source;
            copy.id = dst.clone();
            copy.world = player.dimension;
            copy.x = player.position[0];
            copy.y = player.position[1];
            copy.z = player.position[2];
            copy.yaw = player.yaw;

            state.heads.insert(dst.clone(), copy.clone());
            drop(state);

            spawn_head_entities(ctx, &copy);
            save_state(ctx);
            ctx.reply(format!("{PFX}§aCloned §f{src}§a to §f{dst}§a."));
        }
        "reload" => {
            let heads_to_spawn = {
                let mut state = STATE.lock().unwrap();
                state.initialized = false;
                state.heads.clear();
                state.cooldowns.clear();
                state.anim_ticks.clear();
                ctx.console("kill @e[tag=ph_entity]");
                load_state(ctx);
                let state = STATE.lock().unwrap();
                state.heads.values().cloned().collect::<Vec<_>>()
            };
            for h in &heads_to_spawn {
                if h.enabled {
                    spawn_head_entities(ctx, h);
                }
            }
            ctx.reply(format!("{PFX}§aPhantomHeads reloaded."));
        }
        "info" => {
            let id = match args.get(1) {
                Some(s) => s,
                None => return Err(format!("{PFX}§eUsage: /ph info <id>")),
            };
            let state = STATE.lock().unwrap();
            let Some(h) = state.heads.get(id) else {
                return Err(format!("{PFX}§cNo head found with ID: §f{id}§c."));
            };

            ctx.reply(format!("{PFX}§e--- §f{} §e---", h.id));
            ctx.reply(format!(
                "{PFX}  Status:    {}",
                if h.enabled {
                    "§aEnabled"
                } else {
                    "§cDisabled"
                }
            ));
            ctx.reply(format!(
                "{PFX}  Hologram:  {}",
                if h.hologram_enabled {
                    "§aOn"
                } else {
                    "§cOff"
                }
            ));
            ctx.reply(format!(
                "{PFX}  Location:  §f{} {} {} {}",
                h.world,
                fmt_f(h.x),
                fmt_f(h.y),
                fmt_f(h.z)
            ));
            ctx.reply(format!(
                "{PFX}  Animation: §f{} §7(speed {})",
                h.animation_style,
                fmt_f(h.speed_multiplier)
            ));
            ctx.reply(format!(
                "{PFX}  Particle:  §f{} §7density={}",
                h.particle_type,
                fmt_f(h.particle_density)
            ));
            ctx.reply(format!(
                "{PFX}  Ambient:   §f{}  §7Click: §f{}",
                h.ambient_effect, h.click_effect
            ));
            ctx.reply(format!("{PFX}  Cooldown:  §f{}s", h.cooldown_seconds));
            ctx.reply(format!("{PFX}  Lines ({}):", h.lines.len()));
            for (i, line) in h.lines.iter().enumerate() {
                ctx.reply(format!("{PFX}    §f{i}: {line}"));
            }
            ctx.reply(format!(
                "{PFX}  Actions — player: §f{}§7  console: §f{}§7  message: §f{}",
                h.player_commands.len(),
                h.console_commands.len(),
                h.messages.len()
            ));
        }
        "list" => {
            let state = STATE.lock().unwrap();
            if state.heads.is_empty() {
                ctx.reply(format!("{PFX}§eNo heads created yet."));
                return Ok(());
            }
            ctx.reply(format!("{PFX}§eAll heads ({}):", state.heads.len()));
            for h in state.heads.values() {
                let status = if h.enabled { "§aON" } else { "§cOFF" };
                ctx.reply(format!(
                    "{PFX}  §f{} {} §7@ {} {} {} {}",
                    h.id,
                    status,
                    h.world,
                    fmt_f(h.x),
                    fmt_f(h.y),
                    fmt_f(h.z)
                ));
            }
        }
        "near" => {
            let Some(player) = ctx.try_player().cloned() else {
                return Err(format!("{PFX}§cThis command requires a player."));
            };
            let radius: f64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(32.0);
            let r2 = radius * radius;
            let state = STATE.lock().unwrap();
            let mut matches: Vec<(String, f64)> = Vec::new();
            for h in state.heads.values() {
                if h.world == player.dimension {
                    let dx = h.x - player.position[0];
                    let dy = h.y - player.position[1];
                    let dz = h.z - player.position[2];
                    let dist2 = dx * dx + dy * dy + dz * dz;
                    if dist2 <= r2 {
                        matches.push((h.id.clone(), dist2.sqrt()));
                    }
                }
            }
            if matches.is_empty() {
                ctx.reply(format!("{PFX}§eNo heads within {radius:.1} blocks."));
                return Ok(());
            }
            ctx.reply(format!(
                "{PFX}§eHeads within {radius:.1} blocks ({}):",
                matches.len()
            ));
            for (hid, d) in matches {
                ctx.reply(format!("{PFX}  §f{hid} §7({:.1} blocks)", d));
            }
        }
        "enable" | "disable" | "toggle" => {
            let id = match args.get(1) {
                Some(s) => s.clone(),
                None => return Err(format!("{PFX}§eUsage: /ph {sub} <id>")),
            };
            let mut state = STATE.lock().unwrap();
            let Some(h) = state.heads.get_mut(&id) else {
                return Err(format!("{PFX}§cNo head found with ID: §f{id}§c."));
            };
            let new_state = if sub == "enable" {
                true
            } else if sub == "disable" {
                false
            } else {
                !h.enabled
            };
            h.enabled = new_state;
            let head_clone = h.clone();
            drop(state);

            if new_state {
                spawn_head_entities(ctx, &head_clone);
                ctx.reply(format!("{PFX}§f{id} §aenabled§7."));
            } else {
                despawn_head_entities(ctx, &id, &head_clone.world);
                ctx.reply(format!("{PFX}§f{id} §cdisabled§7."));
            }
            save_state(ctx);
        }
        "hologram" => {
            // /ph hologram <enable|disable> <id>
            if args.len() < 3 {
                return Err(format!("{PFX}§eUsage: /ph hologram <enable|disable> <id>"));
            }
            let enable = args
                .get(1)
                .map(|s| s.eq_ignore_ascii_case("enable"))
                .unwrap_or(false);
            let id = args.get(2).cloned().unwrap_or_default();

            let mut state = STATE.lock().unwrap();
            let Some(h) = state.heads.get_mut(&id) else {
                return Err(format!("{PFX}§cNo head found with ID: §f{id}§c."));
            };
            h.hologram_enabled = enable;
            let head_clone = h.clone();
            drop(state);

            respawn_lines_only(ctx, &head_clone);
            save_state(ctx);
            let status = if enable { "§aenabled" } else { "§cdisabled" };
            ctx.reply(format!("{PFX}Hologram for §f{id} {status}§7."));
        }
        "movehere" => {
            let Some(player) = ctx.try_player().cloned() else {
                return Err(format!("{PFX}§cThis command requires a player."));
            };
            let id = match args.get(1) {
                Some(s) => s.clone(),
                None => return Err(format!("{PFX}§eUsage: /ph movehere <id>")),
            };
            let mut state = STATE.lock().unwrap();
            let Some(h) = state.heads.get_mut(&id) else {
                return Err(format!("{PFX}§cNo head found with ID: §f{id}§c."));
            };
            despawn_head_entities(ctx, &id, &h.world);
            h.world = player.dimension;
            h.x = player.position[0];
            h.y = player.position[1];
            h.z = player.position[2];
            h.yaw = player.yaw;
            let head_clone = h.clone();
            drop(state);

            spawn_head_entities(ctx, &head_clone);
            save_state(ctx);
            ctx.reply(format!("{PFX}§f{id} §7moved to your location."));
        }
        "teleport" | "tp" => {
            let id = match args.get(1) {
                Some(s) => s.clone(),
                None => return Err(format!("{PFX}§eUsage: /ph teleport <id>")),
            };
            let state = STATE.lock().unwrap();
            let Some(h) = state.heads.get(&id) else {
                return Err(format!("{PFX}§cNo head found with ID: §f{id}§c."));
            };
            let (dim, x, y, z) = (h.world.clone(), h.x, h.y, h.z);
            let name = ctx.sender_name();
            drop(state);

            ctx.console(format!(
                "execute in {dim} run tp {name} {x:.2} {y:.2} {z:.2}"
            ));
            ctx.reply(format!("{PFX}§aTeleported to §f{id}§7."));
        }
        "line" => {
            // /ph line <add|set|remove|swap|list> <id> [...]
            if args.len() < 3 {
                return Err(format!(
                    "{PFX}§eUsage: /ph line <add|set|remove|swap|list> <id> [...]"
                ));
            }
            let action = args
                .get(1)
                .map(|s| s.to_ascii_lowercase())
                .unwrap_or_default();
            let id = args.get(2).cloned().unwrap_or_default();

            let mut state = STATE.lock().unwrap();
            let Some(h) = state.heads.get_mut(&id) else {
                return Err(format!("{PFX}§cNo head found with ID: §f{id}§c."));
            };

            match action.as_str() {
                "add" => {
                    let text = args.rest(3);
                    if text.is_empty() {
                        return Err(format!("{PFX}§eUsage: /ph line add <id> <text>"));
                    }
                    h.lines.push(text);
                    let idx = h.lines.len() - 1;
                    let head_clone = h.clone();
                    drop(state);
                    respawn_lines_only(ctx, &head_clone);
                    save_state(ctx);
                    ctx.reply(format!("{PFX}§aLine {idx} added."));
                }
                "set" => {
                    let idx: usize = match args.get(3).and_then(|s| s.parse().ok()) {
                        Some(i) => i,
                        None => {
                            return Err(format!("{PFX}§eUsage: /ph line set <id> <index> <text>"));
                        }
                    };
                    let text = args.rest(4);
                    if text.is_empty() {
                        return Err(format!("{PFX}§eUsage: /ph line set <id> <index> <text>"));
                    }
                    if idx >= h.lines.len() {
                        return Err(format!(
                            "{PFX}§cIndex out of range (0–{}).",
                            h.lines.len().saturating_sub(1)
                        ));
                    }
                    h.lines[idx] = text;
                    let head_clone = h.clone();
                    drop(state);
                    respawn_lines_only(ctx, &head_clone);
                    save_state(ctx);
                    ctx.reply(format!("{PFX}§aLine {idx} updated."));
                }
                "remove" => {
                    let idx: usize = match args.get(3).and_then(|s| s.parse().ok()) {
                        Some(i) => i,
                        None => return Err(format!("{PFX}§eUsage: /ph line remove <id> <index>")),
                    };
                    if idx >= h.lines.len() {
                        return Err(format!("{PFX}§cIndex out of range."));
                    }
                    h.lines.remove(idx);
                    let head_clone = h.clone();
                    drop(state);
                    respawn_lines_only(ctx, &head_clone);
                    save_state(ctx);
                    ctx.reply(format!("{PFX}§cLine {idx} removed."));
                }
                "swap" => {
                    let i: usize = match args.get(3).and_then(|s| s.parse().ok()) {
                        Some(v) => v,
                        None => {
                            return Err(format!(
                                "{PFX}§eUsage: /ph line swap <id> <index1> <index2>"
                            ));
                        }
                    };
                    let j: usize = match args.get(4).and_then(|s| s.parse().ok()) {
                        Some(v) => v,
                        None => {
                            return Err(format!(
                                "{PFX}§eUsage: /ph line swap <id> <index1> <index2>"
                            ));
                        }
                    };
                    if i >= h.lines.len() || j >= h.lines.len() {
                        return Err(format!("{PFX}§cIndex out of range."));
                    }
                    h.lines.swap(i, j);
                    let head_clone = h.clone();
                    drop(state);
                    respawn_lines_only(ctx, &head_clone);
                    save_state(ctx);
                    ctx.reply(format!("{PFX}§aLines {i} and {j} swapped."));
                }
                "list" => {
                    ctx.reply(format!("{PFX}§eLines for §f{id}§e:"));
                    if h.lines.is_empty() {
                        ctx.reply(format!("{PFX}  §7(none)"));
                    } else {
                        for (i, line) in h.lines.iter().enumerate() {
                            ctx.reply(format!("{PFX}  §f{i}: {line}"));
                        }
                    }
                }
                _ => {
                    return Err(format!(
                        "{PFX}§cUnknown line action. Use add/set/remove/swap/list."
                    ));
                }
            }
        }
        "action" => {
            // /ph action <id> <add|remove|list> <player|console|message> [value]
            if args.len() < 4 {
                return Err(format!(
                    "{PFX}§eUsage: /ph action <id> <add|remove|list> <player|console|message> [value]"
                ));
            }
            let id = args.get(1).cloned().unwrap_or_default();
            let op = args
                .get(2)
                .map(|s| s.to_ascii_lowercase())
                .unwrap_or_default();
            let action_type = args
                .get(3)
                .map(|s| s.to_ascii_lowercase())
                .unwrap_or_default();

            let mut state = STATE.lock().unwrap();
            let Some(h) = state.heads.get_mut(&id) else {
                return Err(format!("{PFX}§cNo head found with ID: §f{id}§c."));
            };

            let list = match action_type.as_str() {
                "player" => &mut h.player_commands,
                "console" => &mut h.console_commands,
                "message" => &mut h.messages,
                _ => return Err(format!("{PFX}§cType must be player, console, or message.")),
            };

            match op.as_str() {
                "add" => {
                    let val = args.rest(4);
                    if val.is_empty() {
                        return Err(format!("{PFX}§eProvide a value to add."));
                    }
                    list.push(val.clone());
                    drop(state);
                    save_state(ctx);
                    ctx.reply(format!("{PFX}§aAdded {action_type} action: §f{val}"));
                }
                "remove" => {
                    let idx: usize = match args.get(4).and_then(|s| s.parse().ok()) {
                        Some(i) => i,
                        None => return Err(format!("{PFX}§eProvide an index to remove.")),
                    };
                    if idx >= list.len() {
                        return Err(format!("{PFX}§cIndex out of range."));
                    }
                    list.remove(idx);
                    drop(state);
                    save_state(ctx);
                    ctx.reply(format!("{PFX}§cRemoved {action_type} action {idx}."));
                }
                "list" => {
                    ctx.reply(format!("{PFX}§e{action_type} actions for §f{id}§e:"));
                    if list.is_empty() {
                        ctx.reply(format!("{PFX}  §7(none)"));
                    } else {
                        for (i, item) in list.iter().enumerate() {
                            ctx.reply(format!("{PFX}  §f{i}: {item}"));
                        }
                    }
                }
                _ => return Err(format!("{PFX}§cOp must be add, remove, or list.")),
            }
        }
        _ => {
            return Err(format!("{PFX}§cUnknown subcommand. Use /ph for help."));
        }
    }

    Ok(())
}

pub fn plugin() -> Plugin {
    Plugin::new("phantomheads")
        // Command /ph for full PhantomHeads management
        .command(
            "ph [sub:word] [a:word] [b:word] [c:word] [d:word] [e:text]",
            2,
            |ctx, args| handle_ph(ctx, args),
        )
        // Click interaction: right-click on interaction entity
        .on_action(|ctx, action| {
            if let Action::InteractEntity { entity, .. } = action
                && let Some(head_id) = find_head_by_entity_id(ctx, *entity)
            {
                trigger_head_interaction(ctx, &head_id);
                return true;
            }
            false
        })
        // Click interaction: left-click (attack) on interaction entity
        .on_attack(|ctx, entity_id, _kind| {
            if let Some(head_id) = find_head_by_entity_id(ctx, entity_id) {
                trigger_head_interaction(ctx, &head_id);
                return true;
            }
            false
        })
        // Repeating animation and particle loop
        .every(4, |ctx| {
            load_state(ctx);

            let players = ctx.online();
            if players.is_empty() {
                return;
            }

            let heads: Vec<HeadData> = {
                let state = STATE.lock().unwrap();
                state.heads.values().cloned().collect()
            };
            sync_personal(ctx, &heads, &players);

            for head in heads {
                if !head.enabled {
                    continue;
                }

                // Check if any player is in view range (48 blocks)
                let mut near_players = false;
                for p in &players {
                    if p.dimension == head.world {
                        let dx = head.x - p.position[0];
                        let dy = head.y - p.position[1];
                        let dz = head.z - p.position[2];
                        if dx * dx + dy * dy + dz * dz <= 2304.0 {
                            near_players = true;
                            break;
                        }
                    }
                }
                if !near_players {
                    continue;
                }

                // Advance animation state
                let tick = {
                    let mut state = STATE.lock().unwrap();
                    let t = state.anim_ticks.entry(head.id.clone()).or_insert(0.0);
                    *t += 4.0;
                    *t
                };

                // Rotation and bobbing animation
                let speed = head.speed_multiplier;
                let (bob_y, yaw) = if head.animation_style.eq_ignore_ascii_case("SLERPING") {
                    let dy = (tick * BOB_SPEED * speed * 0.5).sin() * BOB_AMPLITUDE * 0.5;
                    let y_rot = head.yaw + 90.0 * (tick * 0.05 * speed).sin() as f32;
                    (head.y + dy, y_rot)
                } else {
                    let dy = (tick * BOB_SPEED * speed).sin() * BOB_AMPLITUDE;
                    let y_rot = (head.yaw + (tick as f32 * ROT_SPEED * speed as f32)) % 360.0;
                    (head.y + dy, y_rot)
                };

                let dim = &head.world;
                let id = &head.id;
                ctx.console(format!(
                    "execute in {dim} run tp @e[tag=ph_{id}_head] {:.2} {:.2} {:.2} {:.1} 0",
                    head.x, bob_y, head.z, yaw
                ));

                // Ambient particles
                let p = particle_name(&head.particle_type);
                let (x, y, z) = (head.x, bob_y + 0.4, head.z);
                ctx.console(format!(
                    "execute in {dim} run particle {p} {x:.2} {y:.2} {z:.2} 0.3 0.2 0.3 0 2"
                ));
            }
        })
}

// ---------------------------------------------------------------------------
// Personal heads (06/11): a `%player_name%` head shows every viewer their OWN skin, as the Paper plugin did with a
// private entity per viewer (`showEntity`/`hideEntity`). One item_display per nearby viewer, hidden from everyone
// else with `Host::hide_entity` (engine patch). The bob/rotate animation moves them with the head (same tag).
// ---------------------------------------------------------------------------
static PERSONAL: std::sync::Mutex<Vec<(String, u128, u32, String)>> =
    std::sync::Mutex::new(Vec::new()); // (head, viewer, entity, dim)

fn personal_nbt(head_id: &str, viewer_name: &str) -> horizon_world::nbt::Compound {
    use horizon_world::nbt::{JavaString, Tag};
    let s = |v: &str| Tag::String(JavaString::from(v));
    let profile = Tag::compound([("name", s(viewer_name))]);
    let item = Tag::compound([
        ("id", s("minecraft:player_head")),
        ("count", Tag::Int(1)),
        (
            "components",
            Tag::compound([("minecraft:profile", profile)]),
        ),
    ]);
    let mut c = horizon_world::nbt::Compound::new();
    c.insert(JavaString::from("item"), item);
    c.insert(
        JavaString::from("Tags"),
        Tag::List(vec![
            s("ph_entity"),
            s(&format!("ph_{head_id}")),
            s(&format!("ph_{head_id}_head")),
            s(&format!("ph_{head_id}_personal")),
        ]),
    );
    c
}

/// Forget (and remove) the personal heads of `head_id` (all of them when the head is respawned or deleted).
fn clear_personal(ctx: &mut Ctx, head_id: &str) {
    let gone: Vec<(u32, String)> = {
        let mut p = PERSONAL.lock().unwrap_or_else(|e| e.into_inner());
        let g = p
            .iter()
            .filter(|x| x.0 == head_id)
            .map(|x| (x.2, x.3.clone()))
            .collect();
        p.retain(|x| x.0 != head_id);
        g
    };
    for (e, dim) in gone {
        ctx.host.remove_entity(&dim, e);
    }
}

/// Keep one personal head per viewer within 48 blocks of each `%player_name%` head, each hidden from the others.
fn sync_personal(ctx: &mut Ctx, heads: &[HeadData], players: &[hplug::Player]) {
    let dynamic: Vec<&HeadData> = heads
        .iter()
        .filter(|h| {
            h.enabled
                && h.texture
                    .as_deref()
                    .is_some_and(|t| t.eq_ignore_ascii_case("%player_name%"))
        })
        .collect();
    let mut keep: Vec<(String, u128)> = Vec::new();
    for h in &dynamic {
        for v in players.iter().filter(|p| p.dimension == h.world) {
            let d2 = (h.x - v.position[0]).powi(2)
                + (h.y - v.position[1]).powi(2)
                + (h.z - v.position[2]).powi(2);
            if d2 > 2304.0 {
                continue;
            }
            keep.push((h.id.clone(), v.uuid));
            let have = PERSONAL
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .any(|x| x.0 == h.id && x.1 == v.uuid);
            if !have
                && let Some(e) = ctx.host.summon(
                    &h.world,
                    "minecraft:item_display",
                    [h.x, h.y, h.z],
                    personal_nbt(&h.id, &v.name),
                )
            {
                PERSONAL.lock().unwrap_or_else(|e| e.into_inner()).push((
                    h.id.clone(),
                    v.uuid,
                    e,
                    h.world.clone(),
                ));
            }
        }
    }
    // out of range / gone viewers lose theirs
    let drop: Vec<(u32, String)> = {
        let mut p = PERSONAL.lock().unwrap_or_else(|e| e.into_inner());
        let d = p
            .iter()
            .filter(|x| !keep.iter().any(|k| k.0 == x.0 && k.1 == x.1))
            .map(|x| (x.2, x.3.clone()))
            .collect();
        p.retain(|x| keep.iter().any(|k| k.0 == x.0 && k.1 == x.1));
        d
    };
    for (e, dim) in drop {
        ctx.host.remove_entity(&dim, e);
    }
    // everybody sees only their own
    let all: Vec<(u128, u32)> = PERSONAL
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .map(|x| (x.1, x.2))
        .collect();
    for p in players {
        for (owner, e) in &all {
            ctx.host.hide_entity_from(p.uuid, *e, p.uuid != *owner);
        }
    }
}
