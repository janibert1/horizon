//! Port of "Clans" 1.6.1 by MrFiend179 (flubel.com, `tech.flubel.clans.Clans`), the live SMP's clan plugin, from its
//! data, config and English messages (the jar's source is not available). Data and wording follow the real plugin:
//!
//! * Members are kept by NAME (as Clans does): `leader`, `co_leader` [..], `members` [..]; clan key = lowercase
//!   name, `prefix` = the shown (colour-coded) name. Per clan: `pvp` (clan mates may hurt each other), `joins`
//!   (join requests accepted), `max_members`, `prefix_change` (renames left), `enemies`, `allies`, `kills`,
//!   `deaths`, `chest_size` (9..54), `banner` {base, patterns ["COLOR:code"]}, plus here `balance` (the clan bank;
//!   not in the live data: starts at `initial_balance`) and `home`.
//! * Money = the server's economy (Vault on Paper = rewards' Shards here: `hplug::economy`). Costs from config:
//!   `Amount` (create, 50000), `initial_balance` (25000), `upgrade_cost`/`upgrade_slots` (member slots, from the
//!   clan bank), `chest_upgrade_cost` (+9 chest slots, clan bank), `Banner_Cost` (a member's own money).
//! * Commands (`/clan`, alias `/c`; `/cc <msg>`): help, create, list, top, info [clan], invite, accept, deny, join,
//!   requests, raccept, rdeny, leave, kick, promote, demote, transfer, balance|bank, deposit, withdraw|withdrawal,
//!   sethome, home, upgrade, upgradestorage, chest, pvp, togglejoin, change, online, setbanner, getbanner,
//!   enemy, ally, unenemy, unally, chat, delete (op 2 / `clans.admin`), reload (op 2).
//! * Chat tag: members show `[<prefix>] ` before their rank and name in chat and the Tab list
//!   (`Ctx::set_display_tag`; config `chat_tag`, default on). Clan chat: `/cc` and `/clan chat`.
//! * Clan chest: a shared virtual chest of `chest_size` slots (`/clan chest`); a click moves one stack between it and
//!   your inventory through the engine's checked stack moves (`Host::take_player_stack` / `put_player_stack`), so
//!   several members can have it open at once without copying items (the live jar needed a patch for that).
//! * Clan PvP off: an attack on a clan mate is cancelled, both are told (melee; projectiles are not caught: the SDK
//!   has no projectile-owner hook). Kills/deaths: a player killed within 10 s of a hit by a player of another clan
//!   counts a kill for that clan and a death for theirs; any other death of a member counts a death.
//!   Migration of the live data: tools/migrate_clans.py (clans.yml, clans_chest.yml, join_requests.yml).
use hplug::{economy, json, Args, ChatVerdict, CmdResult, Ctx, Plugin};
use json::{Map, Value};
use std::collections::HashMap;
use std::sync::Mutex;

const IND: &str = "§6| §r";

#[derive(Default)]
struct Run {
    invites: HashMap<String, (String, String, u64)>, // invitee (lower name) -> (clan key, inviter, expires ms)
    chat_mode: Vec<u128>,
    outbox: Vec<(u128, String, String)>,
    member_key: HashMap<u128, String>, // online uuid -> clan key, for the host-less chat hook
    last_hit: HashMap<u128, (u128, u64)>, // victim -> (attacker, ms)
    chests: HashMap<u128, (i32, String)>,  // viewer -> (window, clan)
    homes: Vec<(u128, [f64; 3], u64, String)>,
}
static RUN: Mutex<Option<Run>> = Mutex::new(None);
fn run<R>(f: impl FnOnce(&mut Run) -> R) -> R {
    let mut g = RUN.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(Run::default))
}

// ---------- config ----------
fn cfg_u64(ctx: &Ctx, k: &str, d: u64) -> u64 {
    ctx.get("config").and_then(|c| c.get(k).and_then(|v| v.as_u64())).unwrap_or(d)
}
fn cfg_bool(ctx: &Ctx, k: &str, d: bool) -> bool {
    ctx.get("config").and_then(|c| c.get(k).and_then(|v| v.as_bool())).unwrap_or(d)
}
fn banned(ctx: &Ctx, name: &str) -> bool {
    let low = strip(name).to_lowercase();
    ctx.get("config")
        .and_then(|c| c.get("banned_prefixes").and_then(|v| v.as_array().cloned()))
        .unwrap_or_default()
        .iter()
        .filter_map(|w| w.as_str())
        .any(|w| !w.is_empty() && low.contains(&w.to_lowercase()))
}
/// Without `&x` / `§x` colour codes.
fn strip(s: &str) -> String {
    let mut out = String::new();
    let mut skip = false;
    for c in s.chars() {
        if skip {
            skip = false;
            continue;
        }
        if c == '&' || c == '§' {
            skip = true;
            continue;
        }
        out.push(c);
    }
    out
}

// ---------- data ----------
fn clans(ctx: &Ctx) -> Map<String, Value> {
    ctx.get("clans").and_then(|v| v.as_object().cloned()).unwrap_or_default()
}
fn save(ctx: &mut Ctx, c: Map<String, Value>) -> Result<(), String> {
    ctx.set("clans", Value::Object(c))
}
fn names(c: &Value, k: &str) -> Vec<String> {
    c.get(k).and_then(|v| v.as_array()).into_iter().flatten().filter_map(|x| x.as_str().map(str::to_owned)).collect()
}
fn has(list: &[String], name: &str) -> bool {
    list.iter().any(|n| n.eq_ignore_ascii_case(name))
}
fn everyone(c: &Value) -> Vec<String> {
    let mut v = vec![c["leader"].as_str().unwrap_or_default().to_owned()];
    v.extend(names(c, "co_leader"));
    v.extend(names(c, "members"));
    v.retain(|s| !s.is_empty());
    v
}
#[derive(PartialEq, Clone, Copy)]
enum Role {
    Leader,
    Co,
    Member,
}
fn role(c: &Value, name: &str) -> Option<Role> {
    if c["leader"].as_str().is_some_and(|l| l.eq_ignore_ascii_case(name)) {
        Some(Role::Leader)
    } else if has(&names(c, "co_leader"), name) {
        Some(Role::Co)
    } else if has(&names(c, "members"), name) {
        Some(Role::Member)
    } else {
        None
    }
}
/// The clan `name` belongs to.
fn clan_of(all: &Map<String, Value>, name: &str) -> Option<String> {
    all.iter().find(|(_, c)| role(c, name).is_some()).map(|(k, _)| k.clone())
}
/// The key of the clan called `name` (its key, or its shown name after a `/clan change`).
fn find_key(all: &Map<String, Value>, name: &str) -> Option<String> {
    let low = strip(name).to_lowercase();
    if all.contains_key(&low) {
        return Some(low);
    }
    all.iter().find(|(_, c)| strip(&shown(c)).eq_ignore_ascii_case(&low)).map(|(k, _)| k.clone())
}
fn remove_name(c: &mut Value, name: &str) {
    for k in ["co_leader", "members"] {
        let l: Vec<String> = names(c, k).into_iter().filter(|n| !n.eq_ignore_ascii_case(name)).collect();
        c[k] = json::json!(l);
    }
}
fn push_name(c: &mut Value, k: &str, name: &str) {
    let mut l = names(c, k);
    if !has(&l, name) {
        l.push(name.to_owned());
    }
    c[k] = json::json!(l);
}
fn shown(c: &Value) -> String {
    c["prefix"].as_str().unwrap_or("?").replace('&', "§")
}
fn bal(c: &Value) -> u64 {
    c["balance"].as_u64().unwrap_or(0)
}

fn tell_clan(ctx: &mut Ctx, c: &Value, msg: &str) {
    let members = everyone(c);
    for p in ctx.online() {
        if has(&members, &p.name) {
            ctx.tell(p.uuid, msg);
        }
    }
}
fn say(ctx: &mut Ctx, msg: impl AsRef<str>) {
    ctx.reply(format!("{IND}{}", msg.as_ref()));
}
/// The chat tag of an online player, from their clan (or none).
fn refresh_tag(ctx: &mut Ctx, uuid: u128, name: &str) {
    let all = clans(ctx);
    let key = clan_of(&all, name);
    run(|r| match &key {
        Some(k) => {
            r.member_key.insert(uuid, k.clone());
        }
        None => {
            r.member_key.remove(&uuid);
        }
    });
    let tag = key.filter(|_| cfg_bool(ctx, "chat_tag", true)).map(|k| format!("§7[{}§7] §r", shown(&all[&k])));
    let _ = ctx.set_display_tag(uuid, name, tag.as_deref());
}
fn refresh_tags_of(ctx: &mut Ctx, member_names: &[String]) {
    for p in ctx.online() {
        if has(member_names, &p.name) {
            refresh_tag(ctx, p.uuid, &p.name);
        }
    }
}

// ---------- chest ----------
fn chest(ctx: &Ctx, key: &str, size: usize) -> Vec<Option<hplug::ItemStack>> {
    let mut v: Vec<Option<hplug::ItemStack>> = ctx
        .get("chests")
        .and_then(|c| c.get(key).and_then(|l| l.as_array().cloned()))
        .unwrap_or_default()
        .iter()
        .map(|s| s.as_str().and_then(hplug::item_from_snbt))
        .collect();
    v.resize(size, None);
    v
}
fn save_chest(ctx: &mut Ctx, key: &str, items: &[Option<hplug::ItemStack>]) -> Result<(), String> {
    let mut all = ctx.get("chests").and_then(|v| v.as_object().cloned()).unwrap_or_default();
    all.insert(key.to_owned(), json::json!(items.iter().map(|s| s.as_ref().map(hplug::item_to_snbt)).collect::<Vec<_>>()));
    ctx.set("chests", Value::Object(all))
}
fn refresh_chest_viewers(ctx: &mut Ctx, key: &str) {
    let size = clans(ctx).get(key).and_then(|c| c["chest_size"].as_u64()).unwrap_or(9) as usize;
    let items = chest(ctx, key, size);
    let viewers: Vec<u128> = run(|r| r.chests.iter().filter(|(_, (_, k))| k == key).map(|(v, _)| *v).collect());
    for v in viewers {
        ctx.host.set_menu_items(v, items.clone());
    }
}

// ---------- banners ----------
/// Bukkit `PatternType` codes (as the live data stores them) -> 26.x banner pattern ids.
fn pattern_id(code: &str) -> Option<&'static str> {
    Some(match code {
        "b" => "base",
        "bl" => "square_bottom_left",
        "br" => "square_bottom_right",
        "tl" => "square_top_left",
        "tr" => "square_top_right",
        "bs" => "stripe_bottom",
        "ts" => "stripe_top",
        "ls" => "stripe_left",
        "rs" => "stripe_right",
        "cs" => "stripe_center",
        "ms" => "stripe_middle",
        "drs" => "stripe_downright",
        "dls" => "stripe_downleft",
        "ss" => "small_stripes",
        "cr" => "cross",
        "sc" => "straight_cross",
        "bt" => "triangle_bottom",
        "tt" => "triangle_top",
        "bts" => "triangles_bottom",
        "tts" => "triangles_top",
        "ld" => "diagonal_left",
        "rd" => "diagonal_up_right",
        "lud" => "diagonal_up_left",
        "rud" => "diagonal_right",
        "mc" => "circle",
        "mr" => "rhombus",
        "vh" => "half_vertical",
        "hh" => "half_horizontal",
        "vhr" => "half_vertical_right",
        "hhb" => "half_horizontal_bottom",
        "bo" => "border",
        "cbo" => "curly_border",
        "gra" => "gradient",
        "gru" => "gradient_up",
        "bri" => "bricks",
        "glb" => "globe",
        "cre" => "creeper",
        "sku" => "skull",
        "flo" => "flower",
        "moj" => "mojang",
        "pig" => "piglin",
        "flow" => "flow",
        "gus" => "guster",
        _ => return None,
    })
}
fn pattern_code(id: &str) -> Option<String> {
    let id = id.trim_start_matches("minecraft:");
    [
        "b", "bl", "br", "tl", "tr", "bs", "ts", "ls", "rs", "cs", "ms", "drs", "dls", "ss", "cr", "sc", "bt", "tt", "bts", "tts", "ld", "rd", "lud", "rud", "mc", "mr", "vh", "hh", "vhr", "hhb", "bo", "cbo", "gra", "gru", "bri", "glb", "cre", "sku", "flo", "moj", "pig", "flow", "gus",
    ]
    .into_iter()
    .find(|c| pattern_id(c) == Some(id))
    .map(str::to_owned)
}
/// `give` argument for the clan banner (`minecraft:magenta_banner[banner_patterns=[...]]`).
fn banner_item(b: &Value) -> Option<String> {
    let base = b["base"].as_str()?.to_lowercase();
    let pats: Vec<String> = b["patterns"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| {
            let (color, code) = p.as_str()?.split_once(':')?;
            Some(format!("{{pattern:\"minecraft:{}\",color:\"{}\"}}", pattern_id(code)?, color.to_lowercase()))
        })
        .collect();
    Some(format!("minecraft:{base}_banner[minecraft:banner_patterns=[{}]]", pats.join(",")))
}

// ---------- commands ----------
fn help(ctx: &mut Ctx) {
    ctx.reply("§6| Clans Help");
    for (c, d) in [
        ("top", "List ranking for all top clans."),
        ("create <name>", "Create a new clan with the specified name."),
        ("invite <player>", "Invite a player to your clan."),
        ("accept / deny", "Accept or deny a clan invite."),
        ("info [clan]", "View information about the clan you're in."),
        ("leave", "Leave your current clan."),
        ("balance", "View the total clans balance."),
        ("promote / demote <player>", "Promote or demote a clan member."),
        ("kick <player>", "Kick a player from your clan."),
        ("deposit / withdraw <amount>", "Move money to or from the clan's bank."),
        ("transfer <player>", "Transfer clan leadership to another player."),
        ("join <clan>", "Request to join a clan."),
        ("raccept / rdeny <player>", "Accept or deny a player's request to join the clan."),
        ("requests", "View pending requests to join your clan."),
        ("sethome / home", "Set or teleport to the clan home."),
        ("upgrade", "Upgrades the clan player slot."),
        ("upgradestorage", "Upgrades the clan storage slots."),
        ("chest", "Opens the clans chest."),
        ("pvp", "Toggles pvp between clan members."),
        ("togglejoin", "Toggles the receiving of clan join requests."),
        ("change <prefix>", "Change the clans prefix (Color Coded)."),
        ("online", "Shows all Online Clan Members."),
        ("setbanner / getbanner", "Set the held banner as clan banner / get the clan banner."),
        ("ally / unally <clan>", "Declare or end an alliance."),
        ("enemy / unenemy <clan>", "Declare or end an enmity."),
        ("chat <message> or /cc", "Clan chat."),
    ] {
        ctx.reply(format!("§e/clan {c} §7- {d}"));
    }
}

fn clan_cmd(ctx: &mut Ctx, args: &Args) -> CmdResult {
    let sub = args.get(0).map(|s| s.to_lowercase()).unwrap_or_default();
    match sub.as_str() {
        "" => {
            say(ctx, "Usage: /clan create | list | invite | accept | deny | info | top | home | deposit | withdrawal | bank | leave | transfer and more. Use /clan help");
            return Ok(());
        }
        "help" => {
            help(ctx);
            return Ok(());
        }
        "list" | "top" => {
            let all = clans(ctx);
            if all.is_empty() {
                say(ctx, "There are no clans yet!");
                return Ok(());
            }
            let mut v: Vec<(&String, &Value)> = all.iter().collect();
            v.sort_by(|a, b| b.1["kills"].as_u64().unwrap_or(0).cmp(&a.1["kills"].as_u64().unwrap_or(0)).then(a.0.cmp(b.0)));
            say(ctx, "Top Clans List:");
            for (i, (_, c)) in v.iter().enumerate().take(10) {
                ctx.reply(format!(
                    "§e{}. §r{} §7Leader §f{} §7Members §f{} §7Kills §f{} §7Deaths §f{}",
                    i + 1,
                    shown(c),
                    c["leader"].as_str().unwrap_or("?"),
                    everyone(c).len(),
                    c["kills"].as_u64().unwrap_or(0),
                    c["deaths"].as_u64().unwrap_or(0)
                ));
            }
            return Ok(());
        }
        "reload" => {
            if ctx.op_level() < 2 {
                return Err("§c| You do not have permission to reload the configuration.".into());
            }
            say(ctx, "Plugin Config Reloaded");
            return Ok(());
        }
        "delete" => {
            let is_admin = ctx.op_level() >= 2 || ctx.try_player().is_some_and(|p| ctx.has_perm(p.uuid, &p.name, "clans.admin"));
            if !is_admin {
                return Err("§c| You do not have permission to delete clans.".into());
            }
            let name = args.get(1).ok_or("§c| Enter a Clan's name to delete.")?;
            let mut all = clans(ctx);
            let key = find_key(&all, name).unwrap_or_else(|| name.to_lowercase());
            let Some(c) = all.remove(&key) else {
                return Err(format!("§c| Clan {key} does not exist."));
            };
            save(ctx, all)?;
            let members = everyone(&c);
            refresh_tags_of(ctx, &members);
            say(ctx, format!("Clan {} was deleted successfully.", shown(&c)));
            return Ok(());
        }
        _ => {}
    }
    let me = ctx.try_player().cloned().ok_or("§c| Only players can use this command!")?;
    let mut all = clans(ctx);
    let mine = clan_of(&all, &me.name);
    match sub.as_str() {
        "create" => {
            if mine.is_some() {
                return Err("§c| You cannot create a new clan while you are a member/owner of another clan.".into());
            }
            let name = args.get(1).ok_or("§c| Clan Name cannot be empty.")?.clone();
            if args.get(2).is_some() {
                return Err("§c| Clan name cannot include spaces.".into());
            }
            if strip(&name).chars().count() > cfg_u64(ctx, "Prefix_Limit", 20) as usize {
                return Err("§c| Clan Name cannot be longer than 20 characters.".into());
            }
            if banned(ctx, &name) {
                return Err("§c| Sorry! This prefix is banned, try something else.".into());
            }
            let key = strip(&name).to_lowercase();
            if key.is_empty() || all.contains_key(&key) {
                return Err("§c| Clan name is already taken.".into());
            }
            let cost = cfg_u64(ctx, "Amount", 50000);
            if !economy::charge(ctx, me.uuid, cost, "Clan creation") {
                return Err(format!("§c| You need {cost} to create a clan!"));
            }
            all.insert(
                key.clone(),
                json::json!({
                    "leader": me.name, "co_leader": [], "members": [], "prefix": name, "pvp": true, "joins": true,
                    "max_members": cfg_u64(ctx, "max_members", 50), "prefix_change": cfg_u64(ctx, "prefix_change", 1),
                    "enemies": [], "allies": [], "kills": 0, "deaths": 0, "chest_size": 9,
                    "balance": cfg_u64(ctx, "initial_balance", 25000),
                }),
            );
            save(ctx, all)?;
            refresh_tag(ctx, me.uuid, &me.name);
            say(ctx, format!("Clan {} created successfully!", name.replace('&', "§")));
        }
        "accept" => {
            let now = hplug::now_ms();
            let inv = run(|r| r.invites.remove(&me.name.to_lowercase())).ok_or("§c| You don't have any pending clan invites.")?;
            if inv.2 < now {
                return Err("§c| Your clan invite has expired.".into());
            }
            if mine.is_some() {
                return Err("§c| Player is already in a clan.".into());
            }
            let c = all.get_mut(&inv.0).ok_or("§c| There are no clans.")?;
            if everyone(c).len() as u64 >= c["max_members"].as_u64().unwrap_or(50) {
                return Err("§c| Clan is Full!".into());
            }
            push_name(c, "members", &me.name);
            let c = c.clone();
            save(ctx, all)?;
            refresh_tag(ctx, me.uuid, &me.name);
            say(ctx, format!("You have joined the clan {}!", shown(&c)));
            tell_clan(ctx, &c, &format!("{IND}{} has Joined the clan!", me.name));
        }
        "deny" => {
            let inv = run(|r| r.invites.remove(&me.name.to_lowercase())).ok_or("§c| You don't have any pending clan invites.")?;
            say(ctx, "Clan invite rejected.");
            if let Some(p) = ctx.find_online(&inv.1) {
                ctx.tell(p.uuid, format!("{IND}Player {} has rejected your clan Invite.", me.name));
            }
        }
        "join" => {
            if mine.is_some() {
                return Err("§c| You are already in a clan.".into());
            }
            let key = args.get(1).map(|s| find_key(&all, s).unwrap_or_else(|| strip(s).to_lowercase())).ok_or("§c| Enter a Clan Name.")?;
            let c = all.get(&key).ok_or("§c| No such Clan exists.")?.clone();
            if !c["joins"].as_bool().unwrap_or(true) {
                return Err("§c| Clan is not accepting join requests.".into());
            }
            if everyone(&c).len() as u64 >= c["max_members"].as_u64().unwrap_or(50) {
                return Err("§c| Clan is Full.".into());
            }
            let mut req = ctx.get("requests").and_then(|v| v.as_object().cloned()).unwrap_or_default();
            let mut list: Vec<String> = req.get(&key).and_then(|l| l.as_array()).into_iter().flatten().filter_map(|x| x.as_str().map(str::to_owned)).collect();
            if !has(&list, &me.name) {
                list.push(me.name.clone());
            }
            req.insert(key.clone(), json::json!(list));
            ctx.set("requests", Value::Object(req))?;
            say(ctx, format!("Join request sent to the clan {}'s Leader and Co-Leaders.", shown(&c)));
            let leaders: Vec<String> = std::iter::once(c["leader"].as_str().unwrap_or_default().to_owned()).chain(names(&c, "co_leader")).collect();
            for p in ctx.online() {
                if has(&leaders, &p.name) {
                    ctx.tell(p.uuid, format!("{IND}{} has requested to join your Clan.", me.name));
                }
            }
        }
        "info" => {
            let key = match args.get(1) {
                Some(n) => find_key(&all, n).unwrap_or_else(|| strip(n).to_lowercase()),
                None => mine.ok_or("§c| You are not in a clan!")?,
            };
            let c = all.get(&key).ok_or("§c| There is no such clan.")?.clone();
            let kills = c["kills"].as_u64().unwrap_or(0);
            let deaths = c["deaths"].as_u64().unwrap_or(0);
            let kdr = if deaths == 0 { kills as f64 } else { kills as f64 / deaths as f64 };
            let list = |k: &str| {
                let v = names(&c, k);
                if v.is_empty() { "-".to_owned() } else { v.join(", ") }
            };
            ctx.reply(format!("§6| §r{}", shown(&c)));
            ctx.reply(format!("§eLeader: §f{}", c["leader"].as_str().unwrap_or("?")));
            ctx.reply(format!("§eCo_Leaders: §f{}", list("co_leader")));
            ctx.reply(format!("§eMembers: §f{}", list("members")));
            ctx.reply(format!("§eTotal Members: §f{}/{}", everyone(&c).len(), c["max_members"].as_u64().unwrap_or(50)));
            ctx.reply(format!("§eAllies: §f{}", list("allies")));
            ctx.reply(format!("§eEnemies: §f{}", list("enemies")));
            ctx.reply(format!("§eKDR: §f{kdr:.2} §7({kills}/{deaths})"));
            ctx.reply(format!(
                "§eChest: §f{} slots §7| PvP: §f{} §7| Joins: §f{}",
                c["chest_size"].as_u64().unwrap_or(9),
                if c["pvp"].as_bool().unwrap_or(true) { "enabled" } else { "disabled" },
                if c["joins"].as_bool().unwrap_or(true) { "enabled" } else { "disabled" }
            ));
        }
        "invite" => {
            let key = mine.ok_or("§c| You must be in a clan to invite players!")?;
            let c = all[&key].clone();
            if role(&c, &me.name) == Some(Role::Member) {
                return Err("§c| You must be a leader or co-leader of your clan to invite players!".into());
            }
            let who = args.get(1).ok_or("§c| Enter the players name to invite.")?;
            let t = ctx.find_online(who).ok_or_else(|| format!("§c| Player {who} is not online!"))?;
            if t.uuid == me.uuid {
                return Err("§c| You can not invite yourself to the clan you are in, Duck head.".into());
            }
            if role(&c, &t.name).is_some() {
                return Err(format!("§c| {} is already a member of your clan!", t.name));
            }
            if clan_of(&all, &t.name).is_some() {
                return Err("§c| Player is already in a clan.".into());
            }
            if everyone(&c).len() as u64 >= c["max_members"].as_u64().unwrap_or(50) {
                return Err("§c| Clan is Full.".into());
            }
            let exp = hplug::now_ms() + cfg_u64(ctx, "invite_expire", 30) * 1000;
            run(|r| r.invites.insert(t.name.to_lowercase(), (key.clone(), me.name.clone(), exp)));
            ctx.tell(t.uuid, format!("{IND}You are invited to clan {} by {}.", shown(&c), me.name));
            ctx.tell(t.uuid, format!("{IND}Type /clan accept to accept or /clan deny to reject the invitation."));
            say(ctx, "Invite send Successfully!");
        }
        "chat" => {
            let msg = args.rest(1);
            if msg.trim().is_empty() {
                return Err("§c| Usage: /clan chat <message>".into());
            }
            relay(ctx, me.uuid, &me.name, &msg);
        }
        _ => {
            let key = mine.ok_or("§c| You are not in a clan!")?;
            let mut c = all[&key].clone();
            let r = role(&c, &me.name).unwrap_or(Role::Member);
            let lead = r == Role::Leader;
            let staff = r != Role::Member;
            match sub.as_str() {
                "leave" => {
                    if lead {
                        return Err("§c| Leaders cannot leave the clan. Transfer leadership.".into());
                    }
                    remove_name(&mut c, &me.name);
                    all.insert(key.clone(), c.clone());
                    save(ctx, all)?;
                    refresh_tag(ctx, me.uuid, &me.name);
                    say(ctx, "You have left the clan.");
                    tell_clan(ctx, &c, &format!("{IND}{} left the clan.", me.name));
                }
                "kick" => {
                    if !staff {
                        return Err("§c| You do not have permission to kick players.".into());
                    }
                    let who = args.get(1).ok_or("§c| Enter a clan members name to Kick.")?;
                    let t = ctx.find_online(who).ok_or("§c| You can't kick an offline player.")?;
                    match role(&c, &t.name) {
                        None => return Err(format!("§c| {} is not a member or co-leader of your clan.", t.name)),
                        Some(Role::Leader) => return Err("§c| You do not have permission to kick players.".into()),
                        Some(Role::Co) if !lead => return Err("§c| You cannot kick another Co-Leader.".into()),
                        _ => {}
                    }
                    remove_name(&mut c, &t.name);
                    all.insert(key.clone(), c.clone());
                    save(ctx, all)?;
                    refresh_tag(ctx, t.uuid, &t.name);
                    ctx.tell(t.uuid, format!("{IND}You have been kicked from the clan."));
                    say(ctx, format!("Successfully kicked {} from the clan.", t.name));
                }
                "promote" | "demote" | "transfer" => {
                    if !lead {
                        return Err(match sub.as_str() {
                            "promote" => "§c| Only the leader can promote players.",
                            "demote" => "§c| Only the leader can demote players.",
                            _ => "§c| Only the current leader can transfer leadership.",
                        }
                        .into());
                    }
                    let who = args.get(1).cloned().ok_or(match sub.as_str() {
                        "promote" => "§c| Enter a clan members name to Promote.",
                        "demote" => "§c| Enter a clan members name to Demote.",
                        _ => "§c| Enter a clan members name to Transfer leadership to.",
                    })?;
                    let their = role(&c, &who);
                    match sub.as_str() {
                        "promote" => {
                            match their {
                                Some(Role::Co) => return Err(format!("§c| {who} is already a Co_Leader.")),
                                Some(Role::Member) => {}
                                _ => return Err(format!("§c| {who} is not a member of the clan.")),
                            }
                            remove_name(&mut c, &who);
                            push_name(&mut c, "co_leader", &who);
                            say(ctx, format!("Successfully promoted {who} to Co_Leader."));
                        }
                        "demote" => {
                            match their {
                                Some(Role::Leader) => return Err("§c| You cannot demote the leader.".into()),
                                Some(Role::Co) => {}
                                _ => return Err(format!("§c| {who} is not a Co-Leader.")),
                            }
                            remove_name(&mut c, &who);
                            push_name(&mut c, "members", &who);
                            say(ctx, format!("Successfully Demoted {who} to Member."));
                            if let Some(p) = ctx.find_online(&who) {
                                ctx.tell(p.uuid, format!("{IND}You have been Demoted by {}.", me.name));
                            }
                        }
                        _ => {
                            if their.is_none() || their == Some(Role::Leader) {
                                return Err(format!("§c| {who}  is not a valid member or co-leader in your clan."));
                            }
                            remove_name(&mut c, &who);
                            let real = names(&all[&key], "co_leader").into_iter().chain(names(&all[&key], "members")).find(|n| n.eq_ignore_ascii_case(&who)).unwrap_or(who.clone());
                            c["leader"] = json::json!(real);
                            push_name(&mut c, "co_leader", &me.name);
                            say(ctx, format!("You have successfully transferred leadership to {real}."));
                            tell_clan(ctx, &c, &format!("{IND}Clan leadership has been transferred to {real}."));
                        }
                    }
                    all.insert(key.clone(), c);
                    save(ctx, all)?;
                }
                "balance" | "bank" => say(ctx, format!("Clan Balance is: {}", bal(&c))),
                "deposit" | "withdraw" | "withdrawal" => {
                    let amount: u64 = args.get(1).and_then(|s| s.parse().ok()).ok_or(if sub == "deposit" { "§c| Usage: /clan deposit <amount>" } else { "§c| Usage: /clan withdraw <amount>" })?;
                    if amount == 0 {
                        return Err("§c| Amount must be greater than zero.".into());
                    }
                    if sub == "deposit" {
                        if !economy::charge(ctx, me.uuid, amount, "Clan deposit") {
                            return Err("§c| You don't have enough money to Deposit".into());
                        }
                        c["balance"] = json::json!(bal(&c) + amount);
                        tell_clan(ctx, &c, &format!("{IND}{} deposited {amount} to the Clan Bank.", me.name));
                    } else {
                        if !staff {
                            return Err("§c| Only Leaders and Co-Leaders can withdraw money from clan bank.".into());
                        }
                        if bal(&c) < amount {
                            return Err("§c| Clan bank doesn't have enough funds.".into());
                        }
                        c["balance"] = json::json!(bal(&c) - amount);
                        economy::pay(ctx, me.uuid, amount, "Clan withdrawal");
                        tell_clan(ctx, &c, &format!("{IND}{} withdrew {amount} from the Clan's Bank.", me.name));
                    }
                    all.insert(key.clone(), c);
                    save(ctx, all)?;
                }
                "upgrade" => {
                    if !staff {
                        return Err("§c| You must be a leader or co-leader to upgrade your clan.".into());
                    }
                    let cost = cfg_u64(ctx, "upgrade_cost", 10000);
                    if bal(&c) < cost {
                        return Err("§c| You don't have enough money to Upgrade your Clan.".into());
                    }
                    c["balance"] = json::json!(bal(&c) - cost);
                    c["max_members"] = json::json!(c["max_members"].as_u64().unwrap_or(50) + cfg_u64(ctx, "upgrade_slots", 1));
                    tell_clan(ctx, &c, &format!("{IND}Clan's player slot was upgraded by {}", me.name));
                    all.insert(key.clone(), c);
                    save(ctx, all)?;
                }
                "upgradestorage" => {
                    if !cfg_bool(ctx, "Clan_Chest", true) {
                        return Err("§c| Sorry! This feature is disabled.".into());
                    }
                    if !lead {
                        return Err("§c| Only the current leader can upgrade clan chest.".into());
                    }
                    let size = c["chest_size"].as_u64().unwrap_or(9);
                    if size >= 54 {
                        return Err("§c| Can not upgrade chest! max limit reached.".into());
                    }
                    let cost = cfg_u64(ctx, "chest_upgrade_cost", 30000);
                    if bal(&c) < cost {
                        return Err(format!("§c| You dont have enough money. You need {cost} to upgrade Clans chest."));
                    }
                    c["balance"] = json::json!(bal(&c) - cost);
                    c["chest_size"] = json::json!(size + 9);
                    all.insert(key.clone(), c);
                    save(ctx, all)?;
                    say(ctx, "Successfully Upgraded Clan Chest Storage.");
                }
                "chest" => {
                    if !cfg_bool(ctx, "Clan_Chest", true) {
                        return Err("§c| Sorry! This feature is disabled.".into());
                    }
                    let size = c["chest_size"].as_u64().unwrap_or(9).clamp(9, 54) as usize;
                    let items = chest(ctx, &key, size);
                    let (viewer, k, title) = (me.uuid, key.clone(), strip(&shown(&c)));
                    // menus open from a tick (a command has the player's state swapped in)
                    ctx.run_later(1, move |cx| {
                        let t = hplug::Message::literal(format!("{title} Chest"));
                        if let Some(w) = cx.host.open_menu(viewer, (size / 9) as u8, t, items.clone()) {
                            run(|r| r.chests.insert(viewer, (w, k.clone())));
                        }
                    });
                }
                "pvp" => {
                    if !lead {
                        return Err("§c| Only Leaders can toggle pvp between clan mates.".into());
                    }
                    let on = !c["pvp"].as_bool().unwrap_or(true);
                    c["pvp"] = json::json!(on);
                    all.insert(key.clone(), c.clone());
                    save(ctx, all)?;
                    tell_clan(ctx, &c, &format!("{IND}Clan PvP has been {}.", if on { "enabled" } else { "disabled" }));
                }
                "togglejoin" => {
                    if !lead {
                        return Err("§c| Only Leaders can toggle Clan join's.".into());
                    }
                    let on = !c["joins"].as_bool().unwrap_or(true);
                    c["joins"] = json::json!(on);
                    all.insert(key.clone(), c);
                    save(ctx, all)?;
                    say(ctx, format!("Clan Join's via Invite has been {}.", if on { "enabled" } else { "disabled" }));
                }
                "change" => {
                    if !lead {
                        return Err("§c| Only the current leader can change clan's prefix.".into());
                    }
                    let new = args.get(1).ok_or("§c| Usage: /clan change <new_prefix>. Prefixes can be color coded.")?.clone();
                    let left = c["prefix_change"].as_u64().unwrap_or(0);
                    if left == 0 {
                        return Err("§c| Limit for clan's name change reached.".into());
                    }
                    if strip(&new).chars().count() > cfg_u64(ctx, "Prefix_Limit", 20) as usize || banned(ctx, &new) {
                        return Err("§c| Sorry! This prefix is banned, try something else.".into());
                    }
                    if all.iter().any(|(k, o)| *k != key && strip(&shown(o)).eq_ignore_ascii_case(&strip(&new))) {
                        return Err("§c| Clan name is already taken. try something else.".into());
                    }
                    let old = shown(&c);
                    c["prefix"] = json::json!(new);
                    c["prefix_change"] = json::json!(left - 1);
                    all.insert(key.clone(), c.clone());
                    save(ctx, all)?;
                    refresh_tags_of(ctx, &everyone(&c));
                    say(ctx, format!("Clan name changed from {old} to {}.", shown(&c)));
                }
                "online" => {
                    let members = everyone(&c);
                    let on: Vec<String> = ctx.online().into_iter().filter(|p| has(&members, &p.name)).map(|p| p.name).collect();
                    if on.is_empty() {
                        say(ctx, "No clan members are online.");
                    } else {
                        say(ctx, format!("Online clan members: {}", on.join(", ")));
                    }
                }
                "sethome" => {
                    if !lead {
                        return Err("§c| Only the Clan Leader can set the clan home.".into());
                    }
                    c["home"] = json::json!({"world": me.dimension, "x": me.position[0], "y": me.position[1], "z": me.position[2], "yaw": me.yaw, "pitch": me.pitch});
                    all.insert(key.clone(), c);
                    save(ctx, all)?;
                    say(ctx, "Clan home has been set successfully!");
                }
                "home" => {
                    if c.get("home").is_none_or(|h| h.is_null()) {
                        return Err("§c| This clan does not have a home set.".into());
                    }
                    let due = ctx.tick() + cfg_u64(ctx, "home_warmup_secs", 3) * 20;
                    run(|r| {
                        r.homes.retain(|h| h.0 != me.uuid);
                        r.homes.push((me.uuid, me.position, due, key.clone()));
                    });
                    say(ctx, format!("Teleporting in {}s, do not move.", cfg_u64(ctx, "home_warmup_secs", 3)));
                }
                "requests" | "raccept" | "rdeny" => {
                    if !staff {
                        return Err("§c| Only clan leaders or co-leaders can view join requests.".into());
                    }
                    let mut req = ctx.get("requests").and_then(|v| v.as_object().cloned()).unwrap_or_default();
                    let mut list: Vec<String> = req.get(&key).and_then(|l| l.as_array()).into_iter().flatten().filter_map(|x| x.as_str().map(str::to_owned)).collect();
                    if sub == "requests" {
                        if list.is_empty() {
                            say(ctx, "There are no join requests for your clan.");
                        } else {
                            say(ctx, format!("Join Requests for Clan: {}", shown(&c)));
                            ctx.reply(format!("§f{}", list.join(", ")));
                        }
                        return Ok(());
                    }
                    let who = args.get(1).cloned().ok_or(if sub == "raccept" { "§c| Enter the players name to accept the request." } else { "§c| Enter the players name to deny the request." })?;
                    let Some(pos) = list.iter().position(|n| n.eq_ignore_ascii_case(&who)) else {
                        return Err(if sub == "raccept" { format!("§c| No join request found from player {who}.") } else { format!("§c| {who} has not requested to join your clan.") });
                    };
                    let who = list.remove(pos);
                    req.insert(key.clone(), json::json!(list));
                    ctx.set("requests", Value::Object(req))?;
                    let target = ctx.find_online(&who);
                    if sub == "rdeny" {
                        say(ctx, format!("You have denied {who}'s request to join the clan."));
                        if let Some(t) = target {
                            ctx.tell(t.uuid, format!("{IND}Your request to join {} has been denied by {}.", shown(&c), me.name));
                        }
                        return Ok(());
                    }
                    let t = target.ok_or_else(|| format!("§c| {who} is not online."))?;
                    if clan_of(&all, &t.name).is_some() {
                        return Err(format!("§c| {who} is already in a clan"));
                    }
                    if everyone(&c).len() as u64 >= c["max_members"].as_u64().unwrap_or(50) {
                        return Err("§c| Clan is Full!".into());
                    }
                    push_name(&mut c, "members", &t.name);
                    all.insert(key.clone(), c.clone());
                    save(ctx, all)?;
                    refresh_tag(ctx, t.uuid, &t.name);
                    ctx.tell(t.uuid, format!("{IND}You have been accepted into the clan {}!", shown(&c)));
                    tell_clan(ctx, &c, &format!("{IND}{} has Joined the clan! Request Accepted by {}", t.name, me.name));
                }
                "setbanner" => {
                    if !cfg_bool(ctx, "Clan_Banners", true) {
                        return Err("§c| Sorry! This feature is disabled.".into());
                    }
                    if !lead {
                        return Err("§c| Only Leaders can toggle Clan join's.".into());
                    }
                    let held = ctx.main_hand_item(me.uuid).filter(|s| s.item.ends_with("_banner")).ok_or("§c| You must hold a banner to set it as your clan banner.")?;
                    let base = held.item.trim_start_matches("minecraft:").trim_end_matches("_banner").to_uppercase();
                    // the patterns from the item's components (SNBT), as "COLOR:code"
                    let snbt = hplug::item_to_snbt(&held);
                    let mut patterns = Vec::new();
                    let mut rest = snbt.as_str();
                    while let Some(i) = rest.find("pattern:\"") {
                        rest = &rest[i + 9..];
                        let pat = rest.split('"').next().unwrap_or("");
                        let color = rest.find("color:\"").map(|j| rest[j + 7..].split('"').next().unwrap_or("white")).unwrap_or("white");
                        if let Some(code) = pattern_code(pat) {
                            patterns.push(format!("{}:{code}", color.to_uppercase()));
                        }
                    }
                    c["banner"] = json::json!({"base": base, "patterns": patterns});
                    all.insert(key.clone(), c);
                    save(ctx, all)?;
                    say(ctx, "Clan banner has been set!");
                }
                "getbanner" => {
                    if !cfg_bool(ctx, "Clan_Banners", true) {
                        return Err("§c| Sorry! This feature is disabled.".into());
                    }
                    let item = c.get("banner").and_then(banner_item).ok_or("§c| No Clan Banner Exists.")?;
                    let cost = cfg_u64(ctx, "Banner_Cost", 3750);
                    if !economy::charge(ctx, me.uuid, cost, "Clan banner") {
                        return Err(format!("§c| Sorry you don't have enough balance to get clan banner, you need {cost}"));
                    }
                    ctx.console(format!("give {} {item} 1", me.name));
                    say(ctx, "You received your clan's banner!");
                }
                "ally" | "enemy" | "unally" | "unenemy" => {
                    let (list_key, other_list, word) = match sub.as_str() {
                        "ally" | "unally" => ("allies", "enemies", "Allies"),
                        _ => ("enemies", "allies", "Enemies"),
                    };
                    if !lead {
                        return Err(format!("§c| Only the current leader can {}declare {word}.", if sub.starts_with("un") { "un-" } else { "" }));
                    }
                    let other = args.get(1).map(|s| find_key(&all, s).unwrap_or_else(|| strip(s).to_lowercase())).ok_or("§c| You forgot to mention clans name.")?;
                    if other == key {
                        return Err(format!("§c| You can't {}declare your own clan as an {}.", if sub.starts_with("un") { "un-" } else { "" }, if list_key == "allies" { "ally" } else { "enemy" }));
                    }
                    let o = all.get(&other).cloned().ok_or("§c| There is no such clan.")?;
                    let mut mine_l = names(&c, list_key);
                    let adding = !sub.starts_with("un");
                    if adding {
                        if has(&names(&c, other_list), &other) {
                            return Err(if list_key == "allies" { "§c| You are already enemies with this clan.".into() } else { "§c| You are already allied with this clan.".into() });
                        }
                        if !has(&mine_l, &other) {
                            mine_l.push(other.clone());
                        }
                    } else {
                        if !has(&mine_l, &other) {
                            return Err(if list_key == "allies" { "§c| You are not allied with this clan.".into() } else { "§c| You are not enemies with this clan.".into() });
                        }
                        mine_l.retain(|n| !n.eq_ignore_ascii_case(&other));
                    }
                    c[list_key] = json::json!(mine_l);
                    all.insert(key.clone(), c.clone());
                    save(ctx, all)?;
                    let (me_s, o_s) = (shown(&c), shown(&o));
                    let (mine_msg, theirs) = match sub.as_str() {
                        "ally" => (format!("Your clan is now Allied with {o_s}"), format!("{me_s} Has declared alliance on you.")),
                        "unally" => (format!("Your clan is no longer Allied with {o_s}"), format!("{me_s} Has un-declared alliance on you.")),
                        "enemy" => (format!("Your clan is now Enemies with {o_s}"), format!("{me_s} Has declared war on you.")),
                        _ => (format!("Your clan is no longer Enemies with {o_s}"), format!("{me_s} Has un-declared war on you.")),
                    };
                    tell_clan(ctx, &c, &format!("{IND}{mine_msg}"));
                    tell_clan(ctx, &o, &format!("{IND}{theirs}"));
                }
                _ => say(ctx, "Usage: /clan create | list | invite | accept | deny | info | top | home | deposit | withdrawal | bank | leave | transfer and more. Use /clan help"),
            }
        }
    }
    Ok(())
}

fn relay(ctx: &mut Ctx, from: u128, name: &str, msg: &str) {
    let all = clans(ctx);
    let Some(key) = clan_of(&all, name) else {
        ctx.tell(from, "§c| You are not in a clan!");
        return;
    };
    let c = &all[&key];
    let title = match role(c, name) {
        Some(Role::Leader) => "Leader",
        Some(Role::Co) => "Co-Leader",
        _ => "Member",
    };
    let text = format!("§b[Clan] §7[{title}] §f{name}§7: §f{msg}");
    tell_clan(ctx, c, &text);
    ctx.log(format!("clan chat [{key}] {name}: {msg}"));
}

/// A click in an open clan chest: one stack between the chest and the clicker's inventory.
fn chest_click(ctx: &mut Ctx, window: i32, slot: i32) {
    let Some(viewer) = ctx.player_uuid() else { return };
    let Some(key) = run(|r| r.chests.get(&viewer).filter(|(w, _)| *w == window).map(|(_, k)| k.clone())) else { return };
    let all = clans(ctx);
    let Some(c) = all.get(&key) else { return };
    let name = ctx.online().into_iter().find(|p| p.uuid == viewer).map(|p| p.name).unwrap_or_default();
    if role(c, &name).is_none() {
        ctx.host.close_menu(viewer);
        return;
    }
    if slot < 0 {
        return;
    }
    let size = c["chest_size"].as_u64().unwrap_or(9).clamp(9, 54) as usize;
    let mut items = chest(ctx, &key, size);
    let slot = slot as usize;
    if slot < size {
        // chest -> inventory
        let Some(stack) = items[slot].clone() else { return };
        match ctx.host.put_player_stack(viewer, false, &stack) {
            Ok(()) => {
                items[slot] = None;
                let _ = save_chest(ctx, &key, &items);
                ctx.log(format!("chest [{key}] {name} took {}x {}", stack.count, stack.item));
            }
            Err(e) => {
                ctx.tell(viewer, format!("§c| {e}"));
            }
        }
    } else {
        // inventory part of the screen: 27 main slots (9..36) then the hotbar (0..9)
        let k = slot - size;
        let inv = if k < 27 { k + 9 } else if k < 36 { k - 27 } else { return };
        let Ok((stacks, _)) = ctx.host.player_inventory(viewer, false) else { return };
        let Some(Some(stack)) = stacks.get(inv).cloned() else { return };
        let Some(free) = items.iter().position(|s| s.is_none()) else {
            ctx.tell(viewer, "§c| The clan chest is full.");
            return;
        };
        if ctx.host.take_player_stack(viewer, false, inv, &stack).is_ok() {
            items[free] = Some(stack.clone());
            let _ = save_chest(ctx, &key, &items);
            ctx.log(format!("chest [{key}] {name} put {}x {}", stack.count, stack.item));
        }
    }
    refresh_chest_viewers(ctx, &key);
}

pub fn plugin() -> Plugin {
    Plugin::new("clans")
        .command("clan [sub:word] [a:word] [rest:text]", 0, clan_cmd)
        .alias("c")
        .alias("clans")
        .command("cc <message:text>", 0, |ctx, args| {
            let p = ctx.try_player().cloned().ok_or("§c| Only players can use this command!")?;
            relay(ctx, p.uuid, &p.name, &args.rest(0));
            Ok(())
        })
        .on_join(|ctx| {
            let (u, n) = (ctx.player().uuid, ctx.player().name.clone());
            refresh_tag(ctx, u, &n);
        })
        // clan chat mode toggled with `/clan chat` without a message is not in Clans 1.6.1: chat stays public
        .on_chat(|c| {
            let routed = run(|r| {
                if r.chat_mode.contains(&c.player) && r.member_key.contains_key(&c.player) {
                    r.outbox.push((c.player, c.name.to_owned(), c.message.to_owned()));
                    true
                } else {
                    false
                }
            });
            if routed { ChatVerdict::Cancel(None) } else { ChatVerdict::Allow }
        })
        .on_attack(|ctx, entity_id, kind| {
            if kind != "minecraft:player" {
                return false;
            }
            let Some(me) = ctx.try_player().cloned() else { return false };
            let Some(victim) = ctx.online().into_iter().find(|p| p.entity_id == entity_id) else { return false };
            run(|r| r.last_hit.insert(victim.uuid, (me.uuid, hplug::now_ms())));
            let all = clans(ctx);
            let (Some(a), Some(b)) = (clan_of(&all, &me.name), clan_of(&all, &victim.name)) else { return false };
            if a != b || all[&a]["pvp"].as_bool().unwrap_or(true) {
                return false;
            }
            ctx.tell(me.uuid, "§c| PvP is disabled between clan members.");
            ctx.tell(victim.uuid, format!("§c| {} tried to attack you but clan pvp is disabled.", me.name));
            true
        })
        .on_death(|ctx, _cause| {
            let Some(victim) = ctx.try_player().cloned() else { return };
            let hit = run(|r| r.last_hit.remove(&victim.uuid)).filter(|(_, t)| hplug::now_ms().saturating_sub(*t) < 10_000);
            let mut all = clans(ctx);
            let vk = clan_of(&all, &victim.name);
            let killer = hit.and_then(|(u, _)| ctx.online().into_iter().find(|p| p.uuid == u));
            let kk = killer.as_ref().and_then(|k| clan_of(&all, &k.name));
            let mut changed = false;
            if let Some(v) = &vk {
                let c = all.get_mut(v).unwrap();
                c["deaths"] = json::json!(c["deaths"].as_u64().unwrap_or(0) + 1);
                changed = true;
            }
            if let Some(k) = &kk
                && kk != vk
            {
                let c = all.get_mut(k).unwrap();
                c["kills"] = json::json!(c["kills"].as_u64().unwrap_or(0) + 1);
                changed = true;
            }
            if changed {
                let _ = save(ctx, all);
            }
        })
        .on_menu_click(|ctx, window, slot, _button, _mode| chest_click(ctx, window, slot))
        .on_menu_close(|ctx, window| {
            if let Some(v) = ctx.player_uuid() {
                run(|r| {
                    if r.chests.get(&v).is_some_and(|(w, _)| *w == window) {
                        r.chests.remove(&v);
                    }
                });
            }
        })
        .on_quit(|ctx| {
            if let Some(u) = ctx.player_uuid() {
                run(|r| {
                    r.chat_mode.retain(|x| *x != u);
                    r.homes.retain(|h| h.0 != u);
                    r.chests.remove(&u);
                    r.member_key.remove(&u);
                });
            }
        })
        .every(1, |ctx| {
            for (u, n, msg) in run(|r| std::mem::take(&mut r.outbox)) {
                relay(ctx, u, &n, &msg);
            }
            let t = ctx.tick();
            let due: Vec<(u128, [f64; 3], String)> = run(|r| {
                let (now, later): (Vec<_>, Vec<_>) = r.homes.drain(..).partition(|h| h.2 <= t);
                r.homes = later;
                now.into_iter().map(|h| (h.0, h.1, h.3)).collect()
            });
            for (u, start, key) in due {
                let Some(p) = ctx.online().into_iter().find(|p| p.uuid == u) else { continue };
                if (0..3).map(|i| (p.position[i] - start[i]).powi(2)).sum::<f64>() > 0.25 {
                    ctx.tell(u, "§c| You moved: teleport cancelled.");
                    continue;
                }
                let Some(h) = clans(ctx).get(&key).and_then(|c| c.get("home").cloned()) else { continue };
                ctx.console(format!(
                    "execute in {} run tp {} {} {} {} {} {}",
                    h["world"].as_str().unwrap_or("minecraft:overworld"),
                    p.name,
                    h["x"].as_f64().unwrap_or(0.0),
                    h["y"].as_f64().unwrap_or(64.0),
                    h["z"].as_f64().unwrap_or(0.0),
                    h["yaw"].as_f64().unwrap_or(0.0),
                    h["pitch"].as_f64().unwrap_or(0.0)
                ));
                ctx.tell(u, "§6| You have been teleported to your clan's home.");
            }
        })
}
