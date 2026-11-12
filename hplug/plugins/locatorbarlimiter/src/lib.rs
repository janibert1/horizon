//! Port of Jan's locator-bar-limiter (Paper + PacketEvents, 185 LOC): players only see each other on the
//! locator bar within `range` blocks (default 300). The Paper plugin dropped WAYPOINT packets for far players;
//! Horizon has no packet filter, but since the engine change in `engine-patches/locator-bar-attributes.patch`
//! the server honours the vanilla attributes `waypoint_transmit_range` and `waypoint_receive_range` of
//! players (vanilla 26.x does too), so this plugin sets both base values on everyone it limits.
//! The locator bar shows a source to a receiver when `distance < min(source.transmit, receiver.receive)`.
//!
//! Config `config/hplug/locatorbarlimiter.json`: `enabled` (true), `range` (300), `bypass` (names that keep
//! the vanilla 60,000,000 range both ways; ops are NOT bypassed by default, as on Paper where the
//! bypass was its own permission `locatorbar.bypass`, see `bypass_perm`).
use hplug::{json, Ctx, Plugin};

const VANILLA: f64 = 6.0e7;

fn enabled(ctx: &Ctx) -> bool {
    ctx.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true)
}
fn range(ctx: &Ctx) -> f64 {
    ctx.get("range").and_then(|v| v.as_f64()).unwrap_or(300.0).max(0.0)
}
fn bypassed(ctx: &Ctx, uuid: u128, name: &str) -> bool {
    let listed = ctx
        .get("bypass")
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|v| v.as_str())
        .any(|n| n.eq_ignore_ascii_case(name));
    // `locatorbar.bypass` through permissions.json, only when switched on (ops have every node in hplug)
    listed || (ctx.get("bypass_perm").and_then(|v| v.as_bool()).unwrap_or(false) && ctx.has_perm(uuid, name, "locatorbar.bypass"))
}

/// Set both attributes for one player (queued console commands, next tick start).
fn apply(ctx: &mut Ctx, uuid: u128, name: &str) {
    let r = if !enabled(ctx) || bypassed(ctx, uuid, name) { VANILLA } else { range(ctx) };
    for attr in ["minecraft:waypoint_transmit_range", "minecraft:waypoint_receive_range"] {
        ctx.console(format!("attribute {name} {attr} base set {r}"));
    }
}

fn apply_all(ctx: &mut Ctx) -> usize {
    let online = ctx.online();
    for p in &online {
        apply(ctx, p.uuid, &p.name);
    }
    online.len()
}

pub fn plugin() -> Plugin {
    Plugin::new("locatorbarlimiter")
        .command("locatorbar [sub:word] [value:word]", 2, |ctx, args| {
            match args.get(0).map(|s| s.to_lowercase()).as_deref() {
                None | Some("status") => {
                    let (e, r) = (enabled(ctx), range(ctx));
                    ctx.reply(format!("§7Locator bar limit: {} §7range §f{r}§7 blocks", if e { "§aON" } else { "§cOFF" }));
                }
                Some("range") => {
                    let r: f64 = args.get(1).and_then(|s| s.parse().ok()).ok_or("Usage: /locatorbar range <blocks>")?;
                    ctx.set("range", json::json!(r.max(0.0)))?;
                    let n = apply_all(ctx);
                    ctx.reply(format!("§aLocator bar range set to {} blocks ({n} online updated).", r.max(0.0)));
                }
                Some(s @ ("on" | "off")) => {
                    ctx.set("enabled", json::json!(s == "on"))?;
                    let n = apply_all(ctx);
                    ctx.reply(format!("§aLocator bar limit {} ({n} online updated).", if s == "on" { "enabled" } else { "disabled" }));
                }
                Some("reapply") => {
                    let n = apply_all(ctx);
                    ctx.reply(format!("§aRe-applied to {n} online players."));
                }
                Some(_) => ctx.reply("§7Usage: /locatorbar [status|range <n>|on|off|reapply]"),
            }
            Ok(())
        })
        .on_join(|ctx| {
            let (u, n) = (ctx.player().uuid, ctx.player().name.clone());
            apply(ctx, u, &n);
        })
        // A respawn keeps base values (ServerPlayer.restoreFrom), but a player file from Paper or an admin's
        // /attribute may change them: re-apply every minute (two cheap commands per player).
        .every(1200, |ctx| {
            apply_all(ctx);
        })
}
