//! Console/RCON `tps`, `mspt` and `perfmon stats` in the formats Ori (Discord bot) and the safety monitor parse
//! (Paper's `/tps`, `/mspt` and perf-bridge's `/perfmon stats`). Source of the numbers: Horizon's `Host::tick_stats()`
//! = (tick rate, average ms of the ticks lately, as `/tick query`). DIFFERENCES from Paper: the per-tick sample is that
//! smoothed average (not the single tick's time), so min/max/p99 are of the smoothed series and understate spikes;
//! the 1m/5m/15m TPS means are over per-second samples since the plugin started (fewer samples right after start).
use hplug::Plugin;
use std::collections::VecDeque;
use std::sync::Mutex;

struct Samples {
    ms: VecDeque<f64>,  // one per tick, newest last, at most 1200 (1 minute)
    tps: VecDeque<f64>, // one per second, at most 900 (15 minutes)
    ticks: u64,
}
static S: Mutex<Samples> = Mutex::new(Samples { ms: VecDeque::new(), tps: VecDeque::new(), ticks: 0 });

fn mean(v: impl Iterator<Item = f64>) -> f64 {
    let (mut n, mut s) = (0usize, 0.0);
    for x in v {
        n += 1;
        s += x;
    }
    if n == 0 { 0.0 } else { s / n as f64 }
}
fn last<'a>(q: &'a VecDeque<f64>, n: usize) -> impl Iterator<Item = f64> + 'a {
    q.iter().rev().take(n).copied()
}
/// (avg, min, max) of the newest `n` ms samples
fn window(q: &VecDeque<f64>, n: usize) -> (f64, f64, f64) {
    let v: Vec<f64> = last(q, n).collect();
    if v.is_empty() {
        return (0.0, 0.0, 0.0);
    }
    (mean(v.iter().copied()), v.iter().copied().fold(f64::MAX, f64::min), v.iter().copied().fold(0.0, f64::max))
}

pub fn plugin() -> Plugin {
    Plugin::new("serverstats")
        .every(1, |ctx| {
            let (rate, ms) = ctx.host.tick_stats();
            let mut s = S.lock().unwrap_or_else(|e| e.into_inner());
            s.ticks += 1;
            s.ms.push_back(ms);
            if s.ms.len() > 1200 {
                s.ms.pop_front();
            }
            if s.ticks.is_multiple_of(20) {
                // ticks a second the server keeps up: the rate, or 1000 / ms when that is lower
                let tps = if ms > 0.0 { (1000.0 / ms).min(f64::from(rate)) } else { f64::from(rate) };
                s.tps.push_back(tps);
                if s.tps.len() > 900 {
                    s.tps.pop_front();
                }
            }
        })
        .command("tps", 0, |ctx, _| {
            let s = S.lock().unwrap_or_else(|e| e.into_inner());
            let t = |n: usize| mean(last(&s.tps, n));
            let (rate, _) = ctx.host.tick_stats();
            let fallback = f64::from(rate);
            let v = |n: usize| if s.tps.is_empty() { fallback } else { t(n) };
            ctx.reply(format!("TPS from last 1m, 5m, 15m: {:.2}, {:.2}, {:.2}", v(60), v(300), v(900)));
            Ok(())
        })
        .command("mspt", 0, |ctx, _| {
            let s = S.lock().unwrap_or_else(|e| e.into_inner());
            let w = |n: usize| window(&s.ms, n);
            let (a, b, c) = (w(100), w(200), w(1200));
            ctx.reply(format!(
                "Server tick times (avg/min/max) from last 5s, 10s, 1m: {:.2}/{:.2}/{:.2}, {:.2}/{:.2}/{:.2}, {:.2}/{:.2}/{:.2}",
                a.0, a.1, a.2, b.0, b.1, b.2, c.0, c.1, c.2
            ));
            Ok(())
        })
        .command("perfmon [sub:word]", 2, |ctx, args| {
            if args.get(0).map(|s| s.to_lowercase()).as_deref() != Some("stats") {
                return Err("Usage: /perfmon stats".into());
            }
            let s = S.lock().unwrap_or_else(|e| e.into_inner());
            let mut v: Vec<f64> = s.ms.iter().copied().collect();
            let n = v.len();
            if n == 0 {
                ctx.reply("MSPT 1m: min=0.00 avg=0.00 p99=0.00 max=0.00 (n=0)");
                return Ok(());
            }
            v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let p99 = v[((n as f64 * 0.99).ceil() as usize).clamp(1, n) - 1];
            ctx.reply(format!("MSPT 1m: min={:.2} avg={:.2} p99={:.2} max={:.2} (n={n})", v[0], mean(v.iter().copied()), p99, v[n - 1]));
            Ok(())
        })
}
