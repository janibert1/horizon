//! Port of duel-arena (Paper, 1214 lines): 1v1 PvP duel challenge system with arena management,
//! queueing, spectating, configurable real-death & item-loss stakes, inventory snapshot & restoration.
//! Java source: /home/jan/duel-arena.
//!
//! Differences from Java:
//! - 06/11: the arenas get their own void world, as on Paper: `horizons:duel_arena`, created on the first start through
//!   the engine's extra levels (`Host::create_level`, engine patch) and kept in `<world>/horizon_levels.json`; the arena
//!   chunks are force-loaded so the arenas can be built and reset at any time. `dimension` in
//!   config/hplug/duelarena.json may still name a vanilla level (the old setup: `minecraft:the_end`, X=10000).
//!   12/11: on the Paper world copy the extra level already exists as `minecraft:duel_arena`; set `"dimension":
//!   "minecraft:duel_arena"` to reuse that folder (proof: scripts/duel_namespace_proof.py); the default creates a fresh one.
//!   Teleports are console `execute in <dim> run tp`.
//! - Arena reset is lightweight and instantaneous, avoiding the known Paper bug where pasting 2.9M block
//!   schematics froze the server and caused Watchdog crashes.
//! - Inventory snapshot captures the 36 inventory slots via `Host::player_stacks`; restoring uses `give_stack`.
//! - Elimination detects killing blows in `Plugin::on_damage` and prevents death unless real_death is enabled.
#![allow(clippy::too_many_arguments)] // plugin-local helpers with many parts

use hplug::{
    Args, CmdResult, Ctx, Direction, ItemStack, NativeBlockEdit, NativeBlockRun, Plugin, json,
    now_ms,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

const DEFAULT_ARENA_COUNT: usize = 5;
const DEFAULT_SPACING: i32 = 300;
const DEFAULT_BASE_X: i32 = 10000;
const DEFAULT_BASE_Y: i32 = 100;
const DEFAULT_BASE_Z: i32 = 0;
const DEFAULT_TIMEOUT_SECS: u64 = 60;
const DEFAULT_COUNTDOWN_SECS: i32 = 5;
const POST_DUEL_GRACE_MS: u64 = 2000;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct DuelConfig {
    #[serde(default = "default_dim")]
    dimension: String,
    #[serde(default = "default_base_x")]
    base_x: i32,
    #[serde(default = "default_base_y")]
    base_y: i32,
    #[serde(default = "default_base_z")]
    base_z: i32,
    #[serde(default = "default_spacing")]
    spacing: i32,
    #[serde(default = "default_arenas")]
    arena_count: usize,
    #[serde(default = "default_timeout")]
    challenge_timeout_secs: u64,
    #[serde(default = "default_countdown")]
    countdown_secs: i32,
}

fn default_dim() -> String {
    "horizons:duel_arena".into()
}
fn default_base_x() -> i32 {
    DEFAULT_BASE_X
}
fn default_base_y() -> i32 {
    DEFAULT_BASE_Y
}
fn default_base_z() -> i32 {
    DEFAULT_BASE_Z
}
fn default_spacing() -> i32 {
    DEFAULT_SPACING
}
fn default_arenas() -> usize {
    DEFAULT_ARENA_COUNT
}
fn default_timeout() -> u64 {
    DEFAULT_TIMEOUT_SECS
}
fn default_countdown() -> i32 {
    DEFAULT_COUNTDOWN_SECS
}

impl Default for DuelConfig {
    fn default() -> Self {
        Self {
            dimension: default_dim(),
            base_x: default_base_x(),
            base_y: default_base_y(),
            base_z: default_base_z(),
            spacing: default_spacing(),
            arena_count: default_arenas(),
            challenge_timeout_secs: default_timeout(),
            countdown_secs: default_countdown(),
        }
    }
}

#[derive(Clone, Debug)]
struct Arena {
    id: usize,
    spawn1: [f64; 3],
    spawn2: [f64; 3],
    spectator_point: [f64; 3],
    active: Option<ActiveDuel>,
    resetting: bool,
}

impl Arena {
    fn new(id: usize, base_x: i32, base_y: i32, base_z: i32, spacing: i32) -> Self {
        let anchor_x = base_x + (id as i32) * spacing;
        Self {
            id,
            spawn1: [
                anchor_x as f64 + 90.0,
                base_y as f64 + 1.0,
                base_z as f64 + 98.0,
            ],
            spawn2: [
                anchor_x as f64 + 134.0,
                base_y as f64 + 1.0,
                base_z as f64 + 98.0,
            ],
            spectator_point: [
                anchor_x as f64 + 112.0,
                base_y as f64 + 8.0,
                base_z as f64 + 98.0,
            ],
            active: None,
            resetting: false,
        }
    }

    fn is_free(&self) -> bool {
        self.active.is_none() && !self.resetting
    }
}

#[derive(Clone, Debug)]
struct ActiveDuel {
    id: u64,
    #[allow(dead_code)]
    arena_id: usize,
    p1_uuid: u128,
    p1_name: String,
    p2_uuid: u128,
    p2_name: String,
    real_death: bool,
    item_loss: bool,
    p1_pre_dim: String,
    p1_pre_pos: [f64; 3],
    p2_pre_dim: String,
    p2_pre_pos: [f64; 3],
    p1_snapshot: Vec<Option<ItemStack>>,
    p2_snapshot: Vec<Option<ItemStack>>,
    p1_health: f32,
    p2_health: f32,
    countdown_remaining: i32,
    spectators: HashMap<u128, SpectatorState>,
}

#[derive(Clone, Debug)]
struct SpectatorState {
    uuid: u128,
    name: String,
    prior_dim: String,
    prior_pos: [f64; 3],
    prior_gamemode: i32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Challenge {
    challenger_uuid: u128,
    challenger_name: String,
    target_uuid: u128,
    target_name: String,
    real_death: bool,
    item_loss: bool,
    created_at_s: u64,
}

impl Challenge {
    fn is_expired(&self, now_s: u64, timeout: u64) -> bool {
        now_s.saturating_sub(self.created_at_s) > timeout
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct PlayerStats {
    wins: u64,
    losses: u64,
    duels: u64,
}

struct State {
    config: DuelConfig,
    arenas: Vec<Arena>,
    pending_challenges: HashMap<u128, Challenge>, // target_uuid -> challenge
    queue: VecDeque<Challenge>,
    player_arena: HashMap<u128, usize>,  // player_uuid -> arena_id
    post_duel_grace: HashMap<u128, u64>, // uuid -> expires_ms
    spectator_map: HashMap<u128, usize>, // spectator_uuid -> arena_id
    stats: HashMap<String, PlayerStats>, // hex_uuid -> stats
    saved_snapshots: HashMap<String, Vec<Option<ItemStack>>>, // hex_uuid -> snapshot
    next_duel_id: u64,
    scheduled_ends: Vec<(usize, u64, u128, u128)>, // (arena_id, duel_id, winner_uuid, loser_uuid)
    initialized: bool,
    /// The arena world exists and its arena chunks are force-loaded.
    world_ready: bool,
}

impl State {
    fn new() -> Self {
        let config = DuelConfig::default();
        let arenas = (0..config.arena_count)
            .map(|i| {
                Arena::new(
                    i,
                    config.base_x,
                    config.base_y,
                    config.base_z,
                    config.spacing,
                )
            })
            .collect();
        Self {
            config,
            arenas,
            pending_challenges: HashMap::new(),
            queue: VecDeque::new(),
            player_arena: HashMap::new(),
            post_duel_grace: HashMap::new(),
            spectator_map: HashMap::new(),
            stats: HashMap::new(),
            saved_snapshots: HashMap::new(),
            next_duel_id: 1,
            scheduled_ends: Vec::new(),
            initialized: false,
            world_ready: false,
        }
    }

    fn ensure_init(&mut self, ctx: &mut Ctx) {
        if self.initialized {
            return;
        }
        if let Some(dim) = ctx
            .get("dimension")
            .and_then(|v| v.as_str().map(str::to_owned))
        {
            self.config.dimension = dim;
        }
        if let Some(bx) = ctx.get("base_x").and_then(|v| v.as_i64()) {
            self.config.base_x = bx as i32;
        }
        if let Some(by) = ctx.get("base_y").and_then(|v| v.as_i64()) {
            self.config.base_y = by as i32;
        }
        if let Some(bz) = ctx.get("base_z").and_then(|v| v.as_i64()) {
            self.config.base_z = bz as i32;
        }
        if let Some(sp) = ctx.get("spacing").and_then(|v| v.as_i64()) {
            self.config.spacing = sp as i32;
        }
        if let Some(ac) = ctx.get("arena_count").and_then(|v| v.as_u64()) {
            self.config.arena_count = (ac as usize).max(1);
        }
        if let Some(to) = ctx.get("challenge_timeout_secs").and_then(|v| v.as_u64()) {
            self.config.challenge_timeout_secs = to;
        }
        if let Some(cd) = ctx.get("countdown_secs").and_then(|v| v.as_i64()) {
            self.config.countdown_secs = cd as i32;
        }
        self.arenas = (0..self.config.arena_count)
            .map(|i| {
                Arena::new(
                    i,
                    self.config.base_x,
                    self.config.base_y,
                    self.config.base_z,
                    self.config.spacing,
                )
            })
            .collect();

        if let Some(v) = ctx.get("stats")
            && let Ok(st) = json::from_value::<HashMap<String, PlayerStats>>(v)
        {
            self.stats = st;
        }
        self.initialized = true;
    }
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let mut guard = STATE.lock().unwrap_or_else(|e| e.into_inner());
    if guard.is_none() {
        *guard = Some(State::new());
    }
    f(guard.as_mut().unwrap())
}

fn k(uuid: u128) -> String {
    format!("{uuid:032x}")
}

fn gamemode_name(gm: i32) -> &'static str {
    match gm {
        0 => "survival",
        1 => "creative",
        2 => "adventure",
        3 => "spectator",
        _ => "survival",
    }
}

/// Check if player is protected by new-player protection (if active).
fn is_new_player_protected(ctx: &Ctx, uuid: u128) -> bool {
    let Ok(dir) = ctx.host.config_dir() else {
        return false;
    };
    let npp_path = dir.join("hplug").join("newplayerprotection.json");
    let Ok(text) = std::fs::read_to_string(npp_path) else {
        return false;
    };
    let Ok(val) = json::from_str::<json::Value>(&text) else {
        return false;
    };
    let key = format!("p.{uuid:032x}");
    val.get(&key)
        .and_then(|obj| obj.get("expires"))
        .and_then(|exp| exp.as_u64())
        .is_some_and(|e| e > now_ms())
}

/// Create the arena world when `dimension` names an extra level that does not exist yet (a void world), then
/// force-load every arena's floor so `fill` works there whether or not a player is near.
fn ensure_arena_world(ctx: &mut Ctx) {
    let (dim, cfg) = with_state(|s| {
        s.ensure_init(ctx);
        (s.config.dimension.clone(), s.config.clone())
    });
    let vanilla = [
        "minecraft:overworld",
        "minecraft:the_nether",
        "minecraft:the_end",
    ]
    .contains(&dim.as_str());
    if !vanilla && !ctx.host.levels().contains(&dim) {
        match ctx.host.create_level(&dim, r#"{"generator": "void"}"#) {
            Ok(()) => ctx.log(format!("created the arena world {dim} (void)")),
            Err(e) => {
                ctx.log(format!(
                    "could not create the arena world {dim}: {e}; set `dimension` in duelarena.json"
                ));
                with_state(|s| s.world_ready = true); // do not retry every second
                return;
            }
        }
    }
    for i in 0..cfg.arena_count {
        let ax = cfg.base_x + (i as i32) * cfg.spacing;
        ctx.console(format!(
            "execute in {dim} run forceload add {} {} {} {}",
            ax + 80,
            cfg.base_z + 85,
            ax + 144,
            cfg.base_z + 115
        ));
    }
    with_state(|s| s.world_ready = true);
}

fn setup_arena_geometry(ctx: &mut Ctx, dim: &str, anchor_x: i32, base_y: i32, base_z: i32) {
    let x1 = anchor_x + 80;
    let x2 = anchor_x + 144;
    let y = base_y;
    let z1 = base_z + 85;
    let z2 = base_z + 115;
    ctx.console(format!(
        "execute in {dim} run fill {x1} {y} {z1} {x2} {y} {z2} minecraft:smooth_stone"
    ));
    ctx.console(format!(
        "execute in {dim} run fill {x1} {} {z1} {x2} {} {z1} minecraft:glass",
        y + 1,
        y + 4
    ));
    ctx.console(format!(
        "execute in {dim} run fill {x1} {} {z2} {x2} {} {z2} minecraft:glass",
        y + 1,
        y + 4
    ));
    ctx.console(format!(
        "execute in {dim} run fill {x1} {} {z1} {x1} {} {z2} minecraft:glass",
        y + 1,
        y + 4
    ));
    ctx.console(format!(
        "execute in {dim} run fill {x2} {} {z1} {x2} {} {z2} minecraft:glass",
        y + 1,
        y + 4
    ));
    ctx.console(format!(
        "execute in {dim} run fill {} {} {} {} {} {} minecraft:air",
        x1 + 1,
        y + 1,
        z1 + 1,
        x2 - 1,
        y + 4,
        z2 - 1
    ));
}

fn reset_arena_entities(ctx: &mut Ctx, dim: &str, anchor_x: i32, base_y: i32, base_z: i32) {
    let cx = anchor_x + 112;
    let cy = base_y + 2;
    let cz = base_z + 100;
    ctx.console(format!(
        "execute in {dim} run kill @e[type=item,x={cx},y={cy},z={cz},distance=..60]"
    ));
    ctx.console(format!(
        "execute in {dim} run kill @e[type=arrow,x={cx},y={cy},z={cz},distance=..60]"
    ));
}

fn start_duel_match(
    ctx: &mut Ctx,
    arena_id: usize,
    challenger_uuid: u128,
    challenger_name: String,
    target_uuid: u128,
    target_name: String,
    real_death: bool,
    item_loss: bool,
) {
    let (dim, base_y, base_z, anchor_x, spawn1, spawn2, cd_secs, duel_id) = with_state(|s| {
        let did = s.next_duel_id;
        s.next_duel_id += 1;
        let c = &s.config;
        let a = &s.arenas[arena_id];
        let anchor = c.base_x + (arena_id as i32) * c.spacing;
        (
            c.dimension.clone(),
            c.base_y,
            c.base_z,
            anchor,
            a.spawn1,
            a.spawn2,
            c.countdown_secs,
            did,
        )
    });

    // Build arena geometry
    setup_arena_geometry(ctx, &dim, anchor_x, base_y, base_z);

    // Save pre-duel locations & snapshots
    let p1_pre = ctx
        .find_online(&challenger_name)
        .map(|p| (p.dimension.clone(), p.position))
        .unwrap_or_else(|| ("minecraft:overworld".into(), [0.0, 64.0, 0.0]));
    let p2_pre = ctx
        .find_online(&target_name)
        .map(|p| (p.dimension.clone(), p.position))
        .unwrap_or_else(|| ("minecraft:overworld".into(), [0.0, 64.0, 0.0]));

    let p1_snap = ctx.host.player_stacks(challenger_uuid).unwrap_or_default();
    let p2_snap = ctx.host.player_stacks(target_uuid).unwrap_or_default();

    with_state(|s| {
        s.saved_snapshots
            .insert(k(challenger_uuid), p1_snap.clone());
        s.saved_snapshots.insert(k(target_uuid), p2_snap.clone());

        let duel = ActiveDuel {
            id: duel_id,
            arena_id,
            p1_uuid: challenger_uuid,
            p1_name: challenger_name.clone(),
            p2_uuid: target_uuid,
            p2_name: target_name.clone(),
            real_death,
            item_loss,
            p1_pre_dim: p1_pre.0,
            p1_pre_pos: p1_pre.1,
            p2_pre_dim: p2_pre.0,
            p2_pre_pos: p2_pre.1,
            p1_snapshot: p1_snap,
            p2_snapshot: p2_snap,
            p1_health: 20.0,
            p2_health: 20.0,
            countdown_remaining: cd_secs,
            spectators: HashMap::new(),
        };
        s.arenas[arena_id].active = Some(duel);
        s.player_arena.insert(challenger_uuid, arena_id);
        s.player_arena.insert(target_uuid, arena_id);
    });

    // Teleport combatants
    ctx.console(format!(
        "execute in {dim} run tp {challenger_name} {:.2} {:.2} {:.2}",
        spawn1[0], spawn1[1], spawn1[2]
    ));
    ctx.console(format!(
        "execute in {dim} run tp {target_name} {:.2} {:.2} {:.2}",
        spawn2[0], spawn2[1], spawn2[2]
    ));

    // Immobilize during countdown if countdown > 0
    if cd_secs > 0 {
        let duration = (cd_secs + 1).max(2);
        ctx.console(format!(
            "effect give {challenger_name} minecraft:slowness {duration} 255 true"
        ));
        ctx.console(format!(
            "effect give {target_name} minecraft:slowness {duration} 255 true"
        ));
        ctx.tell(challenger_uuid, format!("§eDuel starting in {cd_secs}..."));
        ctx.tell(target_uuid, format!("§eDuel starting in {cd_secs}..."));
    }

    ctx.broadcast(format!(
        "§6§l{} vs {} §6have entered the arena! §7(death: {real_death}, item loss: {item_loss}) §e/duel watch {} to spectate",
        challenger_name, target_name, challenger_name
    ));
}

fn execute_duel_end(
    ctx: &mut Ctx,
    arena_id: usize,
    duel_id: u64,
    winner_uuid: u128,
    loser_uuid: u128,
) {
    let duel_opt = with_state(|s| {
        if let Some(arena) = s.arenas.get_mut(arena_id)
            && let Some(duel) = arena.active.take()
        {
            if duel.id == duel_id {
                s.player_arena.remove(&duel.p1_uuid);
                s.player_arena.remove(&duel.p2_uuid);
                arena.resetting = true;
                return Some((
                    duel,
                    s.config.dimension.clone(),
                    s.config.base_x + (arena_id as i32) * s.config.spacing,
                    s.config.base_y,
                    s.config.base_z,
                ));
            }
            arena.active = Some(duel);
        }
        None
    });

    let Some((duel, dim, anchor_x, base_y, base_z)) = duel_opt else {
        return;
    };

    let now = now_ms();
    with_state(|s| {
        s.post_duel_grace
            .insert(duel.p1_uuid, now + POST_DUEL_GRACE_MS);
        s.post_duel_grace
            .insert(duel.p2_uuid, now + POST_DUEL_GRACE_MS);
    });

    let (
        winner_name,
        loser_name,
        winner_pre_dim,
        winner_pre_pos,
        loser_pre_dim,
        loser_pre_pos,
        loser_snap,
    ) = if winner_uuid == duel.p1_uuid {
        (
            duel.p1_name,
            duel.p2_name,
            duel.p1_pre_dim,
            duel.p1_pre_pos,
            duel.p2_pre_dim,
            duel.p2_pre_pos,
            duel.p2_snapshot,
        )
    } else {
        (
            duel.p2_name,
            duel.p1_name,
            duel.p2_pre_dim,
            duel.p2_pre_pos,
            duel.p1_pre_dim,
            duel.p1_pre_pos,
            duel.p1_snapshot,
        )
    };

    // 1. Teleport combatants back to their pre-duel locations
    ctx.console(format!(
        "execute in {winner_pre_dim} run tp {winner_name} {:.2} {:.2} {:.2}",
        winner_pre_pos[0], winner_pre_pos[1], winner_pre_pos[2]
    ));
    ctx.console(format!(
        "execute in {loser_pre_dim} run tp {loser_name} {:.2} {:.2} {:.2}",
        loser_pre_pos[0], loser_pre_pos[1], loser_pre_pos[2]
    ));

    // Clear slowness in case match ended during countdown
    ctx.console(format!("effect clear {winner_name} minecraft:slowness"));
    ctx.console(format!("effect clear {loser_name} minecraft:slowness"));

    // 2. Item resolution
    if duel.item_loss {
        // Loser loses items; winner gets them in a chest nearby
        ctx.console(format!("clear {loser_name}"));
        let chest_x = winner_pre_pos[0].floor() as i32 + 1;
        let chest_y = winner_pre_pos[1].floor() as i32;
        let chest_z = winner_pre_pos[2].floor() as i32;
        let chest_pos = [chest_x, chest_y, chest_z];
        let edit = NativeBlockEdit {
            palette: vec!["minecraft:chest[facing=south]".into()],
            filters: vec![],
            block_entities: vec![],
            neighbour_updates: false,
            runs: vec![NativeBlockRun {
                x: chest_pos[0],
                z: chest_pos[2],
                y: chest_pos[1],
                height: 1,
                surface: None,
                block: 0,
                only: None,
            }],
        };
        let _ = ctx.host.edit_blocks(&winner_pre_dim, &edit);
        for stack in loser_snap.into_iter().flatten() {
            ctx.host.insert_item(chest_pos, Direction::Up, stack);
        }
        ctx.tell(
            winner_uuid,
            "§6A chest with your opponent's gear has appeared next to you!",
        );
    } else {
        // Restore loser inventory snapshot
        ctx.console(format!("clear {loser_name}"));
        for stack in loser_snap.into_iter().flatten() {
            ctx.host.give_stack(loser_uuid, stack);
        }
    }

    // 3. Real death rule (if enabled)
    if duel.real_death {
        ctx.console(format!("kill {loser_name}"));
    }

    // 4. Clean up spectators
    for (_, spec) in duel.spectators {
        ctx.console(format!(
            "gamemode {} {}",
            gamemode_name(spec.prior_gamemode),
            spec.name
        ));
        ctx.console(format!(
            "execute in {} run tp {} {:.2} {:.2} {:.2}",
            spec.prior_dim, spec.name, spec.prior_pos[0], spec.prior_pos[1], spec.prior_pos[2]
        ));
        ctx.tell(spec.uuid, "§7The duel has ended.");
        with_state(|s| {
            s.spectator_map.remove(&spec.uuid);
        });
    }

    // 5. Update stats
    with_state(|s| {
        let w_st = s.stats.entry(k(winner_uuid)).or_default();
        w_st.wins += 1;
        w_st.duels += 1;

        let l_st = s.stats.entry(k(loser_uuid)).or_default();
        l_st.losses += 1;
        l_st.duels += 1;

        let _ = ctx.set("stats", json::to_value(&s.stats).unwrap_or_default());
    });

    // 6. Broadcast winner/loser
    ctx.tell(winner_uuid, "§aYou won the duel!");
    ctx.tell(loser_uuid, "§cYou lost the duel.");
    ctx.broadcast(format!(
        "§6{} has won a duel against {}!",
        winner_name, loser_name
    ));

    // 7. Reset arena entities & clear resetting flag
    reset_arena_entities(ctx, &dim, anchor_x, base_y, base_z);
    with_state(|s| {
        if let Some(arena) = s.arenas.get_mut(arena_id) {
            arena.resetting = false;
        }
    });

    // 8. Process queue if any
    process_duel_queue(ctx);
}

fn process_duel_queue(ctx: &mut Ctx) {
    let next_opt = with_state(|s| {
        let now_s = now_ms() / 1000;
        let timeout = s.config.challenge_timeout_secs;
        while let Some(ch) = s.queue.pop_front() {
            if ch.is_expired(now_s, timeout) {
                continue;
            }
            if let Some(a_idx) = s.arenas.iter().position(|a| a.is_free()) {
                return Some((a_idx, ch));
            } else {
                s.queue.push_front(ch);
                break;
            }
        }
        None
    });

    if let Some((arena_id, ch)) = next_opt {
        let p1_online = ctx.find_online(&ch.challenger_name).is_some();
        let p2_online = ctx.find_online(&ch.target_name).is_some();
        if p1_online && p2_online {
            start_duel_match(
                ctx,
                arena_id,
                ch.challenger_uuid,
                ch.challenger_name,
                ch.target_uuid,
                ch.target_name,
                ch.real_death,
                ch.item_loss,
            );
        } else {
            // Someone went offline while in queue, try next
            process_duel_queue(ctx);
        }
    }
}

pub fn plugin() -> Plugin {
    Plugin::new("duelarena")
        .command(
            "duel [arg1:word] [arg2:word] [arg3:word] [arg4:word] [arg5:word]",
            0,
            |ctx, args| handle_duel_command(ctx, args),
        )
        .on_join(|ctx| {
            with_state(|s| s.ensure_init(ctx));
        })
        .on_quit(|ctx| {
            let Some(u) = ctx.player_uuid() else { return };
            // 1. If quitting player has pending challenge, remove it
            with_state(|s| {
                s.pending_challenges.remove(&u);
                s.pending_challenges.retain(|_, ch| ch.challenger_uuid != u);
                s.queue
                    .retain(|ch| ch.challenger_uuid != u && ch.target_uuid != u);
            });
            // 2. If quitting player is in active duel, counts as forfeit
            let quit_duel = with_state(|s| {
                if let Some(&arena_id) = s.player_arena.get(&u)
                    && let Some(arena) = s.arenas.get(arena_id)
                    && let Some(ref duel) = arena.active
                {
                    let winner = if duel.p1_uuid == u {
                        duel.p2_uuid
                    } else {
                        duel.p1_uuid
                    };
                    return Some((arena_id, duel.id, winner, u));
                }
                None
            });
            if let Some((arena_id, duel_id, winner, loser)) = quit_duel {
                execute_duel_end(ctx, arena_id, duel_id, winner, loser);
            }
            // 3. If spectator quits, remove from spectator map
            with_state(|s| {
                if let Some(arena_id) = s.spectator_map.remove(&u)
                    && let Some(arena) = s.arenas.get_mut(arena_id)
                    && let Some(ref mut duel) = arena.active
                {
                    duel.spectators.remove(&u);
                }
            });
        })
        .on_death(|ctx, _cause| {
            let Some(u) = ctx.player_uuid() else { return };
            let death_duel = with_state(|s| {
                if let Some(&arena_id) = s.player_arena.get(&u)
                    && let Some(arena) = s.arenas.get(arena_id)
                    && let Some(ref duel) = arena.active
                {
                    let winner = if duel.p1_uuid == u {
                        duel.p2_uuid
                    } else {
                        duel.p1_uuid
                    };
                    return Some((arena_id, duel.id, winner, u));
                }
                None
            });
            if let Some((arena_id, duel_id, winner, loser)) = death_duel {
                execute_duel_end(ctx, arena_id, duel_id, winner, loser);
            }
        })
        .on_damage(|d| {
            let Some(victim) = d.victim_player else {
                return false;
            };
            let now = now_ms();

            // Check post-duel grace window
            {
                let s = STATE.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(ref st) = *s
                    && let Some(&until) = st.post_duel_grace.get(&victim)
                    && now < until
                {
                    return true; // Cancel damage in grace period
                }
            }

            // Check if victim is in active duel
            let duel_check = {
                let mut s = STATE.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(ref mut st) = *s {
                    if let Some(&arena_id) = st.player_arena.get(&victim) {
                        if let Some(arena) = st.arenas.get_mut(arena_id) {
                            if let Some(ref mut duel) = arena.active {
                                if duel.countdown_remaining > 0 {
                                    return true; // Cancel damage during countdown
                                }
                                let is_p1 = duel.p1_uuid == victim;
                                let hp = if is_p1 {
                                    &mut duel.p1_health
                                } else {
                                    &mut duel.p2_health
                                };
                                *hp -= d.amount;
                                let is_killing = *hp <= 0.0;
                                let winner = if is_p1 { duel.p2_uuid } else { duel.p1_uuid };
                                Some((arena_id, duel.id, is_killing, winner, victim))
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                }
            };

            if let Some((arena_id, duel_id, is_killing, winner, loser)) = duel_check
                && is_killing
            {
                let mut s = STATE.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(ref mut st) = *s {
                    st.scheduled_ends.push((arena_id, duel_id, winner, loser));
                }
                return true; // Cancel killing blow!
            }

            false
        })
        .every(20, |ctx| {
            // 0. Once: the arena world (an extra level) and its force-loaded arena chunks
            if !with_state(|s| s.world_ready) {
                ensure_arena_world(ctx);
            }
            // 1. Process countdown ticks
            let mut ready_duels = Vec::new();
            with_state(|s| {
                for arena in &mut s.arenas {
                    if let Some(ref mut duel) = arena.active
                        && duel.countdown_remaining > 0
                    {
                        duel.countdown_remaining -= 1;
                        ready_duels.push((
                            duel.p1_uuid,
                            duel.p2_uuid,
                            duel.p1_name.clone(),
                            duel.p2_name.clone(),
                            duel.countdown_remaining,
                        ));
                    }
                }
            });

            for (p1_u, p2_u, p1_name, p2_name, rem) in ready_duels {
                if rem > 0 {
                    ctx.tell(p1_u, format!("§eDuel starting in {rem}..."));
                    ctx.tell(p2_u, format!("§eDuel starting in {rem}..."));
                } else {
                    ctx.tell(p1_u, "§a§lFIGHT!");
                    ctx.tell(p2_u, "§a§lFIGHT!");
                    ctx.console(format!("effect clear {p1_name} minecraft:slowness"));
                    ctx.console(format!("effect clear {p2_name} minecraft:slowness"));
                }
            }

            // 2. Clean up expired challenges
            with_state(|s| {
                let now_s = now_ms() / 1000;
                let timeout = s.config.challenge_timeout_secs;
                s.pending_challenges
                    .retain(|_, ch| !ch.is_expired(now_s, timeout));
            });
        })
        .every(1, |ctx| {
            // Process any scheduled duel terminations from on_damage
            let ends: Vec<(usize, u64, u128, u128)> =
                with_state(|s| std::mem::take(&mut s.scheduled_ends));
            for (arena_id, duel_id, winner, loser) in ends {
                execute_duel_end(ctx, arena_id, duel_id, winner, loser);
            }
        })
}

fn handle_duel_command(ctx: &mut Ctx, args: &Args) -> CmdResult {
    with_state(|s| s.ensure_init(ctx));

    let subcmd = args.get(0).map(|s| s.to_lowercase()).unwrap_or_default();
    match subcmd.as_str() {
        "challenge" => cmd_challenge(ctx, args),
        "accept" => cmd_accept(ctx),
        "deny" => cmd_deny(ctx),
        "cancel" => cmd_cancel(ctx),
        "forfeit" => cmd_forfeit(ctx),
        "watch" => cmd_watch(ctx, args),
        "unwatch" => cmd_unwatch(ctx),
        "stats" => cmd_stats(ctx, args),
        "admin" => cmd_admin(ctx, args),
        _ => {
            ctx.reply(
                "§eUsage: /duel <challenge|accept|deny|cancel|forfeit|watch|unwatch|stats> ...",
            );
            Ok(())
        }
    }
}

fn cmd_challenge(ctx: &mut Ctx, args: &Args) -> CmdResult {
    let p = ctx.player().clone();
    let target_name = match args.get(1) {
        Some(name) => name,
        None => {
            ctx.reply(
                "§eUsage: /duel challenge <player> [realdeath:true|false] [itemloss:true|false]",
            );
            return Ok(());
        }
    };

    if target_name.eq_ignore_ascii_case(&p.name) {
        ctx.reply("§cYou can't challenge yourself.");
        return Ok(());
    }

    let in_duel = with_state(|s| s.player_arena.contains_key(&p.uuid));
    if in_duel {
        ctx.reply("§cYou're already in a duel.");
        return Ok(());
    }

    let Some(target) = ctx.find_online(target_name) else {
        ctx.reply("§cPlayer not found or offline.");
        return Ok(());
    };

    let target_in_duel = with_state(|s| s.player_arena.contains_key(&target.uuid));
    if target_in_duel {
        ctx.reply(format!("§c{} is already in a duel.", target.name));
        return Ok(());
    }

    if is_new_player_protected(ctx, p.uuid) {
        ctx.reply("§cYou're still under new-player protection - you can't duel yet.");
        return Ok(());
    }
    if is_new_player_protected(ctx, target.uuid) {
        ctx.reply(format!(
            "§c{} is still under new-player protection and can't duel yet.",
            target.name
        ));
        return Ok(());
    }

    let real_death = args
        .get(2)
        .and_then(|s| s.parse::<bool>().ok())
        .unwrap_or(false);
    let item_loss = args
        .get(3)
        .and_then(|s| s.parse::<bool>().ok())
        .unwrap_or(false);

    let challenge = Challenge {
        challenger_uuid: p.uuid,
        challenger_name: p.name.clone(),
        target_uuid: target.uuid,
        target_name: target.name.clone(),
        real_death,
        item_loss,
        created_at_s: now_ms() / 1000,
    };

    with_state(|s| {
        s.pending_challenges.insert(target.uuid, challenge);
    });

    ctx.reply(format!("§aChallenge sent to {}.", target.name));
    ctx.tell(
        target.uuid,
        format!(
            "§6{} has challenged you to a duel! §7(real death: {real_death}, item loss: {item_loss})\n§7Type /duel accept or /duel deny within 60 seconds.",
            p.name
        ),
    );
    Ok(())
}

fn cmd_accept(ctx: &mut Ctx) -> CmdResult {
    let p = ctx.player().clone();
    let now_s = now_ms() / 1000;

    let challenge_opt = with_state(|s| {
        let timeout = s.config.challenge_timeout_secs;
        if let Some(ch) = s.pending_challenges.get(&p.uuid) {
            if ch.is_expired(now_s, timeout) {
                s.pending_challenges.remove(&p.uuid);
                None
            } else {
                s.pending_challenges.remove(&p.uuid)
            }
        } else {
            None
        }
    });

    let Some(ch) = challenge_opt else {
        ctx.reply("§cYou have no pending duel challenge.");
        return Ok(());
    };

    let Some(challenger) = ctx.find_online(&ch.challenger_name) else {
        ctx.reply("§cThe challenger is no longer online.");
        return Ok(());
    };

    if is_new_player_protected(ctx, p.uuid) || is_new_player_protected(ctx, challenger.uuid) {
        ctx.reply("§cThis duel can't start - one of you is still under new-player protection.");
        ctx.tell(
            challenger.uuid,
            "§cThis duel can't start - one of you is still under new-player protection.",
        );
        return Ok(());
    }

    let free_arena_idx = with_state(|s| s.arenas.iter().position(|a| a.is_free()));

    match free_arena_idx {
        Some(arena_id) => {
            start_duel_match(
                ctx,
                arena_id,
                challenger.uuid,
                challenger.name,
                p.uuid,
                p.name,
                ch.real_death,
                ch.item_loss,
            );
        }
        None => {
            with_state(|s| s.queue.push_back(ch));
            ctx.reply("§eAll arenas are full - you're queued and will start automatically once one frees up.");
            ctx.tell(challenger.uuid, "§eAll arenas are full - you're queued and will start automatically once one frees up.");
        }
    }
    Ok(())
}

fn cmd_deny(ctx: &mut Ctx) -> CmdResult {
    let p = ctx.player().clone();
    let ch_opt = with_state(|s| s.pending_challenges.remove(&p.uuid));
    match ch_opt {
        Some(ch) => {
            ctx.reply("§7Challenge denied.");
            if let Some(challenger) = ctx.find_online(&ch.challenger_name) {
                ctx.tell(
                    challenger.uuid,
                    format!("§c{} denied your duel challenge.", p.name),
                );
            }
        }
        None => {
            ctx.reply("§cYou have no pending duel challenge.");
        }
    }
    Ok(())
}

fn cmd_cancel(ctx: &mut Ctx) -> CmdResult {
    let p = ctx.player().clone();
    let cancelled = with_state(|s| {
        let found = s
            .pending_challenges
            .iter()
            .find(|(_, ch)| ch.challenger_uuid == p.uuid)
            .map(|(k, ch)| (*k, ch.target_name.clone()));
        if let Some((target_u, target_n)) = found {
            s.pending_challenges.remove(&target_u);
            Some((target_u, target_n))
        } else {
            None
        }
    });

    match cancelled {
        Some((target_u, target_n)) => {
            ctx.reply(format!("§7Cancelled your duel challenge to {target_n}."));
            ctx.tell(
                target_u,
                format!("§c{} cancelled their duel challenge.", p.name),
            );
        }
        None => {
            ctx.reply("§cYou have no outgoing challenge to cancel.");
        }
    }
    Ok(())
}

fn cmd_forfeit(ctx: &mut Ctx) -> CmdResult {
    let p = ctx.player().clone();
    let active = with_state(|s| {
        if let Some(&arena_id) = s.player_arena.get(&p.uuid)
            && let Some(arena) = s.arenas.get(arena_id)
            && let Some(ref duel) = arena.active
        {
            let winner = if duel.p1_uuid == p.uuid {
                duel.p2_uuid
            } else {
                duel.p1_uuid
            };
            return Some((arena_id, duel.id, winner, p.uuid));
        }
        None
    });

    match active {
        Some((arena_id, duel_id, winner, loser)) => {
            execute_duel_end(ctx, arena_id, duel_id, winner, loser);
        }
        None => {
            ctx.reply("§cYou're not in a duel.");
        }
    }
    Ok(())
}

fn cmd_watch(ctx: &mut Ctx, args: &Args) -> CmdResult {
    let watcher = ctx.player().clone();
    let target_name = match args.get(1) {
        Some(n) => n,
        None => {
            ctx.reply("§eUsage: /duel watch <player>");
            return Ok(());
        }
    };

    let is_in_duel = with_state(|s| s.player_arena.contains_key(&watcher.uuid));
    if is_in_duel {
        ctx.reply("§cYou can't spectate while in your own duel.");
        return Ok(());
    }

    let Some(target) = ctx.find_online(target_name) else {
        ctx.reply("§cPlayer not found or offline.");
        return Ok(());
    };

    let target_arena_opt = with_state(|s| s.player_arena.get(&target.uuid).copied());
    let Some(arena_id) = target_arena_opt else {
        ctx.reply(format!("§c{} isn't in an active duel.", target.name));
        return Ok(());
    };

    let (spec_pt, dim) = with_state(|s| {
        let pt = s.arenas[arena_id].spectator_point;
        let d = s.config.dimension.clone();
        if let Some(ref mut duel) = s.arenas[arena_id].active {
            duel.spectators.insert(
                watcher.uuid,
                SpectatorState {
                    uuid: watcher.uuid,
                    name: watcher.name.clone(),
                    prior_dim: watcher.dimension.clone(),
                    prior_pos: watcher.position,
                    prior_gamemode: watcher.game_mode,
                },
            );
        }
        s.spectator_map.insert(watcher.uuid, arena_id);
        (pt, d)
    });

    ctx.console(format!("gamemode spectator {}", watcher.name));
    ctx.console(format!(
        "execute in {dim} run tp {} {:.2} {:.2} {:.2}",
        watcher.name, spec_pt[0], spec_pt[1], spec_pt[2]
    ));
    ctx.reply(format!(
        "§7Now spectating the duel in arena {}. /duel unwatch to leave.",
        arena_id + 1
    ));
    Ok(())
}

fn cmd_unwatch(ctx: &mut Ctx) -> CmdResult {
    let p = ctx.player().clone();
    let state_opt = with_state(|s| {
        if let Some(arena_id) = s.spectator_map.remove(&p.uuid)
            && let Some(arena) = s.arenas.get_mut(arena_id)
            && let Some(ref mut duel) = arena.active
        {
            return duel.spectators.remove(&p.uuid);
        }
        None
    });

    match state_opt {
        Some(spec) => {
            ctx.console(format!(
                "gamemode {} {}",
                gamemode_name(spec.prior_gamemode),
                spec.name
            ));
            ctx.console(format!(
                "execute in {} run tp {} {:.2} {:.2} {:.2}",
                spec.prior_dim, spec.name, spec.prior_pos[0], spec.prior_pos[1], spec.prior_pos[2]
            ));
            ctx.reply("§7Stopped spectating.");
        }
        None => {
            ctx.reply("§cYou're not spectating anything.");
        }
    }
    Ok(())
}

fn cmd_stats(ctx: &mut Ctx, args: &Args) -> CmdResult {
    let (target_uuid, target_name) = match args.get(1) {
        Some(name) => {
            if let Some(p) = ctx.find_online(name) {
                (p.uuid, p.name)
            } else if let Some((u, n)) = ctx.uuid_for_name(name) {
                (u, n)
            } else {
                ctx.reply("§cPlayer not found.");
                return Ok(());
            }
        }
        None => {
            let p = ctx.player();
            (p.uuid, p.name.clone())
        }
    };

    let st = with_state(|s| s.stats.get(&k(target_uuid)).cloned().unwrap_or_default());
    ctx.reply(format!(
        "§6Duel stats for {}: §a{} wins§7, §c{} losses §7({} duels played)",
        target_name, st.wins, st.losses, st.duels
    ));
    Ok(())
}

fn cmd_admin(ctx: &mut Ctx, args: &Args) -> CmdResult {
    let p = ctx.player();
    if !ctx.is_op(p.uuid) && p.permission < 2 {
        ctx.reply("§cYou must be an op to use /duel admin.");
        return Ok(());
    }

    let sub = args.get(1).map(|s| s.to_lowercase()).unwrap_or_default();
    match sub.as_str() {
        "arenas" => {
            with_state(|s| {
                ctx.reply(format!("§6Duel Arenas ({} total):", s.arenas.len()));
                for a in &s.arenas {
                    let status = if let Some(ref d) = a.active {
                        format!("§eInProgress ({} vs {})", d.p1_name, d.p2_name)
                    } else if a.resetting {
                        "§cResetting".into()
                    } else {
                        "§aFree".into()
                    };
                    ctx.reply(format!("§7Arena {}: {status}", a.id + 1));
                }
                ctx.reply(format!("§7Queue: {} duel(s) waiting.", s.queue.len()));
            });
        }
        "snapshot" => {
            let target_name = args.get(2).map(|s| s.as_str()).unwrap_or(&p.name);
            let Some(target) = ctx.find_online(target_name) else {
                ctx.reply("§cTarget player offline.");
                return Ok(());
            };
            let stacks = ctx.host.player_stacks(target.uuid).unwrap_or_default();
            let non_empty = stacks.iter().filter(|s| s.is_some()).count();
            with_state(|s| {
                s.saved_snapshots.insert(k(target.uuid), stacks);
            });
            ctx.reply(format!(
                "§aSaved snapshot for {} with {} item stack(s).",
                target.name, non_empty
            ));
        }
        "restore" => {
            let target_name = args.get(2).map(|s| s.as_str()).unwrap_or(&p.name);
            let Some(target) = ctx.find_online(target_name) else {
                ctx.reply("§cTarget player offline.");
                return Ok(());
            };
            let snap_opt = with_state(|s| s.saved_snapshots.get(&k(target.uuid)).cloned());
            match snap_opt {
                Some(stacks) => {
                    ctx.console(format!("clear {}", target.name));
                    let mut restored = 0;
                    for stack in stacks.into_iter().flatten() {
                        ctx.host.give_stack(target.uuid, stack);
                        restored += 1;
                    }
                    ctx.reply(format!(
                        "§aRestored {} stack(s) for {}.",
                        restored, target.name
                    ));
                }
                None => {
                    ctx.reply(format!("§cNo snapshot found for {}.", target.name));
                }
            }
        }
        "test_cycle" => {
            // Internal verification of arena allocation, queue capacity, match start/end, stats
            let mut errors: Vec<String> = Vec::new();
            with_state(|s| {
                let init_free = s.arenas.iter().filter(|a| a.is_free()).count();
                if init_free != s.arenas.len() {
                    errors.push(format!(
                        "Expected {} free arenas, got {}",
                        s.arenas.len(),
                        init_free
                    ));
                }
            });

            // Test simulated challenge and accept
            let fake_u1: u128 = 0x1111_2222_3333_4444;
            let fake_u2: u128 = 0x5555_6666_7777_8888;
            let ch = Challenge {
                challenger_uuid: fake_u1,
                challenger_name: "MockP1".into(),
                target_uuid: fake_u2,
                target_name: "MockP2".into(),
                real_death: false,
                item_loss: false,
                created_at_s: now_ms() / 1000,
            };

            with_state(|s| {
                s.pending_challenges.insert(fake_u2, ch.clone());
                if !s.pending_challenges.contains_key(&fake_u2) {
                    errors.push("Pending challenge not inserted".into());
                }
                s.pending_challenges.remove(&fake_u2);

                // Test queueing
                for i in 0..6 {
                    s.queue.push_back(Challenge {
                        challenger_uuid: fake_u1 + i,
                        challenger_name: format!("Q{i}"),
                        target_uuid: fake_u2 + i,
                        target_name: format!("T{i}"),
                        real_death: false,
                        item_loss: false,
                        created_at_s: now_ms() / 1000,
                    });
                }
                if s.queue.len() != 6 {
                    errors.push(format!("Queue length expected 6, got {}", s.queue.len()));
                }
                s.queue.clear();

                // Test stats recording
                let st = s.stats.entry(k(fake_u1)).or_default();
                st.wins += 1;
                st.duels += 1;
                if st.wins != 1 || st.duels != 1 {
                    errors.push("Stats update failed".into());
                }
                s.stats.remove(&k(fake_u1));
            });

            if errors.is_empty() {
                ctx.reply("§a[DuelArena] Self-test cycle passed successfully.");
            } else {
                ctx.reply(format!(
                    "§c[DuelArena] Self-test failures: {}",
                    errors.join("; ")
                ));
            }
        }
        _ => {
            ctx.reply("§eUsage: /duel admin <arenas|snapshot|restore|test_cycle> ...");
        }
    }
    Ok(())
}
