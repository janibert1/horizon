//! EssentialsX (+ EssentialsSpawn) replacement: the everyday player commands of the live SMP.
//!
//! Commands: spawn, setspawn, home, sethome, delhome, homes, warp, setwarp, delwarp, warps, tpa, tpahere, tpaccept,
//! tpdeny, tpacancel, tptoggle, back, r (reply to vanilla /msg|/tell|/w), nick, afk, seen, god, heal, feed, top,
//! speed, broadcast, motd, rules, suicide, and (src/extra.rs) enderchest, fly, hat, workbench, kit(s), createkit,
//! delkit, mail, setjail/deljail/jails/jail/unjail and signs. Vanilla names (msg, tell, tp, gamemode, ...) stay
//! vanilla's: Horizon refuses plugin registrations of them. Not here: economy (rewards' Shards are the economy), ping.
//!
//! Permissions (`Ctx::has_perm`, LuckPerms-style through the `perms` plugin): every command is `essentials.<cmd>`;
//! the ones in config `open_commands` need no node (default: the usual default-group set). Max homes:
//! `sethome_multiple` {"default": 3, "vip": 5} with nodes `essentials.sethome.multiple.<key>` (and `.unlimited`).
//! Config/data: `config/hplug/essentials.json`. Migration from EssentialsX: tools/migrate_essentials.py.
mod extra;
use hplug::{Args, CmdResult, Ctx, Damage, Plugin, json};
use json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

const OPEN_DEFAULT: &[&str] = &[
    "spawn",
    "home",
    "sethome",
    "delhome",
    "homes",
    "warp",
    "warps",
    "tpa",
    "tpahere",
    "tpaccept",
    "tpdeny",
    "tpacancel",
    "tptoggle",
    "back",
    "r",
    "afk",
    "seen",
    "rules",
    "motd",
    "suicide",
    "kit",
    "kits.tools",
    "mail",
    "mail.send",
];

#[derive(Clone, Debug)]
struct Loc {
    dim: String,
    pos: [f64; 3],
    look: [f32; 2],
}
impl Loc {
    fn of(p: &hplug::Player) -> Self {
        Loc {
            dim: p.dimension.clone(),
            pos: p.position,
            look: [p.yaw, p.pitch],
        }
    }
    fn to_json(&self) -> Value {
        json::json!({"world": self.dim, "x": self.pos[0], "y": self.pos[1], "z": self.pos[2], "yaw": self.look[0], "pitch": self.look[1]})
    }
    fn from_json(v: &Value) -> Option<Self> {
        Some(Loc {
            dim: v.get("world")?.as_str()?.to_owned(),
            pos: [
                v.get("x")?.as_f64()?,
                v.get("y")?.as_f64()?,
                v.get("z")?.as_f64()?,
            ],
            look: [
                v.get("yaw").and_then(|y| y.as_f64()).unwrap_or(0.0) as f32,
                v.get("pitch").and_then(|y| y.as_f64()).unwrap_or(0.0) as f32,
            ],
        })
    }
}

struct Tpa {
    from: u128,
    from_name: String,
    here: bool,
    expires: u64,
}
struct Pending {
    name: String,
    to: Loc,
    start: [f64; 3],
    at: u64,
    what: String,
}

#[derive(Default)]
struct State {
    tpa: HashMap<u128, Tpa>,      // target -> request
    reply: HashMap<u128, String>, // player -> last conversation partner (name)
    back: HashMap<u128, Loc>,
    pending: HashMap<u128, Pending>,
    afk: HashSet<u128>,
    idle: HashMap<u128, ([f64; 3], u64)>,
    gods: HashSet<i32>, // entity ids of players in god mode (damage hook is host-less)
    god_uuids: HashSet<u128>,
}
static STATE: Mutex<Option<State>> = Mutex::new(None);
fn st<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let mut g = STATE.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(State::default))
}

fn key(u: u128) -> String {
    hplug::uuid_dashed(u)
}
fn cfg_u64(ctx: &Ctx, k: &str, d: u64) -> u64 {
    ctx.get(k).and_then(|v| v.as_u64()).unwrap_or(d)
}
fn allowed(ctx: &Ctx, cmd: &str) -> bool {
    let Some(p) = ctx.try_player() else {
        return true;
    };
    let open: Vec<String> = ctx
        .get("open_commands")
        .and_then(|v| v.as_array().cloned())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_else(|| OPEN_DEFAULT.iter().map(|s| s.to_string()).collect());
    open.iter().any(|c| c == cmd) || ctx.has_perm(p.uuid, &p.name, &format!("essentials.{cmd}"))
}
fn me(ctx: &Ctx) -> Result<hplug::Player, String> {
    ctx.try_player()
        .cloned()
        .ok_or_else(|| "§cOnly players can do that.".to_owned())
}
fn guard(ctx: &Ctx, cmd: &str) -> CmdResult {
    if allowed(ctx, cmd) {
        Ok(())
    } else {
        Err("§4You do not have access to that command.".into())
    }
}

/// Teleport with Essentials' warmup (`teleport_delay` seconds, cancelled by moving), remembering /back.
fn go(ctx: &mut Ctx, p: &hplug::Player, to: Loc, what: &str) {
    let delay = cfg_u64(ctx, "teleport_delay", 0);
    if delay == 0 || ctx.has_perm(p.uuid, &p.name, "essentials.teleport.timer.bypass") {
        st(|s| s.back.insert(p.uuid, Loc::of(p)));
        ctx.teleport_to(&p.name, &to.dim, to.pos, Some(to.look));
        ctx.tell(
            p.uuid,
            format!(
                "§6Teleporting{}...",
                if what.is_empty() {
                    String::new()
                } else {
                    format!(" to §c{what}§6")
                }
            ),
        );
        return;
    }
    let at = ctx.tick() + delay * 20;
    st(|s| {
        s.pending.insert(
            p.uuid,
            Pending {
                name: p.name.clone(),
                to,
                start: p.position,
                at,
                what: what.to_owned(),
            },
        )
    });
    ctx.tell(
        p.uuid,
        format!("§6Teleportation will commence in §c{delay} seconds§6. Don't move."),
    );
}

fn max_homes(ctx: &Ctx, p: &hplug::Player) -> usize {
    if ctx.has_perm(p.uuid, &p.name, "essentials.sethome.multiple.unlimited") {
        return usize::MAX;
    }
    let table = ctx
        .get("sethome_multiple")
        .unwrap_or_else(|| json::json!({"default": 3}));
    table
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(k, _)| {
            k.as_str() == "default"
                || ctx.has_perm(p.uuid, &p.name, &format!("essentials.sethome.multiple.{k}"))
        })
        .filter_map(|(_, v)| v.as_u64())
        .max()
        .unwrap_or(1) as usize
}
fn homes_of(ctx: &Ctx, u: u128) -> json::Map<String, Value> {
    ctx.get("homes")
        .and_then(|h| h.get(key(u)).and_then(|x| x.as_object().cloned()))
        .unwrap_or_default()
}
fn set_homes(ctx: &mut Ctx, u: u128, m: json::Map<String, Value>) -> Result<(), String> {
    let mut all = ctx
        .get("homes")
        .and_then(|h| h.as_object().cloned())
        .unwrap_or_default();
    if m.is_empty() {
        all.remove(&key(u));
    } else {
        all.insert(key(u), Value::Object(m));
    }
    ctx.set("homes", Value::Object(all))
}
fn warps(ctx: &Ctx) -> json::Map<String, Value> {
    ctx.get("warps")
        .and_then(|w| w.as_object().cloned())
        .unwrap_or_default()
}
fn target(ctx: &Ctx, name: &str) -> Result<hplug::Player, String> {
    ctx.find_online(name)
        .ok_or_else(|| format!("§4Error: §cPlayer {name} not found."))
}

fn cmd_spawn(ctx: &mut Ctx, args: &Args) -> CmdResult {
    guard(ctx, "spawn")?;
    let spawn = ctx
        .get("spawn")
        .and_then(|v| Loc::from_json(&v))
        .ok_or("§4Error: §cThe spawn has not been set (/setspawn).")?;
    let who = match args.get(0) {
        Some(n)
            if ctx.op_level() >= 2
                || ctx
                    .try_player()
                    .is_some_and(|p| ctx.has_perm(p.uuid, &p.name, "essentials.spawn.others")) =>
        {
            target(ctx, n)?
        }
        _ => me(ctx)?,
    };
    go(ctx, &who, spawn, "spawn");
    Ok(())
}
fn cmd_setspawn(ctx: &mut Ctx, _: &Args) -> CmdResult {
    guard(ctx, "setspawn")?;
    let p = me(ctx)?;
    ctx.set("spawn", Loc::of(&p).to_json())?;
    ctx.console(format!(
        "execute in {} run setworldspawn {} {} {}",
        p.dimension,
        p.position[0].floor(),
        p.position[1].floor(),
        p.position[2].floor()
    ));
    ctx.reply("§6Spawn location set.");
    Ok(())
}
fn cmd_sethome(ctx: &mut Ctx, args: &Args) -> CmdResult {
    guard(ctx, "sethome")?;
    let p = me(ctx)?;
    let name = args
        .get(0)
        .map(|s| s.to_lowercase())
        .unwrap_or_else(|| "home".into());
    if !name
        .chars()
        .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
    {
        return Err("§4Error: §cInvalid home name.".into());
    }
    let mut h = homes_of(ctx, p.uuid);
    if !h.contains_key(&name) && h.len() >= max_homes(ctx, &p) {
        return Err(format!(
            "§4Error: §cYou cannot set more than §4{}§c homes.",
            max_homes(ctx, &p)
        ));
    }
    h.insert(name.clone(), Loc::of(&p).to_json());
    set_homes(ctx, p.uuid, h)?;
    ctx.reply(format!("§6Home§c {name}§6 set to your current location."));
    Ok(())
}
fn cmd_home(ctx: &mut Ctx, args: &Args) -> CmdResult {
    guard(ctx, "home")?;
    let p = me(ctx)?;
    // `home <player>:<home>` for staff (essentials.home.others)
    let (owner, name) = match args.get(0).and_then(|a| a.split_once(':')) {
        Some((who, h)) if ctx.has_perm(p.uuid, &p.name, "essentials.home.others") => {
            let (u, _) = ctx
                .find_online(who)
                .map(|x| (x.uuid, x.name))
                .or_else(|| ctx.uuid_for_name(who))
                .ok_or_else(|| format!("§4Error: §cPlayer {who} not found."))?;
            (u, h.to_lowercase())
        }
        _ => (
            p.uuid,
            args.get(0).map(|s| s.to_lowercase()).unwrap_or_default(),
        ),
    };
    let h = homes_of(ctx, owner);
    let name = if name.is_empty() {
        if h.len() == 1 {
            h.keys().next().unwrap().clone()
        } else if h.contains_key("home") {
            "home".into()
        } else if h.is_empty() {
            return Err("§4Error: §cYou do not have a home (/sethome).".into());
        } else {
            return Err(format!(
                "§6Homes: §r{}",
                h.keys().cloned().collect::<Vec<_>>().join(", ")
            ));
        }
    } else {
        name
    };
    let loc = h
        .get(&name)
        .and_then(Loc::from_json)
        .ok_or_else(|| format!("§4Error: §cHome {name} does not exist."))?;
    go(ctx, &p, loc, &name);
    Ok(())
}
fn cmd_delhome(ctx: &mut Ctx, args: &Args) -> CmdResult {
    guard(ctx, "delhome")?;
    let p = me(ctx)?;
    let name = args
        .get(0)
        .map(|s| s.to_lowercase())
        .ok_or("§cUsage: /delhome <name>")?;
    let mut h = homes_of(ctx, p.uuid);
    if h.remove(&name).is_none() {
        return Err(format!("§4Error: §cHome {name} does not exist."));
    }
    set_homes(ctx, p.uuid, h)?;
    ctx.reply(format!("§6Home§c {name}§6 has been removed."));
    Ok(())
}
fn cmd_homes(ctx: &mut Ctx, _: &Args) -> CmdResult {
    guard(ctx, "homes")?;
    let p = me(ctx)?;
    let h = homes_of(ctx, p.uuid);
    ctx.reply(format!(
        "§6Homes ({}/{}): §r{}",
        h.len(),
        match max_homes(ctx, &p) {
            usize::MAX => "∞".into(),
            n => n.to_string(),
        },
        h.keys().cloned().collect::<Vec<_>>().join(", ")
    ));
    Ok(())
}
fn cmd_warp(ctx: &mut Ctx, args: &Args) -> CmdResult {
    let Some(name) = args.get(0).map(|s| s.to_lowercase()) else {
        return cmd_warps(ctx, args);
    };
    guard(ctx, "warp")?;
    let p = me(ctx)?;
    if ctx
        .get("per_warp_permission")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
        && !ctx.has_perm(p.uuid, &p.name, &format!("essentials.warps.{name}"))
    {
        return Err("§4You do not have access to that warp.".into());
    }
    let loc = warps(ctx)
        .get(&name)
        .and_then(Loc::from_json)
        .ok_or_else(|| format!("§4Error: §cWarp {name} does not exist."))?;
    go(ctx, &p, loc, &name);
    Ok(())
}
fn cmd_setwarp(ctx: &mut Ctx, args: &Args) -> CmdResult {
    guard(ctx, "setwarp")?;
    let p = me(ctx)?;
    let name = args
        .get(0)
        .map(|s| s.to_lowercase())
        .ok_or("§cUsage: /setwarp <name>")?;
    let mut w = warps(ctx);
    w.insert(name.clone(), Loc::of(&p).to_json());
    ctx.set("warps", Value::Object(w))?;
    ctx.reply(format!("§6Warp§c {name}§6 set."));
    Ok(())
}
fn cmd_delwarp(ctx: &mut Ctx, args: &Args) -> CmdResult {
    guard(ctx, "delwarp")?;
    let name = args
        .get(0)
        .map(|s| s.to_lowercase())
        .ok_or("§cUsage: /delwarp <name>")?;
    let mut w = warps(ctx);
    if w.remove(&name).is_none() {
        return Err(format!("§4Error: §cWarp {name} does not exist."));
    }
    ctx.set("warps", Value::Object(w))?;
    ctx.reply(format!("§6Warp§c {name}§6 has been removed."));
    Ok(())
}
fn cmd_warps(ctx: &mut Ctx, _: &Args) -> CmdResult {
    guard(ctx, "warps")?;
    let names: Vec<String> = warps(ctx).keys().cloned().collect();
    ctx.reply(if names.is_empty() {
        "§6No warps defined.".into()
    } else {
        format!("§6Warps: §r{}", names.join(", "))
    });
    Ok(())
}
fn tpa_request(ctx: &mut Ctx, args: &Args, here: bool) -> CmdResult {
    guard(ctx, if here { "tpahere" } else { "tpa" })?;
    let p = me(ctx)?;
    let t = target(ctx, args.get(0).ok_or("§cUsage: /tpa <player>")?)?;
    if t.uuid == p.uuid {
        return Err("§4Error: §cYou cannot teleport to yourself.".into());
    }
    let blocked = ctx
        .get("tptoggle")
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .any(|x| x.as_str() == Some(&key(t.uuid)));
    if blocked {
        return Err(format!("§4Error: §c{} has teleportation disabled.", t.name));
    }
    let expires = ctx.tick() + cfg_u64(ctx, "tpa_timeout", 120) * 20;
    st(|s| {
        s.tpa.insert(
            t.uuid,
            Tpa {
                from: p.uuid,
                from_name: p.name.clone(),
                here,
                expires,
            },
        )
    });
    ctx.tell(
        t.uuid,
        format!(
            "§c{}§6 has requested {}.",
            p.name,
            if here {
                "that you teleport to them"
            } else {
                "to teleport to you"
            }
        ),
    );
    ctx.tell(
        t.uuid,
        "§6To teleport, type §c/tpaccept§6. To deny this request, type §c/tpdeny§6.",
    );
    ctx.reply(format!(
        "§6Request sent to§c {}§6. To cancel this request, type §c/tpacancel§6.",
        t.name
    ));
    Ok(())
}
fn cmd_tpaccept(ctx: &mut Ctx, _: &Args) -> CmdResult {
    guard(ctx, "tpaccept")?;
    let p = me(ctx)?;
    let now = ctx.tick();
    let req = st(|s| s.tpa.remove(&p.uuid))
        .filter(|r| r.expires > now)
        .ok_or("§4Error: §cYou do not have a pending request.")?;
    let other = ctx
        .online()
        .into_iter()
        .find(|x| x.uuid == req.from)
        .ok_or_else(|| format!("§4Error: §c{} is no longer online.", req.from_name))?;
    ctx.reply("§6Teleport request accepted.");
    ctx.tell(
        other.uuid,
        format!("§c{}§6 accepted your teleport request.", p.name),
    );
    if req.here {
        go(ctx, &p, Loc::of(&other), &other.name)
    } else {
        go(ctx, &other, Loc::of(&p), &p.name)
    }
    Ok(())
}
fn cmd_tpdeny(ctx: &mut Ctx, _: &Args) -> CmdResult {
    guard(ctx, "tpdeny")?;
    let p = me(ctx)?;
    let req =
        st(|s| s.tpa.remove(&p.uuid)).ok_or("§4Error: §cYou do not have a pending request.")?;
    ctx.reply("§6Teleport request denied.");
    ctx.tell(
        req.from,
        format!("§c{}§6 denied your teleport request.", p.name),
    );
    Ok(())
}
fn cmd_tpacancel(ctx: &mut Ctx, _: &Args) -> CmdResult {
    guard(ctx, "tpacancel")?;
    let p = me(ctx)?;
    let n = st(|s| {
        let before = s.tpa.len();
        s.tpa.retain(|_, r| r.from != p.uuid);
        before - s.tpa.len()
    });
    ctx.reply(if n > 0 {
        "§6All outstanding teleport requests cancelled."
    } else {
        "§4Error: §cYou do not have a pending request."
    });
    Ok(())
}
fn cmd_tptoggle(ctx: &mut Ctx, _: &Args) -> CmdResult {
    guard(ctx, "tptoggle")?;
    let p = me(ctx)?;
    let mut list = ctx
        .get("tptoggle")
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default();
    let k = key(p.uuid);
    let off = if list.iter().any(|x| x.as_str() == Some(&k)) {
        list.retain(|x| x.as_str() != Some(&k));
        false
    } else {
        list.push(Value::String(k));
        true
    };
    ctx.set("tptoggle", Value::Array(list))?;
    ctx.reply(if off {
        "§6Teleportation §cdisabled§6."
    } else {
        "§6Teleportation §cenabled§6."
    });
    Ok(())
}
fn cmd_back(ctx: &mut Ctx, _: &Args) -> CmdResult {
    guard(ctx, "back")?;
    let p = me(ctx)?;
    let loc = st(|s| s.back.get(&p.uuid).cloned())
        .ok_or("§4Error: §cYou have no last location to return to.")?;
    go(ctx, &p, loc, "your previous location");
    Ok(())
}
fn cmd_r(ctx: &mut Ctx, args: &Args) -> CmdResult {
    guard(ctx, "r")?;
    let p = me(ctx)?;
    let to = st(|s| s.reply.get(&p.uuid).cloned())
        .ok_or("§4Error: §cYou have nobody to whom you can reply.")?;
    if args.is_empty() {
        return Err("§cUsage: /r <message>".into());
    }
    let t = target(ctx, &to)?;
    // vanilla's /msg does the delivery (chat format, signing rules, the receiver's ignore settings)
    ctx.console(format!(
        "execute as {} run msg {} {}",
        p.name,
        t.name,
        args.rest(0)
    ));
    st(|s| {
        s.reply.insert(t.uuid, p.name.clone());
    });
    Ok(())
}
fn cmd_nick(ctx: &mut Ctx, args: &Args) -> CmdResult {
    guard(ctx, "nick")?;
    let raw = args
        .get(0)
        .ok_or("§cUsage: /nick <nickname|off> [player]")?
        .clone();
    let who = match args.get(1) {
        Some(n)
            if ctx.op_level() >= 2
                || ctx
                    .try_player()
                    .is_some_and(|p| ctx.has_perm(p.uuid, &p.name, "essentials.nick.others")) =>
        {
            target(ctx, n)?
        }
        _ => me(ctx)?,
    };
    let color = ctx
        .try_player()
        .is_none_or(|p| ctx.has_perm(p.uuid, &p.name, "essentials.nick.color"));
    let nick = if raw.eq_ignore_ascii_case("off") {
        None
    } else {
        let n = if color {
            raw.replace('&', "§")
        } else {
            raw.replace('§', "")
        };
        let plain: String = strip_codes(&n);
        let max = cfg_u64(ctx, "max_nick_length", 16) as usize;
        if plain.is_empty()
            || plain.chars().count() > max
            || !plain.chars().all(|c| c.is_alphanumeric() || c == '_')
        {
            return Err(format!(
                "§4Error: §cNicknames are 1-{max} letters, digits or _."
            ));
        }
        if ctx.host.text_allowed(&plain) == Some(false) {
            return Err("§4Error: §cThat nickname is not allowed.".into());
        }
        if ctx
            .online()
            .iter()
            .any(|o| o.uuid != who.uuid && o.name.eq_ignore_ascii_case(&plain))
        {
            return Err("§4Error: §cThat name is already in use.".into());
        }
        Some(n)
    };
    hplug::set_nickname_file(&*ctx.host, who.uuid, nick.as_deref())?;
    ctx.refresh_display_name(who.uuid, &who.name);
    ctx.reply(match &nick {
        Some(n) => format!("§6Nickname set to §r{n}§6."),
        None => "§6You no longer have a nickname.".into(),
    });
    Ok(())
}
fn strip_codes(s: &str) -> String {
    let mut out = String::new();
    let mut skip = false;
    for c in s.chars() {
        if skip {
            skip = false;
            continue;
        }
        if c == '§' {
            skip = true;
            continue;
        }
        out.push(c);
    }
    out
}
fn set_afk(ctx: &mut Ctx, p: &hplug::Player, afk: bool) {
    let changed = st(|s| {
        if afk {
            s.afk.insert(p.uuid)
        } else {
            s.afk.remove(&p.uuid)
        }
    });
    if changed
        && ctx
            .get("broadcast_afk")
            .and_then(|v| v.as_bool())
            .unwrap_or(true)
    {
        ctx.broadcast(format!(
            "§7* {} {}",
            p.name,
            if afk {
                "is now AFK."
            } else {
                "is no longer AFK."
            }
        ));
    }
}
/// Whether a player is AFK (for other plugins, e.g. claims' non-AFK time).
pub fn is_afk(uuid: u128) -> bool {
    st(|s| s.afk.contains(&uuid))
}
fn cmd_afk(ctx: &mut Ctx, _: &Args) -> CmdResult {
    guard(ctx, "afk")?;
    let p = me(ctx)?;
    let now = !is_afk(p.uuid);
    set_afk(ctx, &p, now);
    st(|s| {
        s.idle.insert(p.uuid, (p.position, ctx.tick()));
    });
    Ok(())
}
fn cmd_seen(ctx: &mut Ctx, args: &Args) -> CmdResult {
    guard(ctx, "seen")?;
    let name = args.get(0).ok_or("§cUsage: /seen <player>")?;
    if let Some(o) = ctx.find_online(name) {
        ctx.reply(format!("§6Player§c {}§6 has been §aonline§6.", o.name));
        return Ok(());
    }
    let (u, real) = ctx
        .uuid_for_name(name)
        .ok_or_else(|| format!("§4Error: §cPlayer {name} not found."))?;
    let last = ctx
        .get("seen")
        .and_then(|s| s.get(key(u)).and_then(|x| x.as_u64()));
    match last {
        Some(ms) => {
            let mins = hplug::now_ms().saturating_sub(ms) / 60_000;
            let ago = if mins < 60 {
                format!("{mins} minutes")
            } else if mins < 2880 {
                format!("{} hours", mins / 60)
            } else {
                format!("{} days", mins / 1440)
            };
            ctx.reply(format!(
                "§6Player§c {real}§6 has been §coffline§6 since §c{ago}§6."
            ));
        }
        None => ctx.reply(format!(
            "§6Player§c {real}§6 has not been seen since this server started tracking."
        )),
    }
    Ok(())
}
fn cmd_god(ctx: &mut Ctx, args: &Args) -> CmdResult {
    guard(ctx, "god")?;
    let who = match args.get(0) {
        Some(n) => target(ctx, n)?,
        None => me(ctx)?,
    };
    let on = st(|s| {
        if s.god_uuids.remove(&who.uuid) {
            s.gods.remove(&who.entity_id);
            false
        } else {
            s.god_uuids.insert(who.uuid);
            s.gods.insert(who.entity_id);
            true
        }
    });
    ctx.tell(
        who.uuid,
        format!(
            "§6God mode §c{}§6.",
            if on { "enabled" } else { "disabled" }
        ),
    );
    Ok(())
}
fn cmd_heal(ctx: &mut Ctx, args: &Args, feed_only: bool) -> CmdResult {
    guard(ctx, if feed_only { "feed" } else { "heal" })?;
    let who = match args.get(0) {
        Some(n) => target(ctx, n)?,
        None => me(ctx)?,
    };
    ctx.console(format!(
        "effect give {} minecraft:saturation 1 20 true",
        who.name
    ));
    if !feed_only {
        ctx.console(format!(
            "effect give {} minecraft:regeneration 3 10 true",
            who.name
        ));
        ctx.console(format!("effect clear {} minecraft:poison", who.name));
        ctx.console(format!("effect clear {} minecraft:wither", who.name));
        ctx.console(format!("extinguish {}", who.name));
    }
    ctx.tell(
        who.uuid,
        if feed_only {
            "§6Your appetite was sated."
        } else {
            "§6You have been healed."
        },
    );
    Ok(())
}
fn cmd_top(ctx: &mut Ctx, _: &Args) -> CmdResult {
    guard(ctx, "top")?;
    let p = me(ctx)?;
    let (x, z) = (p.position[0].floor() as i32, p.position[2].floor() as i32);
    let h = ctx
        .host
        .surface_heights(&p.dimension, &[(x, z)], hplug::Heightmap::MotionBlocking)
        .ok()
        .and_then(|v| v.first().copied().flatten())
        .ok_or("§4Error: §cNo safe spot above you.")?;
    go(
        ctx,
        &p,
        Loc {
            dim: p.dimension.clone(),
            pos: [x as f64 + 0.5, h as f64, z as f64 + 0.5],
            look: [p.yaw, p.pitch],
        },
        "the top",
    );
    Ok(())
}
fn cmd_speed(ctx: &mut Ctx, args: &Args) -> CmdResult {
    guard(ctx, "speed")?;
    let p = me(ctx)?;
    let n: f32 = args
        .get(0)
        .and_then(|s| s.parse().ok())
        .filter(|n: &f32| (0.0..=10.0).contains(n))
        .ok_or("§cUsage: /speed <0-10> [walk|fly]")?;
    let fly = match args.get(1).map(|s| s.to_lowercase()).as_deref() {
        Some("fly") => true,
        Some("walk") => false,
        _ => p.game_mode == 1 || p.game_mode == 3,
    };
    if fly {
        let s = if n == 1.0 { None } else { Some(0.05 * n) };
        let u = p.uuid;
        ctx.run_later(1, move |c| {
            c.host.set_flying_speed(u, s);
        });
    } else {
        ctx.console(format!(
            "attribute {} minecraft:movement_speed base set {:.4}",
            p.name,
            0.1 * n as f64
        ));
    }
    ctx.reply(format!(
        "§6Set {} speed to§c {n}§6.",
        if fly { "fly" } else { "walk" }
    ));
    Ok(())
}

pub fn plugin() -> Plugin {
    extra::register(
        Plugin::new("essentials")
            .command("spawn [player:word]", 0, cmd_spawn)
            .command("setspawn", 0, cmd_setspawn)
            .command("home [name:word]", 0, cmd_home)
            .command("sethome [name:word]", 0, cmd_sethome)
            .alias("createhome")
            .command("delhome <name:word>", 0, cmd_delhome)
            .alias("remhome")
            .command("homes", 0, cmd_homes)
            .command("warp [name:word]", 0, cmd_warp)
            .command("setwarp <name:word>", 0, cmd_setwarp)
            .command("delwarp <name:word>", 0, cmd_delwarp)
            .command("warps", 0, cmd_warps)
            .command("tpa <player:word>", 0, |c, a| tpa_request(c, a, false))
            .alias("call")
            .command("tpahere <player:word>", 0, |c, a| tpa_request(c, a, true))
            .command("tpaccept", 0, cmd_tpaccept)
            .alias("tpyes")
            .command("tpdeny", 0, cmd_tpdeny)
            .alias("tpno")
            .command("tpacancel", 0, cmd_tpacancel)
            .command("tptoggle", 0, cmd_tptoggle)
            .command("back", 0, cmd_back)
            .alias("return")
            .command("r <message:text>", 0, cmd_r)
            .alias("reply")
            .command("nick <nick:word> [player:word]", 0, cmd_nick)
            .alias("nickname")
            .command("afk", 0, cmd_afk)
            .alias("away")
            .command("seen <player:word>", 0, cmd_seen)
            .command("god [player:word]", 0, cmd_god)
            .alias("godmode")
            .command("heal [player:word]", 0, |c, a| cmd_heal(c, a, false))
            .command("feed [player:word]", 0, |c, a| cmd_heal(c, a, true))
            .alias("eat")
            .command("top", 0, cmd_top)
            .command("speed <n:word> [kind:word]", 0, cmd_speed)
            .command("broadcast <message:text>", 0, |ctx, args| {
                guard(ctx, "broadcast")?;
                let msg = args.rest(0).replace('&', "§");
                ctx.broadcast(format!("§6[§4Broadcast§6]§a {msg}"));
                Ok(())
            })
            .alias("bc")
            .command("motd", 0, |ctx, _| {
                guard(ctx, "motd")?;
                for l in ctx
                    .get("motd")
                    .and_then(|v| v.as_array().cloned())
                    .unwrap_or_default()
                {
                    if let Some(s) = l.as_str() {
                        ctx.reply(s.replace('&', "§"));
                    }
                }
                Ok(())
            })
            .command("rules", 0, |ctx, _| {
                guard(ctx, "rules")?;
                let rules = ctx
                    .get("rules")
                    .and_then(|v| v.as_array().cloned())
                    .unwrap_or_default();
                if rules.is_empty() {
                    ctx.reply("§6No rules set.");
                }
                for l in rules {
                    if let Some(s) = l.as_str() {
                        ctx.reply(s.replace('&', "§"));
                    }
                }
                Ok(())
            })
            .command("suicide", 0, |ctx, _| {
                guard(ctx, "suicide")?;
                let p = me(ctx)?;
                ctx.console(format!("kill {}", p.name));
                Ok(())
            })
            .on_join(|ctx| {
                let p = ctx.player().clone();
                for l in ctx
                    .get("motd")
                    .and_then(|v| v.as_array().cloned())
                    .unwrap_or_default()
                {
                    if let Some(s) = l.as_str() {
                        ctx.tell(p.uuid, s.replace('&', "§").replace("{player}", &p.name));
                    }
                }
                ctx.refresh_display_name(p.uuid, &p.name);
                // first join: to the spawn (EssentialsSpawn `spawn-on-join` for new players)
                let seen = ctx.get("seen").and_then(|s| s.get(key(p.uuid)).cloned());
                if seen.is_none()
                    && ctx
                        .get("spawn_new_players")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(true)
                    && let Some(spawn) = ctx.get("spawn").and_then(|v| Loc::from_json(&v))
                {
                    ctx.teleport_to(&p.name, &spawn.dim, spawn.pos, Some(spawn.look));
                }
                if st(|s| s.god_uuids.contains(&p.uuid)) {
                    st(|s| {
                        s.gods.insert(p.entity_id);
                    });
                }
                st(|s| {
                    s.idle.insert(p.uuid, (p.position, 0));
                });
                let mut seen_all = ctx
                    .get("seen")
                    .and_then(|v| v.as_object().cloned())
                    .unwrap_or_default();
                seen_all.insert(key(p.uuid), json::json!(hplug::now_ms()));
                let _ = ctx.set("seen", Value::Object(seen_all));
            })
            .on_quit(|ctx| {
                let Some(u) = ctx.player_uuid() else { return };
                let mut seen_all = ctx
                    .get("seen")
                    .and_then(|v| v.as_object().cloned())
                    .unwrap_or_default();
                seen_all.insert(key(u), json::json!(hplug::now_ms()));
                let _ = ctx.set("seen", Value::Object(seen_all));
                st(|s| {
                    s.afk.remove(&u);
                    s.idle.remove(&u);
                    s.pending.remove(&u);
                    s.tpa.remove(&u);
                    s.tpa.retain(|_, r| r.from != u);
                });
            })
            // /back: where a command teleport or a death left
            .on_teleport(|ctx, t| {
                if let Some(p) = ctx.try_player() {
                    let loc = Loc {
                        dim: t.from_dimension.to_owned(),
                        pos: t.from,
                        look: [p.yaw, p.pitch],
                    };
                    st(|s| {
                        s.back.insert(t.player, loc);
                    });
                }
                false
            })
            .on_death(|ctx, _| {
                if let Some(p) = ctx.try_player().cloned() {
                    st(|s| {
                        s.back.insert(p.uuid, Loc::of(&p));
                    });
                }
            })
            // remember conversation partners of vanilla /msg|/tell|/w for /r
            .on_command_line(|ctx, sender, line| {
                let mut w = line.split_whitespace();
                if let (
                    Some("msg" | "tell" | "w" | "minecraft:msg" | "minecraft:tell" | "minecraft:w"),
                    Some(to),
                ) = (w.next(), w.next())
                    && let (Some(me), Some(t)) = (
                        ctx.find_online(sender)
                            .or_else(|| ctx.try_player().cloned()),
                        ctx.find_online(to),
                    )
                {
                    st(|s| {
                        s.reply.insert(me.uuid, t.name.clone());
                        s.reply.insert(t.uuid, me.name.clone());
                    });
                }
                None
            })
            .on_damage(|d: &Damage| {
                d.victim_player
                    .is_some_and(|u| st(|s| s.god_uuids.contains(&u)))
            })
            .every(10, |ctx| {
                let now = ctx.tick();
                let online = ctx.online();
                // teleport warmups
                let due: Vec<(u128, Pending)> = st(|s| {
                    let keys: Vec<u128> = s
                        .pending
                        .iter()
                        .filter(|(_, p)| p.at <= now)
                        .map(|(k, _)| *k)
                        .collect();
                    keys.into_iter()
                        .filter_map(|k| s.pending.remove(&k).map(|p| (k, p)))
                        .collect()
                });
                for (u, p) in due {
                    if let Some(cur) = online.iter().find(|o| o.uuid == u) {
                        st(|s| {
                            s.back.insert(u, Loc::of(cur));
                        });
                        ctx.teleport_to(&p.name, &p.to.dim, p.to.pos, Some(p.to.look));
                        ctx.tell(
                            u,
                            format!(
                                "§6Teleporting{}...",
                                if p.what.is_empty() {
                                    String::new()
                                } else {
                                    format!(" to §c{}§6", p.what)
                                }
                            ),
                        );
                    }
                }
                let moved: Vec<u128> = st(|s| {
                    let gone: Vec<u128> = s
                        .pending
                        .iter()
                        .filter(|(k, p)| {
                            online
                                .iter()
                                .find(|o| o.uuid == **k)
                                .is_none_or(|o| dist2(o.position, p.start) > 0.25)
                        })
                        .map(|(k, _)| *k)
                        .collect();
                    for k in &gone {
                        s.pending.remove(k);
                    }
                    gone
                });
                for u in moved {
                    ctx.tell(u, "§4Pending teleportation request cancelled.");
                }
                // expired tpa requests
                st(|s| s.tpa.retain(|_, r| r.expires > now));
                // auto-afk (`auto_afk` seconds without moving, 300 by default; 0 = off)
                let auto = cfg_u64(ctx, "auto_afk", 300) * 20;
                for p in &online {
                    let (went_afk, back) = st(|s| {
                        let e = s.idle.entry(p.uuid).or_insert((p.position, now));
                        if dist2(e.0, p.position) > 0.01 {
                            *e = (p.position, now);
                            (false, s.afk.contains(&p.uuid))
                        } else {
                            (
                                auto > 0
                                    && now.saturating_sub(e.1) >= auto
                                    && !s.afk.contains(&p.uuid),
                                false,
                            )
                        }
                    });
                    if went_afk {
                        set_afk(ctx, p, true);
                    }
                    if back {
                        set_afk(ctx, p, false);
                    }
                }
                // entity ids change on respawn: keep the god set in step
                st(|s| {
                    s.gods = online
                        .iter()
                        .filter(|o| s.god_uuids.contains(&o.uuid))
                        .map(|o| o.entity_id)
                        .collect();
                });
            }),
    )
}

fn dist2(a: [f64; 3], b: [f64; 3]) -> f64 {
    (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)
}
