# Porting a Paper plugin to Horizon: step-by-step (for humans and small AI models)

Work only inside `hplug/`. Do not edit Horizon's own crates. Build box: the iMac or fedora-1, never the 4 GB netcup VPS (the compile gets OOM-killed). Use `nice -n 19` and `HP_JOBS=4`.

## 0. Check the plugin is portable
```
python3 -I tools/scan_plugin.py /path/to/plugin-src-or.jar
```
Any `GAP` line is a blocker or needs a rewrite (see PAPER_MAPPING.md). No GAP + score < 20 = port it.

## 1. Scaffold
```
hplug/hp new myplugin        # creates hplug/plugins/myplugin with a working /myplugin command + test.script
hplug/hp test myplugin       # must print [PASS] before you change anything
```

## 2. Translate, one Java concept at a time
Open `hplug/plugins/myplugin/src/lib.rs`. For each thing in the Java class, use the table in PAPER_MAPPING.md. Keep the structure flat: one `Plugin::new("id")` builder chain. Rules:
1. Fields that survive a restart (`getConfig().set`) -> `ctx.set("key", json!(value))`; read with `ctx.get("key")`. Defaults go in the `unwrap_or(...)`.
2. Fields only used while running -> `static X: Mutex<..>` or `AtomicBool`. Never keep a `Ctx` or `Host`.
3. `sender.sendMessage` inside a command -> `ctx.reply`. Errors: `return Err("text".into())` (shown red).
4. Op-only commands: pass level 2 (default op) or higher. The test player `Tester` is op.
5. Cancel events by returning `true` (hplug closures) or `Flow::Cancel` (raw hooks).
6. If the table says RAW, add a small wrapper in `hplug/sdk/src/lib.rs` by copying `on_portal` / `player_portal` and changing the hook name and event fields (read the event struct in `crates/horizon-client/src/mod_api/generated.rs`: `grep -n "pub struct <Name>Event" -A8`).
7. If the table says GAP, stop and write down what is missing in the plugin's `NOTES.md`; do not fake it.

## 3. Test (always)
Edit `hplug/plugins/myplugin/test.script`, one op per line:
```
cmd endlock status         # sends /endlock status as the op player Tester
expect currently LOCKED    # a chat line containing this must arrive within 10 s
break 96 144 -32           # start-destroy a block (server runs creative in tests)
expect-pos 0.5 100 0.5 3   # position within 3 blocks of x y z within 10 s
expect-sign-editor          # the server opened a sign editor (after `use X Y Z` placed a sign, 10 s)
sign 6 100 0 [Warp]|dest||  # write the front of the sign being edited (SignUpdate); lines split on |
wait 2                     # keep the connection alive for 2 s
```
`hplug/hp test myplugin` builds, starts a throw-away server on port 25571, runs the script, prints `[PASS]/[FAIL]` and the server log path. Exit code 1 on failure. A first full build is ~10 min on an 8-thread Intel iMac; later builds that only touch plugins take seconds.

## 3b. Testing extras (all optional, all used by the ported plugins)
- `plugins/NAME/fixture/` is copied into the throw-away server folder before it starts: put files the plugin reads there (`config/hplug/NAME.json` for settings, `world/players/stats/<uuid>.json`, `usercache.json`). See `playtime` and `joinqueue`.
- Comment directives in a script: `# client: Guest` (log in under this name; default `Tester`, who is op) and `# expect-refused: TEXT` (the login must be refused with a reason containing TEXT; nothing else runs). A name starting with `.` plays a Bedrock player (`vcnotifier`).
- Several scripts per plugin: `test.script`, `test2.script`, ... run in that order against the same server.
- Ops are not special in tests except `Tester`: a command with level 2 is invisible ("unknown command") to any other name.
- Anything vanilla can do as an op can set the scene: `cmd setblock 0 100 0 minecraft:end_portal`, `cmd tp @s 0.5 100 0.5` (this is how the endlock portal test works). The client reports its position every tick and sends "player loaded", so portals, pressure plates etc. really fire.
- `fixture/server.toml.append` adds settings to the test server's server.toml (e.g. switch off a Horizon feature the plugin replaces).
- Time-based plugin behaviour: make the delay a config key with the original as default (`java-delay-ticks`, default 1400) and let the fixture shorten it.

## 4. Worked example: EndLock (Java 75 lines -> Rust 40 lines)
Java:
```java
getCommand("endlock").setExecutor(this);                       // plugin.yml: permission: endlock.admin, default: op
case "unlock" -> { locked = false; getConfig().set("locked", false); saveConfig();
                   sender.sendMessage("EndLock: End portal is now OPEN."); }
@EventHandler public void onPortalTeleport(PlayerTeleportEvent e) {
    if (!locked) return;
    if (e.getCause() != TeleportCause.END_PORTAL) return;
    e.setCancelled(true); e.getPlayer().sendMessage(...); }
```
Rust (`hplug/plugins/endlock/src/lib.rs`):
```rust
.command("endlock [action:word]", 2, |ctx, args| {            // 2 = op
    match args.get(0).map(|s| s.to_lowercase()).as_deref() {
        Some("unlock") => { ctx.set("locked", json!(false))?;  // saveConfig() is implicit
                            ctx.reply("EndLock: End portal is now OPEN."); }
        ... } Ok(()) })
.on_portal(|ctx, _from, to, cause| {                           // PlayerTeleportEvent
    if !locked(ctx) || cause != "end_portal" || to != "minecraft:the_end" { return false; }
    if let Some(p) = ctx.try_player().map(|p| p.uuid) { ctx.tell(p, "The End is not open yet..."); }
    true })                                                    // true = setCancelled(true)
```
Differences to remember: world names are dimension ids (`minecraft:the_end`, not `world_the_end`); the default of a missing config key lives in code; no tab-complete function (usage kinds do it).

## 4b. Things that bit the first four ports (read before you start)
1. **Chat interception needs the fork patch** (`patches/player-chat-hook.patch`, hook `player_chat`, `Plugin::on_chat`): without it mute/chat-filter plugins cannot work. Horizon already has its own `/mute` + chat filter (server.toml `[chat_filter]`); to test your own `/mute` set `enabled = false` in `fixture/server.toml.append`. Message rewriting (formatters) is not possible.
2. **`ctx.try_player()` can be None** in hooks that run without the player swapped in (portal, quit, death, login, tame): use `ctx.player_uuid()` and `ctx.tell(uuid, ..)` (hplug queues the message to the next tick if the game refuses it).
3. **Vanilla command names are taken** (`tell`, `msg`, `w`, `gamemode`, ...): your registration is dropped with a warning in the server log. Rename or drop it.
4. **Permissions are op levels**: `default: op` -> level 2; `default: true` -> 0; a LuckPerms-only node has no equivalent, port it as op-only and say so.
5. **Players are `u128` uuids**, written as 32 hex digits in JSON by hplug (`{:032x}`); the vanilla files use the dashed form (`usercache.json`); `ctx.uuid_for_name` returns the numeric form.
6. **Per-player/offline data**: Paper's `getOfflinePlayer` -> `ctx.uuid_for_name` (only players who joined before). Stats: `world_dir()/players/stats/<dashed uuid>.json` exists and has `minecraft:play_time` (Horizon writes these on disconnect with `minecraft:custom/minecraft:play_time` in ticks, checked on a live server 03/11/2026; the playtime test uses a fixture file of the same shape).
7. **Not portable at all**: Simple Voice Chat/Geyser/WorldGuard/GriefPrevention/Vault APIs, ProtocolLib, NMS, async tasks, `ServerListPingEvent`, per-player join-message suppression. `hp` scanner reports these as GAP.
8. **JVM-specific plugins are obsolete, not portable**: `gc-governor` (hooks Java GC beans) has no meaning on a Rust server. Mark it N/A.

## 4c. Running tests on a box that cannot compile (4 GB VPS)
Build elsewhere (Linux x86-64 + same glibc or older), copy `hplug-runner`, `horizon-server`, `hp-client` into one folder and the prepared `.local/horizon-client/26.3` next to the checkout, then:
`HP_BIN_DIR=/path/to/folder HP_NO_BUILD=1 systemd-run --user --scope -p MemoryMax=3500M -p CPUQuota=300% python3 -I hplug/hp test`. A slower box needs patience in scripts (`wait 8` before commands that need generated chunks).

## 5. When the compile fails
Read the FIRST `error[E....]`. Typical: (a) "no field named x on PlayerFooEvent": the event struct differs, `grep -n "pub struct PlayerFooEvent" -A10 horizon/crates/horizon-client/src/mod_api/generated.rs`; (b) "cannot construct non_exhaustive struct": you tried to build an event/player, don't; (c) closure must be `Send + Sync`: use `Mutex`/atomics, not `Rc`/`RefCell`.
