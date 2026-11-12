# Paper/Bukkit -> Horizon (hplug) mapping

Status: **OK** = `hplug` wrapper exists and was tested. **RAW** = Horizon hook exists (name in `native-hooks.json`, trait method in `crates/horizon-client/src/mod_api/generated.rs`), no wrapper yet: implement `Hooks::<hook>` yourself, or add a wrapper to `hplug/sdk/src/lib.rs` (copy `on_portal`, 12 lines). **PART** = works with limits. **GAP** = nothing equivalent today.
Tested = verified by `hp test` against a real horizon-server (03/11/2026).

| Paper / Bukkit | Horizon | Status |
|---|---|---|
| `plugin.yml commands:` + `onCommand` | `Plugin::command("name [arg:kind]", op_level, \|ctx,args\| ..)` | OK, tested |
| `TabCompleter` | automatic from the usage kinds (`word text integer number boolean player item block_pos`) | PART (no custom suggestions) |
| `plugin.yml permission: x default: op` | op level in `command()` (2 = op). `default: true` = 0 | PART (levels only, no nodes) |
| `sender.sendMessage(String)` | `ctx.reply(..)` inside a command; `§a`-style colour codes work (colours only) | OK, tested |
| `player.sendMessage` | `ctx.tell(uuid, ..)`. In hooks that run without the player swapped in (portal, quit, death, damage) the game refuses a direct tell; hplug then queues it for the next tick start | OK, tested |
| `Bukkit.broadcastMessage` | `ctx.broadcast(..)` | OK |
| Adventure/MiniMessage, colors | `Text::colored(s, 0xRRGGBB)`, `Text::literal`, `Text::tr` | PART (no MiniMessage parser) |
| `PlayerJoinEvent` | `Plugin::on_join` | OK, tested |
| `PlayerQuitEvent` | `Plugin::on_quit` (hook `player_logged_out`); `try_player()` may be None, use `ctx.player_uuid()` | OK (untested) |
| `BlockBreakEvent` | `Plugin::on_block_break(ctx,[x,y,z]) -> cancel` (hook `destroy_block`) | OK, tested |
| `PlayerTeleportEvent` portal causes | `Plugin::on_portal(ctx,from,to,cause) -> cancel` (hook `player_portal`; causes `nether_portal`, `end_portal`, `end_gateway`) | OK, tested live with a real End portal block (endlock) |
| `PlayerLoginEvent` / `AsyncPlayerPreLoginEvent` deny | `Plugin::on_login(ctx,uuid,name) -> Some(reason)`; the client sees an in-game disconnect with the reason | OK, tested (joinqueue) |
| `PlayerInteractEvent` (right click block) | `Plugin::on_interact(ctx,pos,face) -> cancel` | OK (untested) |
| `EntityDamageByEntityEvent` (player attacks) | `Plugin::on_attack(ctx,entity_id,type) -> cancel` | OK (untested) |
| `EntityDamageEvent` | `Plugin::on_damage(&Damage) -> cancel`. Host-less hook: **no Ctx, entity ids only** (cannot map a player victim to a uuid; upstream has a host-aware `living_incoming_damage_v2` fragment for mob victims only) | PART (untested) |
| `PlayerDeathEvent` | `Plugin::on_death(ctx, cause)` | OK (untested) |
| `PlayerDeathEvent#getDrops()` / `setKeepInventory` partly | `Plugin::on_death_drops(ctx, dimension, point, &mut drops)`: remove stacks to keep them (death grave), add to drop others. Needs the engine change of horizon-rust/horizon#8919 (PR #8935); not called with keep_inventory on. `ItemStack` keeps enchantments/names; put stacks in a container with `ctx.host.insert_item(pos, Direction::Up, stack)`, place blocks synchronously with `ctx.host.edit_blocks(dim, &NativeBlockEdit{..})` (palette = block state strings, runs = columns) | OK, tested (deathchest) |
| `EntityTameEvent` | `Plugin::on_tame(ctx,type,owner)` | OK (untested) |
| console commands (`sender` is console) | commands also run from the server console with `ctx.is_player() == false`; `ctx.try_player()` None; `ctx.sender_name()` "CONSOLE" | OK |
| `aliases:` | `.alias("pt")` after `.command(..)` | OK, tested |
| `BlockPlaceEvent` | hook `place_block` (cancel via `use_item_on`) | RAW |
| `PlayerInteractEvent` | hooks `right_click_block`, `left_click_block`, `use_item_on`, `use_item`, `interact_entity` | RAW |
| `PlayerDeathEvent` / respawn | hooks `player_died`, `player_respawned` | RAW |
| `BlockDropItemEvent`, `EntityDeathEvent#getDrops()`, `LootGenerateEvent` | `Plugin::on_loot(ctx, &Loot, &mut drops)`: every drop batch the engine makes (`Loot { table, dimension, origin, block, entity, player, tool, explosion }`); a dying player's inventory too (`entity` = `minecraft:player`) | OK (compiles 06/11, used by itemprovenance genesis, not run) |
| `PrepareItemCraftEvent` (`getMatrix`, `setResult`) | `Plugin::on_craft(ctx, table_pos, &matrix, &mut result)`; needs engine hook `item_craft_result` (patch) | OK (compiles, not run) |
| Chunky console commands | plugin `compatcmds` on Horizon's `/pregen` + `Host::pregen_status` (engine patch) | OK (not run) |
| `ClientboundMapItemDataPacket` / `MapRenderer` | `ctx.host.send_map(uuid, map_id, &colors_16384)` (merged upstream #9592) | OK (mapvideo) |
| `EntityDamage(ByEntity)Event` | hooks `living_incoming_damage`, `attack_entity` | RAW |
| chat (`AsyncChatEvent`, `AsyncPlayerChatEvent`): cancel a player's message | `Plugin::on_chat(&Chat) -> ChatVerdict::{Allow, Cancel(reply)}`. **Needs hook `player_chat`, which upstream lacks: added in the local fork, see `patches/player-chat-hook.patch`** (cancel + answer the sender only; the text cannot be rewritten because signed chat would stop verifying; host-less: keep state in a `static Mutex`) | OK in the fork, tested (mute); **GAP on stock Horizon** |
| `PlayerLoginEvent` / pre-login deny | hook `player_login` (cancel + reason) | RAW |
| `InventoryClickEvent` on own GUI | `host.open_menu(uuid, rows, title, items)` + hooks `menu_clicked`/`menu_closed` (vanilla chest look) | RAW |
| vanilla container events | hooks `container_opened`, `inventory_move_item`, `item_picked_up`, `item_dropped` | RAW |
| `runTaskTimer` / `runTaskLater` (sync) | `Plugin::every(ticks, f)` / `ctx.run_later(ticks, f)` | OK, tested (`every`) |
| async scheduler | none | GAP |
| `getConfig()` / `saveConfig()` | `ctx.get(key)` / `ctx.set(key, json)` -> `<server dir>/config/hplug/<id>.json` | OK, tested |
| YAML configs | convert to JSON by hand (serde_json is re-exported as `hplug::json`) | PART |
| `PersistentDataContainer` | `AttachmentType<T>` + `Holder::{Player,Entity,BlockEntity,Chunk,Level}` | RAW |
| `player.teleport(loc)` same dim | `ctx.teleport(name,x,y,z)` (queued `/tp`, runs next tick start) | OK, tested |
| teleport to other dimension | `ctx.console("execute in minecraft:the_nether run tp NAME x y z")` | PART |
| `dispatchCommand(console, ..)` | `ctx.console(..)` (next tick start, level 4) | OK |
| `world.getBlockAt().setType` | `host.set_block(..)`, `host.set_block_property(..)` | RAW |
| give items | `ctx.give(item_id, n)` (acting player) / `host.give_stack` | OK/RAW |
| `getOnlinePlayers()` | `ctx.online()` -> `Vec<NativeServerPlayer>` | OK, tested |
| `getOfflinePlayer(name)` for known names | `ctx.uuid_for_name(name)` (usercache.json) ; ops: `ctx.is_op(uuid)` (ops.json) | OK, tested |
| never-seen offline players | none | GAP |
| Horizon already ships `/mute`, `/unmute`, a chat filter and mute list (`[chat_filter]` in server.toml, on by default); a plugin that registers `/mute` is shadowed unless `chat_filter.enabled = false` | note | 
| overriding a vanilla command (`/tell`, `/msg`, `/w`, ...) | rejected with a warning ("a name vanilla has is left out"); use another name or the `player_command` hook to intercept | GAP |
| Simple Voice Chat API, Geyser API | none | GAP |
| `ServerListPingEvent` (max players / MOTD per ping) | none: use `server.toml` | GAP |
| Scoreboard/bossbar/title | `host.action_bar`; others: check `native-hooks.json` helpers `sidebar`, `set_display` | PART |
| `ProtocolLib` / packet listeners | none | GAP |
| NMS / CraftBukkit | none; rewrite against hooks | GAP |
| WorldGuard / WorldEdit / GriefPrevention / Vault / LuckPerms / PlaceholderAPI / CoreProtect APIs | none | GAP |
| BungeeCord plugin messaging, Velocity API | none (proxy stays Velocity; custom channel only to Horizon clients: `net_send_to_player`) | GAP |
| RCON | none in horizon-server (control API/MCP exists) | GAP |
| hot reload (`/reload`, PlugMan) | none: rebuild engine, restart (`hp build`, ~10-60 s relink on fast profile) | GAP |
| sandboxing | none: plugins are compiled into the engine, full process rights | GAP (by design) |

## Raw API cheat sheet
`Hooks` methods all have the shape `fn <hook>(&self, host: &mut dyn Host, event: &mut XEvent<'_>) [-> Flow]`; return `Flow::Cancel` to take/cancel. Events are `#[non_exhaustive]`: read fields, never construct them. Player-state caveat: in some hooks the player is "swapped in" so `host.acting_player()`, `host.feedback`, `host.give_items` work; in others use uuid-based helpers (`host.tell(uuid, ..)`, `host.give_stack(uuid, ..)`). `host.run_command` is refused inside a player's hook: queue it (hplug does this for you).

## Added 03/11/2026 (Wave 0)
| Paper | hplug |
|---|---|
| `runTaskLater` (returns BukkitTask) | `ctx.run_later(ticks, f) -> TaskId` |
| `runTaskTimer(plugin, f, delay, period)` | `ctx.run_repeating(delay, period, f) -> TaskId` (a task may cancel itself) |
| `task.cancel()` | `ctx.cancel(id) -> bool` |
| `player.hasPermission("a.b")` | `ctx.has_perm(uuid, name, "a.b")`: ops always; others via `<config_dir>/hplug/permissions.json` `{"default":[..],"players":{"<uuid-with-dashes or lowercase name>":["x.*"]}}`, `*` and `prefix.*` wildcards |
Tests: `plugins/hello/test2.script` + `fixture/config/hplug/permissions.json`. Not async: tasks run on the server thread at tick start (Paper's async tasks have no equivalent; use a std thread that never touches `Host`).
