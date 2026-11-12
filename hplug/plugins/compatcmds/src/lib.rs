//! Console/RCON compatibility for two third-party Paper plugins that Ori (discord-bot `status.py`) and the
//! monitors call, but that have no Horizon port:
//!
//! * **Chunky** -> Horizon's own `/pregen` (docs/pregeneration.md; needs `horizon-pregen = true` under
//!   `[properties]` in server.toml). `chunky start|pause|continue|cancel|progress` plus the selection
//!   commands `world|center|radius|shape` answer with Chunky's English message shapes, prefixed `[Chunky]`.
//!   Progress comes from `Host::pregen_status` (engine patch `engine-patches/host-pregen-status.patch`).
//!   Several tasks (one per world, as Chunky): the engine generates one at a time, the others wait in
//!   `tasks` and start in turn; `chunky progress` prints one line per task (the running one in Chunky's
//!   `Task running for <world>. Processed: N chunks (P%), ETA: H:MM:SS` shape, which Ori parses; waiting
//!   ones as `Task queued for ...`). Each task is also written in Chunky's format to
//!   `<server>/plugins/Chunky/tasks/<world>.properties` (world, cancelled, center-x/z, radius, shape,
//!   pattern, chunks, time), which Ori reads while tasks are paused. `continue_on_restart` (Chunky's
//!   `continue-on-restart`, false on the live server) restarts unfinished tasks after a restart (the
//!   engine skips chunks that exist). Shapes `square` and `circle` only (others fall back to square), no
//!   `trim`/`spawn`/`worldborder`/`silent`/`quiet`.
//! * **BlueMap** -> there is no map renderer on Horizon (options: docs/BLUEMAP_OPTIONS.md). With
//!   `config/hplug/bluemap_status.json` (written by an external BlueMap CLI run, option B) `bluemap` answers
//!   in BlueMap's shape from it (`progress: X%`, `remaining time:`, `N maps have pending updates`). Without
//!   it, `bluemap` (any arguments) answers with a fixed
//!   status in BlueMap's shape ("0 pending render tasks", render threads stopped) so a caller's status
//!   page shows "idle" instead of failing on "Unknown command"; `bluemap stop|start|...` are accepted
//!   and change nothing. Check Ori's parser against these lines before cutover (docs/RCON_PATHS.md).
//!
//! Both commands are op level 2 for players and always available to the console/RCON.
use hplug::{json, Args, CmdResult, Ctx, Plugin};

const P: &str = "[Chunky] ";

/// Bukkit world names (what Chunky prints and accepts) <-> dimension ids.
fn world_name(dim: &str) -> String {
    match dim {
        "minecraft:overworld" => "world".into(),
        "minecraft:the_nether" => "world_nether".into(),
        "minecraft:the_end" => "world_the_end".into(),
        other => other.trim_start_matches("minecraft:").into(),
    }
}
fn dimension(world: &str) -> Option<String> {
    Some(match world.to_lowercase().as_str() {
        "world" | "overworld" | "minecraft:overworld" => "minecraft:overworld".into(),
        "world_nether" | "nether" | "the_nether" | "minecraft:the_nether" => "minecraft:the_nether".into(),
        "world_the_end" | "end" | "the_end" | "minecraft:the_end" => "minecraft:the_end".into(),
        w if w.contains(':') => w.to_owned(),
        _ => return None,
    })
}

/// Chunky's ETA format: `H:MM:SS`.
fn hms(secs: f64) -> String {
    let s = secs.max(0.0).round() as u64;
    format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}

struct Selection {
    world: String,
    center: [f64; 2],
    radius: f64,
    shape: String,
}
fn selection(ctx: &Ctx) -> Selection {
    let s = ctx.get("selection").unwrap_or_default();
    Selection {
        world: s.get("world").and_then(|v| v.as_str()).unwrap_or("minecraft:overworld").to_owned(),
        center: [s.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0), s.get("z").and_then(|v| v.as_f64()).unwrap_or(0.0)],
        radius: s.get("radius").and_then(|v| v.as_f64()).unwrap_or(500.0),
        shape: s.get("shape").and_then(|v| v.as_str()).unwrap_or("square").to_owned(),
    }
}
fn save(ctx: &mut Ctx, s: &Selection) -> Result<(), String> {
    ctx.set("selection", json::json!({"world": s.world, "x": s.center[0], "z": s.center[1], "radius": s.radius, "shape": s.shape}))
}

// ---------- tasks (one per world) ----------
fn tasks(ctx: &Ctx) -> json::Map<String, json::Value> {
    ctx.get("tasks").and_then(|v| v.as_object().cloned()).unwrap_or_default()
}
fn save_tasks(ctx: &mut Ctx, t: json::Map<String, json::Value>) -> Result<(), String> {
    write_properties(ctx, &t);
    ctx.set("tasks", json::Value::Object(t))
}
/// Chunky's `tasks/<world>.properties` beside the server, for tools that read them (Ori).
fn write_properties(ctx: &Ctx, t: &json::Map<String, json::Value>) {
    let Some(dir) = ctx.server_dir().map(|d| d.join("plugins/Chunky/tasks")) else { return };
    let _ = std::fs::create_dir_all(&dir);
    for (dim, v) in t {
        let w = world_name(dim);
        let body = format!(
            "world={w}\ncancelled={}\ncenter-x={:.1}\ncenter-z={:.1}\nradius={:.1}\nshape={}\npattern=region\nchunks={}\ntime={}\n",
            v["cancelled"].as_bool().unwrap_or(false),
            v["x"].as_f64().unwrap_or(0.0),
            v["z"].as_f64().unwrap_or(0.0),
            v["radius"].as_f64().unwrap_or(0.0),
            v["shape"].as_str().unwrap_or("square"),
            v["chunks"].as_u64().unwrap_or(0),
            v["time"].as_u64().unwrap_or(0),
        );
        let _ = std::fs::write(dir.join(format!("{w}.properties")), body);
    }
}
fn total_chunks(v: &json::Value) -> u64 {
    let d = (2.0 * v["radius"].as_f64().unwrap_or(0.0) / 16.0).round() as u64;
    let sq = d * d;
    if v["shape"].as_str() == Some("circle") { (sq as f64 * std::f64::consts::PI / 4.0) as u64 } else { sq }
}
fn start_engine(ctx: &mut Ctx, dim: &str, v: &json::Value) {
    ctx.console(format!(
        "execute in {dim} run pregen start {} {} {} {}",
        v["radius"].as_f64().unwrap_or(0.0).round() as i64,
        v["shape"].as_str().unwrap_or("square"),
        v["x"].as_f64().unwrap_or(0.0).round() as i64,
        v["z"].as_f64().unwrap_or(0.0).round() as i64
    ));
}
/// Every 20 ticks: record the running task's progress, finish it, start the next waiting one.
fn tick_tasks(ctx: &mut Ctx) {
    let mut t = tasks(ctx);
    if t.is_empty() {
        return;
    }
    let status = ctx.host.pregen_status();
    let mut changed = false;
    if let Some((dim, done, total, _paused, secs)) = &status
        && let Some(v) = t.get_mut(dim)
    {
        v["chunks"] = json::json!(done);
        v["time"] = json::json!(*secs as u64);
        v["running"] = json::json!(true);
        if done >= total {
            v["done"] = json::json!(true);
            v["running"] = json::json!(false);
        }
        changed = true;
    }
    // a task that ran and is no longer the engine's: finished (or cancelled through /pregen)
    let running_dim = status.as_ref().map(|s| s.0.clone());
    for (dim, v) in t.iter_mut() {
        if v["running"].as_bool() == Some(true) && running_dim.as_deref() != Some(dim.as_str()) {
            v["running"] = json::json!(false);
            v["done"] = json::json!(true);
            v["chunks"] = json::json!(total_chunks(v));
            changed = true;
        }
    }
    let finished: Vec<String> = t.iter().filter(|(_, v)| v["done"].as_bool() == Some(true)).map(|(k, _)| k.clone()).collect();
    for k in &finished {
        ctx.log(format!("chunky task finished for {}", world_name(k)));
        t.remove(k);
        if let Some(dir) = ctx.server_dir() {
            let _ = std::fs::remove_file(dir.join(format!("plugins/Chunky/tasks/{}.properties", world_name(k))));
        }
        changed = true;
    }
    if status.is_none()
        && let Some((dim, v)) = t.iter_mut().find(|(_, v)| !v["paused"].as_bool().unwrap_or(false) && !v["cancelled"].as_bool().unwrap_or(false))
    {
        v["running"] = json::json!(true);
        let (dim, v) = (dim.clone(), v.clone());
        start_engine(ctx, &dim, &v);
        changed = true;
    }
    if changed {
        let _ = save_tasks(ctx, t);
    }
}

fn progress_line(ctx: &mut Ctx) -> Option<String> {
    let (dim, done, total, paused, secs) = ctx.host.pregen_status()?;
    let pct = 100.0 * done as f64 / total.max(1) as f64;
    let rate = done as f64 / secs.max(1e-3);
    let eta = if rate > 0.0 { hms((total - done) as f64 / rate) } else { "0:00:00".into() };
    let world = world_name(&dim);
    if done >= total {
        return Some(format!("{P}Task finished for {world}. Processed: {done} chunks ({pct:.2}%), Total time: {}", hms(secs)));
    }
    let state = if paused { " (paused)" } else { "" };
    Some(format!("{P}Task running for {world}{state}. Processed: {done} chunks ({pct:.2}%), ETA: {eta}, Rate: {rate:.1} cps"))
}

fn chunky(ctx: &mut Ctx, args: &Args) -> CmdResult {
    let sub = args.get(0).map(|s| s.to_lowercase()).unwrap_or_default();
    let mut sel = selection(ctx);
    match sub.as_str() {
        "progress" => {
            let running = progress_line(ctx);
            let running_dim = ctx.host.pregen_status().map(|s| s.0);
            let waiting: Vec<String> = tasks(ctx)
                .iter()
                .filter(|(d, _)| running_dim.as_deref() != Some(d.as_str()))
                .map(|(d, v)| {
                    let pct = 100.0 * v["chunks"].as_u64().unwrap_or(0) as f64 / total_chunks(v).max(1) as f64;
                    let state = if v["paused"].as_bool().unwrap_or(false) { "paused" } else { "queued" };
                    format!("{P}Task {state} for {}. Processed: {} chunks ({pct:.2}%)", world_name(d), v["chunks"].as_u64().unwrap_or(0))
                })
                .collect();
            match running {
                Some(l) => ctx.reply(l),
                None if waiting.is_empty() => ctx.reply(format!("{P}No tasks running.")),
                None => ctx.reply(format!("{P}No tasks running (tasks paused).")),
            }
            for l in waiting {
                ctx.reply(l);
            }
        }
        "world" => {
            let w = args.get(1).ok_or("Usage: chunky world <world>")?;
            sel.world = dimension(w).ok_or_else(|| format!("{P}Invalid world {w}"))?;
            save(ctx, &sel)?;
            ctx.reply(format!("{P}World changed to {}.", world_name(&sel.world)));
        }
        "center" => {
            let (x, z) = match (args.get(1).and_then(|s| s.parse().ok()), args.get(2).and_then(|s| s.parse().ok())) {
                (Some(x), Some(z)) => (x, z),
                _ => ctx.try_player().map(|p| (p.position[0].floor(), p.position[2].floor())).ok_or("Usage: chunky center <x> <z>")?,
            };
            sel.center = [x, z];
            save(ctx, &sel)?;
            ctx.reply(format!("{P}Center changed to {x}, {z}."));
        }
        "radius" => {
            let r: f64 = args.get(1).and_then(|s| s.trim_start_matches(['+', '-']).trim_end_matches('c').parse().ok()).ok_or("Usage: chunky radius <radius>")?;
            sel.radius = r.max(0.0);
            save(ctx, &sel)?;
            ctx.reply(format!("{P}Radius changed to {}.", sel.radius));
        }
        "shape" => {
            let s = args.get(1).map(|s| s.to_lowercase()).ok_or("Usage: chunky shape <square|circle>")?;
            sel.shape = if s == "circle" { "circle".into() } else { "square".into() };
            save(ctx, &sel)?;
            ctx.reply(format!("{P}Shape changed to {}.", sel.shape));
        }
        "start" => {
            // `chunky start [world] [shape] [centerX] [centerZ] [radius]`
            if let Some(w) = args.get(1) {
                sel.world = dimension(w).ok_or_else(|| format!("{P}Invalid world {w}"))?;
            }
            if let Some(s) = args.get(2) {
                sel.shape = if s.eq_ignore_ascii_case("circle") { "circle".into() } else { "square".into() };
            }
            if let (Some(x), Some(z)) = (args.get(3).and_then(|s| s.parse().ok()), args.get(4).and_then(|s| s.parse().ok())) {
                sel.center = [x, z];
            }
            if let Some(r) = args.get(5).and_then(|s| s.parse::<f64>().ok()) {
                sel.radius = r.max(0.0);
            }
            let mut t = tasks(ctx);
            if t.contains_key(&sel.world) {
                return Err(format!("{P}Task already exists for {}. Use chunky cancel first.", world_name(&sel.world)));
            }
            save(ctx, &sel)?;
            let v = json::json!({"x": sel.center[0], "z": sel.center[1], "radius": sel.radius, "shape": sel.shape, "chunks": 0, "time": 0, "cancelled": false, "paused": false});
            let engine_free = ctx.host.pregen_status().is_none();
            if engine_free {
                let mut v = v.clone();
                v["running"] = json::json!(true);
                start_engine(ctx, &sel.world, &v);
                t.insert(sel.world.clone(), v);
            } else {
                t.insert(sel.world.clone(), v);
            }
            save_tasks(ctx, t)?;
            ctx.reply(format!(
                "{P}Task started for {} at {}, {} with radius {}.",
                world_name(&sel.world), sel.center[0], sel.center[1], sel.radius
            ));
            if !engine_free {
                ctx.reply("§7(Horizon generates one world at a time: this task starts when the running one is done)");
            }
        }
        "pause" | "continue" | "cancel" => {
            // `chunky pause [world]`: one world, or all tasks
            let only = args.get(1).and_then(|w| dimension(w));
            let mut t = tasks(ctx);
            if t.is_empty() {
                ctx.reply(format!("{P}No tasks running."));
                return Ok(());
            }
            let running = ctx.host.pregen_status().map(|s| s.0);
            let verb = match sub.as_str() { "pause" => "paused", "continue" => "continuing", _ => "cancelled" };
            let keys: Vec<String> = t.keys().filter(|k| only.as_deref().is_none_or(|o| o == k.as_str())).cloned().collect();
            for k in keys {
                if running.as_deref() == Some(k.as_str()) {
                    // the pregen task keeps its own dimension: no `execute in` needed
                    ctx.console(format!("pregen {}", if sub == "continue" { "continue" } else { sub.as_str() }));
                }
                match sub.as_str() {
                    "cancel" => {
                        t.remove(&k);
                        if let Some(dir) = ctx.server_dir() {
                            let _ = std::fs::remove_file(dir.join(format!("plugins/Chunky/tasks/{}.properties", world_name(&k))));
                        }
                    }
                    "pause" => t[&k]["paused"] = json::json!(true),
                    _ => {
                        t[&k]["paused"] = json::json!(false);
                        if running.is_none() && !t[&k]["running"].as_bool().unwrap_or(false) {
                            // a resumed task after a restart: the engine starts it at the next task tick
                        }
                    }
                }
                ctx.reply(format!("{P}Task {verb} for {}.", world_name(&k)));
            }
            save_tasks(ctx, t)?;
        }
        "selection" => {
            ctx.reply(format!(
                "{P}Current selection: world {}, shape {}, center {}, {}, radius {}",
                world_name(&sel.world), sel.shape, sel.center[0], sel.center[1], sel.radius
            ));
        }
        _ => {
            ctx.reply(format!("{P}Commands: start, pause, continue, cancel, progress, world, center, radius, shape, selection"));
            ctx.reply("§7(Horizon: runs the built-in /pregen; enable horizon-pregen = true in server.toml)");
        }
    }
    Ok(())
}

fn bluemap(ctx: &mut Ctx, args: &Args) -> CmdResult {
    match args.get(0).map(|s| s.to_lowercase()).as_deref() {
        None | Some("status") => {
            // option B (docs/BLUEMAP_OPTIONS.md): an external BlueMap CLI run writes its state here
            let file = ctx.host.config_dir().ok().map(|d| d.join("hplug/bluemap_status.json"));
            if let Some(v) = file.and_then(|f| std::fs::read_to_string(f).ok()).and_then(|t| json::from_str::<json::Value>(&t).ok()) {
                let pending = v["pending"].as_u64().unwrap_or(0);
                if v["running"].as_bool().unwrap_or(false) {
                    ctx.reply("Render-Threads are running!");
                    if let Some(p) = v["progress"].as_f64() {
                        ctx.reply(format!("progress: {p:.2}%"));
                    }
                    if let Some(r) = v["remaining"].as_str() {
                        ctx.reply(format!("remaining time: {r}"));
                    }
                } else {
                    ctx.reply("Render-Threads are stopped!");
                }
                if pending > 0 {
                    ctx.reply(format!("{pending} maps have pending updates"));
                }
                return Ok(());
            }
            ctx.reply("BlueMap - not available on this server (Horizon has no map renderer)");
            ctx.reply("Render-Threads are stopped!");
            ctx.reply("0 pending render tasks");
        }
        Some(other) => {
            ctx.reply(format!("BlueMap is not available on this server: '{other}' did nothing."));
        }
    }
    Ok(())
}

pub fn plugin() -> Plugin {
    Plugin::new("compatcmds")
        .command("chunky [sub:word] [a:word] [b:word] [c:word] [d:word] [e:word]", 2, chunky)
        .command("bluemap [sub:word] [rest:text]", 2, bluemap)
        .every(20, |ctx| {
            // after a restart: tasks the engine no longer runs wait (Chunky `continue-on-restart: false`), or start again
            if ctx.tick() <= 20 {
                let resume = ctx.get("continue_on_restart").and_then(|v| v.as_bool()).unwrap_or(false);
                let mut t = tasks(ctx);
                for (_, v) in t.iter_mut() {
                    v["running"] = json::json!(false);
                    if !resume {
                        v["paused"] = json::json!(true);
                    }
                }
                let _ = save_tasks(ctx, t);
                return;
            }
            tick_tasks(ctx);
        })
}
