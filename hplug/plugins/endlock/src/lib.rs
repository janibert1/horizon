//! Port of nl.jdries.endlock.EndLock (Paper, 75 lines of Java): /endlock lock|unlock|status and a
//! portal block while locked. Java source: /home/jan/end-lock/src/main/java/nl/jdries/endlock/EndLock.java
use hplug::{json, Plugin};

fn locked(ctx: &hplug::Ctx) -> bool {
    ctx.get("locked").and_then(|v| v.as_bool()).unwrap_or(true) // config.yml default: locked: true
}

pub fn plugin() -> Plugin {
    Plugin::new("endlock")
        // plugin.yml: permission endlock.admin default: op  ->  op level 2
        .command("endlock [action:word]", 2, |ctx, args| {
            match args.get(0).map(|s| s.to_lowercase()).as_deref() {
                None | Some("status") => {
                    let l = locked(ctx);
                    ctx.reply(format!("EndLock: End is currently {}", if l { "LOCKED" } else { "OPEN" }));
                }
                Some(a @ ("lock" | "unlock")) => {
                    let lock = a == "lock";
                    ctx.set("locked", json::json!(lock))?;
                    // RCON/console (Ori re-sends `endlock lock|unlock` every tick) has no acting player
                    let who = ctx.try_player().map(|p| p.name.clone()).unwrap_or_else(|| "console".to_owned());
                    println!("[endlock] End {a}ed by {who}");
                    ctx.reply(format!("EndLock: End portal is now {}.", if lock { "LOCKED" } else { "OPEN" }));
                }
                Some(_) => ctx.reply("Usage: /endlock <lock|unlock|status>"),
            }
            Ok(())
        })
        // PlayerTeleportEvent, cause END_PORTAL, destination world_the_end
        .on_portal(|ctx, _from, to, cause| {
            if !locked(ctx) || cause != "end_portal" || to != "minecraft:the_end" {
                return false;
            }
            if let Some(p) = ctx.player_uuid() {
                ctx.tell(p, "The End is not open yet this season — check #announcements for when it opens.");
            }
            true
        })
}
