//! Port of new-player-protection (Paper, 225 lines): a one-time 20-minute damage immunity for a player's
//! first-ever join, and a protected player cannot hurt other players (3 blocked attempts in 20 s end the
//! protection). Needs `NativeServerPlayer::entity_id` (patch `player-entity-id`) for the damage hook.
//! Config (`config/hplug/newplayerprotection.json`): `duration_secs` (default 1200).
//! Other plugins ask `newplayerprotection::is_protected(uuid)` (Paper: player metadata "newplayerprotection").
use hplug::{Plugin, json, now_ms};
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

const STRIKE_LIMIT: usize = 3;
const STRIKE_WINDOW_MS: u64 = 20_000;

/// uuid -> expiry (epoch ms) of players inside their window.
static ACTIVE: Mutex<Option<HashMap<u128, u64>>> = Mutex::new(None);
/// uuid -> times (ms) of blocked attacks on other players.
static STRIKES: Mutex<Option<HashMap<u128, VecDeque<u64>>>> = Mutex::new(None);

pub fn is_protected(uuid: u128) -> bool {
    ACTIVE.lock().unwrap().as_ref().and_then(|m| m.get(&uuid)).is_some_and(|&e| e > now_ms())
}
fn set_active(uuid: u128, expires: u64) {
    ACTIVE.lock().unwrap().get_or_insert_with(Default::default).insert(uuid, expires);
}
fn end(uuid: u128) -> bool {
    STRIKES.lock().unwrap().get_or_insert_with(Default::default).remove(&uuid);
    ACTIVE.lock().unwrap().get_or_insert_with(Default::default).remove(&uuid).is_some()
}
/// Records a blocked attack; true once more than STRIKE_LIMIT happened within the window.
fn strike(uuid: u128, now: u64) -> bool {
    let mut g = STRIKES.lock().unwrap();
    let q = g.get_or_insert_with(Default::default).entry(uuid).or_default();
    q.push_back(now);
    while q.front().is_some_and(|t| now - t > STRIKE_WINDOW_MS) {
        q.pop_front();
    }
    q.len() > STRIKE_LIMIT
}

pub fn plugin() -> Plugin {
    Plugin::new("newplayerprotection")
        .on_join(|ctx| {
            let u = ctx.player().uuid;
            let key = format!("p.{u:032x}");
            let now = now_ms();
            let secs = ctx.get("duration_secs").and_then(|v| v.as_u64()).unwrap_or(1200);
            let expires = match ctx.get(&key).and_then(|v| v.get("expires").and_then(|e| e.as_u64())) {
                // Seen before: re-arm only if the window is still running (restart mid-window)
                Some(e) if e > now => e,
                Some(_) => return,
                None => {
                    let e = now + secs * 1000;
                    let _ = ctx.set(&key, json::json!({"granted": now, "expires": e}));
                    e
                }
            };
            set_active(u, expires);
            let first = expires > now + secs.saturating_sub(5) * 1000;
            if first {
                ctx.run_later(40, move |c| {
                    c.tell(u, "§aWelcome! You're protected from all damage and can't be attacked for the next 20 minutes.");
                    c.tell(u, "§7Note: you can't hurt other players during this time either - a stray hit is fine, but attacking someone more than 3 times within 20 seconds will end your protection early.");
                });
            }
            let ticks = ((expires - now) / 50).max(1);
            ctx.run_later(ticks, move |c| {
                if end(u) {
                    c.tell(u, "§eYour new-player protection has ended. Watch your back!");
                    c.action_bar(u, "§cProtection ended");
                }
            });
        })
        .on_quit(|ctx| {
            if let Some(u) = ctx.player_uuid() {
                ACTIVE.lock().unwrap().get_or_insert_with(Default::default).remove(&u);
            }
        })
        .on_damage(|d| {
            let Some(victim) = d.victim_player else { return false };
            if is_protected(victim) {
                return true;
            }
            if let Some(att) = d.attacker_player.filter(|a| is_protected(*a)) {
                // protected player hitting an unprotected player: block, and end it after repeated tries
                if strike(att, now_ms()) {
                    let ended = end(att);
                    // keep the one-time grant used: the stored expiry stays, but is now in the past
                    if ended {
                        d.tell(att, "§cYour new-player protection has ended - you attacked another player.");
                    }
                }
                return true;
            }
            false
        })
}
