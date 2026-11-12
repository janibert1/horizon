//! Port of player-heads-plugin (Paper, 35 lines): a player drops their own head on death, named
//! "<Name>'s Head" in yellow, textured with their skin (profile component). The drop is a `summon item`
//! console command queued to the next tick (Host has no item-drop call yet).
#![allow(clippy::type_complexity)] // plugin-local helpers with many parts
use hplug::Plugin;
use std::collections::HashMap;
use std::sync::Mutex;

/// uuid -> (name, dimension, position) as of the last tick: the death hook runs without the player's
/// record (they are already dead), so the place of death is the last known one.
static LAST: Mutex<Option<HashMap<u128, (String, String, [f64; 3])>>> = Mutex::new(None);

pub fn plugin() -> Plugin {
    Plugin::new("playerheads").every(1, |ctx| {
        let now: HashMap<_, _> = ctx.online().into_iter().filter(|p| p.health > 0.0).map(|p| (p.uuid, (p.name, p.dimension, p.position))).collect();
        let mut g = LAST.lock().unwrap();
        let m = g.get_or_insert_with(Default::default);
        m.extend(now);
    }).on_death(|ctx, _cause| {
        let known = ctx.player_uuid().and_then(|u| LAST.lock().unwrap().as_ref().and_then(|m| m.get(&u).cloned()));
        let Some((name, dimension, position)) = known else {
            ctx.log("death of a player with no known position: no head dropped");
            return;
        };
        let name = name.replace(['"', '\\'], "");
        let [x, y, z] = position;
        ctx.console(format!(
            "execute in {dimension} run summon item {x:.2} {y:.2} {z:.2} {{PickupDelay:10s,Item:{{id:\"minecraft:player_head\",count:1,components:{{\"minecraft:profile\":{{name:\"{name}\"}},\"minecraft:custom_name\":{{text:\"{name}'s Head\",color:\"yellow\",italic:false}}}}}}}}"
        ));
    })
}
