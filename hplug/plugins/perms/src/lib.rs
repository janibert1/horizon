//! LuckPerms replacement: `/lp` (alias `luckperms`, `perms`) over hplug's `config/hplug/permissions.json`, which every
//! plugin reads through `Ctx::has_perm`. The live server granted cosmetics through `lp user <name> permission set <node>`
//! (web shop / rewards over RCON); those command lines work unchanged here.
//!
//! Supported (LuckPerms syntax, op level 3 for players, always for console/RCON):
//! `lp user|group <who> info
//!   | permission set <node> [true|false] [ctx..] | settemp <node> <true|false> <duration> [ctx..] | unset|unsettemp
//!     <node> [ctx..] | check <node> | clear [ctx..] | info
//!   | parent add|remove|set|addtemp <group> [duration]|removetemp|info [ctx..]
//!   | meta setprefix|setsuffix [weight] <text> | removeprefix|removesuffix | set <key> <value> | unset <key> | clear`
//! `lp user <user> promote|demote <track>`, `lp group <group> setweight <n>`
//! `lp createtrack|deletetrack <t> | listtracks | track <t> info|append <g>|insert <g> <pos>|remove <g>|clear`
//! `lp creategroup <g> | deletegroup <g> | listgroups | sync | reload | info`
//! Contexts are `world=<bukkit name or dimension id>` and `server=<name>` (permissions.json `server`, default
//! `global`); other context keys never match. Durations: `1d2h`, `30m`, seconds, or a unix time. Expired entries
//! are pruned every minute. Evaluation (contexts, expiry, meta, suffix) is the SDK's (`Ctx::has_perm`,
//! `Ctx::perm_meta`). Not here: the web editor, `lp export/import` (tools/migrate_luckperms.py), bulkupdate.
//! `<user>` is a name (usercache.json or online) or a uuid. Unknown users are created on first grant (Paper does too).
use hplug::{json, Args, CmdResult, Ctx, Plugin};
use json::{Map, Value};

fn load(ctx: &Ctx) -> Value {
    let mut v = hplug::read_permissions(&*ctx.host);
    if !v.is_object() {
        v = Value::Object(Map::new());
    }
    for k in ["groups", "players", "tracks"] {
        if v.get(k).is_none_or(|x| !x.is_object()) {
            v[k] = Value::Object(Map::new());
        }
    }
    if v["groups"].get("default").is_none() {
        v["groups"]["default"] = json::json!({"nodes": [], "weight": 0});
    }
    v
}
fn save(ctx: &Ctx, v: &Value) -> Result<(), String> {
    hplug::write_permissions(&*ctx.host, v)
}

/// The JSON object for a group / player, upgraded from the list-only shape.
fn obj<'v>(v: &'v mut Value, table: &str, key: &str) -> &'v mut Map<String, Value> {
    let slot = &mut v[table][key];
    if slot.is_array() {
        let nodes = slot.take();
        *slot = json::json!({ "nodes": nodes });
    } else if !slot.is_object() {
        *slot = json::json!({ "nodes": [] });
    }
    slot.as_object_mut().unwrap()
}
fn list<'m>(m: &'m mut Map<String, Value>, key: &str) -> &'m mut Vec<Value> {
    if !m.get(key).is_some_and(|x| x.is_array()) {
        m.insert(key.into(), Value::Array(vec![]));
    }
    m.get_mut(key).unwrap().as_array_mut().unwrap()
}
/// The node of an entry (`"x"`, `"-x"` or `{"node": "x", ...}`), without the `-`.
fn entry_key(e: &Value) -> Option<&str> {
    let raw = e.as_str().or_else(|| e.get("node").and_then(|n| n.as_str()))?;
    Some(raw.strip_prefix('-').unwrap_or(raw))
}
fn entry_ctx(e: &Value) -> Map<String, Value> {
    e.get("context").and_then(|c| c.as_object().cloned()).unwrap_or_default()
}
fn is_temp(e: &Value) -> bool {
    e.get("expiry").and_then(|x| x.as_u64()).is_some_and(|x| x > 0)
}
/// Remove `node` entries in exactly context `ctx` (LuckPerms: an unset without contexts touches the global ones),
/// temporary ones only when `temp`.
fn strip(nodes: &mut Vec<Value>, node: &str, ctx: &Map<String, Value>, temp: bool) -> usize {
    let before = nodes.len();
    nodes.retain(|n| !(entry_key(n) == Some(node) && entry_ctx(n) == *ctx && is_temp(n) == temp));
    before - nodes.len()
}
/// `world=world server=lobby` words (anywhere after the fixed arguments) as a context map, the other words kept.
fn split_context(words: &[String]) -> (Vec<String>, Map<String, Value>) {
    let mut rest = Vec::new();
    let mut ctx = Map::new();
    for w in words {
        match w.split_once('=') {
            Some((k, v)) if !k.is_empty() && !v.is_empty() && !k.contains('.') => {
                ctx.insert(k.to_lowercase(), Value::String(v.to_lowercase()));
            }
            _ => rest.push(w.clone()),
        }
    }
    (rest, ctx)
}
/// LuckPerms durations: `1d2h30m`, `90s`, `1w`, a bare number of seconds; or an absolute unix time > 10^9.
fn parse_duration(s: &str) -> Option<u64> {
    if let Ok(n) = s.parse::<u64>() {
        return Some(n);
    }
    let mut total = 0u64;
    let mut num = String::new();
    let mut unit = String::new();
    let flush = |num: &mut String, unit: &mut String, total: &mut u64| -> Option<()> {
        if num.is_empty() {
            return if unit.is_empty() { Some(()) } else { None };
        }
        let n: u64 = num.parse().ok()?;
        let mult = match unit.as_str() {
            "s" | "sec" | "secs" | "second" | "seconds" => 1,
            "m" | "min" | "mins" | "minute" | "minutes" => 60,
            "h" | "hr" | "hour" | "hours" => 3600,
            "d" | "day" | "days" => 86400,
            "w" | "week" | "weeks" => 604800,
            "mo" | "month" | "months" => 2_592_000,
            "y" | "year" | "years" => 31_536_000,
            _ => return None,
        };
        *total += n * mult;
        num.clear();
        unit.clear();
        Some(())
    };
    for c in s.to_lowercase().chars() {
        if c.is_ascii_digit() {
            if !unit.is_empty() {
                flush(&mut num, &mut unit, &mut total)?;
            }
            num.push(c);
        } else if c.is_ascii_alphabetic() {
            unit.push(c);
        } else {
            return None;
        }
    }
    flush(&mut num, &mut unit, &mut total)?;
    (total > 0).then_some(total)
}
fn fmt_ctx(ctx: &Map<String, Value>) -> String {
    if ctx.is_empty() {
        String::new()
    } else {
        format!(" §7in context §f{}", ctx.iter().map(|(k, v)| format!("{k}={}", v.as_str().unwrap_or(""))).collect::<Vec<_>>().join(", "))
    }
}
fn fmt_left(expiry: u64) -> String {
    let left = expiry.saturating_sub(hplug::now_ms() / 1000);
    let (d, h, m, s) = (left / 86400, left / 3600 % 24, left / 60 % 60, left % 60);
    let mut v = Vec::new();
    for (n, u) in [(d, "d"), (h, "h"), (m, "m"), (s, "s")] {
        if n > 0 {
            v.push(format!("{n}{u}"));
        }
    }
    if v.is_empty() { "0s".into() } else { v.join(" ") }
}
fn describe(e: &Value) -> String {
    let key = e.as_str().or_else(|| e.get("node").and_then(|n| n.as_str())).or_else(|| e.get("group").and_then(|n| n.as_str())).unwrap_or("?");
    let neg = e.get("value").and_then(|v| v.as_bool()) == Some(false);
    let mut out = if neg { format!("-{key}") } else { key.to_owned() };
    if let Some(x) = e.get("expiry").and_then(|x| x.as_u64()).filter(|x| *x > 0) {
        out.push_str(&format!(" (expires in {})", fmt_left(x)));
    }
    if !entry_ctx(e).is_empty() {
        out.push_str(&format!(" [{}]", entry_ctx(e).iter().map(|(k, v)| format!("{k}={}", v.as_str().unwrap_or(""))).collect::<Vec<_>>().join(",")));
    }
    out
}
/// A node entry: a plain string when global and permanent (the old format), else an object.
fn make_entry(field: &str, key: &str, value: bool, expiry: Option<u64>, ctx: &Map<String, Value>) -> Value {
    if expiry.is_none() && ctx.is_empty() {
        return Value::String(if field == "node" && !value { format!("-{key}") } else { key.to_owned() });
    }
    let mut o = Map::new();
    o.insert(field.into(), Value::String(key.to_owned()));
    if field == "node" {
        o.insert("value".into(), Value::Bool(value));
    }
    if let Some(x) = expiry {
        o.insert("expiry".into(), json::json!(x));
    }
    if !ctx.is_empty() {
        o.insert("context".into(), Value::Object(ctx.clone()));
    }
    Value::Object(o)
}
/// Drop expired temporary nodes and parents (LuckPerms does on its own); true when something went.
fn prune(v: &mut Value) -> bool {
    let now = hplug::now_ms() / 1000;
    let mut changed = false;
    for table in ["groups", "players"] {
        for (_, e) in v[table].as_object_mut().into_iter().flatten() {
            for k in ["nodes", "groups", "inherit"] {
                if let Some(l) = e.get_mut(k).and_then(|x| x.as_array_mut()) {
                    let before = l.len();
                    l.retain(|n| n.get("expiry").and_then(|x| x.as_u64()).is_none_or(|x| x == 0 || x > now));
                    changed |= l.len() != before;
                }
            }
        }
    }
    changed
}

struct User {
    uuid: u128,
    name: String,
    key: String,
}
fn resolve_user(ctx: &Ctx, raw: &str) -> Result<User, String> {
    if let Some(u) = hplug::parse_uuid(raw) {
        let name = ctx.online().into_iter().find(|p| p.uuid == u).map(|p| p.name).unwrap_or_else(|| raw.to_owned());
        return Ok(User { uuid: u, name, key: hplug::uuid_dashed(u) });
    }
    let (uuid, name) = ctx
        .find_online(raw)
        .map(|p| (p.uuid, p.name))
        .or_else(|| ctx.uuid_for_name(raw))
        .ok_or_else(|| format!("§cA user for §4{raw}§c could not be found."))?;
    Ok(User { uuid, name, key: hplug::uuid_dashed(uuid) })
}

fn bool_arg(a: Option<&String>) -> Result<bool, String> {
    match a.map(|s| s.to_lowercase()).as_deref() {
        None | Some("true") => Ok(true),
        Some("false") => Ok(false),
        Some(o) => Err(format!("§c'{o}' is not true/false.")),
    }
}

/// `permission ...` on a group or player object; returns the reply.
fn words(args: &Args) -> Vec<String> {
    args.0.iter().flat_map(|a| a.split_whitespace().map(str::to_owned).collect::<Vec<_>>()).collect()
}

fn permission_cmd(m: &mut Map<String, Value>, who: &str, args: &Args, at: usize, check: impl Fn(&str) -> bool) -> Result<String, String> {
    let (w, ctx) = split_context(&words(args));
    let sub = w.get(at).map(|s| s.to_lowercase()).unwrap_or_default();
    let node = w.get(at + 1).cloned();
    match sub.as_str() {
        "set" | "settemp" => {
            let temp = sub == "settemp";
            let node = node.ok_or(if temp { "§cUsage: permission settemp <node> <true|false> <duration> [context...]" } else { "§cUsage: permission set <node> [true|false] [context...]" })?;
            let value = bool_arg(w.get(at + 2))?;
            let expiry = if temp {
                let d = w.get(at + 3).and_then(|d| parse_duration(d)).ok_or("§cUsage: permission settemp <node> <true|false> <duration> [context...]")?;
                Some(if d > 1_000_000_000 { d } else { hplug::now_ms() / 1000 + d })
            } else {
                None
            };
            let nodes = list(m, "nodes");
            strip(nodes, &node, &ctx, temp);
            nodes.push(make_entry("node", &node, value, expiry, &ctx));
            Ok(match expiry {
                Some(x) => format!("§aSet §b{node}§a to §b{value}§a for §b{who}§a for a duration of §b{}§a{}.", fmt_left(x), fmt_ctx(&ctx)),
                None => format!("§aSet §b{node}§a to §b{value}§a for §b{who}§a{}.", fmt_ctx(&ctx)),
            })
        }
        "unset" | "unsettemp" => {
            let node = node.ok_or("§cUsage: permission unset <node> [context...]")?;
            let n = strip(list(m, "nodes"), &node, &ctx, sub == "unsettemp");
            Ok(if n > 0 { format!("§aUnset §b{node}§a for §b{who}§a{}.", fmt_ctx(&ctx)) } else { format!("§b{who}§a does not have §b{node}§a set{}.", fmt_ctx(&ctx)) })
        }
        "check" => {
            let node = node.ok_or("§cUsage: permission check <node>")?;
            Ok(format!("§b{who}§a has §b{node}§a set to §b{}§a.", check(&node)))
        }
        "clear" => {
            list(m, "nodes").retain(|n| !ctx.is_empty() && entry_ctx(n) != ctx);
            Ok(format!("§b{who}§a's permissions were cleared{}.", fmt_ctx(&ctx)))
        }
        "info" | "" => {
            let nodes: Vec<String> = m.get("nodes").and_then(|n| n.as_array()).into_iter().flatten().map(describe).collect();
            Ok(format!("§b{who}§a's permissions: §f{}", if nodes.is_empty() { "(none)".into() } else { nodes.join(", ") }))
        }
        o => Err(format!("§cUnknown permission subcommand '{o}'.")),
    }
}

/// `parent add|remove|set|addtemp|removetemp|info <group> [duration] [context...]` over `field` ("groups" of a
/// user, "inherit" of a group).
fn parent_cmd(v: &mut Value, table: &str, key: &str, field: &str, who: &str, args: &Args, at: usize) -> Result<String, String> {
    let (w, ctx) = split_context(&words(args));
    let op = w.get(at).map(|s| s.to_lowercase()).unwrap_or_else(|| "info".into());
    if op == "info" {
        let m = obj(v, table, key);
        let l: Vec<String> = m.get(field).and_then(|x| x.as_array()).into_iter().flatten().map(describe).collect();
        return Ok(format!("§b{who}§a's parents: §f{}", if l.is_empty() { "default".into() } else { l.join(", ") }));
    }
    let group = w.get(at + 1).map(|s| s.to_lowercase()).ok_or(format!("§cUsage: parent {op} <group>"))?;
    if !op.starts_with("remove") && v["groups"].get(&group).is_none() {
        return Err(format!("§cA group named §4{group}§c could not be found."));
    }
    let expiry = if op == "addtemp" {
        let d = w.get(at + 2).and_then(|d| parse_duration(d)).ok_or("§cUsage: parent addtemp <group> <duration> [context...]")?;
        Some(if d > 1_000_000_000 { d } else { hplug::now_ms() / 1000 + d })
    } else {
        None
    };
    let m = obj(v, table, key);
    if table == "players" {
        m.insert("name".into(), Value::String(who.to_owned()));
    }
    let l = list(m, field);
    let same = |e: &Value, temp: bool| e.as_str().or_else(|| e.get("group").and_then(|g| g.as_str())) == Some(&group) && entry_ctx(e) == ctx && is_temp(e) == temp;
    Ok(match op.as_str() {
        "add" | "addtemp" => {
            let temp = op == "addtemp";
            if !temp && l.iter().any(|e| same(e, false)) {
                format!("§b{who}§a already inherits from §b{group}§a{}.", fmt_ctx(&ctx))
            } else {
                l.retain(|e| !same(e, temp));
                l.push(make_entry("group", &group, true, expiry, &ctx));
                match expiry {
                    Some(x) => format!("§b{who}§a now inherits permissions from §b{group}§a for a duration of §b{}§a{}.", fmt_left(x), fmt_ctx(&ctx)),
                    None => format!("§b{who}§a now inherits permissions from §b{group}§a{}.", fmt_ctx(&ctx)),
                }
            }
        }
        "remove" | "removetemp" => {
            let temp = op == "removetemp";
            let before = l.len();
            l.retain(|e| !same(e, temp));
            if l.len() == before {
                format!("§b{who}§a does not inherit from §b{group}§a{}.", fmt_ctx(&ctx))
            } else {
                format!("§b{who}§a no longer inherits permissions from §b{group}§a{}.", fmt_ctx(&ctx))
            }
        }
        "set" => {
            l.retain(|e| entry_ctx(e) != ctx);
            l.push(make_entry("group", &group, true, None, &ctx));
            format!("§b{who}§a had their existing parent groups cleared, and now only inherits §b{group}§a{}.", fmt_ctx(&ctx))
        }
        _ => return Err("§cUsage: parent add|remove|set|addtemp|removetemp|info <group>".into()),
    })
}

/// `promote|demote <track>`: the user's place on a track (the first track group they are in, global context).
fn track_move(v: &mut Value, user: &User, args: &Args, up: bool) -> Result<String, String> {
    let w = words(args);
    let t = w.get(3).map(|s| s.to_lowercase()).ok_or("§cUsage: lp user <user> promote|demote <track>")?;
    let track: Vec<String> = v["tracks"].get(&t).and_then(|x| x.as_array()).ok_or_else(|| format!("§cA track named §4{t}§c could not be found."))?.iter().filter_map(|x| x.as_str().map(str::to_owned)).collect();
    if track.is_empty() {
        return Err(format!("§cThe track §4{t}§c is empty."));
    }
    let m = obj(v, "players", &user.key);
    m.insert("name".into(), Value::String(user.name.clone()));
    let l = list(m, "groups");
    let mine: Vec<String> = l.iter().filter(|e| entry_ctx(e).is_empty() && !is_temp(e)).filter_map(|e| e.as_str().or_else(|| e.get("group").and_then(|g| g.as_str())).map(str::to_owned)).collect();
    let at = track.iter().position(|g| mine.contains(g) || (mine.is_empty() && g == "default"));
    let who = &user.name;
    match (at, up) {
        (None, true) => {
            l.push(Value::String(track[0].clone()));
            Ok(format!("§b{who}§a isn't in any groups on §b{t}§a, so they were added to the first group, §b{}§a.", track[0]))
        }
        (None, false) => Err(format!("§b{who}§c isn't in any groups on §4{t}§c.")),
        (Some(i), true) if i + 1 >= track.len() => Err(format!("§b{who}§c is already at the end of §4{t}§c.")),
        (Some(i), true) => {
            l.retain(|e| e.as_str().or_else(|| e.get("group").and_then(|g| g.as_str())) != Some(&track[i]) || !entry_ctx(e).is_empty());
            l.push(Value::String(track[i + 1].clone()));
            Ok(format!("§aPromoting §b{who}§a along track §b{t}§a from §b{}§a to §b{}§a.", track[i], track[i + 1]))
        }
        (Some(0), false) => {
            l.retain(|e| e.as_str().or_else(|| e.get("group").and_then(|g| g.as_str())) != Some(&track[0]) || !entry_ctx(e).is_empty());
            Ok(format!("§aDemoting §b{who}§a along track §b{t}§a: removed from §b{}§a.", track[0]))
        }
        (Some(i), false) => {
            l.retain(|e| e.as_str().or_else(|| e.get("group").and_then(|g| g.as_str())) != Some(&track[i]) || !entry_ctx(e).is_empty());
            l.push(Value::String(track[i - 1].clone()));
            Ok(format!("§aDemoting §b{who}§a along track §b{t}§a from §b{}§a to §b{}§a.", track[i], track[i - 1]))
        }
    }
}

fn track_cmd(v: &mut Value, args: &Args) -> Result<String, String> {
    let w = words(args);
    let sub = w.first().map(|s| s.to_lowercase()).unwrap_or_default();
    if !v["tracks"].is_object() {
        v["tracks"] = Value::Object(Map::new());
    }
    match sub.as_str() {
        "createtrack" => {
            let t = w.get(1).map(|s| s.to_lowercase()).ok_or("§cUsage: lp createtrack <name>")?;
            if v["tracks"].get(&t).is_some() {
                return Err(format!("§4{t}§c already exists!"));
            }
            v["tracks"][&t] = json::json!([]);
            Ok(format!("§b{t}§a was successfully created."))
        }
        "deletetrack" => {
            let t = w.get(1).map(|s| s.to_lowercase()).ok_or("§cUsage: lp deletetrack <name>")?;
            v["tracks"].as_object_mut().unwrap().remove(&t).ok_or_else(|| format!("§cA track named §4{t}§c could not be found."))?;
            Ok(format!("§b{t}§a was successfully deleted."))
        }
        "listtracks" => {
            let t: Vec<String> = v["tracks"].as_object().unwrap().keys().cloned().collect();
            Ok(format!("§aShowing track entries: §f{}", if t.is_empty() { "(none)".into() } else { t.join(", ") }))
        }
        _ => {
            // track <name> info|append|insert|remove|clear
            let t = w.get(1).map(|s| s.to_lowercase()).ok_or("§cUsage: lp track <track> info|append|insert|remove|clear")?;
            let groups = v["groups"].clone();
            let tr = v["tracks"].get_mut(&t).and_then(|x| x.as_array_mut()).ok_or_else(|| format!("§cA track named §4{t}§c could not be found."))?;
            let op = w.get(2).map(|s| s.to_lowercase()).unwrap_or_else(|| "info".into());
            let g = w.get(3).map(|s| s.to_lowercase());
            let need_group = |g: &Option<String>| -> Result<String, String> {
                let g = g.clone().ok_or("§cUsage: lp track <track> append|insert|remove <group>")?;
                if groups.get(&g).is_none() {
                    return Err(format!("§cA group named §4{g}§c could not be found."));
                }
                Ok(g)
            };
            Ok(match op.as_str() {
                "info" => format!("§b> Showing Track: §f{t}
§3- Path: §f{}", tr.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(" ---> ")),
                "append" => {
                    let g = need_group(&g)?;
                    if tr.iter().any(|x| x.as_str() == Some(&g)) {
                        return Err(format!("§b{g}§c already exists on the track §4{t}§c."));
                    }
                    tr.push(Value::String(g.clone()));
                    format!("§aGroup §b{g}§a was appended to track §b{t}§a.")
                }
                "insert" => {
                    let g = need_group(&g)?;
                    let pos: usize = w.get(4).and_then(|p| p.parse().ok()).ok_or("§cUsage: lp track <track> insert <group> <position>")?;
                    tr.retain(|x| x.as_str() != Some(&g));
                    let i = pos.saturating_sub(1).min(tr.len());
                    tr.insert(i, Value::String(g.clone()));
                    format!("§aGroup §b{g}§a was inserted into track §b{t}§a at position §b{pos}§a.")
                }
                "remove" => {
                    let g = g.ok_or("§cUsage: lp track <track> remove <group>")?;
                    tr.retain(|x| x.as_str() != Some(&g));
                    format!("§aGroup §b{g}§a was removed from track §b{t}§a.")
                }
                "clear" => {
                    tr.clear();
                    format!("§b{t}§a's groups were cleared.")
                }
                o => return Err(format!("§cUnknown track subcommand '{o}'.")),
            })
        }
    }
}

fn meta_cmd(m: &mut Map<String, Value>, who: &str, args: &Args, at: usize) -> Result<String, String> {
    match args.get(at).map(|s| s.to_lowercase()).as_deref() {
        Some("setprefix") | Some("addprefix") => {
            // `setprefix <weight> <prefix>` or `setprefix <prefix>`
            let (w, text) = match args.get(at + 1).and_then(|s| s.parse::<i64>().ok()) {
                Some(w) => (Some(w), args.rest(at + 2)),
                None => (None, args.rest(at + 1)),
            };
            let text = text.trim_matches('"').replace('&', "§");
            if text.is_empty() {
                return Err("§cUsage: meta setprefix <weight> <prefix>".into());
            }
            m.insert("prefix".into(), Value::String(text.clone()));
            if let Some(w) = w {
                m.insert("weight".into(), json::json!(w));
            }
            Ok(format!("§b{who}§a now has prefix §r{text}§a."))
        }
        Some("setsuffix") | Some("addsuffix") => {
            let text = match args.get(at + 1).and_then(|s| s.parse::<i64>().ok()) {
                Some(_) => args.rest(at + 2),
                None => args.rest(at + 1),
            };
            let text = text.trim_matches('"').replace('&', "§");
            if text.is_empty() {
                return Err("§cUsage: meta setsuffix <weight> <suffix>".into());
            }
            m.insert("suffix".into(), Value::String(text.clone()));
            Ok(format!("§b{who}§a now has suffix §r{text}§a."))
        }
        Some("removeprefix") => {
            m.remove("prefix");
            Ok(format!("§aRemoved the prefix of §b{who}§a."))
        }
        Some("removesuffix") => {
            m.remove("suffix");
            Ok(format!("§aRemoved the suffix of §b{who}§a."))
        }
        Some("clear") => {
            m.remove("prefix");
            m.remove("suffix");
            m.remove("meta");
            Ok(format!("§b{who}§a's meta was cleared."))
        }
        Some("set") => {
            let key = args.get(at + 1).cloned().ok_or("§cUsage: meta set <key> <value>")?;
            let val = args.rest(at + 2).trim_matches('"').to_owned();
            if val.is_empty() {
                return Err("§cUsage: meta set <key> <value>".into());
            }
            if !m.get("meta").is_some_and(|x| x.is_object()) {
                m.insert("meta".into(), Value::Object(Map::new()));
            }
            m.get_mut("meta").unwrap().as_object_mut().unwrap().insert(key.clone(), Value::String(val.clone()));
            Ok(format!("§aSet meta key §b{key}§a to §b{val}§a for §b{who}§a."))
        }
        Some("unset") => {
            let key = args.get(at + 1).cloned().ok_or("§cUsage: meta unset <key>")?;
            let gone = m.get_mut("meta").and_then(|x| x.as_object_mut()).and_then(|x| x.remove(&key)).is_some();
            Ok(if gone { format!("§aUnset meta key §b{key}§a for §b{who}§a.") } else { format!("§b{who}§a doesn't have meta key §b{key}§a set.") })
        }
        Some("info") | None => {
            let meta = m.get("meta").and_then(|x| x.as_object()).map(|o| o.iter().map(|(k, v)| format!("{k}={}", v.as_str().map(str::to_owned).unwrap_or_else(|| v.to_string()))).collect::<Vec<_>>().join(", ")).unwrap_or_default();
            Ok(format!(
                "§b{who}§a prefix: §r{}\n§b{who}§a suffix: §r{}\n§b{who}§a meta: §f{}",
                m.get("prefix").and_then(|p| p.as_str()).unwrap_or("(none)"),
                m.get("suffix").and_then(|p| p.as_str()).unwrap_or("(none)"),
                if meta.is_empty() { "(none)".into() } else { meta }
            ))
        }
        Some(o) => Err(format!("§cUnknown meta subcommand '{o}'.")),
    }
}

fn lp(ctx: &mut Ctx, args: &Args) -> CmdResult {
    let mut v = load(ctx);
    prune(&mut v);
    let sub = args.get(0).map(|s| s.to_lowercase()).unwrap_or_default();
    let reply = match sub.as_str() {
        "user" => {
            let user = resolve_user(ctx, args.get(1).ok_or("§cUsage: lp user <user> ...")?)?;
            let action = args.get(2).map(|s| s.to_lowercase()).unwrap_or_else(|| "info".into());
            let who = user.name.clone();
            let r = match action.as_str() {
                "info" => {
                    let groups = hplug::player_groups(&v, user.uuid, &user.name);
                    let prefix = hplug::player_prefix(&v, user.uuid, &user.name).unwrap_or_default();
                    format!("§b> User Info: §f{}\n§3- UUID: §f{}\n§3- Parent Groups: §f{}\n§3- Prefix: §r{prefix}", user.name, user.key, groups.join(", "))
                }
                "permission" => {
                    let check_v = v.clone();
                    let (u, n) = (user.uuid, user.name.clone());
                    let env = hplug::perm_env_for(&*ctx.host, &check_v, u);
                    // LuckPerms' check reports the stored value (op is a separate, vanilla matter)
                    let m = obj(&mut v, "players", &user.key);
                    m.insert("name".into(), Value::String(user.name.clone()));
                    permission_cmd(m, &who, args, 3, |node| hplug::with_perm_env(env.clone(), || player_has(&check_v, u, &n, node)))?
                }
                "parent" => parent_cmd(&mut v, "players", &user.key, "groups", &who, args, 3)?,
                "meta" => {
                    let m = obj(&mut v, "players", &user.key);
                    meta_cmd(m, &who, args, 3)?
                }
                "promote" | "demote" => track_move(&mut v, &user, args, action == "promote")?,
                o => return Err(format!("§cUnknown user subcommand '{o}'.")),
            };
            save(ctx, &v)?;
            ctx.refresh_display_name(user.uuid, &user.name);
            r
        }
        "group" => {
            let group = args.get(1).map(|s| s.to_lowercase()).ok_or("§cUsage: lp group <group> ...")?;
            if v["groups"].get(&group).is_none() {
                return Err(format!("§cA group named §4{group}§c could not be found."));
            }
            let action = args.get(2).map(|s| s.to_lowercase()).unwrap_or_else(|| "info".into());
            let r = match action.as_str() {
                "info" => {
                    let g = &v["groups"][&group];
                    let inherit = g.get("inherit").and_then(|i| i.as_array()).map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(", ")).unwrap_or_default();
                    format!("§b> Group Info: §f{group}\n§3- Weight: §f{}\n§3- Parents: §f{inherit}\n§3- Prefix: §r{}", g.get("weight").and_then(|w| w.as_i64()).unwrap_or(0), g.get("prefix").and_then(|p| p.as_str()).unwrap_or("(none)"))
                }
                "permission" => {
                    let check_v = v.clone();
                    let gname = group.clone();
                    let m = obj(&mut v, "groups", &group);
                    permission_cmd(m, &gname, args, 3, |node| group_has(&check_v, &gname, node))?
                }
                "parent" => parent_cmd(&mut v, "groups", &group, "inherit", &group, args, 3)?,
                "meta" => {
                    let m = obj(&mut v, "groups", &group);
                    meta_cmd(m, &group, args, 3)?
                }
                "setweight" => {
                    let w: i64 = args.get(3).and_then(|s| s.parse().ok()).ok_or("§cUsage: lp group <group> setweight <n>")?;
                    obj(&mut v, "groups", &group).insert("weight".into(), json::json!(w));
                    format!("§aSet weight to §b{w}§a for group §b{group}§a.")
                }
                o => return Err(format!("§cUnknown group subcommand '{o}'.")),
            };
            save(ctx, &v)?;
            refresh_all(ctx);
            r
        }
        "creategroup" => {
            let g = args.get(1).map(|s| s.to_lowercase()).ok_or("§cUsage: lp creategroup <name>")?;
            if v["groups"].get(&g).is_some() {
                return Err(format!("§4{g}§c already exists!"));
            }
            v["groups"][&g] = json::json!({"nodes": [], "weight": 0});
            save(ctx, &v)?;
            format!("§b{g}§a was successfully created.")
        }
        "deletegroup" => {
            let g = args.get(1).map(|s| s.to_lowercase()).ok_or("§cUsage: lp deletegroup <name>")?;
            if g == "default" {
                return Err("§cThe default group cannot be deleted.".into());
            }
            v["groups"].as_object_mut().unwrap().remove(&g);
            for (_, p) in v["players"].as_object_mut().unwrap().iter_mut() {
                if let Some(gs) = p.get_mut("groups").and_then(|x| x.as_array_mut()) {
                    gs.retain(|x| x.as_str().or_else(|| x.get("group").and_then(|y| y.as_str())) != Some(&g));
                }
            }
            save(ctx, &v)?;
            refresh_all(ctx);
            format!("§b{g}§a was successfully deleted.")
        }
        "listgroups" => {
            let mut gs: Vec<(i64, String)> = v["groups"].as_object().unwrap().iter().map(|(k, g)| (g.get("weight").and_then(|w| w.as_i64()).unwrap_or(0), k.clone())).collect();
            gs.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
            format!("§aShowing group entries: §f{}", gs.iter().map(|(w, g)| format!("{g}({w})")).collect::<Vec<_>>().join(", "))
        }
        "sync" | "reload" | "networksync" => {
            refresh_all(ctx);
            "§aPermissions reloaded from config/hplug/permissions.json.".into()
        }
        "track" | "createtrack" | "deletetrack" | "listtracks" => {
            let r = track_cmd(&mut v, args)?;
            save(ctx, &v)?;
            r
        }
        "export" | "import" | "editor" | "bulkupdate" => {
            return Err("§cNot available on Horizon (see tools/migrate_luckperms.py for importing a LuckPerms export).".into())
        }
        _ => "§2Running §bLuckPerms-compatible perms§2 (hplug). Commands: user, group, track, creategroup, deletegroup, listgroups, createtrack, deletetrack, listtracks, sync".into(),
    };
    for line in reply.lines() {
        ctx.reply(line);
    }
    Ok(())
}

fn player_has(v: &Value, uuid: u128, name: &str, node: &str) -> bool {
    // same resolution as Ctx::has_perm without the op shortcut
    hplug_perm(v, uuid, name, node)
}
fn group_has(v: &Value, group: &str, node: &str) -> bool {
    let fake = json::json!({"groups": v["groups"].clone(), "players": {"x": {"groups": [group]}}});
    hplug_perm(&fake, 0, "x", node)
}
fn hplug_perm(v: &Value, uuid: u128, name: &str, node: &str) -> bool {
    hplug::permission_matches(v, uuid, name, node)
}

fn refresh_all(ctx: &mut Ctx) {
    for p in ctx.online() {
        ctx.refresh_display_name(p.uuid, &p.name);
    }
}

pub fn plugin() -> Plugin {
    Plugin::new("perms")
        .command("lp [a:word] [b:word] [c:word] [d:word] [e:word] [rest:text]", 3, lp)
        .alias("luckperms")
        .alias("perms")
        .on_join(|ctx| {
            let (u, n) = (ctx.player().uuid, ctx.player().name.clone());
            ctx.refresh_display_name(u, &n);
        })
        // world contexts: a prefix from a world-bound group follows the player
        .on_teleport(|ctx, t| {
            if t.from_dimension != t.to_dimension
                && let Some(p) = ctx.try_player().cloned()
            {
                ctx.refresh_display_name(p.uuid, &p.name);
            }
            false
        })
        // expired temporary nodes leave the file (and the Tab/chat prefix) within a minute
        .every(1200, |ctx| {
            let mut v = load(ctx);
            if prune(&mut v) && save(ctx, &v).is_ok() {
                refresh_all(ctx);
            }
        })
}
