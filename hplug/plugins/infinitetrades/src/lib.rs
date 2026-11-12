//! Port of infinite-trades (Paper, 66 lines): villager and wandering-trader trades never run out and never get
//! dearer from use. Java: TradeListener.neverRunOut on PlayerTradeEvent and VillagerAcquireTradeEvent.
//! Needs the engine hook `merchant_offer_traded` (horizon-rust/horizon, see NOTES.md). DIFFERENCES: the Java also
//! resets a recipe when a villager acquires it; here an offer is reset after its first trade (the first trade of a
//! fresh offer is always possible), and a shift-click that trades several times in one click can still run an offer
//! down to its original `max_uses` inside that one click. `/infinitetrades on|off` (op) is new: it exists for the test's
//! negative control and is persisted.
use hplug::{Plugin, json};

fn enabled(ctx: &hplug::Ctx) -> bool {
    ctx.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true)
}

pub fn plugin() -> Plugin {
    Plugin::new("infinitetrades")
        .command("infinitetrades [state:word]", 2, |ctx, args| {
            match args.get(0).map(|s| s.to_lowercase()).as_deref() {
                Some(a @ ("on" | "off")) => {
                    ctx.set("enabled", json::json!(a == "on"))?;
                    ctx.reply(format!("infinite trades: {a}"));
                }
                _ => ctx.reply(format!("infinite trades are {}", if enabled(ctx) { "on" } else { "off" })),
            }
            Ok(())
        })
        .on_trade_offer(|ctx, _kind, offer| {
            if !enabled(ctx) {
                return;
            }
            // MerchantRecipe: setMaxUses(Integer.MAX_VALUE); setUses(0); setDemand(0); setSpecialPrice(0)
            offer.max_uses = i32::MAX;
            offer.uses = 0;
            offer.demand = 0;
            offer.special_price_diff = 0;
        })
}
