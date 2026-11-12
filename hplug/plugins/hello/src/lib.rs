//! Demo plugin: /hello, /demosethome + /demohome (persisted; not /sethome or /home, which are Essentials), /spawn-ish teleport, join greeting, block-break guard.
use hplug::{json, Plugin};

static BLOCK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static ADV_ECHO: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn plugin() -> Plugin {
    Plugin::new("hello")
        .on_advancement(|ctx, id| {
            // off by default: other plugins' scripts earn advancements and would see this chat line
            if !ADV_ECHO.load(std::sync::atomic::Ordering::Relaxed) {
                return;
            }
            if let Some(u) = ctx.player_uuid() {
                ctx.tell(u, format!("[hello] advancement {id}"));
            }
        })
        .command("advecho <state:word>", 2, |ctx, args| {
            let on = args.get(0).map(|s| s.as_str()) == Some("on");
            ADV_ECHO.store(on, std::sync::atomic::Ordering::Relaxed);
            ctx.reply(format!("advancement echo {}", if on { "on" } else { "off" }));
            Ok(())
        })
        .command("hello [name:word]", 0, |ctx, args| {
            let who = args.get(0).cloned().unwrap_or_else(|| ctx.player().name.clone());
            ctx.reply(format!("Hello, {who}! (from a Rust plugin on Horizon)"));
            Ok(())
        })
        .command("demosethome", 0, |ctx, _| {
            let p = ctx.player().clone();
            ctx.set(&format!("home.{}", p.name), json::json!(p.position))?;
            ctx.reply(format!("Home saved at {:.1} {:.1} {:.1}", p.position[0], p.position[1], p.position[2]));
            Ok(())
        })
        .command("demohome", 0, |ctx, _| {
            let name = ctx.player().name.clone();
            let pos = ctx.get(&format!("home.{name}")).ok_or("no home set; use /demosethome")?;
            let v: Vec<f64> = json::from_value(pos).map_err(|e| e.to_string())?;
            ctx.teleport(&name, v[0], v[1], v[2]);
            ctx.reply("Teleporting home...");
            Ok(())
        })
        .command("spawnhub", 0, |ctx, _| {
            let name = ctx.player().name.clone();
            ctx.teleport(&name, 0.5, 100.0, 0.5);
            ctx.reply("Teleporting to the hub...");
            Ok(())
        })
        // SDK tests: scheduler (repeat + cancel, cancelled one-shot) and permission nodes
        .command("tasktest", 0, |ctx, _| {
            let u = ctx.player().uuid;
            let never = ctx.run_later(5, move |c| { c.tell(u, "TASK-CANCELLED-RAN"); });
            ctx.cancel(never);
            let n = std::sync::atomic::AtomicU64::new(0);
            let id: std::sync::Arc<std::sync::Mutex<Option<hplug::TaskId>>> = Default::default();
            let id2 = id.clone();
            let t = ctx.run_repeating(4, 4, move |c| {
                let k = n.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                c.tell(u, format!("TASK-REPEAT {k}"));
                if k == 3 {
                    if let Some(t) = *id2.lock().unwrap() { c.cancel(t); }
                    c.tell(u, "TASK-DONE");
                }
            });
            *id.lock().unwrap() = Some(t);
            ctx.reply("tasks scheduled");
            Ok(())
        })
        .command("panictest", 2, |ctx, _| {
            ctx.run_later(2, |_| panic!("deliberate test panic in a tick task"));
            ctx.reply("panic scheduled");
            Ok(())
        })
        .command("permtest", 0, |ctx, _| {
            let p = ctx.player().clone();
            let a = ctx.has_perm(p.uuid, &p.name, "hello.vip");
            let b = ctx.has_perm(p.uuid, &p.name, "hello.admin");
            let c = ctx.has_perm(p.uuid, &p.name, "other.thing.x");
            ctx.reply(format!("PERM vip={a} admin={b} other={c}"));
            Ok(())
        })
        .command("blockdamage [on:word]", 2, |ctx, args| {
            let on = args.get(0).map(|a| a == "on").unwrap_or(true);
            BLOCK.store(on, std::sync::atomic::Ordering::SeqCst);
            ctx.reply(format!("blockdamage {on}"));
            Ok(())
        })
        .on_damage(|d| {
            if !BLOCK.load(std::sync::atomic::Ordering::SeqCst) { return false; }
            if let Some(v) = d.victim_player {
                d.tell(v, format!("DAMAGE-BLOCKED {:.1} ({})", d.amount, d.damage_type));
                return true;
            }
            false
        })
        .on_join(|ctx| {
            let (u, n) = (ctx.player().uuid, ctx.player().name.clone());
            let joins = ctx.get("joins").and_then(|v| v.as_u64()).unwrap_or(0) + 1;
            let _ = ctx.set("joins", json::json!(joins));
            ctx.tell(u, format!("Welcome {n}! This server is Rust (join #{joins})."));
            println!("[hello] {n} joined (total joins {joins})");
        })
        .on_block_break(|ctx, pos| {
            println!("[hello] {} broke block at {:?}", ctx.try_player().map(|p| p.name.as_str()).unwrap_or("?"), pos);
            if let Some(u) = ctx.try_player().map(|p| p.uuid) {
                ctx.tell(u, format!("[hello] you broke a block at {} {} {}", pos[0], pos[1], pos[2]));
            }
            false
        })
        .every(200, |ctx| {
            let n = ctx.online().len();
            println!("[hello] tick {} players online: {n}", ctx.tick());
        })
}
