//! SkinsRestorer replacement for the offline-mode (cracked) backend: players get a real skin.
//! * On join, a player without a skin (offline-mode profile, not premium-forwarded) gets their chosen skin, else
//!   (config `auto_by_name`, default true) the skin of the premium account with their own name, if one exists.
//! * `/skin <name>` takes the skin of that premium account; `/skin clear` back to the default; `/skin update`
//!   refetches; staff: `/skin set <player> <name>`, `/skin clear <player>` (`skins.others`).
//!   Textures come from Mojang (api.mojang.com + sessionserver, signed) through `curl` in a background thread
//!   (needs curl and outbound HTTPS on the host), cached 24 h in config/hplug/skins.json. Applied with
//!   `Host::set_player_skin` (engine patch): others see it at once, the player after a respawn/dimension change.
//!   Permissions: `skins.set` (open by default: config `open`), `skins.others`.
//!   NOT: skin URLs (mineskin.org upload), per-world skins, the SkinsRestorer GUI. Migration: tools/migrate_skinsrestorer.py.
use hplug::{Args, CmdResult, Ctx, Plugin, json};
use std::sync::Mutex;

const DAY_MS: u64 = 24 * 3600 * 1000;

/// Finished fetches: (skin name lowercase, Ok((value, signature)) | Err(reason), players to apply it to)
type Done = (String, Result<(String, String), String>, Vec<u128>);
static DONE: Mutex<Vec<Done>> = Mutex::new(Vec::new());
static INFLIGHT: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn curl_json(url: &str) -> Result<json::Value, String> {
    let out = std::process::Command::new("curl")
        .args([
            "-s",
            "-m",
            "10",
            "-A",
            "hplug-skins/1.0",
            "-w",
            "\n%{http_code}",
            url,
        ])
        .output()
        .map_err(|e| format!("curl: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    let (body, code) = text.rsplit_once('\n').unwrap_or((&text, "0"));
    match code.trim() {
        "200" => json::from_str(body).map_err(|e| e.to_string()),
        "204" | "404" => Err("no premium account with that name".into()),
        "429" => Err("Mojang rate limit, try again in a minute".into()),
        c => Err(format!("HTTP {c}")),
    }
}

fn fetch(skin: String, apply_to: Vec<u128>) {
    {
        let mut f = INFLIGHT.lock().unwrap_or_else(|e| e.into_inner());
        if f.contains(&skin) {
            return;
        }
        f.push(skin.clone());
    }
    std::thread::spawn(move || {
        let r = (|| {
            let id = curl_json(&format!(
                "https://api.mojang.com/users/profiles/minecraft/{skin}"
            ))?;
            let id = id
                .get("id")
                .and_then(|i| i.as_str())
                .ok_or("no id")?
                .to_owned();
            let prof = curl_json(&format!(
                "https://sessionserver.mojang.com/session/minecraft/profile/{id}?unsigned=false"
            ))?;
            let tex = prof
                .get("properties")
                .and_then(|p| p.as_array())
                .into_iter()
                .flatten()
                .find(|p| p.get("name").and_then(|n| n.as_str()) == Some("textures"))
                .ok_or("profile has no textures")?;
            Ok((
                tex.get("value")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_owned(),
                tex.get("signature")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_owned(),
            ))
        })();
        INFLIGHT
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|s| *s != skin);
        DONE.lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((skin, r, apply_to));
    });
}

fn valid_name(s: &str) -> bool {
    (3..=16).contains(&s.len()) && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}
fn cached(ctx: &Ctx, skin: &str) -> Option<(String, String)> {
    let c = ctx.get("cache")?.get(skin)?.clone();
    let at = c.get("fetched")?.as_u64()?;
    if hplug::now_ms().saturating_sub(at) > DAY_MS {
        return None;
    }
    Some((
        c.get("value")?.as_str()?.to_owned(),
        c.get("signature")?.as_str()?.to_owned(),
    ))
}
fn choice(ctx: &Ctx, uuid: u128) -> Option<String> {
    ctx.get("players")?
        .get(hplug::uuid_dashed(uuid))?
        .as_str()
        .map(str::to_owned)
}
fn set_choice(ctx: &mut Ctx, uuid: u128, skin: Option<&str>) -> Result<(), String> {
    let mut m = ctx
        .get("players")
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    match skin {
        Some(s) => {
            m.insert(hplug::uuid_dashed(uuid), json::json!(s));
        }
        None => {
            m.remove(&hplug::uuid_dashed(uuid));
        }
    }
    ctx.set("players", json::Value::Object(m))
}
/// Apply `skin` (lowercase name, "" = default) to `uuid` now if cached, else fetch then apply.
fn apply(ctx: &mut Ctx, uuid: u128, skin: &str) {
    if skin.is_empty() {
        ctx.run_later(1, move |c| {
            c.set_player_skin(uuid, "", None);
        });
        return;
    }
    match cached(ctx, skin) {
        Some((v, s)) => {
            ctx.run_later(1, move |c| {
                c.set_player_skin(uuid, &v, Some(&s));
            });
        }
        None => fetch(skin.to_owned(), vec![uuid]),
    }
}

fn cmd(ctx: &mut Ctx, args: &Args) -> CmdResult {
    let me = ctx.try_player().cloned();
    let may = |ctx: &Ctx, node: &str| {
        me.as_ref().is_none_or(|p| {
            let open: Vec<String> = ctx
                .get("open")
                .and_then(|v| v.as_array().cloned())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_else(|| vec!["skins.set".into()]);
            open.iter().any(|n| n == node) || ctx.has_perm(p.uuid, &p.name, node)
        })
    };
    let sub = args
        .get(0)
        .map(|s| s.to_lowercase())
        .ok_or("§cUsage: /skin <name> | clear | update | set <player> <name>")?;
    match sub.as_str() {
        "set" | "clear" if args.len() >= 2 && (sub == "clear" || args.len() >= 3) => {
            if !may(ctx, "skins.others") {
                return Err("§cYou do not have permission.".into());
            }
            let t = ctx
                .find_online(args.get(1).unwrap())
                .ok_or("§cThat player is not online.")?;
            let skin = if sub == "clear" {
                String::new()
            } else {
                args.get(2).unwrap().to_lowercase()
            };
            if !skin.is_empty() && !valid_name(&skin) {
                return Err("§cThat is not a valid skin name.".into());
            }
            set_choice(
                ctx,
                t.uuid,
                if skin.is_empty() { None } else { Some(&skin) },
            )?;
            apply(ctx, t.uuid, &skin);
            ctx.reply(format!(
                "§aSkin of {} {}.",
                t.name,
                if skin.is_empty() {
                    "cleared".to_owned()
                } else {
                    format!("set to {skin}")
                }
            ));
        }
        "clear" => {
            let p = me.clone().ok_or("§cUsage: skin clear <player>")?;
            set_choice(ctx, p.uuid, None)?;
            apply(ctx, p.uuid, "");
            ctx.reply("§aYour skin was cleared.");
        }
        "update" => {
            let p = me.clone().ok_or("§cPlayers only.")?;
            let skin = choice(ctx, p.uuid).unwrap_or_else(|| p.name.to_lowercase());
            let mut c = ctx
                .get("cache")
                .and_then(|v| v.as_object().cloned())
                .unwrap_or_default();
            c.remove(&skin);
            ctx.set("cache", json::Value::Object(c))?;
            fetch(skin, vec![p.uuid]);
            ctx.reply("§aRefreshing your skin...");
        }
        name => {
            let p = me.clone().ok_or("§cUsage: skin set <player> <name>")?;
            if !may(ctx, "skins.set") {
                return Err("§cYou do not have permission.".into());
            }
            if !valid_name(name) {
                return Err("§cThat is not a valid skin name.".into());
            }
            set_choice(ctx, p.uuid, Some(name))?;
            apply(ctx, p.uuid, name);
            ctx.reply(format!("§aSetting your skin to {name}... others see it at once, you after a respawn or dimension change."));
        }
    }
    Ok(())
}

pub fn plugin() -> Plugin {
    Plugin::new("skins")
        .command("skin <a:word> [b:word] [c:word]", 0, cmd)
        .alias("skins")
        .on_join(|ctx| {
            let p = ctx.player().clone();
            let chosen = choice(ctx, p.uuid);
            let uuid = p.uuid;
            let auto = ctx
                .get("auto_by_name")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            // premium (forwarded) players keep their own skin unless they chose another
            ctx.run_later(5, move |c| {
                let has = c.host.player_textures(uuid).is_some();
                let skin = match (&chosen, has) {
                    (Some(s), _) => s.clone(),
                    (None, false) if auto && valid_name(&p.name) => p.name.to_lowercase(),
                    _ => return,
                };
                apply(c, uuid, &skin);
            });
        })
        .every(10, |ctx| {
            let done: Vec<Done> =
                std::mem::take(&mut *DONE.lock().unwrap_or_else(|e| e.into_inner()));
            for (skin, r, to) in done {
                match r {
                    Ok((v, s)) => {
                        let mut c = ctx
                            .get("cache")
                            .and_then(|x| x.as_object().cloned())
                            .unwrap_or_default();
                        c.insert(
                            skin.clone(),
                            json::json!({"value": v, "signature": s, "fetched": hplug::now_ms()}),
                        );
                        let _ = ctx.set("cache", json::Value::Object(c));
                        for u in to {
                            ctx.set_player_skin(u, &v, if s.is_empty() { None } else { Some(&s) });
                        }
                    }
                    Err(e) => {
                        for u in to {
                            ctx.tell(u, format!("§cCould not get the skin '{skin}': {e}"));
                        }
                    }
                }
            }
        })
}
