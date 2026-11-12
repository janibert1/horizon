//! TAB replacement: Tab-list header and footer, the list's sorting by rank, the scoreboard sidebar and boss bars,
//! with TAB's placeholders and animations, refreshed every `refresh_ticks` (40). Rank prefixes and suffixes in the
//! list and chat come from `Ctx::refresh_display_name` (permissions.json prefix/suffix + nickname + display tags).
//!
//! Config `config/hplug/tab.json` (tools/migrate_tab.py writes it from TAB's config.yml + animations.yml):
//! * `header` / `footer`: lists of lines.
//! * `sorting_groups`: TAB `sorting-types: GROUPS:a,b,c` order (first = top). Each player's primary group (their
//!   highest-weight parent, LuckPerms' primary group) puts them in scoreboard team `tab_<nn>`; the client sorts by
//!   team, then by name (TAB's second type `PLACEHOLDER_A_TO_Z:%player%`). Empty list = no sorting teams.
//! * `scoreboard`: {enabled, title, lines} (TAB `scoreboard`, off on the live server): a vanilla sidebar objective
//!   `tab_sb` shown to everyone (per-player placeholders in it show the values of no one: `{player}` etc. are blank;
//!   vanilla sidebars are shared). `/sb` (op 2) hides or shows it for everyone (vanilla display slots are global).
//! * `bossbar`: {enabled, bars: {name: {text, color, style, progress}}} (TAB `bossbar`, off on the live server):
//!   vanilla boss bars `tab:<name>`, shown to `@a[tag=!tab_nobar]`; `/tab bossbar` hides/shows them for yourself
//!   (TAB's `/bossbar` is vanilla's name on Horizon).
//! * `animations`: {name: {interval_ms, texts}} for `{animation:name}`.
//!
//! Placeholders: `{player}` `{online}` `{staffonline}` (online with `tab.staff` or op) `{max}` `{tps}` `{mspt}`
//! `{world}` `{worldonline}` `{group}` `{x}` `{y}` `{z}` `{time}` (HH:MM) `{date}` (YYYY-MM-DD) `{memory_used}` (MB,
//! this process) `{ping}` (not known to plugins: `?`) `{animation:name}`. Vanished players (vanish plugin) are
//! left out of the counts. `&` colour codes; TAB's `<#RRGGBB>` gradients are dropped by the migration (cosmetic).
use hplug::{json, Ctx, Plugin};
use json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

#[derive(Default)]
struct State {
    team_of: HashMap<u128, String>,
    teams: HashSet<String>,
    sidebar: Vec<String>,
    sidebar_hidden: bool,
    bars: HashMap<String, String>, // id -> last applied spec
    bar_hidden: HashSet<u128>,
}
static ST: Mutex<Option<State>> = Mutex::new(None);
fn st<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let mut g = ST.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(State::default))
}

fn lines(ctx: &Ctx, key: &str, default: &[&str]) -> Vec<String> {
    ctx.get(key)
        .and_then(|v| v.as_array().cloned())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_owned)).collect())
        .unwrap_or_else(|| default.iter().map(|s| s.to_string()).collect())
}
fn max_players(ctx: &Ctx) -> String {
    let Some(dir) = ctx.server_dir() else { return "?".into() };
    for (f, k) in [("server.toml", "max_players"), ("server.toml", "max-players"), ("server.properties", "max-players")] {
        if let Ok(t) = std::fs::read_to_string(dir.join(f)) {
            for l in t.lines() {
                let l = l.trim();
                if let Some(rest) = l.strip_prefix(k) {
                    let v = rest.trim_start_matches([' ', '=']).trim().trim_matches('"');
                    if v.chars().all(|c| c.is_ascii_digit()) && !v.is_empty() {
                        return v.to_owned();
                    }
                }
            }
        }
    }
    "?".into()
}
fn memory_mb() -> u64 {
    std::fs::read_to_string("/proc/self/statm")
        .ok()
        .and_then(|t| t.split_whitespace().nth(1).and_then(|x| x.parse::<u64>().ok()))
        .map(|pages| pages * 4096 / 1_048_576)
        .unwrap_or(0)
}
/// Civil date of a unix day number (Howard Hinnant's algorithm).
fn ymd(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}
/// The primary group: the highest-weight direct parent (LuckPerms `parents-by-weight`), else `default`.
fn primary_group(perms: &Value, uuid: u128, name: &str) -> String {
    let groups = hplug::player_groups(perms, uuid, name);
    groups
        .into_iter()
        .max_by_key(|g| perms["groups"].get(g).and_then(|x| x.get("weight")).and_then(|w| w.as_i64()).unwrap_or(0))
        .unwrap_or_else(|| "default".into())
}

/// Everything a line can show that is the same for everyone.
struct Global {
    online: usize,
    staff: usize,
    max: String,
    tps: f32,
    mspt: f64,
    time: String,
    date: String,
    memory: u64,
    per_world: HashMap<String, usize>,
    anim: HashMap<String, String>,
}
fn global(ctx: &Ctx, online: &[hplug::Player]) -> Global {
    let hidden_file = ctx.host.config_dir().ok().and_then(|d| std::fs::read_to_string(d.join("hplug/vanish.json")).ok()).unwrap_or_default();
    let visible: Vec<&hplug::Player> = online.iter().filter(|p| !hidden_file.contains(&hplug::uuid_dashed(p.uuid))).collect();
    let (rate, ms) = ctx.host.tick_stats();
    let tps = rate.min(if ms > 0.0 { (1000.0 / ms) as f32 } else { rate });
    let now = hplug::now_ms();
    let secs = now / 1000;
    let (y, m, d) = ymd((secs / 86400) as i64);
    let mut per_world = HashMap::new();
    for p in &visible {
        *per_world.entry(p.dimension.clone()).or_insert(0) += 1;
    }
    let mut anim = HashMap::new();
    for (name, a) in ctx.get("animations").and_then(|v| v.as_object().cloned()).unwrap_or_default() {
        let texts: Vec<String> = a["texts"].as_array().into_iter().flatten().filter_map(|x| x.as_str().map(str::to_owned)).collect();
        if texts.is_empty() {
            continue;
        }
        let every = a["interval_ms"].as_u64().unwrap_or(1000).max(50);
        anim.insert(name, texts[((now / every) % texts.len() as u64) as usize].clone());
    }
    Global {
        online: visible.len(),
        staff: visible.iter().filter(|p| p.permission >= 2 || ctx.has_perm(p.uuid, &p.name, "tab.staff")).count(),
        max: max_players(ctx),
        tps,
        mspt: ms,
        time: format!("{:02}:{:02}", secs / 3600 % 24, secs / 60 % 60),
        date: format!("{y:04}-{m:02}-{d:02}"),
        memory: memory_mb(),
        per_world,
        anim,
    }
}
fn fill(s: &str, g: &Global, p: Option<(&hplug::Player, &str)>) -> String {
    let mut out = s.to_owned();
    // animations first: their frames may hold other placeholders
    for _ in 0..2 {
        if !out.contains("{animation:") {
            break;
        }
        for (k, v) in &g.anim {
            out = out.replace(&format!("{{animation:{k}}}"), v);
        }
    }
    let (player, world, x, y, z, worldonline, group) = match p {
        Some((p, group)) => (
            p.name.clone(),
            p.dimension.trim_start_matches("minecraft:").to_owned(),
            format!("{:.0}", p.position[0]),
            format!("{:.0}", p.position[1]),
            format!("{:.0}", p.position[2]),
            g.per_world.get(&p.dimension).copied().unwrap_or(0).to_string(),
            group.to_owned(),
        ),
        None => Default::default(),
    };
    out.replace('&', "§")
        .replace("{player}", &player)
        .replace("{online}", &g.online.to_string())
        .replace("{staffonline}", &g.staff.to_string())
        .replace("{max}", &g.max)
        .replace("{tps}", &format!("{:.1}", g.tps))
        .replace("{mspt}", &format!("{:.1}", g.mspt))
        .replace("{world}", &world)
        .replace("{worldonline}", &worldonline)
        .replace("{group}", &group)
        .replace("{x}", &x)
        .replace("{y}", &y)
        .replace("{z}", &z)
        .replace("{time}", &g.time)
        .replace("{date}", &g.date)
        .replace("{memory_used}", &g.memory.to_string())
        .replace("{ping}", "?")
}
fn json_text(s: &str) -> String {
    json::json!(s).to_string()
}

fn sort_teams(ctx: &mut Ctx, online: &[hplug::Player], perms: &Value) {
    let order: Vec<String> = lines(ctx, "sorting_groups", &[]).into_iter().map(|g| g.to_lowercase()).collect();
    if order.is_empty() {
        return;
    }
    for p in online {
        let g = primary_group(perms, p.uuid, &p.name);
        let idx = order.iter().position(|o| *o == g).unwrap_or(order.len());
        let team = format!("tab_{idx:02}");
        let (new_team, changed) = st(|s| {
            let new_team = s.teams.insert(team.clone());
            let changed = s.team_of.get(&p.uuid) != Some(&team);
            if changed {
                s.team_of.insert(p.uuid, team.clone());
            }
            (new_team, changed)
        });
        if new_team {
            ctx.console(format!("team add {team}"));
        }
        if changed {
            ctx.console(format!("team join {team} {}", p.name));
        }
    }
}

fn sidebar(ctx: &mut Ctx, g: &Global) {
    let cfg = ctx.get("scoreboard").unwrap_or(Value::Null);
    let on = cfg["enabled"].as_bool().unwrap_or(false) && !st(|s| s.sidebar_hidden);
    if !on {
        if st(|s| std::mem::take(&mut s.sidebar)).is_empty() {
            return;
        }
        ctx.console("scoreboard objectives remove tab_sb");
        return;
    }
    let title = fill(cfg["title"].as_str().unwrap_or("TAB"), g, None);
    let mut want: Vec<String> = cfg["lines"].as_array().into_iter().flatten().filter_map(|x| x.as_str()).map(|l| fill(&l.replace("||", " "), g, None)).collect();
    want.truncate(15);
    let mut spec = want.clone();
    spec.push(title.clone());
    let old = st(|s| std::mem::replace(&mut s.sidebar, spec.clone()));
    if old == spec {
        return;
    }
    if old.is_empty() {
        ctx.console(format!("scoreboard objectives add tab_sb dummy {}", json_text(&title)));
        ctx.console("scoreboard objectives setdisplay sidebar tab_sb");
        ctx.console("scoreboard objectives modify tab_sb numberformat blank");
    } else {
        ctx.console(format!("scoreboard objectives modify tab_sb displayname {}", json_text(&title)));
    }
    let n = want.len();
    for (i, l) in want.iter().enumerate() {
        let holder = format!("#tab{i:02}");
        ctx.console(format!("scoreboard players set {holder} tab_sb {}", n - i));
        ctx.console(format!("scoreboard players display name {holder} tab_sb {}", json_text(if l.is_empty() { " " } else { l })));
    }
    for i in n..old.len().saturating_sub(1).max(n) {
        ctx.console(format!("scoreboard players reset #tab{i:02} tab_sb"));
    }
}

fn bossbars(ctx: &mut Ctx, g: &Global, online: &[hplug::Player]) {
    let cfg = ctx.get("bossbar").unwrap_or(Value::Null);
    let enabled = cfg["enabled"].as_bool().unwrap_or(false);
    let bars = cfg["bars"].as_object().cloned().unwrap_or_default();
    let shown_to: Vec<String> = st(|s| online.iter().filter(|p| !s.bar_hidden.contains(&p.uuid)).map(|p| p.name.clone()).collect());
    for (name, b) in &bars {
        let id = format!("tab:{}", name.to_lowercase().replace(|c: char| !c.is_ascii_alphanumeric() && c != '_', "_"));
        if !enabled {
            if st(|s| s.bars.remove(&id)).is_some() {
                ctx.console(format!("bossbar remove {id}"));
            }
            continue;
        }
        let text = fill(b["text"].as_str().unwrap_or(""), g, None);
        let color = fill(b["color"].as_str().unwrap_or("white"), g, None).to_lowercase();
        let color = if ["pink", "blue", "red", "green", "yellow", "purple", "white"].contains(&color.as_str()) { color } else { "white".into() };
        let style = match b["style"].as_str().unwrap_or("PROGRESS").to_uppercase().as_str() {
            "NOTCHED_6" => "notched_6",
            "NOTCHED_10" => "notched_10",
            "NOTCHED_12" => "notched_12",
            "NOTCHED_20" => "notched_20",
            _ => "progress",
        };
        let progress = fill(&b["progress"].as_str().map(str::to_owned).unwrap_or_else(|| b["progress"].to_string()), g, None).trim().parse::<f64>().unwrap_or(100.0).clamp(0.0, 100.0) as u32;
        let spec = format!("{text}|{color}|{style}|{progress}|{}", shown_to.join(","));
        let old = st(|s| s.bars.insert(id.clone(), spec.clone()));
        if old.as_deref() == Some(&spec) {
            continue;
        }
        if old.is_none() {
            ctx.console(format!("bossbar add {id} {}", json_text(&text)));
            ctx.console(format!("bossbar set {id} max 100"));
        }
        ctx.console(format!("bossbar set {id} name {}", json_text(&text)));
        ctx.console(format!("bossbar set {id} color {color}"));
        ctx.console(format!("bossbar set {id} style {style}"));
        ctx.console(format!("bossbar set {id} value {progress}"));
        // one selector: players who hid the bars carry the tag `tab_nobar`
        ctx.console(format!("bossbar set {id} players @a[tag=!tab_nobar]"));
        ctx.console(format!("bossbar set {id} visible true"));
    }
}

fn refresh(ctx: &mut Ctx) {
    let header = lines(ctx, "header", &["&b&lHorizons SMP", "&7{online}/{max} online"]);
    let footer = lines(ctx, "footer", &["&7TPS &a{tps} &8| &7MSPT &a{mspt}", "&8discord.gg/horizons"]);
    let online = ctx.online();
    let g = global(ctx, &online);
    let perms = hplug::read_permissions(&*ctx.host);
    for p in &online {
        let group = primary_group(&perms, p.uuid, &p.name);
        let h = hplug::legacy(&header.iter().map(|l| fill(l, &g, Some((p, &group)))).collect::<Vec<_>>().join("\n"));
        let f = hplug::legacy(&footer.iter().map(|l| fill(l, &g, Some((p, &group)))).collect::<Vec<_>>().join("\n"));
        ctx.host.tab_list(p.uuid, &h, &f);
    }
    sort_teams(ctx, &online, &perms);
    sidebar(ctx, &g);
    bossbars(ctx, &g, &online);
}

pub fn plugin() -> Plugin {
    Plugin::new("tab")
        .command("tab [sub:word]", 0, |ctx, args| {
            match args.get(0).map(|s| s.to_lowercase()).as_deref() {
                Some("bossbar") => {
                    let p = ctx.try_player().cloned().ok_or("§cOnly players can do that.")?;
                    let hidden = st(|s| {
                        let now = !s.bar_hidden.remove(&p.uuid);
                        if now {
                            s.bar_hidden.insert(p.uuid);
                        }
                        now
                    });
                    ctx.console(format!("tag {} {} tab_nobar", p.name, if hidden { "add" } else { "remove" }));
                    ctx.reply(if hidden { "§7Boss bars hidden." } else { "§aBoss bars shown." });
                    refresh(ctx);
                }
                Some("reload") | None => {
                    if ctx.op_level() < 2 {
                        return Err("§cYou do not have permission to do that.".into());
                    }
                    for p in ctx.online() {
                        ctx.refresh_display_name(p.uuid, &p.name);
                    }
                    st(|s| {
                        s.team_of.clear();
                        s.bars.clear();
                        s.sidebar.clear();
                    });
                    refresh(ctx);
                    ctx.reply("§aTab list and display names refreshed.");
                }
                Some(_) => ctx.reply("§7Usage: /tab reload | bossbar"),
            }
            Ok(())
        })
        .command("sb", 2, |ctx, _| {
            let on = st(|s| {
                s.sidebar_hidden = !s.sidebar_hidden;
                !s.sidebar_hidden
            });
            ctx.reply(if on { "§aSidebar shown." } else { "§7Sidebar hidden." });
            refresh(ctx);
            Ok(())
        })
        .on_join(|ctx| {
            let (u, n) = (ctx.player().uuid, ctx.player().name.clone());
            ctx.refresh_display_name(u, &n);
            st(|s| {
                s.team_of.remove(&u);
                s.bar_hidden.remove(&u); // TAB `remember-toggle-choice: false`
                s.bars.clear(); // the new player is added to the bars at the next refresh
            });
            ctx.console(format!("tag {n} remove tab_nobar"));
        })
        .on_quit(|ctx| {
            if let Some(u) = ctx.player_uuid() {
                st(|s| {
                    s.team_of.remove(&u);
                    s.bars.clear();
                });
            }
        })
        .every(20, |ctx| {
            let every = ctx.get("refresh_ticks").and_then(|v| v.as_u64()).unwrap_or(40).max(20);
            if ctx.tick() % every >= 20 {
                return;
            }
            refresh(ctx);
        })
}
