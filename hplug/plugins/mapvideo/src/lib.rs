use hplug::{json, Action, Args, CmdResult, Ctx, Damage, Plugin};
use std::collections::HashMap;
use std::sync::Mutex;

const MAP_PIXELS: usize = 128;
const FRAME_BYTES: usize = MAP_PIXELS * MAP_PIXELS;

// ---------------------------------------------------------------------------
// WallConfig: persisted settings in config/hplug/mapvideo.json
// ---------------------------------------------------------------------------
#[derive(Clone, Debug)]
pub struct WallConfig {
    pub cols: usize,
    pub rows: usize,
    pub target_fps: u32,
    pub loop_playback: bool,
    pub map_ids: Vec<i32>,
    pub rotation: u8,
    pub intro_enabled: bool,
    pub intro_camera: [f64; 3],
    pub intro_yaw: f32,
    pub intro_pitch: f32,
    pub intro_skip_slot: u32,
    pub wall_origin: [f64; 3],
    pub frame_cache_dir: String,
    /// Dimension the wall is in; only players there receive frames.
    pub wall_dimension: String,
    /// Players further than this (blocks, from `wall_origin`) get no frames (Paper: only players
    /// tracking the item frames are drawn for). 0 = everybody in the dimension.
    pub view_distance: f64,
}

impl Default for WallConfig {
    fn default() -> Self {
        Self {
            cols: 4,
            rows: 3,
            target_fps: 10,
            loop_playback: true,
            map_ids: vec![-1; 12],
            rotation: 0,
            // off by default (decision 06/11): the intro lock puts every joining player in spectator
            intro_enabled: false,
            intro_camera: [1.7, 65.0, 9.0],
            intro_yaw: 180.0,
            intro_pitch: 0.0,
            intro_skip_slot: 8,
            wall_origin: [96.0, 146.0, -31.0],
            frame_cache_dir: "wall-frames".to_string(),
            wall_dimension: "minecraft:overworld".to_string(),
            view_distance: 96.0,
        }
    }
}

impl WallConfig {
    pub fn positions(&self) -> usize {
        self.cols * self.rows
    }

    pub fn from_ctx(ctx: &Ctx) -> Self {
        let mut cfg = Self::default();
        if let Some(v) = ctx.get("cols").and_then(|v| v.as_u64()) {
            cfg.cols = (v as usize).max(1);
        }
        if let Some(v) = ctx.get("rows").and_then(|v| v.as_u64()) {
            cfg.rows = (v as usize).max(1);
        }
        if let Some(v) = ctx.get("target_fps").and_then(|v| v.as_u64()) {
            cfg.target_fps = (v as u32).max(1);
        }
        if let Some(v) = ctx.get("loop").and_then(|v| v.as_bool()) {
            cfg.loop_playback = v;
        }
        if let Some(v) = ctx.get("rotation").and_then(|v| v.as_u64()) {
            cfg.rotation = (v as u8) % 4;
        }
        if let Some(v) = ctx.get("intro_enabled").and_then(|v| v.as_bool()) {
            cfg.intro_enabled = v;
        }
        if let Some(v) = ctx.get("intro_skip_slot").and_then(|v| v.as_u64()) {
            cfg.intro_skip_slot = v as u32;
        }
        if let Some(arr) = ctx.get("map_ids").and_then(|v| v.as_array().cloned()) {
            let ids: Vec<i32> = arr.iter().filter_map(|x| x.as_i64().map(|n| n as i32)).collect();
            if !ids.is_empty() {
                cfg.map_ids = ids;
            }
        }
        if cfg.map_ids.len() < cfg.positions() {
            cfg.map_ids.resize(cfg.positions(), -1);
        }
        if let Some(cam) = ctx.get("intro_camera").and_then(|v| v.as_array().cloned())
            && cam.len() >= 3
                && let (Some(x), Some(y), Some(z)) = (cam[0].as_f64(), cam[1].as_f64(), cam[2].as_f64()) {
                    cfg.intro_camera = [x, y, z];
                }
        if let Some(v) = ctx.get("intro_yaw").and_then(|v| v.as_f64()) {
            cfg.intro_yaw = v as f32;
        }
        if let Some(v) = ctx.get("intro_pitch").and_then(|v| v.as_f64()) {
            cfg.intro_pitch = v as f32;
        }
        if let Some(orig) = ctx.get("wall_origin").and_then(|v| v.as_array().cloned())
            && orig.len() >= 3
                && let (Some(x), Some(y), Some(z)) = (orig[0].as_f64(), orig[1].as_f64(), orig[2].as_f64()) {
                    cfg.wall_origin = [x, y, z];
                }
        if let Some(s) = ctx.get("frame_cache_dir").and_then(|v| v.as_str().map(str::to_string)) {
            cfg.frame_cache_dir = s;
        }
        if let Some(s) = ctx.get("wall_dimension").and_then(|v| v.as_str().map(str::to_string)) {
            cfg.wall_dimension = s;
        }
        if let Some(v) = ctx.get("view_distance").and_then(|v| v.as_f64()) {
            cfg.view_distance = v.max(0.0);
        }
        cfg
    }

    pub fn save(&self, ctx: &mut Ctx) {
        let _ = ctx.set("cols", json::json!(self.cols));
        let _ = ctx.set("rows", json::json!(self.rows));
        let _ = ctx.set("target_fps", json::json!(self.target_fps));
        let _ = ctx.set("loop", json::json!(self.loop_playback));
        let _ = ctx.set("rotation", json::json!(self.rotation));
        let _ = ctx.set("intro_enabled", json::json!(self.intro_enabled));
        let _ = ctx.set("intro_skip_slot", json::json!(self.intro_skip_slot));
        let _ = ctx.set("map_ids", json::json!(self.map_ids));
        let _ = ctx.set("intro_camera", json::json!(self.intro_camera));
        let _ = ctx.set("intro_yaw", json::json!(self.intro_yaw));
        let _ = ctx.set("intro_pitch", json::json!(self.intro_pitch));
        let _ = ctx.set("wall_origin", json::json!(self.wall_origin));
        let _ = ctx.set("frame_cache_dir", json::json!(self.frame_cache_dir));
        let _ = ctx.set("wall_dimension", json::json!(self.wall_dimension));
        let _ = ctx.set("view_distance", json::json!(self.view_distance));
    }

    /// `frame_cache_dir` as a path: absolute as given, relative ones are looked up beside
    /// server.properties first, then in `<config>/hplug/` (Paper: `getDataFolder()`).
    pub fn cache_path(&self, ctx: &Ctx) -> std::path::PathBuf {
        let raw = std::path::PathBuf::from(&self.frame_cache_dir);
        if raw.is_absolute() {
            return raw;
        }
        let mut candidates = Vec::new();
        if let Some(dir) = ctx.server_dir() {
            candidates.push(dir.join(&raw));
        }
        if let Ok(dir) = ctx.host.config_dir() {
            candidates.push(dir.join("hplug").join("mapvideo").join(&raw));
            candidates.push(dir.join("hplug").join(&raw));
        }
        candidates.iter().find(|c| c.is_dir()).cloned().or_else(|| candidates.into_iter().next()).unwrap_or(raw)
    }
}

// ---------------------------------------------------------------------------
// FrameStore: in-memory quantized map pixel store (128x128 bytes per frame)
// ---------------------------------------------------------------------------
#[derive(Default)]
pub struct FrameStore {
    position_data: Vec<Vec<u8>>,
    frame_counts: Vec<usize>,
}


impl FrameStore {
    pub fn load_all(&mut self, cache_dir: &std::path::Path, positions: usize) {
        self.position_data = vec![Vec::new(); positions];
        self.frame_counts = vec![0; positions];
        if !cache_dir.is_dir() {
            return;
        }
        for p in 0..positions {
            let path = cache_dir.join(format!("pos_{p}.bin"));
            if let Ok(bytes) = std::fs::read(&path) {
                let frames = bytes.len() / FRAME_BYTES;
                self.position_data[p] = bytes;
                self.frame_counts[p] = frames;
            }
        }
    }

    pub fn has_data(&self, position: usize) -> bool {
        position < self.position_data.len() && self.frame_counts[position] > 0
    }

    pub fn frame_count(&self, position: usize) -> usize {
        if self.has_data(position) {
            self.frame_counts[position]
        } else {
            0
        }
    }

    pub fn max_frame_count(&self) -> usize {
        self.frame_counts.iter().copied().max().unwrap_or(0)
    }

    /// The whole 128x128 picture of `position` at `frame_index` (modulo its own frame count), rotated,
    /// ready for `Host::send_map`. Paper: `VideoMapRenderer.render` drawing onto the `MapCanvas`.
    pub fn frame(&self, position: usize, frame_index: usize, rotation: u8) -> Option<Vec<u8>> {
        if !self.has_data(position) {
            return None;
        }
        let count = self.frame_counts[position];
        let base = (frame_index % count) * FRAME_BYTES;
        let src = &self.position_data[position][base..base + FRAME_BYTES];
        if rotation.is_multiple_of(4) {
            return Some(src.to_vec());
        }
        let mut out = vec![0u8; FRAME_BYTES];
        for y in 0..MAP_PIXELS {
            for x in 0..MAP_PIXELS {
                let (sx, sy) = match rotation % 4 {
                    1 => (y, 127 - x),
                    2 => (127 - x, 127 - y),
                    _ => (127 - y, x),
                };
                out[y * MAP_PIXELS + x] = src[sy * MAP_PIXELS + sx];
            }
        }
        Some(out)
    }

    pub fn pixel(&self, position: usize, frame_index: usize, x: usize, y: usize, rotation: u8) -> u8 {
        if !self.has_data(position) {
            return 0;
        }
        let (src_x, src_y) = match rotation {
            1 => (y, 127 - x),
            2 => (127 - x, 127 - y),
            3 => (127 - y, x),
            _ => (x, y),
        };
        let count = self.frame_counts[position];
        let idx = frame_index % count;
        let base = idx * FRAME_BYTES;
        self.position_data[position][base + src_y * MAP_PIXELS + src_x]
    }
}

// ---------------------------------------------------------------------------
// MapSink: where a frame goes
// ---------------------------------------------------------------------------
/// Sink interface for pushing 128x128 palette bytes to a player.
///
/// In Paper, `VideoMapRenderer` draws onto Bukkit's `MapCanvas` and the server sends
/// `ClientboundMapItemDataPacket`. In Horizon the same packet goes out through
/// `Host::send_map(uuid, map_id, colors)` (horizon-rust/horizon#9592, merged).
pub trait MapSink: Send + Sync {
    fn write_frame(&self, ctx: &mut Ctx, viewer: u128, map_id: i32, colors: &[u8]) -> bool;
}

/// Sends through `Host::send_map`.
pub struct HostMapSink;

impl MapSink for HostMapSink {
    fn write_frame(&self, ctx: &mut Ctx, viewer: u128, map_id: i32, colors: &[u8]) -> bool {
        colors.len() == FRAME_BYTES && ctx.host.send_map(viewer, map_id, colors)
    }
}

/// Writes nothing (dry runs, unit tests).
pub struct NoOpMapSink;

impl MapSink for NoOpMapSink {
    fn write_frame(&self, _ctx: &mut Ctx, _viewer: u128, _map_id: i32, _colors: &[u8]) -> bool {
        true
    }
}

// ---------------------------------------------------------------------------
// WallClock: lockstep playback state machine
// ---------------------------------------------------------------------------
pub struct WallClock {
    pub current_frame: usize,
    pub playing: bool,
    pub blit_count: u64,
}

impl Default for WallClock {
    fn default() -> Self {
        Self {
            current_frame: 0,
            playing: true,
            blit_count: 0,
        }
    }
}

impl WallClock {
    pub fn advance(&mut self, max_frames: usize, loop_playback: bool) -> usize {
        if !self.playing || max_frames == 0 {
            return self.current_frame;
        }
        self.current_frame += 1;
        if self.current_frame >= max_frames {
            if loop_playback {
                self.current_frame = 0;
            } else {
                self.current_frame = max_frames.saturating_sub(1);
                self.playing = false;
            }
        }
        self.current_frame
    }
}

// ---------------------------------------------------------------------------
// IntroLock: joiner freeze & spectator view management
// ---------------------------------------------------------------------------
#[derive(Clone, Debug)]
pub struct LockedPlayer {
    pub name: String,
    pub start_tick: u64,
    pub duration_ticks: u64,
}

// ---------------------------------------------------------------------------
// Global Runtime State
// ---------------------------------------------------------------------------
pub struct State {
    pub clock: WallClock,
    pub store: FrameStore,
    pub locked: HashMap<u128, LockedPlayer>,
    pub sink: Box<dyn MapSink>,
    pub tick_counter: u64,
    /// What each viewer last got, per map id: (frame index, rotation). A frame is only sent again when
    /// it changed (Paper's renderer skips unchanged frames per viewer too); paused = no traffic.
    pub sent: HashMap<u128, HashMap<i32, (usize, u8)>>,
    /// The frame cache was read once (at the first tick, or by `/mapvideo preprocess`).
    pub loaded: bool,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let mut g = STATE.lock().unwrap();
    if g.is_none() {
        *g = Some(State {
            clock: WallClock::default(),
            store: FrameStore::default(),
            locked: HashMap::new(),
            sink: Box::new(HostMapSink),
            tick_counter: 0,
            sent: HashMap::new(),
            loaded: false,
        });
    }
    f(g.as_mut().unwrap())
}

// ---------------------------------------------------------------------------
// Intro Lock Helpers
// ---------------------------------------------------------------------------
fn is_locked(uuid: u128) -> bool {
    with_state(|s| s.locked.contains_key(&uuid))
}

fn lock_player(ctx: &mut Ctx, uuid: u128, name: &str, duration_ticks: u64, cfg: &WallConfig) {
    let tick = ctx.tick();
    with_state(|s| {
        s.locked.insert(
            uuid,
            LockedPlayer {
                name: name.to_string(),
                start_tick: tick,
                duration_ticks,
            },
        );
    });

    // Freeze motion via Slowness 255 & Jump Boost 128 (proven HorizonsAuth pattern)
    ctx.console(format!("effect give {name} minecraft:slowness 120 255 true"));
    ctx.console(format!("effect give {name} minecraft:jump_boost 120 128 true"));
    ctx.console(format!("gamemode spectator {name}"));

    let cam = cfg.intro_camera;
    ctx.console(format!(
        "tp {name} {:.2} {:.2} {:.2} {:.1} {:.1}",
        cam[0], cam[1], cam[2], cfg.intro_yaw, cfg.intro_pitch
    ));

    // The skip item (paper named "Skip Intro") for the locked player, not the command sender. Using it skips.
    ctx.console(format!(
        "item replace entity {name} hotbar.{} with minecraft:paper[minecraft:custom_name={{text:\"Skip Intro\",color:\"yellow\",italic:false}},minecraft:custom_data={{mapvideo_skip:1b}}] 1",
        cfg.intro_skip_slot.min(8)
    ));
    ctx.tell(uuid, "§eViewing intro video. Select skip item or type /mapvideo skip to skip.");
}

fn unlock_player(ctx: &mut Ctx, uuid: u128, name: &str, skipped: bool) {
    let existed = with_state(|s| s.locked.remove(&uuid).is_some());
    if !existed {
        return;
    }

    ctx.console(format!("effect clear {name} minecraft:slowness"));
    ctx.console(format!("effect clear {name} minecraft:jump_boost"));
    ctx.console(format!("gamemode survival {name}"));
    ctx.console(format!("title {name} clear"));
    ctx.console(format!("clear {name} minecraft:paper[minecraft:custom_data={{mapvideo_skip:1b}}]"));

    if skipped {
        ctx.tell(uuid, "§7Intro skipped.");
    } else {
        ctx.tell(uuid, "§aIntro finished. Welcome to Horizons SMP!");
    }
}

// ---------------------------------------------------------------------------
// Command Execution
// ---------------------------------------------------------------------------
fn handle_command(ctx: &mut Ctx, args: &Args) -> CmdResult {
    let sub = args.get(0).map(|s| s.to_lowercase()).unwrap_or_default();
    let is_admin = ctx.op_level() >= 2 || {
        let u = ctx.player_uuid().unwrap_or(0);
        let name = ctx.sender_name();
        ctx.has_perm(u, &name, "mapvideo.admin")
    };

    if sub == "skip" {
        if let Some(u) = ctx.player_uuid() {
            let name = ctx.sender_name();
            if is_locked(u) {
                unlock_player(ctx, u, &name, true);
                ctx.reply("§aIntro skipped.");
            } else {
                ctx.reply("§7You are not currently watching the intro.");
            }
            return Ok(());
        }
        return Err("Console cannot skip player intro.".into());
    }

    if !is_admin {
        return Err("§cYou do not have permission to manage the map video wall.".into());
    }

    match sub.as_str() {
        "status" => cmd_status(ctx),
        "play" => {
            with_state(|s| s.clock.playing = true);
            ctx.reply("§aWall playback resumed.");
            Ok(())
        }
        "pause" => {
            with_state(|s| s.clock.playing = false);
            ctx.reply("§eWall playback paused.");
            Ok(())
        }
        // `/mapvideo maptest <map_id> [colour 0-255]`: sends one solid 128x128 frame to the sender through Host::send_map
        // (horizon-rust/horizon#9592, merged); playback uses the same call through HostMapSink.
        "maptest" => {
            let id: i32 = args.get(1).and_then(|s| s.parse().ok()).ok_or("Usage: /mapvideo maptest <map_id> [colour]")?;
            let colour: u8 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(36);
            let uuid = ctx.player_uuid().ok_or("Players only.")?;
            let ok = ctx.host.send_map(uuid, id, &vec![colour; 128 * 128]);
            let short = ctx.host.send_map(uuid, id, &[0u8; 10]);
            ctx.reply(format!("§amaptest sent={ok} short_refused={}", !short));
            Ok(())
        }
        "viewdistance" => {
            let mut cfg = WallConfig::from_ctx(ctx);
            let v: f64 = args.get(1).and_then(|s| s.parse().ok()).ok_or("Usage: /mapvideo viewdistance <blocks, 0 = whole dimension>")?;
            cfg.view_distance = v.max(0.0);
            cfg.save(ctx);
            with_state(|s| s.sent.clear());
            ctx.reply(format!("§aView distance set to {}.", cfg.view_distance));
            Ok(())
        }
        "dimension" => {
            let mut cfg = WallConfig::from_ctx(ctx);
            cfg.wall_dimension = match args.get(1) {
                Some(d) if d.contains(':') => d.clone(),
                Some(d) => format!("minecraft:{d}"),
                None => ctx.try_player().map(|p| p.dimension.clone()).ok_or("Usage: /mapvideo dimension <id>")?,
            };
            cfg.save(ctx);
            ctx.reply(format!("§aWall dimension set to {}.", cfg.wall_dimension));
            Ok(())
        }
        "setorigin" => {
            let mut cfg = WallConfig::from_ctx(ctx);
            let p = ctx.try_player().ok_or("Players only.")?;
            cfg.wall_origin = [p.position[0].floor(), p.position[1].floor(), p.position[2].floor()];
            cfg.wall_dimension = p.dimension.clone();
            cfg.save(ctx);
            ctx.reply(format!("§aWall origin set to {:?} in {}.", cfg.wall_origin, cfg.wall_dimension));
            Ok(())
        }
        "rotate" => cmd_rotate(ctx, args),
        "assign" => cmd_assign(ctx, args),
        "createmaps" => cmd_createmaps(ctx, args),
        "spawnwall" => cmd_spawnwall(ctx, args),
        "preprocess" => cmd_preprocess(ctx),
        "lock" => cmd_lock(ctx, args),
        "unlock" => cmd_unlock(ctx, args),
        _ => {
            ctx.reply("§7=== MapVideo Commands ===");
            ctx.reply("§7/mapvideo status - View playback and wall status");
            ctx.reply("§7/mapvideo play | pause - Control playback state");
            ctx.reply("§7/mapvideo rotate <0-3> - Set wall rotation (0=0°, 1=90°, 2=180°, 3=270°)");
            ctx.reply("§7/mapvideo assign <position> [map_id] - Assign map ID to wall slot");
            ctx.reply("§7/mapvideo createmaps [start_id] - Create sequential map IDs");
            ctx.reply("§7/mapvideo spawnwall [facing] - Summon wall item frames");
            ctx.reply("§7/mapvideo preprocess - (Re)load the frame cache (pos_N.bin)");
            ctx.reply("§7/mapvideo setorigin | dimension <id> | viewdistance <n> - Where the wall is, who gets frames");
            ctx.reply("§7/mapvideo lock <player> - Lock player in intro");
            ctx.reply("§7/mapvideo unlock <player> - Release player from intro");
            ctx.reply("§7/mapvideo skip - Skip intro (player command)");
            Ok(())
        }
    }
}

fn cmd_status(ctx: &mut Ctx) -> CmdResult {
    let cfg = WallConfig::from_ctx(ctx);
    let (playing, frame, blits, loaded_positions, locked_count) = with_state(|s| {
        let loaded = (0..cfg.positions()).filter(|&p| s.store.has_data(p)).count();
        (s.clock.playing, s.clock.current_frame, s.clock.blit_count, loaded, s.locked.len())
    });
    let assigned_count = cfg.map_ids.iter().filter(|&&id| id >= 0).count();

    ctx.reply("§7=== Map Video Wall Status ===");
    ctx.reply(format!(
        "§7Grid: §f{}x{} ({} positions)",
        cfg.cols,
        cfg.rows,
        cfg.positions()
    ));
    ctx.reply(format!(
        "§7Positions with frame data: §f{}/{}",
        loaded_positions,
        cfg.positions()
    ));
    ctx.reply(format!(
        "§7Positions with map ID assigned: §f{}/{}",
        assigned_count,
        cfg.positions()
    ));
    ctx.reply(format!(
        "§7Playback: §f{} §7frame §f{}§7 @ §f{}fps §7(blitted: §f{}§7)",
        if playing { "§aPLAYING" } else { "§ePAUSED" },
        frame,
        cfg.target_fps,
        blits
    ));
    ctx.reply(format!(
        "§7Rotation: §f{} §7(0=0°, 1=90°cw, 2=180°, 3=270°ccw)",
        cfg.rotation
    ));
    ctx.reply(format!("§7Locked viewers: §f{}", locked_count));
    Ok(())
}

fn cmd_rotate(ctx: &mut Ctx, args: &Args) -> CmdResult {
    let mut cfg = WallConfig::from_ctx(ctx);
    if let Some(r_str) = args.get(1) {
        match r_str.parse::<u8>() {
            Ok(r) if r <= 3 => {
                cfg.rotation = r;
                cfg.save(ctx);
                ctx.reply(format!("§aWall rotation set to {r}."));
            }
            _ => return Err("Rotation must be 0, 1, 2, or 3.".into()),
        }
    } else {
        ctx.reply(format!(
            "§7Current rotation: {} (0=0°, 1=90°cw, 2=180°, 3=270°ccw). Usage: /mapvideo rotate <0-3>",
            cfg.rotation
        ));
    }
    Ok(())
}

fn cmd_assign(ctx: &mut Ctx, args: &Args) -> CmdResult {
    let mut cfg = WallConfig::from_ctx(ctx);
    let pos_idx = match args.get(1).and_then(|s| s.parse::<usize>().ok()) {
        Some(p) if p < cfg.positions() => p,
        _ => return Err(format!("Position must be 0 to {}.", cfg.positions().saturating_sub(1))),
    };

    let map_id = match args.get(2).and_then(|s| s.parse::<i32>().ok()) {
        Some(id) => id,
        None => {
            // Default sequential ID or prompt
            1000 + pos_idx as i32
        }
    };

    if pos_idx >= cfg.map_ids.len() {
        cfg.map_ids.resize(cfg.positions(), -1);
    }
    cfg.map_ids[pos_idx] = map_id;
    cfg.save(ctx);
    ctx.reply(format!("§aAssigned map #{map_id} to wall position {pos_idx}."));
    Ok(())
}

fn cmd_createmaps(ctx: &mut Ctx, args: &Args) -> CmdResult {
    let mut cfg = WallConfig::from_ctx(ctx);
    let start_id: i32 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(100);
    cfg.map_ids.clear();
    for i in 0..cfg.positions() {
        cfg.map_ids.push(start_id + i as i32);
    }
    cfg.save(ctx);
    ctx.reply(format!(
        "§aAssigned {} map IDs starting at #{}: {:?}",
        cfg.positions(),
        start_id,
        cfg.map_ids
    ));
    Ok(())
}

fn cmd_spawnwall(ctx: &mut Ctx, args: &Args) -> CmdResult {
    let cfg = WallConfig::from_ctx(ctx);
    let facing = args.get(1).map(|s| s.as_str()).unwrap_or("north");
    let (dx, dy, dz, facing_byte) = match facing.to_lowercase().as_str() {
        "south" => (1.0, 1.0, 0.0, 3),
        "west" => (0.0, 1.0, 1.0, 4),
        "east" => (0.0, 1.0, -1.0, 5),
        _ => (-1.0, 1.0, 0.0, 2), // north default
    };

    let orig = cfg.wall_origin;
    let mut spawned = 0;
    for row in 0..cfg.rows {
        for col in 0..cfg.cols {
            let pos = row * cfg.cols + col;
            let map_id = cfg.map_ids.get(pos).copied().unwrap_or(-1);
            if map_id < 0 {
                continue;
            }
            let y_off = (cfg.rows - 1 - row) as f64 * dy;
            let x = orig[0] + (col as f64) * dx;
            let y = orig[1] + y_off;
            let z = orig[2] + (col as f64) * dz;

            // Summon item_frame tagged with mapvideo_wall
            let summon_cmd = format!(
                "summon item_frame {:.2} {:.2} {:.2} {{Facing:{}b,Tags:[\"mapvideo_wall\"],Item:{{id:\"minecraft:filled_map\",components:{{\"minecraft:map_id\":{}}}}}}}",
                x, y, z, facing_byte, map_id
            );
            ctx.console(summon_cmd);
            spawned += 1;
        }
    }
    ctx.reply(format!(
        "§aSpawned {spawned} wall item frames facing {facing} starting at ({:.1}, {:.1}, {:.1}).",
        orig[0], orig[1], orig[2]
    ));
    Ok(())
}

fn cmd_preprocess(ctx: &mut Ctx) -> CmdResult {
    let cfg = WallConfig::from_ctx(ctx);
    let cache_path = cfg.cache_path(ctx);
    with_state(|s| {
        s.store.load_all(&cache_path, cfg.positions());
        s.loaded = true;
        s.sent.clear();
    });
    let loaded = with_state(|s| (0..cfg.positions()).filter(|&p| s.store.has_data(p)).count());
    ctx.reply(format!(
        "§aChecked frame cache in '{}': {}/{} positions loaded.",
        cache_path.display(),
        loaded,
        cfg.positions()
    ));
    Ok(())
}

fn cmd_lock(ctx: &mut Ctx, args: &Args) -> CmdResult {
    let raw_name = args.get(1).ok_or("Usage: /mapvideo lock <player>")?;
    let (target_uuid, target_name) = ctx
        .find_online(raw_name)
        .map(|p| (p.uuid, p.name.clone()))
        .or_else(|| ctx.uuid_for_name(raw_name))
        .ok_or_else(|| format!("Player '{raw_name}' not found."))?;

    let cfg = WallConfig::from_ctx(ctx);
    let max_frames = with_state(|s| s.store.max_frame_count()).max(380);
    let duration_ticks = ((max_frames as f64 / cfg.target_fps as f64) * 20.0).round() as u64;

    lock_player(ctx, target_uuid, &target_name, duration_ticks.max(100), &cfg);
    ctx.reply(format!("§aLocked {} into intro ({} ticks).", target_name, duration_ticks));
    Ok(())
}

fn cmd_unlock(ctx: &mut Ctx, args: &Args) -> CmdResult {
    let target_name = args.get(1).ok_or("Usage: /mapvideo unlock <player>")?;
    let target_uuid = ctx
        .find_online(target_name)
        .map(|p| p.uuid)
        .or_else(|| ctx.uuid_for_name(target_name).map(|(u, _)| u))
        .ok_or_else(|| format!("Player '{target_name}' not found."))?;

    unlock_player(ctx, target_uuid, target_name, false);
    ctx.reply(format!("§aUnlocked {target_name} from intro."));
    Ok(())
}

// ---------------------------------------------------------------------------
// Plugin Entrypoint
// ---------------------------------------------------------------------------
pub fn plugin() -> Plugin {
    Plugin::new("mapvideo")
        .command("mapvideo [sub:word] [arg:word] [val:word]", 0, |ctx, args| {
            handle_command(ctx, args)
        })
        // no `/mv` alias: that is Multiverse's (the worlds plugin) on the live server
        .on_join(|ctx| {
            let cfg = WallConfig::from_ctx(ctx);
            if !cfg.intro_enabled {
                return;
            }
            if let Some(p) = ctx.try_player() {
                let uuid = p.uuid;
                let name = p.name.clone();
                let max_frames = with_state(|s| s.store.max_frame_count()).max(380);
                let duration_ticks = ((max_frames as f64 / cfg.target_fps as f64) * 20.0).round() as u64;
                lock_player(ctx, uuid, &name, duration_ticks.max(100), &cfg);
            }
        })
        .on_quit(|ctx| {
            if let Some(u) = ctx.player_uuid() {
                with_state(|s| {
                    s.locked.remove(&u);
                    s.sent.remove(&u);
                });
            }
        })
        // Movement & action gating while locked
        .on_action(|ctx, a: &Action| {
            if let Some(u) = ctx.player_uuid()
                && is_locked(u) {
                    // Using the skip paper ends the intro (Paper: the PDC-tagged skip item).
                    if let Action::UseItem { item } = a
                        && item == "minecraft:paper" {
                            let name = ctx.sender_name();
                            unlock_player(ctx, u, &name, true);
                        }
                    return true; // Cancel action while locked
                }
            // Protect wall frames from interaction
            if let Action::InteractEntity { kind, .. } = a
                && (kind == "minecraft:item_frame" || kind == "minecraft:glow_item_frame") {
                    return true;
                }
            false
        })
        .on_block_break(|ctx, _| {
            ctx.player_uuid().is_some_and(is_locked)
        })
        .on_interact(|ctx, _, _| {
            ctx.player_uuid().is_some_and(is_locked)
        })
        .on_attack(|ctx, _entity_id, kind| {
            if ctx.player_uuid().is_some_and(is_locked) {
                return true;
            }
            // Protect wall frames from attack
            if kind == "minecraft:item_frame" || kind == "minecraft:glow_item_frame" {
                return true;
            }
            false
        })
        .on_damage(|d: &Damage| {
            d.victim_player.is_some_and(is_locked)
        })
        // Periodic ticker: advances wall clock and checks intro timeouts
        .every(1, |ctx| {
            let cfg = WallConfig::from_ctx(ctx);
            let interval = (20 / cfg.target_fps).max(1) as u64;

            let (should_advance, expired_uuids) = with_state(|s| {
                s.tick_counter += 1;
                let adv = s.tick_counter % interval == 0;

                // Check expired locks
                let current_tick = ctx.tick();
                let mut expired = Vec::new();
                for (&uuid, locked) in &s.locked {
                    if current_tick.saturating_sub(locked.start_tick) >= locked.duration_ticks {
                        expired.push((uuid, locked.name.clone()));
                    }
                }
                (adv, expired)
            });

            // Unlock expired viewers
            for (uuid, name) in expired_uuids {
                unlock_player(ctx, uuid, &name, false);
            }

            // Load the frame cache once (Paper: FrameStore loaded in onEnable).
            if !with_state(|s| s.loaded) {
                let path = cfg.cache_path(ctx);
                with_state(|s| {
                    s.store.load_all(&path, cfg.positions());
                    s.loaded = true;
                });
                let n = with_state(|s| (0..cfg.positions()).filter(|&p| s.store.has_data(p)).count());
                ctx.log(format!("frame cache {}: {}/{} positions loaded", path.display(), n, cfg.positions()));
            }

            // Advance wall playback
            if should_advance {
                let (max_frames, frame_idx) = with_state(|s| {
                    let max = s.store.max_frame_count();
                    let f = s.clock.advance(max, cfg.loop_playback);
                    (max, f)
                });
                if max_frames == 0 {
                    return;
                }
                // Viewers: players in the wall's dimension within view_distance of the wall.
                let o = cfg.wall_origin;
                let r2 = cfg.view_distance * cfg.view_distance;
                let viewers: Vec<u128> = ctx
                    .online()
                    .into_iter()
                    .filter(|p| p.dimension == cfg.wall_dimension)
                    .filter(|p| {
                        let d = [p.position[0] - o[0], p.position[1] - o[1], p.position[2] - o[2]];
                        cfg.view_distance <= 0.0 || d[0] * d[0] + d[1] * d[1] + d[2] * d[2] <= r2
                    })
                    .map(|p| p.uuid)
                    .collect();
                if viewers.is_empty() {
                    return;
                }
                for pos in 0..cfg.positions() {
                    let map_id = cfg.map_ids.get(pos).copied().unwrap_or(-1);
                    if map_id < 0 {
                        continue;
                    }
                    // positions with fewer frames loop on their own count (as FrameStore.pixel did)
                    let Some((key, colors)) = with_state(|s| {
                        let count = s.store.frame_count(pos);
                        if count == 0 {
                            return None;
                        }
                        let key = (frame_idx % count, cfg.rotation);
                        s.store.frame(pos, frame_idx, cfg.rotation).map(|c| (key, c))
                    }) else {
                        continue;
                    };
                    for &viewer in &viewers {
                        let fresh = with_state(|s| s.sent.get(&viewer).and_then(|m| m.get(&map_id)) != Some(&key));
                        if !fresh {
                            continue;
                        }
                        // take the sink out so the state lock is not held while the Host sends
                        let sink = with_state(|s| std::mem::replace(&mut s.sink, Box::new(NoOpMapSink)));
                        let ok = sink.write_frame(ctx, viewer, map_id, &colors);
                        with_state(|s| {
                            s.sink = sink;
                            if ok {
                                s.clock.blit_count += 1;
                                s.sent.entry(viewer).or_default().insert(map_id, key);
                            }
                        });
                    }
                }
            }
        })
}
