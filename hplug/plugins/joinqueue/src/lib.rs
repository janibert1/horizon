//! Port of nl.jdries.joinqueue.JoinQueuePlugin (fair login queue when the server is full).
//! Java source: /home/jan/join-queue-plugin/src/main/java/nl/jdries/joinqueue/JoinQueuePlugin.java
//! Config keys (set in config/hplug/joinqueue.json): capacity (15), stale-seconds (120), retry-seconds (10).
//! Not ported: ServerListPingEvent.setMaxPlayers (no hook): set max_players in server.toml instead.
use hplug::{json, Ctx, Plugin};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

struct Entry { id: u128, name: String, last_seen: u64 }
static QUEUE: Mutex<Option<Vec<Entry>>> = Mutex::new(None);

fn now() -> u64 { SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64 }
fn cfg(ctx: &Ctx, key: &str, default: u64) -> u64 { ctx.get(key).and_then(|v| v.as_u64()).unwrap_or(default) }

fn with_queue<R>(ctx: &mut Ctx, f: impl FnOnce(&mut Vec<Entry>) -> R) -> R {
    let mut g = QUEUE.lock().unwrap();
    if g.is_none() {
        // loadState(): rows "uuid|lastSeen|name"
        let rows = ctx.get("queue").and_then(|v| v.as_array().cloned()).unwrap_or_default();
        *g = Some(rows.iter().filter_map(|r| {
            let mut p = r.as_str()?.splitn(3, '|');
            Some(Entry { id: u128::from_str_radix(&p.next()?.replace('-', ""), 16).ok()?, last_seen: p.next()?.parse().ok()?, name: p.next()?.to_owned() })
        }).collect());
    }
    let q = g.as_mut().unwrap();
    let before = q.iter().map(|e| (e.id, e.last_seen)).collect::<Vec<_>>();
    let r = f(q);
    if before != q.iter().map(|e| (e.id, e.last_seen)).collect::<Vec<_>>() {
        let rows: Vec<String> = q.iter().map(|e| format!("{:032x}|{}|{}", e.id, e.last_seen, e.name)).collect();
        let _ = ctx.set("queue", json::json!(rows));
    }
    r
}

pub fn plugin() -> Plugin {
    Plugin::new("joinqueue")
        .on_login(|ctx, id, name| {
            if ctx.is_op(id) {                                  // joinqueue.bypass, default: op
                with_queue(ctx, |q| q.retain(|e| e.id != id));
                return None;
            }
            let (capacity, stale, retry) = (cfg(ctx, "capacity", 15), cfg(ctx, "stale-seconds", 120), cfg(ctx, "retry-seconds", 10));
            let online = ctx.online().len() as u64;
            let t = now();
            with_queue(ctx, |q| {
                q.retain(|e| e.last_seen + stale * 1000 >= t);  // pruneStale
                if online < capacity && q.is_empty() {
                    return None;
                }
                match q.iter_mut().find(|e| e.id == id) {
                    Some(e) => { e.name = name.to_owned(); e.last_seen = t; }
                    None => q.push(Entry { id, name: name.to_owned(), last_seen: t }),
                }
                if online < capacity && q.first().map(|e| e.id) == Some(id) {
                    q.remove(0);
                    return None;
                }
                let pos = q.iter().position(|e| e.id == id).map(|i| i + 1).unwrap_or(q.len() + 1);
                Some(format!("\nServer is full.\nQueue position: {pos}\nRetry in about {retry} seconds.\nThe queue keeps the next free slot for position 1."))
            })
        })
        .on_join(|ctx| {
            let id = ctx.player().uuid;
            with_queue(ctx, |q| q.retain(|e| e.id != id));
        })
}
