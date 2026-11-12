//! Port of anchor-value-lock (Paper, 200 lines + Discord notifier): "wealth becomes a permanent monument, not a bank".
//! Breaking value blocks (diamond/netherite/emerald/gold/iron/coal/redstone/lapis/copper/raw blocks, ancient debris) inside
//! the spawn region still works but drops nothing: the break is cancelled and the block replaced by air without drops.
//! `/anchorlock status|wipecontainers` (op/console). The region is `claims`' spawn region (it replaced WorldGuard's `spawn`).
//! DIFFERENCE: Horizon has no way to list or clear block-entity inventories, so `wipecontainers` REMOVES the containers
//! (chest, trapped chest, barrel, shulker boxes, hopper, dropper, dispenser, furnaces) with their contents via `fill ... replace`
//! (no drops) instead of emptying them. Config: `no_drop_blocks` (list of ids), `discord_bot_token`, `discord_channel_id`.
use hplug::{discord_post, Plugin};

const DEFAULT: &[&str] = &["diamond_block", "netherite_block", "emerald_block", "gold_block", "iron_block", "coal_block", "redstone_block", "lapis_block", "copper_block", "raw_iron_block", "raw_gold_block", "raw_copper_block", "ancient_debris"];
const CONTAINERS: &[&str] = &["chest", "trapped_chest", "barrel", "hopper", "dropper", "dispenser", "furnace", "blast_furnace", "smoker", "white_shulker_box", "shulker_box", "black_shulker_box", "blue_shulker_box", "brown_shulker_box", "cyan_shulker_box", "gray_shulker_box", "green_shulker_box", "light_blue_shulker_box", "light_gray_shulker_box", "lime_shulker_box", "magenta_shulker_box", "orange_shulker_box", "pink_shulker_box", "purple_shulker_box", "red_shulker_box", "yellow_shulker_box"];

fn no_drop(ctx: &hplug::Ctx, block: &str) -> bool {
    let id = block.trim_start_matches("minecraft:");
    match ctx.get("no_drop_blocks").and_then(|v| v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_lowercase())).collect::<Vec<_>>())) {
        Some(list) => list.iter().any(|b| b.trim_start_matches("minecraft:") == id),
        None => DEFAULT.contains(&id),
    }
}

pub fn plugin() -> Plugin {
    Plugin::new("anchorvaluelock")
        .on_block_break(|ctx, pos| {
            let Some(p) = ctx.try_player().cloned() else { return false };
            let Some((dim, x1, z1, x2, z2)) = hplug_plugin_claims::spawn_box() else { return false };
            if p.dimension != dim || pos[0] < x1 || pos[0] > x2 || pos[2] < z1 || pos[2] > z2 { return false; }
            let block = ctx.host.block(pos);
            if !no_drop(ctx, &block) { return false; }
            // refuse the normal break (it would drop the block) and remove it without drops
            ctx.console(format!("execute in {dim} run setblock {} {} {} minecraft:air", pos[0], pos[1], pos[2]));
            ctx.tell(p.uuid, "§7The block crumbles without leaving anything behind.");
            true
        })
        .command("anchorlock <sub:word>", 2, |ctx, args| {
            match args.get(0).map(|s| s.to_lowercase()).as_deref() {
                Some("status") => {
                    let n = ctx.get("no_drop_blocks").and_then(|v| v.as_array().map(|a| a.len())).unwrap_or(DEFAULT.len());
                    let r = hplug_plugin_claims::spawn_box().map(|b| format!("{} [{},{}]-[{},{}]", b.0, b.1, b.2, b.3, b.4)).unwrap_or("not set (use /claimadmin spawn set)".into());
                    ctx.reply(format!("AnchorValueLock: protecting {n} block type(s) in the spawn region: {r}."));
                }
                Some("wipecontainers") => {
                    let Some((dim, x1, z1, x2, z2)) = hplug_plugin_claims::spawn_box() else { return Err("No spawn region set: nothing wiped.".into()) };
                    // fill is limited to 32768 blocks: slices of 16x16 columns, 384 high would be too big -> 8x8 columns x 384
                    let mut cmds = 0;
                    let mut x = x1;
                    while x <= x2 {
                        let xe = (x + 7).min(x2);
                        let mut z = z1;
                        while z <= z2 {
                            let ze = (z + 7).min(z2);
                            for c in CONTAINERS {
                                ctx.console(format!("execute in {dim} run fill {x} -64 {z} {xe} 319 {ze} minecraft:air replace minecraft:{c}"));
                                cmds += 1;
                            }
                            z = ze + 1;
                        }
                        x = xe + 1;
                    }
                    ctx.log(format!("{} wiped containers in the spawn region ({cmds} fill commands queued)", ctx.sender_name()));
                    ctx.reply(format!("AnchorValueLock: removing containers in the spawn region ({cmds} fill commands queued; they run over the next ticks)."));
                    if let (Some(t), Some(c)) = (ctx.get("discord_bot_token"), ctx.get("discord_channel_id")) {
                        discord_post(t.as_str().unwrap_or(""), c.as_str().unwrap_or(""), &format!("🧹 **Anchor containers wiped** by `{}` as part of the season reset.", ctx.sender_name()));
                    }
                }
                _ => return Err("Usage: /anchorlock <wipecontainers|status>".into()),
            }
            Ok(())
        })
}
