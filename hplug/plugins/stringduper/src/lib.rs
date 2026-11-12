//! Port of string-duper-plugin (Paper, 36 lines): breaking an ATTACHED tripwire hook outside creative drops
//! one bonus string at the hook (an intentional feature on Horizons SMP).
use hplug::Plugin;

pub fn plugin() -> Plugin {
    Plugin::new("stringduper").on_block_break(|ctx, pos| {
        let Some(p) = ctx.try_player().cloned() else { return false };
        if p.game_mode == 1 {
            return false; // creative
        }
        let state = ctx.host.block(pos);
        if state == "minecraft:tripwire_hook" && ctx.host.block_property(pos, "attached").as_deref() == Some("true") {
            let [x, y, z] = pos;
            ctx.console(format!(
                "execute in {} run summon item {}.5 {}.5 {}.5 {{PickupDelay:10s,Item:{{id:\"minecraft:string\",count:1}}}}",
                p.dimension, x, y, z
            ));
        }
        false
    })
}
