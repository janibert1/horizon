//! EssentialsX commands that need engine helpers or their own data: enderchest, fly, hat, workbench, kits, mail,
//! jails and signs. Wording follows EssentialsX's English messages; permissions are `essentials.<cmd>`
//! (`.others` for another player), kits `essentials.kits.<name>`, signs `essentials.signs.create|use.<type>`.
//!
//! * `/enderchest [player]` (ec, echest): a 3-row view of the ender chest; a click moves one stack between it and
//!   your inventory through the engine's checked moves (`Host::move_player_stack`), so nothing is copied. Another
//!   player's (online or off: their file) needs `essentials.enderchest.others`; `.modify` to change it.
//! * `/fly [player] [on|off]`: `Host::set_may_fly`; kept over relogs (Essentials' `flymode`), and a game mode change
//!   resets it as on Paper.
//! * `/hat [remove]`: the held item goes on the head (the old helmet into the hand).
//! * `/workbench` (wb, craft): `Host::open_workbench`, a real 3x3 crafting menu without a table.
//! * `/kit [name] [player]`, `/kits`, `/createkit <name> <delay>`, `/delkit <name>`: config `kits`
//!   {name: {delay (s, -1 = once), items: [Essentials item lines | "/command {USERNAME}"]}} (migrated from kits.yml).
//! * `/mail read|send <player> <msg>|sendall <msg>|clear [n]`: stored by UUID, offline players by their last name;
//!   "You have N messages" on join.
//! * `/setjail <name>`, `/deljail <name>`, `/jails`, `/jail <player> <jail> [time]` (togglejail), `/unjail <player>`:
//!   jailed players are held within `jail_radius` (10) blocks, may not break or use blocks, may only run
//!   `jail_allowed_commands`; time like `10m`, `2h`, `1d` (counted online, as Essentials' `jail-online-time`
//!   default false = wall clock: here wall clock). Freed to where they were (`teleport-when-freed: back`).
//! * Signs (`enabled_signs`, default none, as on the live server): a sign whose first line is `[Warp]`, `[Spawn]`,
//!   `[Kit]`, `[Heal]`, `[Disposal]`, `[Workbench]`, `[Enderchest]` (the type, case-insensitive) becomes `§1[Warp]`
//!   etc. when the writer has `essentials.signs.create.<type>`; a right click with `essentials.signs.use.<type>` runs
//!   it (line 2 = warp/kit name). Kept in `signs` by position.
use super::{Loc, guard, key, me, target};
use hplug::{Args, CmdResult, Ctx, EquipmentSlot, ItemStack, Plugin, json};
use json::{Map, Value};
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Default)]
struct Views {
    ender: HashMap<u128, (i32, u128, bool)>, // viewer -> (window, owner, may modify)
    disposal: HashMap<u128, i32>,
}
static VIEWS: Mutex<Option<Views>> = Mutex::new(None);
fn views<R>(f: impl FnOnce(&mut Views) -> R) -> R {
    let mut g = VIEWS.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(Views::default))
}

fn obj(ctx: &Ctx, k: &str) -> Map<String, Value> {
    ctx.get(k)
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}
fn perm(ctx: &Ctx, node: &str) -> bool {
    match ctx.try_player() {
        Some(p) => ctx.has_perm(p.uuid, &p.name, node),
        None => true,
    }
}
/// UUID of an online player, or of one seen before (`names`).
fn uuid_of(ctx: &Ctx, name: &str) -> Option<(u128, String)> {
    if let Some(p) = ctx.find_online(name) {
        return Some((p.uuid, p.name));
    }
    let n = obj(ctx, "names");
    let v = n.get(&name.to_lowercase())?;
    let u = hplug::parse_uuid(v.get("uuid").and_then(|x| x.as_str()).or(v.as_str())?)?;
    let shown = v
        .get("name")
        .and_then(|x| x.as_str())
        .unwrap_or(name)
        .to_owned();
    Some((u, shown))
}

// ---------- enderchest ----------
fn open_ender(ctx: &mut Ctx, viewer: u128, owner: u128, modify: bool, title: String) {
    ctx.run_later(1, move |c| {
        let items = match c.host.player_inventory(owner, true) {
            Ok((v, _)) => v,
            Err(e) => {
                c.tell(viewer, format!("§4Error: §c{e}"));
                return;
            }
        };
        if let Some(w) = c
            .host
            .open_menu(viewer, 3, hplug::Message::literal(title.clone()), items)
        {
            views(|v| v.ender.insert(viewer, (w, owner, modify)));
        }
    });
}
fn refresh_ender(ctx: &mut Ctx, owner: u128) {
    let Ok((items, _)) = ctx.host.player_inventory(owner, true) else {
        return;
    };
    let viewers: Vec<u128> = views(|v| {
        v.ender
            .iter()
            .filter(|(_, (_, o, _))| *o == owner)
            .map(|(k, _)| *k)
            .collect()
    });
    for v in viewers {
        ctx.host.set_menu_items(v, items.clone());
    }
}
fn cmd_enderchest(ctx: &mut Ctx, args: &Args) -> CmdResult {
    guard(ctx, "enderchest")?;
    let p = me(ctx)?;
    let (owner, name) = match args.get(0) {
        Some(n) if !n.eq_ignore_ascii_case(&p.name) => {
            if !perm(ctx, "essentials.enderchest.others") {
                return Err("§4You do not have access to that command.".into());
            }
            uuid_of(ctx, n).ok_or_else(|| format!("§4Error: §cPlayer {n} not found."))?
        }
        _ => (p.uuid, p.name.clone()),
    };
    let modify = owner == p.uuid || perm(ctx, "essentials.enderchest.modify");
    let title = if owner == p.uuid {
        "Ender Chest".to_owned()
    } else {
        format!("{name}'s Ender Chest")
    };
    open_ender(ctx, p.uuid, owner, modify, title);
    Ok(())
}
fn ender_click(ctx: &mut Ctx, window: i32, slot: i32) -> bool {
    let Some(viewer) = ctx.player_uuid() else {
        return false;
    };
    let Some((_, owner, modify)) =
        views(|v| v.ender.get(&viewer).copied()).filter(|(w, ..)| *w == window)
    else {
        return false;
    };
    if !modify || slot < 0 {
        return true;
    }
    let slot = slot as usize;
    let res = if slot < 27 {
        let Ok((items, _)) = ctx.host.player_inventory(owner, true) else {
            return true;
        };
        let Some(Some(stack)) = items.get(slot).cloned() else {
            return true;
        };
        ctx.host
            .move_player_stack(owner, true, slot, &stack, viewer, false)
    } else {
        let k = slot - 27;
        let inv = if k < 27 {
            k + 9
        } else if k < 36 {
            k - 27
        } else {
            return true;
        };
        let Ok((items, _)) = ctx.host.player_inventory(viewer, false) else {
            return true;
        };
        let Some(Some(stack)) = items.get(inv).cloned() else {
            return true;
        };
        ctx.host
            .move_player_stack(viewer, false, inv, &stack, owner, true)
    };
    if let Err(e) = res {
        ctx.tell(viewer, format!("§c{e}"));
    }
    refresh_ender(ctx, owner);
    true
}

// ---------- fly / hat / workbench ----------
fn cmd_fly(ctx: &mut Ctx, args: &Args) -> CmdResult {
    guard(ctx, "fly")?;
    let (who, word) = match (args.get(0), args.get(1)) {
        (Some(a), None)
            if ["on", "off", "enable", "disable"].contains(&a.to_lowercase().as_str()) =>
        {
            (None, Some(a.clone()))
        }
        (a, b) => (a.cloned(), b.cloned()),
    };
    let t = match &who {
        Some(n) => {
            let t = target(ctx, n)?;
            if ctx.try_player().is_some_and(|p| p.uuid != t.uuid)
                && !perm(ctx, "essentials.fly.others")
            {
                return Err("§4You do not have access to that command.".into());
            }
            t
        }
        None => me(ctx)?,
    };
    let mut fly = obj(ctx, "fly");
    let now = fly.contains_key(&key(t.uuid));
    let on = match word.as_deref().map(str::to_lowercase).as_deref() {
        Some("on" | "enable") => true,
        Some("off" | "disable") => false,
        _ => !now,
    };
    if on {
        fly.insert(key(t.uuid), json::json!(true));
    } else {
        fly.remove(&key(t.uuid));
    }
    ctx.set("fly", Value::Object(fly))?;
    let u = t.uuid;
    ctx.run_later(1, move |c| {
        c.set_may_fly(u, on);
    });
    let state = if on { "§cenabled" } else { "§cdisabled" };
    ctx.reply(format!("§6Set fly mode {state}§6 for {}§6.", t.name));
    if ctx.try_player().is_some_and(|p| p.uuid != t.uuid) {
        ctx.tell(
            t.uuid,
            format!("§6Set fly mode {state}§6 for {}§6.", t.name),
        );
    }
    Ok(())
}
fn cmd_hat(ctx: &mut Ctx, args: &Args) -> CmdResult {
    guard(ctx, "hat")?;
    let p = me(ctx)?;
    let u = p.uuid;
    if args
        .get(0)
        .is_some_and(|a| a.eq_ignore_ascii_case("remove"))
    {
        let head = ctx
            .equipment_of(u, EquipmentSlot::Head)
            .ok_or("§4Error: §cYou are not wearing a hat.")?;
        ctx.run_later(1, move |c| {
            if c.host.put_player_stack(u, false, &head).is_ok() {
                c.set_equipment_of(u, EquipmentSlot::Head, None);
                c.tell(u, "§6Your hat has been removed.");
            } else {
                c.tell(u, "§4Error: §cYou do not have room in your inventory.");
            }
        });
        return Ok(());
    }
    let hand = ctx
        .equipment_of(u, EquipmentSlot::MainHand)
        .ok_or("§4Error: §cYou must be holding an item to use as a hat.")?;
    if ctx
        .get("hat_blacklist")
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .any(|x| x.as_str() == Some(&*hand.item))
    {
        return Err("§4Error: §cYou cannot use that item as a hat!".into());
    }
    let old = ctx.equipment_of(u, EquipmentSlot::Head);
    ctx.run_later(1, move |c| {
        c.set_equipment_of(u, EquipmentSlot::Head, Some(hand.clone()));
        c.set_equipment_of(u, EquipmentSlot::MainHand, old.clone());
        c.tell(u, "§6Enjoy your new hat!");
    });
    Ok(())
}
fn open_workbench(ctx: &mut Ctx, u: u128) {
    ctx.run_later(1, move |c| {
        if !c.host.open_workbench(u) {
            c.tell(u, "§4Error: §cCould not open the workbench.");
        }
    });
}

// ---------- kits ----------
/// An Essentials item name (`stonesword`, `dpickaxe`, `lhelm`, `minecraft:oak_log`) as a vanilla item id.
pub(crate) fn item_id(ctx: &Ctx, name: &str) -> Option<String> {
    let n = name.to_lowercase();
    let n = n.trim_start_matches("minecraft:");
    let ok = |id: &str| {
        ctx.host
            .max_stack_size(&format!("minecraft:{id}"))
            .is_some()
    };
    if ok(n) {
        return Some(format!("minecraft:{n}"));
    }
    let aliases = [
        ("helm", "helmet"),
        ("chest", "chestplate"),
        ("plate", "chestplate"),
        ("legs", "leggings"),
        ("pants", "leggings"),
        ("boots", "boots"),
        ("pick", "pickaxe"),
        ("spade", "shovel"),
        ("rocket", "firework_rocket"),
    ];
    let materials = [
        ("d", "diamond"),
        ("i", "iron"),
        ("g", "golden"),
        ("gold", "golden"),
        ("w", "wooden"),
        ("wood", "wooden"),
        ("s", "stone"),
        ("l", "leather"),
        ("n", "netherite"),
        ("c", "chainmail"),
    ];
    let mut cands: Vec<String> = vec![n.to_owned()];
    for (m, full) in materials {
        if let Some(rest) = n.strip_prefix(m) {
            cands.push(format!("{full}_{rest}"));
            for (a, f) in aliases {
                if rest == a {
                    cands.push(format!("{full}_{f}"));
                }
            }
        }
    }
    for (a, f) in aliases {
        if let Some(head) = n.strip_suffix(a) {
            cands.push(format!("{head}_{f}"));
        }
    }
    for c in &cands {
        if ok(c) {
            return Some(format!("minecraft:{c}"));
        }
        // one or two underscores put back: stonesword -> stone_sword, goldenapple -> golden_apple
        let chars: Vec<char> = c.chars().collect();
        for i in 1..chars.len() {
            let a: String = chars[..i]
                .iter()
                .chain(['_'].iter())
                .chain(chars[i..].iter())
                .collect();
            if ok(&a) {
                return Some(format!("minecraft:{a}"));
            }
            for j in i + 1..chars.len() {
                let b: String = chars[..i]
                    .iter()
                    .chain(['_'].iter())
                    .chain(chars[i..j].iter())
                    .chain(['_'].iter())
                    .chain(chars[j..].iter())
                    .collect();
                if ok(&b) {
                    return Some(format!("minecraft:{b}"));
                }
            }
        }
    }
    None
}
fn enchant_id(e: &str) -> &str {
    match e {
        "durability" | "unbreak" => "unbreaking",
        "digspeed" | "eff" => "efficiency",
        "lootbonusblocks" | "fortune" => "fortune",
        "lootbonusmobs" | "looting" => "looting",
        "damageall" | "sharp" => "sharpness",
        "protection_environmental" | "prot" => "protection",
        "arrowdamage" => "power",
        "arrowinfinite" | "inf" => "infinity",
        "silktouch" | "silk" => "silk_touch",
        other => other,
    }
}
fn snbt_str(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}
/// One Essentials kit line (`dpickaxe 1 efficiency:1 name:&4Gigadrill lore:a|b`) as a `/give` item argument and
/// count. Unknown meta (colours, books, fireworks: cosmetic) is left out.
pub(crate) fn kit_give_arg(ctx: &Ctx, line: &str) -> Option<(String, u32)> {
    let mut w = line.split_whitespace();
    let first = w.next()?;
    let (name, damage) = match first.split_once(':') {
        Some((n, d)) if d.chars().all(|c| c.is_ascii_digit()) => (n, d.parse::<u32>().ok()),
        _ => (first, None),
    };
    let id = item_id(ctx, name)?;
    let count: u32 = w.next().and_then(|c| c.parse().ok()).unwrap_or(1).max(1);
    let mut comps = Vec::new();
    let mut ench = Vec::new();
    for m in w {
        let Some((k, v)) = m.split_once(':') else {
            continue;
        };
        let strip = |s: &str| super::strip_codes(&s.replace('_', " "));
        match k.to_lowercase().as_str() {
            "name" => comps.push(format!("minecraft:custom_name={}", snbt_str(&strip(v)))),
            "lore" => comps.push(format!(
                "minecraft:lore=[{}]",
                v.split('|')
                    .map(|l| snbt_str(&strip(l)))
                    .collect::<Vec<_>>()
                    .join(",")
            )),
            "player" if id == "minecraft:player_head" => {
                comps.push(format!("minecraft:profile={}", snbt_str(v)))
            }
            "color" | "fade" | "type" | "shape" | "effect" | "power" | "title" | "author"
            | "book" => {}
            e => {
                if let Ok(lvl) = v.parse::<u32>() {
                    ench.push(format!("\"minecraft:{}\":{lvl}", enchant_id(e)));
                }
            }
        }
    }
    if let Some(d) = damage.filter(|d| *d > 0) {
        comps.push(format!("minecraft:damage={d}"));
    }
    if !ench.is_empty() {
        comps.push(format!("minecraft:enchantments={{{}}}", ench.join(",")));
    }
    let arg = if comps.is_empty() {
        id
    } else {
        format!("{id}[{}]", comps.join(","))
    };
    Some((arg, count))
}
fn kits(ctx: &Ctx) -> Map<String, Value> {
    obj(ctx, "kits")
}
fn kit_allowed(ctx: &Ctx, kit: &str) -> bool {
    super::allowed(ctx, &format!("kits.{kit}")) || perm(ctx, "essentials.kits.*")
}
fn fmt_secs(s: u64) -> String {
    let (d, h, m, sec) = (s / 86400, s / 3600 % 24, s / 60 % 60, s % 60);
    let mut v = Vec::new();
    for (n, u) in [(d, "days"), (h, "hours"), (m, "minutes"), (sec, "seconds")] {
        if n > 0 {
            v.push(format!("{n} {u}"));
        }
    }
    if v.is_empty() {
        "now".into()
    } else {
        v.join(" ")
    }
}
fn cmd_kit(ctx: &mut Ctx, args: &Args) -> CmdResult {
    guard(ctx, "kit")?;
    let all = kits(ctx);
    let Some(name) = args.get(0).map(|s| s.to_lowercase()) else {
        let list: Vec<String> = all
            .keys()
            .filter(|k| kit_allowed(ctx, k))
            .cloned()
            .collect();
        ctx.reply(format!(
            "§6Kits:§r {}",
            if list.is_empty() {
                "§cnone".into()
            } else {
                list.join(", ")
            }
        ));
        return Ok(());
    };
    let kit = all
        .get(&name)
        .cloned()
        .ok_or("§4Error: §cThat kit does not exist.")?;
    if !kit_allowed(ctx, &name) {
        return Err(format!(
            "§4You need the §cessentials.kits.{name}§4 permission to use that kit."
        ));
    }
    let t = match args.get(1) {
        Some(n) => {
            if !perm(ctx, "essentials.kit.others") {
                return Err("§4You do not have access to that command.".into());
            }
            target(ctx, n)?
        }
        None => me(ctx)?,
    };
    let delay = kit["delay"].as_i64().unwrap_or(0);
    let mut used = obj(ctx, "kit_used");
    let mine = used
        .get(&key(t.uuid))
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    let last = mine.get(&name).and_then(|v| v.as_u64());
    let now = hplug::now_ms();
    let exempt = perm(ctx, "essentials.kit.exemptdelay");
    if let (Some(last), false) = (last, exempt) {
        if delay < 0 {
            return Err("§4You can't use that kit again.".into());
        }
        let ready = last + delay as u64 * 1000;
        if delay > 0 && ready > now {
            return Err(format!(
                "§4You can't use that kit again for another§c {}§4.",
                fmt_secs((ready - now).div_ceil(1000))
            ));
        }
    }
    let mut missing = Vec::new();
    let mut stacks: Vec<ItemStack> = Vec::new();
    for line in kit["items"].as_array().cloned().unwrap_or_default() {
        let Some(l) = line.as_str() else { continue };
        if let Some(snbt) = l.strip_prefix("@snbt ") {
            match hplug::item_from_snbt(snbt) {
                Some(s) => stacks.push(s),
                None => missing.push("@snbt".into()),
            }
            continue;
        }
        if let Some(cmd) = l.strip_prefix('/') {
            ctx.console(
                cmd.replace("{USERNAME}", &t.name)
                    .replace("{PLAYER}", &t.name),
            );
            continue;
        }
        match kit_give_arg(ctx, l) {
            Some((arg, n)) => ctx.console(format!("give {} {arg} {n}", t.name)),
            None => missing.push(l.split_whitespace().next().unwrap_or(l).to_owned()),
        }
    }
    if !missing.is_empty() {
        ctx.log(format!("kit {name}: unknown items {missing:?}"));
    }
    if !stacks.is_empty() {
        let u = t.uuid;
        ctx.run_later(1, move |c| {
            for s in &stacks {
                if let Some(left) = c.host.give_stack(u, s.clone()) {
                    c.tell(
                        u,
                        format!(
                            "§4Your inventory was full: {}x {} could not be given.",
                            left.count, left.item
                        ),
                    );
                }
            }
        });
    }
    let mut mine = mine;
    mine.insert(name.clone(), json::json!(now));
    used.insert(key(t.uuid), Value::Object(mine));
    ctx.set("kit_used", Value::Object(used))?;
    ctx.tell(t.uuid, format!("§6Received kit§c {name}§6."));
    if ctx.try_player().is_some_and(|p| p.uuid != t.uuid) {
        ctx.reply(format!("§6Gave kit§c {name}§6 to §c{}§6.", t.name));
    }
    Ok(())
}
fn cmd_createkit(ctx: &mut Ctx, args: &Args) -> CmdResult {
    guard(ctx, "createkit")?;
    let p = me(ctx)?;
    let name = args
        .get(0)
        .ok_or("§4Usage: /createkit <kitname> <delay>")?
        .to_lowercase();
    let delay = args.get(1).and_then(|d| d.parse::<i64>().ok()).unwrap_or(0);
    let u = p.uuid;
    // the inventory is read a tick later: inside the player's own command their state is swapped out
    ctx.run_later(1, move |c| {
        let inv = match c.host.player_inventory(u, false) {
            Ok((inv, _)) => inv,
            Err(e) => {
                c.tell(u, format!("§4Error: §c{e}"));
                return;
            }
        };
        // the items as SNBT (lossless; Essentials' own `use-nbt-serialization-in-createkit`)
        let items: Vec<Value> = inv
            .iter()
            .take(36)
            .flatten()
            .map(|s| json::json!(format!("@snbt {}", hplug::item_to_snbt(s))))
            .collect();
        if items.is_empty() {
            c.tell(u, "§4Error: §cYour inventory is empty.");
            return;
        }
        let mut all = kits(c);
        let n = items.len();
        all.insert(name.clone(), json::json!({"delay": delay, "items": items}));
        if c.set("kits", Value::Object(all)).is_ok() {
            c.tell(
                u,
                format!("§6Created kit §c{name}§6 with §c{n}§6 entries and delay §c{delay}§6."),
            );
        }
    });
    Ok(())
}

// ---------- mail ----------
fn mail_of(ctx: &Ctx, u: u128) -> Vec<Value> {
    ctx.get("mail")
        .and_then(|m| m.get(key(u)).and_then(|v| v.as_array().cloned()))
        .unwrap_or_default()
}
fn set_mail(ctx: &mut Ctx, u: u128, v: Vec<Value>) -> Result<(), String> {
    let mut all = obj(ctx, "mail");
    if v.is_empty() {
        all.remove(&key(u));
    } else {
        all.insert(key(u), json::json!(v));
    }
    ctx.set("mail", Value::Object(all))
}
fn deliver(ctx: &mut Ctx, to: u128, from: &str, msg: &str) -> Result<(), String> {
    let mut v = mail_of(ctx, to);
    v.push(json::json!({"from": from, "message": msg, "time": hplug::now_ms(), "read": false}));
    set_mail(ctx, to, v)?;
    ctx.tell(
        to,
        "§6You have new mail! Type §c/mail read§6 to view your mail.",
    );
    Ok(())
}
fn cmd_mail(ctx: &mut Ctx, args: &Args) -> CmdResult {
    guard(ctx, "mail")?;
    let sub = args.get(0).map(|s| s.to_lowercase()).unwrap_or_default();
    let from = ctx
        .try_player()
        .map(|p| p.name.clone())
        .unwrap_or_else(|| "Console".into());
    match sub.as_str() {
        "send" => {
            if !super::allowed(ctx, "mail.send") {
                return Err("§4You do not have access to that command.".into());
            }
            let to = args
                .get(1)
                .ok_or("§4Usage: /mail send <player> <message>")?;
            let msg = args.rest(2);
            if msg.trim().is_empty() {
                return Err("§4Usage: /mail send <player> <message>".into());
            }
            let (u, shown) =
                uuid_of(ctx, to).ok_or_else(|| format!("§4Error: §cPlayer {to} not found."))?;
            deliver(ctx, u, &from, &msg)?;
            ctx.reply(format!("§6Mail sent to §c{shown}§6!"));
        }
        "sendall" => {
            if !perm(ctx, "essentials.mail.sendall") {
                return Err("§4You do not have access to that command.".into());
            }
            let msg = args.rest(1);
            let names = obj(ctx, "names");
            let mut n = 0;
            for v in names.values() {
                if let Some(u) = v
                    .get("uuid")
                    .and_then(|x| x.as_str())
                    .and_then(hplug::parse_uuid)
                {
                    deliver(ctx, u, &from, &msg)?;
                    n += 1;
                }
            }
            ctx.reply(format!("§6Mail sent to §c{n}§6 players!"));
        }
        "clear" => {
            let p = me(ctx)?;
            let mut v = mail_of(ctx, p.uuid);
            match args.get(1).and_then(|s| s.parse::<usize>().ok()) {
                Some(i) if i >= 1 && i <= v.len() => {
                    v.remove(i - 1);
                }
                Some(_) => return Err("§4Error: §cThat mail does not exist.".into()),
                None => v.clear(),
            }
            set_mail(ctx, p.uuid, v)?;
            ctx.reply("§6Mail cleared!");
        }
        "" | "read" => {
            let p = me(ctx)?;
            let mut v = mail_of(ctx, p.uuid);
            if v.is_empty() {
                ctx.reply("§6You do not have any mail.");
                return Ok(());
            }
            ctx.reply(format!("§6Mail ({}):", v.len()));
            for (i, m) in v.iter_mut().enumerate() {
                ctx.reply(format!(
                    "§6{}. §7[{}]§r {}",
                    i + 1,
                    m["from"].as_str().unwrap_or("?"),
                    m["message"].as_str().unwrap_or("")
                ));
                m["read"] = json::json!(true);
            }
            set_mail(ctx, p.uuid, v)?;
            ctx.reply("§6To mark your mail as read, type§c /mail clear§6.");
        }
        _ => {
            return Err(
                "§4Usage: /mail [read|clear|clear [number]|send [to] [message]|sendall [message]]"
                    .into(),
            );
        }
    }
    Ok(())
}

// ---------- jails ----------
fn parse_duration(s: &str) -> Option<u64> {
    let mut total = 0u64;
    let mut num = String::new();
    for c in s.chars() {
        if c.is_ascii_digit() {
            num.push(c);
            continue;
        }
        let n: u64 = num.parse().ok()?;
        num.clear();
        total += n * match c {
            's' => 1,
            'm' => 60,
            'h' => 3600,
            'd' => 86400,
            'w' => 604800,
            _ => return None,
        };
    }
    if !num.is_empty() {
        total += num.parse::<u64>().ok()? * 60; // a bare number: minutes
    }
    (total > 0).then_some(total * 1000)
}
fn jails(ctx: &Ctx) -> Map<String, Value> {
    obj(ctx, "jails")
}
fn jailed(ctx: &Ctx) -> Map<String, Value> {
    obj(ctx, "jailed")
}
pub(crate) fn is_jailed(ctx: &Ctx, u: u128) -> bool {
    jailed(ctx).contains_key(&key(u))
}
fn cmd_setjail(ctx: &mut Ctx, args: &Args) -> CmdResult {
    guard(ctx, "setjail")?;
    let p = me(ctx)?;
    let name = args
        .get(0)
        .ok_or("§4Usage: /setjail <jailname>")?
        .to_lowercase();
    let mut j = jails(ctx);
    j.insert(name.clone(), Loc::of(&p).to_json());
    ctx.set("jails", Value::Object(j))?;
    ctx.reply(format!("§6Jail§c {name} §6has been set."));
    Ok(())
}
fn cmd_deljail(ctx: &mut Ctx, args: &Args) -> CmdResult {
    guard(ctx, "deljail")?;
    let name = args
        .get(0)
        .ok_or("§4Usage: /deljail <jailname>")?
        .to_lowercase();
    let mut j = jails(ctx);
    j.remove(&name)
        .ok_or("§4Error: §cThat jail does not exist.")?;
    ctx.set("jails", Value::Object(j))?;
    ctx.reply(format!("§6Jail§c {name} §6has been removed."));
    Ok(())
}
fn cmd_jail(ctx: &mut Ctx, args: &Args) -> CmdResult {
    guard(ctx, "jail")?;
    let who = args
        .get(0)
        .ok_or("§4Usage: /jail <player> <jailname> [datediff]")?;
    let (u, name) =
        uuid_of(ctx, who).ok_or_else(|| format!("§4Error: §cPlayer {who} not found."))?;
    let mut jd = jailed(ctx);
    if args.get(1).is_none() && jd.contains_key(&key(u)) {
        return unjail(ctx, u, &name);
    }
    let jail = args
        .get(1)
        .ok_or("§4Usage: /jail <player> <jailname> [datediff]")?
        .to_lowercase();
    let at = jails(ctx)
        .get(&jail)
        .and_then(Loc::from_json)
        .ok_or("§4Error: §cThat jail does not exist.")?;
    if let Some(on) = ctx.find_online(&name)
        && ctx.has_perm(on.uuid, &on.name, "essentials.jail.exempt")
    {
        return Err("§4Error: §cYou may not jail that person.".into());
    }
    let until = match args.get(2) {
        Some(d) => {
            Some(hplug::now_ms() + parse_duration(d).ok_or("§4Error: §cIllegal date format.")?)
        }
        None => None,
    };
    let from = ctx.find_online(&name).map(|p| Loc::of(&p).to_json());
    jd.insert(
        key(u),
        json::json!({"jail": jail, "until": until, "from": from, "name": name}),
    );
    ctx.set("jailed", Value::Object(jd))?;
    if ctx.find_online(&name).is_some() {
        ctx.teleport_to(&name, &at.dim, at.pos, Some(at.look));
        ctx.tell(u, "§4You do the crime, you do the time.");
    }
    let time = until
        .map(|t| format!(" §6for§c {}", fmt_secs((t - hplug::now_ms()) / 1000)))
        .unwrap_or_default();
    ctx.reply(format!("§6Player§c {name} §6jailed{time}§6."));
    Ok(())
}
fn unjail(ctx: &mut Ctx, u: u128, name: &str) -> CmdResult {
    let mut jd = jailed(ctx);
    let rec = jd
        .remove(&key(u))
        .ok_or_else(|| format!("§4Error: §cPlayer {name} is not jailed."))?;
    ctx.set("jailed", Value::Object(jd))?;
    if ctx.find_online(name).is_some() {
        let back = rec
            .get("from")
            .and_then(Loc::from_json)
            .or_else(|| ctx.get("spawn").and_then(|v| Loc::from_json(&v)));
        if let Some(b) = back {
            ctx.teleport_to(name, &b.dim, b.pos, Some(b.look));
        }
        ctx.tell(u, "§6You have been released.");
    }
    ctx.reply(format!("§6Player§c {name} §6unjailed."));
    Ok(())
}
fn jail_tick(ctx: &mut Ctx) {
    let jd = jailed(ctx);
    if jd.is_empty() {
        return;
    }
    let now = hplug::now_ms();
    let radius = ctx
        .get("jail_radius")
        .and_then(|v| v.as_f64())
        .unwrap_or(10.0);
    let all_jails = jails(ctx);
    for p in ctx.online() {
        let Some(rec) = jd.get(&key(p.uuid)) else {
            continue;
        };
        if rec["until"].as_u64().is_some_and(|t| t <= now) {
            let _ = unjail(ctx, p.uuid, &p.name);
            continue;
        }
        let Some(at) = rec["jail"]
            .as_str()
            .and_then(|j| all_jails.get(j))
            .and_then(Loc::from_json)
        else {
            continue;
        };
        let d2 = (0..3)
            .map(|i| (p.position[i] - at.pos[i]).powi(2))
            .sum::<f64>();
        if p.dimension != at.dim || d2 > radius * radius {
            ctx.teleport_to(&p.name, &at.dim, at.pos, Some(at.look));
        }
    }
}

// ---------- signs ----------
const SIGN_TYPES: &[&str] = &[
    "warp",
    "spawn",
    "kit",
    "heal",
    "disposal",
    "workbench",
    "enderchest",
];
fn sign_key(dim: &str, pos: [i32; 3]) -> String {
    format!("{dim} {} {} {}", pos[0], pos[1], pos[2])
}
#[allow(clippy::ptr_arg)] // the SDK's sign hook hands a Vec (lines may be rewritten)
fn on_sign(ctx: &mut Ctx, dim: &str, pos: [i32; 3], lines: &mut Vec<String>) -> bool {
    let mut signs = obj(ctx, "signs");
    let k = sign_key(dim, pos);
    let first = lines
        .first()
        .map(|l| l.trim().to_lowercase())
        .unwrap_or_default();
    let Some(kind) = first
        .strip_prefix('[')
        .and_then(|r| r.strip_suffix(']'))
        .map(str::to_owned)
    else {
        if signs.remove(&k).is_some() {
            let _ = ctx.set("signs", Value::Object(signs));
        }
        return false;
    };
    let enabled: Vec<String> = ctx
        .get("enabled_signs")
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|x| x.as_str().map(str::to_lowercase))
        .collect();
    if !SIGN_TYPES.contains(&kind.as_str()) || !enabled.contains(&kind) {
        return false;
    }
    if !perm(ctx, &format!("essentials.signs.create.{kind}")) {
        lines[0] = format!("§4[{}]", cap(&kind));
        return false;
    }
    lines[0] = format!("§1[{}]", cap(&kind));
    let arg = lines
        .get(1)
        .map(|l| l.trim().to_lowercase())
        .unwrap_or_default();
    signs.insert(k, json::json!({"type": kind, "arg": arg}));
    let _ = ctx.set("signs", Value::Object(signs));
    false
}
fn cap(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
        .unwrap_or_default()
}
fn on_sign_use(ctx: &mut Ctx, pos: [i32; 3]) -> bool {
    let Some(p) = ctx.try_player().cloned() else {
        return false;
    };
    let Some(s) = obj(ctx, "signs").get(&sign_key(&p.dimension, pos)).cloned() else {
        return false;
    };
    let kind = s["type"].as_str().unwrap_or_default().to_owned();
    let arg = s["arg"].as_str().unwrap_or_default().to_owned();
    if !ctx.has_perm(p.uuid, &p.name, &format!("essentials.signs.use.{kind}")) {
        ctx.tell(p.uuid, "§4You do not have access to that sign.");
        return true;
    }
    let line = match kind.as_str() {
        "warp" => format!("warp {arg}"),
        "spawn" => "spawn".into(),
        "kit" => format!("kit {arg}"),
        "heal" => {
            ctx.console(format!(
                "effect give {} minecraft:instant_health 1 10 true",
                p.name
            ));
            ctx.tell(p.uuid, "§6You have been healed.");
            return true;
        }
        "workbench" => {
            open_workbench(ctx, p.uuid);
            return true;
        }
        "enderchest" => {
            open_ender(ctx, p.uuid, p.uuid, true, "Ender Chest".into());
            return true;
        }
        "disposal" => {
            let u = p.uuid;
            ctx.run_later(1, move |c| {
                if let Some(w) = c
                    .host
                    .open_menu(u, 4, hplug::Message::literal("Disposal"), vec![])
                {
                    views(|v| v.disposal.insert(u, w));
                }
            });
            return true;
        }
        _ => return false,
    };
    // as the player: their own permissions and cooldowns apply
    ctx.console(format!("execute as {} run {line}", p.name));
    true
}
/// Disposal: a click on an inventory slot destroys that stack.
fn disposal_click(ctx: &mut Ctx, window: i32, slot: i32) -> bool {
    let Some(u) = ctx.player_uuid() else {
        return false;
    };
    if views(|v| v.disposal.get(&u).copied()) != Some(window) {
        return false;
    }
    if slot >= 36 {
        let k = (slot - 36) as usize;
        let inv = if k < 27 {
            k + 9
        } else if k < 36 {
            k - 27
        } else {
            return true;
        };
        if let Ok((items, _)) = ctx.host.player_inventory(u, false)
            && let Some(Some(stack)) = items.get(inv)
        {
            let _ = ctx.host.take_player_stack(u, false, inv, stack);
        }
    }
    true
}

pub(crate) fn register(p: Plugin) -> Plugin {
    p.command("enderchest [player:word]", 0, cmd_enderchest)
        .alias("ec")
        .alias("echest")
        .command("fly [a:word] [b:word]", 0, cmd_fly)
        .command("hat [remove:word]", 0, cmd_hat)
        .alias("head")
        .command("workbench", 0, |ctx, _| {
            guard(ctx, "workbench")?;
            let p = me(ctx)?;
            open_workbench(ctx, p.uuid);
            Ok(())
        })
        .alias("wb")
        .alias("craft")
        .command("kit [name:word] [player:word]", 0, cmd_kit)
        .command("kits", 0, |ctx, _| cmd_kit(ctx, &Args(vec![])))
        .command("createkit <name:word> [delay:word]", 0, cmd_createkit)
        .command("delkit <name:word>", 0, |ctx, args| {
            guard(ctx, "delkit")?;
            let name = args.get(0).cloned().unwrap_or_default().to_lowercase();
            let mut all = kits(ctx);
            all.remove(&name)
                .ok_or("§4Error: §cThat kit does not exist.")?;
            ctx.set("kits", Value::Object(all))?;
            ctx.reply(format!("§6Deleted kit§c {name}§6."));
            Ok(())
        })
        .command("mail [sub:word] [a:word] [rest:text]", 0, cmd_mail)
        .alias("email")
        .command("setjail <name:word>", 0, cmd_setjail)
        .alias("createjail")
        .command("deljail <name:word>", 0, cmd_deljail)
        .alias("remjail")
        .command("jails", 0, |ctx, _| {
            guard(ctx, "jails")?;
            let j: Vec<String> = jails(ctx).keys().cloned().collect();
            ctx.reply(format!(
                "§6Jails:§r {}",
                if j.is_empty() {
                    "§cnone".into()
                } else {
                    j.join(" ")
                }
            ));
            Ok(())
        })
        .command("jail <player:word> [jail:word] [time:word]", 0, cmd_jail)
        .alias("togglejail")
        .command("unjail <player:word>", 0, |ctx, args| {
            guard(ctx, "jail")?;
            let who = args.get(0).cloned().unwrap_or_default();
            let (u, name) =
                uuid_of(ctx, &who).ok_or_else(|| format!("§4Error: §cPlayer {who} not found."))?;
            unjail(ctx, u, &name)
        })
        .on_join(|ctx| {
            let p = ctx.player().clone();
            let mut names = obj(ctx, "names");
            names.insert(
                p.name.to_lowercase(),
                json::json!({"uuid": key(p.uuid), "name": p.name}),
            );
            let _ = ctx.set("names", Value::Object(names));
            let unread = mail_of(ctx, p.uuid)
                .iter()
                .filter(|m| !m["read"].as_bool().unwrap_or(false))
                .count();
            if unread > 0 {
                ctx.tell(
                    p.uuid,
                    format!(
                        "§6You have §c{unread}§6 messages! Type §c/mail read§6 to view your mail."
                    ),
                );
            } else if ctx
                .get("notify_no_new_mail")
                .and_then(|v| v.as_bool())
                .unwrap_or(true)
            {
                ctx.tell(p.uuid, "§6You have no new messages.");
            }
            if obj(ctx, "fly").contains_key(&key(p.uuid)) {
                let u = p.uuid;
                ctx.run_later(20, move |c| {
                    c.set_may_fly(u, true);
                });
            }
        })
        .on_sign_change(on_sign)
        .on_interact(|ctx, pos, _face| {
            if let Some(p) = ctx.try_player().cloned()
                && is_jailed(ctx, p.uuid)
            {
                return true;
            }
            on_sign_use(ctx, pos)
        })
        .on_block_break(|ctx, pos| {
            let Some(p) = ctx.try_player().cloned() else {
                return false;
            };
            if is_jailed(ctx, p.uuid) {
                return true;
            }
            let mut signs = obj(ctx, "signs");
            if signs.remove(&sign_key(&p.dimension, pos)).is_some() {
                let _ = ctx.set("signs", Value::Object(signs));
            }
            false
        })
        .on_command_line(|ctx, sender, line| {
            let p = ctx
                .find_online(sender)
                .or_else(|| ctx.try_player().cloned())?;
            if !is_jailed(ctx, p.uuid) {
                return None;
            }
            let cmd = line
                .split_whitespace()
                .next()
                .unwrap_or("")
                .trim_start_matches('/')
                .to_lowercase();
            let allowed: Vec<String> = ctx
                .get("jail_allowed_commands")
                .and_then(|v| v.as_array().cloned())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(str::to_lowercase))
                        .collect()
                })
                .unwrap_or_else(|| {
                    ["msg", "tell", "r", "mail", "rules", "help", "helpop"]
                        .map(String::from)
                        .to_vec()
                });
            if allowed.contains(&cmd) {
                None
            } else {
                Some("§4You cannot do that while jailed.".into())
            }
        })
        .on_menu_click(|ctx, window, slot, _b, _m| {
            let _ = ender_click(ctx, window, slot) || disposal_click(ctx, window, slot);
        })
        .on_menu_close(|ctx, window| {
            if let Some(u) = ctx.player_uuid() {
                views(|v| {
                    if v.ender.get(&u).is_some_and(|(w, ..)| *w == window) {
                        v.ender.remove(&u);
                    }
                    if v.disposal.get(&u) == Some(&window) {
                        v.disposal.remove(&u);
                    }
                });
            }
        })
        .on_quit(|ctx| {
            if let Some(u) = ctx.player_uuid() {
                views(|v| {
                    v.ender.remove(&u);
                    v.disposal.remove(&u);
                });
            }
        })
        .every(20, |ctx| {
            jail_tick(ctx);
            // ender chests changed outside the view (an ender chest block, a hopper...) show up within a second
            let owners: Vec<u128> = views(|v| v.ender.values().map(|(_, o, _)| *o).collect());
            for o in owners {
                refresh_ender(ctx, o);
            }
        })
}
