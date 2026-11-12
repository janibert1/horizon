//! InvSee++ replacement. `/invsee <player>` opens a live 5-row view of a player's inventory: rows 1-3 their main
//! inventory, row 4 the hotbar, row 5 helmet, chestplate, leggings, boots and offhand. `/endersee <player>` (also
//! `/invsee <player> ender`) opens their ender chest (3 rows). Online players are read live, offline ones from their
//! player file (name from the server's usercache.json), through `Host::player_inventory`.
//! With `invsee.modify` (ops have it): a click on a stack in the view moves it into your inventory; a click on a stack
//! in your own inventory (below the view) moves it to them. Each move is the engine's checked move
//! (`Host::move_player_stack`: the stack must still be what the view showed), logged to the server log.
//! Permissions `invsee.use` / `invsee.ender.use` to look, `invsee.modify` / `invsee.ender.modify` to change.
use hplug::{Args, CmdResult, Ctx, ItemStack, Plugin};
use std::collections::HashMap;
use std::sync::Mutex;

struct View {
    target: u128,
    target_name: String,
    window: i32,
    modify: bool,
    ender: bool,
}
static VIEWS: Mutex<Option<HashMap<u128, View>>> = Mutex::new(None);
fn views<R>(f: impl FnOnce(&mut HashMap<u128, View>) -> R) -> R {
    let mut g = VIEWS.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(HashMap::new))
}

/// Player inventory index (0..9 hotbar, 9..36 main, 36 feet, 37 legs, 38 chest, 39 head, 40 offhand) per menu
/// slot of the 5-row view.
fn inv_of_menu(menu: usize) -> Option<usize> {
    Some(match menu {
        0..27 => menu + 9,
        27..36 => menu - 27,
        36 => 39,
        37 => 38,
        38 => 37,
        39 => 36,
        40 => 40,
        _ => return None,
    })
}
fn layout(stacks: &[Option<ItemStack>], ender: bool) -> Vec<Option<ItemStack>> {
    if ender {
        let mut v: Vec<Option<ItemStack>> = stacks.iter().take(27).cloned().collect();
        v.resize(27, None);
        return v;
    }
    (0..45).map(|m| inv_of_menu(m).and_then(|i| stacks.get(i).cloned().flatten())).collect()
}
/// UUID of an online player, else of one in usercache.json.
fn lookup(ctx: &Ctx, name: &str) -> Option<(u128, String)> {
    if let Some(p) = ctx.find_online(name) {
        return Some((p.uuid, p.name));
    }
    let text = std::fs::read_to_string(ctx.server_dir()?.join("usercache.json")).ok()?;
    let v: hplug::json::Value = hplug::json::from_str(&text).ok()?;
    v.as_array()?.iter().find(|e| e["name"].as_str().is_some_and(|n| n.eq_ignore_ascii_case(name))).and_then(|e| {
        Some((hplug::parse_uuid(e["uuid"].as_str()?)?, e["name"].as_str()?.to_owned()))
    })
}

fn open(ctx: &mut Ctx, args: &Args, ender: bool) -> CmdResult {
    let me = ctx.try_player().cloned().ok_or("§cOnly players can do that.")?;
    let (use_node, mod_node) = if ender { ("invsee.ender.use", "invsee.ender.modify") } else { ("invsee.use", "invsee.modify") };
    if !ctx.has_perm(me.uuid, &me.name, use_node) {
        return Err("§cYou do not have permission.".into());
    }
    let name = args.get(0).ok_or(if ender { "§cUsage: /endersee <player>" } else { "§cUsage: /invsee <player>" })?;
    let (target, tname) = lookup(ctx, name).ok_or_else(|| format!("§cPlayer {name} is not online and has never joined."))?;
    if target == me.uuid && !ender {
        return Err("§cThat is your own inventory.".into());
    }
    let modify = ctx.has_perm(me.uuid, &me.name, mod_node);
    let viewer = me.uuid;
    // the menu helpers are refused while a player's state is swapped in (this command): open next tick
    ctx.run_later(1, move |c| {
        let stacks = match c.host.player_inventory(target, ender) {
            Ok((s, _)) => s,
            Err(e) => {
                c.tell(viewer, format!("§cCannot open that: {e}"));
                return;
            }
        };
        let what = if ender { "ender chest" } else { "inventory" };
        let title = hplug::Message::literal(format!("{tname}'s {what}{}", if modify { "" } else { " (view)" }));
        if let Some(window) = c.host.open_menu(viewer, if ender { 3 } else { 5 }, title, layout(&stacks, ender)) {
            views(|v| v.insert(viewer, View { target, target_name: tname.clone(), window, modify, ender }));
        }
    });
    Ok(())
}
fn refresh(ctx: &mut Ctx, viewer: u128, target: u128, ender: bool) {
    if let Ok((stacks, _)) = ctx.host.player_inventory(target, ender) {
        ctx.host.set_menu_items(viewer, layout(&stacks, ender));
    }
}

pub fn plugin() -> Plugin {
    Plugin::new("invsee")
        .command("invsee <player:word> [what:word]", 0, |ctx, args| {
            let ender = args.get(1).is_some_and(|w| w.eq_ignore_ascii_case("ender"));
            open(ctx, args, ender)
        })
        .alias("isee")
        .command("endersee <player:word>", 0, |ctx, args| open(ctx, args, true))
        .alias("esee")
        .on_menu_click(|ctx, window, slot, _button, _mode| {
            let Some(viewer) = ctx.player_uuid() else { return };
            let Some((target, tname, modify, ender)) =
                views(|v| v.get(&viewer).filter(|x| x.window == window).map(|x| (x.target, x.target_name.clone(), x.modify, x.ender)))
            else {
                return;
            };
            if !modify || slot < 0 {
                return;
            }
            let slot = slot as usize;
            let top = if ender { 27 } else { 45 };
            let result = if slot < top {
                // take from them
                let Some(inv) = (if ender { Some(slot) } else { inv_of_menu(slot) }) else { return };
                let Ok((stacks, _)) = ctx.host.player_inventory(target, ender) else { return };
                let Some(Some(stack)) = stacks.get(inv).cloned() else { return };
                ctx.host.move_player_stack(target, ender, inv, &stack, viewer, false).map(|_| format!("took {}x {} from {tname}", stack.count, stack.item))
            } else {
                // give from the viewer's own inventory part of the screen (27 main, then 9 hotbar)
                let k = slot - top;
                let inv = if k < 27 { k + 9 } else if k < 36 { k - 27 } else { return };
                let Ok((stacks, _)) = ctx.host.player_inventory(viewer, false) else { return };
                let Some(Some(stack)) = stacks.get(inv).cloned() else { return };
                ctx.host.move_player_stack(viewer, false, inv, &stack, target, ender).map(|_| format!("gave {}x {} to {tname}", stack.count, stack.item))
            };
            match result {
                Ok(what) => ctx.log(format!("{viewer:032x} {what}{}", if ender { " (ender chest)" } else { "" })),
                Err(e) => {
                    ctx.tell(viewer, format!("§cCould not move that: {e}"));
                }
            }
            refresh(ctx, viewer, target, ender);
        })
        .on_menu_close(|ctx, window| {
            if let Some(viewer) = ctx.player_uuid() {
                views(|v| { if v.get(&viewer).is_some_and(|x| x.window == window) { v.remove(&viewer); } });
            }
        })
        .on_quit(|ctx| {
            if let Some(u) = ctx.player_uuid() {
                let closed: Vec<u128> = views(|v| {
                    v.remove(&u);
                    let gone: Vec<u128> = v.iter().filter(|(_, x)| x.target == u).map(|(k, _)| *k).collect();
                    for k in &gone { v.remove(k); }
                    gone
                });
                for viewer in closed {
                    ctx.run_later(1, move |c| c.host.close_menu(viewer));
                }
            }
        })
        .every(10, |ctx| {
            let open: Vec<(u128, u128, bool)> = views(|v| v.iter().map(|(k, x)| (*k, x.target, x.ender)).collect());
            for (viewer, target, ender) in open {
                refresh(ctx, viewer, target, ender);
            }
        })
}
