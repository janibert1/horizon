//! `hplug`: a small facade for writing Horizon server plugins in the shape Paper plugins have.
//!
//! ```ignore
//! pub fn plugin() -> hplug::Plugin {
//!     hplug::Plugin::new("hello")
//!         .command("hello [name:word]", 0, |ctx, args| {
//!             ctx.reply(format!("Hello {}!", args.get(0).unwrap_or(&ctx.sender_name())));
//!             Ok(())
//!         })
//!         .on_join(|ctx| { let u = ctx.player().uuid; ctx.tell(u, "welcome"); })
//! }
//! ```
//! Everything here is a thin layer over `horizon_client::mod_api::{Hooks, Host}`; the raw API stays
//! reachable through [`Ctx::host`]. Mapping from Paper: see PAPER_MAPPING.md.
use horizon_client::mod_api::{
    AttackEntityEvent, BlockBurnEvent, BlockIgniteEvent, ContainerOpenedEvent,
    CustomPayloadFromClientEvent, DestroyBlockEvent, EntityTameEvent, ExperienceOrbPickupEvent,
    ExplosionDetonateEvent, FillBucketEvent, Flow, Hooks, Host, InteractEntityEvent,
    ItemCraftResultEvent, ItemPickedUpEvent, LeftClickBlockEvent, LevelDimension, LevelHost,
    LivingIncomingDamageEvent, LootDropsEvent, MenuClickedEvent, MenuClosedEvent,
    MerchantOfferTradedEvent, NativeCommand, NativeServerPlayer, PerformCommandEvent,
    PerformConsoleCommandEvent, PlayerAdvancementDoneEvent, PlayerChatEvent, PlayerCommandEvent,
    PlayerDiedEvent, PlayerLoggedInEvent, PlayerLoggedOutEvent, PlayerLoginEvent,
    PlayerPortalEvent, PlayerTeleportEvent, RegisterCommandsEvent, RightClickBlockEvent,
    ServerTickEvent, ServerTickPhase, SignChangedEvent, Text, UseItemEvent, UseItemOnEvent,
    serde_json::{self, Map, Value},
};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Handle of a scheduled task, for [`Ctx::cancel`]. Paper: `BukkitTask#getTaskId`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TaskId(pub u64);

struct Task {
    at: u64,
    id: u64,
    period: Option<u64>,
    run: Arc<dyn Fn(&mut Ctx) + Send + Sync>,
}

pub use horizon_client::mod_api::{
    Direction, EquipmentSlot, ItemStack, MerchantOffer, NativeBlockEdit, NativeBlockRun,
    NativeHeightmap as Heightmap, NativeServerPlayer as Player, Text as Message,
    serde_json as json,
};

/// Result of a command handler. `Err(msg)` is shown to the sender in red.
pub type CmdResult = Result<(), String>;

/// Words after the command name, as typed.
pub struct Args(pub Vec<String>);
impl Args {
    pub fn get(&self, i: usize) -> Option<&String> {
        self.0.get(i)
    }
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn int(&self, i: usize) -> Result<i64, String> {
        self.get(i)
            .ok_or_else(|| format!("missing argument #{}", i + 1))?
            .parse()
            .map_err(|_| format!("argument #{} must be an integer", i + 1))
    }
    pub fn rest(&self, from: usize) -> String {
        self.0
            .iter()
            .skip(from)
            .cloned()
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// Facts about damage, for [`Plugin::on_damage`]. That hook has no `Host`, so no `Ctx`.
pub struct Damage<'a> {
    pub entity: i32,
    pub kind: &'a str,
    pub damage_type: &'a str,
    pub attacker: i32,
    pub amount: f32,
    /// The victim as a player (uuid), when `entity` is an online player. hplug keeps an entity-id -> player
    /// table (`NativeServerPlayer::entity_id`), refreshed on join, quit and every 100 ticks.
    pub victim_player: Option<u128>,
    /// The attacker as a player, when `attacker` is an online player (PvP, a player's arrow).
    pub attacker_player: Option<u128>,
    shared: &'a Mutex<Shared>,
}
impl Damage<'_> {
    /// Message to an online player; this hook has no `Host`, so it is delivered at the next tick start.
    pub fn tell(&self, uuid: u128, msg: impl AsRef<str>) {
        self.shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .tells
            .push_back((uuid, msg.as_ref().to_owned()));
    }
}

type CmdFn = Box<dyn Fn(&mut Ctx, &Args) -> CmdResult + Send + Sync>;
type JoinFn = Box<dyn Fn(&mut Ctx) + Send + Sync>;
type BreakFn = Box<dyn Fn(&mut Ctx, [i32; 3]) -> bool + Send + Sync>; // true = cancel
type PickupFn = Box<dyn Fn(&mut Ctx, &str, u32) -> bool + Send + Sync>; // true = cancel
type SignFn = Box<dyn Fn(&mut Ctx, &str, [i32; 3], &mut Vec<String>) -> bool + Send + Sync>; // true = cancel
type TickFn = Box<dyn Fn(&mut Ctx) + Send + Sync>;
/// (from_dimension, to_dimension, cause) -> true to cancel
type PortalFn = Box<dyn Fn(&mut Ctx, &str, &str, &str) -> bool + Send + Sync>;
type InteractFn = Box<dyn Fn(&mut Ctx, [i32; 3], [i32; 3]) -> bool + Send + Sync>;
type AttackFn = Box<dyn Fn(&mut Ctx, i32, &str) -> bool + Send + Sync>;
type DamageFn = Box<dyn Fn(&Damage) -> bool + Send + Sync>;
type TameFn = Box<dyn Fn(&mut Ctx, &str, Option<u128>) + Send + Sync>;
/// (uuid, name) -> Some(reason) to refuse the login
type LoginFn = Box<dyn Fn(&mut Ctx, u128, &str) -> Option<String> + Send + Sync>;
/// Player actions a plugin can refuse with [`Plugin::on_action`] (Paper: the matching `PlayerInteractEvent`,
/// `PlayerInteractEntityEvent`, `InventoryOpenEvent`; block break/place have their own handlers).
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// Right click with an item on a block (also what places a block).
    UseItemOn {
        pos: [i32; 3],
        face: [i32; 3],
        item: String,
    },
    /// Right click with an item in the air or an item's use.
    UseItem { item: String },
    /// A bucket is filled at `pos` (water/lava picked up).
    FillBucket { pos: [i32; 3] },
    /// Right click on an entity (`entity` id, `kind` like `minecraft:villager`).
    InteractEntity { entity: i32, kind: String },
    /// Left click on a block (start of breaking).
    LeftClickBlock { pos: [i32; 3] },
    /// A container (`kind`) opens for the player.
    ContainerOpen { kind: String },
}
/// (dimension, center, blocks the blast would destroy: remove entries to protect them)
type ExplosionFn = Box<dyn Fn(&mut Ctx, &str, [f64; 3], &mut Vec<[i32; 3]>) + Send + Sync>;
type ActionFn = Box<dyn Fn(&mut Ctx, &Action) -> bool + Send + Sync>;
/// (raw command line without slash) -> `Some(reply)` to cancel it, the reply goes to the player ("" = silent)
type CommandFilterFn = Box<dyn Fn(&mut Ctx, &str, &str) -> Option<String> + Send + Sync>;
/// What [`Plugin::on_chat`] decides.
pub enum ChatVerdict {
    Allow,
    /// Drop the message for everybody; the optional text goes to the sender (`§` colours work).
    Cancel(Option<String>),
}
/// A chat message, for [`Plugin::on_chat`]. That hook has no `Host`, so no `Ctx`.
pub struct Chat<'a> {
    pub player: u128,
    pub name: &'a str,
    pub message: &'a str,
}
type ChatFn = Box<dyn Fn(&Chat) -> ChatVerdict + Send + Sync>;
type MenuClickFn = Box<dyn Fn(&mut Ctx, i32, i32, i32, i32) + Send + Sync>;
type MenuCloseFn = Box<dyn Fn(&mut Ctx, i32) + Send + Sync>;
type AdvancementFn = Box<dyn Fn(&mut Ctx, &str) + Send + Sync>;
type TradeOfferFn = Box<dyn Fn(&mut Ctx, &str, &mut MerchantOffer) + Send + Sync>;
type DeathDropsFn = Box<dyn Fn(&mut Ctx, &str, [f64; 3], &mut Vec<ItemStack>) + Send + Sync>;
type DeathFn = Box<dyn Fn(&mut Ctx, Option<&str>) + Send + Sync>;
type LootFn = Box<dyn Fn(&mut Ctx, &Loot, &mut Vec<ItemStack>) + Send + Sync>;
/// A teleport about to happen, for [`Plugin::on_teleport`].
pub struct Teleport<'a> {
    pub player: u128,
    pub name: &'a str,
    pub from: [f64; 3],
    pub to: [f64; 3],
    pub from_dimension: &'a str,
    pub to_dimension: &'a str,
    /// `command` (`/tp`, `/teleport`, also hplug's own `ctx.teleport`) or `spectate` (spectator menu).
    pub cause: &'a str,
}
type TeleportFn = Box<dyn Fn(&mut TeleportCtx, &Teleport) -> bool + Send + Sync>;
/// Fire, for [`Plugin::on_fire`]. These hooks run inside the level's tick with no `Host`, so no `Ctx`.
pub struct Fire<'a> {
    /// `minecraft:overworld`, `minecraft:the_nether`, `minecraft:the_end`.
    pub dimension: &'static str,
    pub pos: [i32; 3],
    /// `burn` (a burning block is about to be destroyed) or the ignite cause (`spread`, `lava`, `flint_and_steel`,
    /// `lightning`, `fireball`, ... as the engine names it).
    pub cause: &'a str,
    /// The player who lit it, when there is one.
    pub player: Option<u128>,
}
type FireFn = Box<dyn Fn(&Fire) -> bool + Send + Sync>;
fn dim_id(d: LevelDimension) -> &'static str {
    match d {
        LevelDimension::Nether => "minecraft:the_nether",
        LevelDimension::End => "minecraft:the_end",
        _ => "minecraft:overworld",
    }
}
type PluginMessageFn = Box<dyn Fn(&mut Ctx, &str, &[u8]) + Send + Sync>;
/// (table position or None for the inventory grid, grid slots row by row, result to change)
type CraftFn = Box<
    dyn Fn(&mut Ctx, Option<[i32; 3]>, &[Option<ItemStack>], &mut Option<ItemStack>) + Send + Sync,
>;

/// Where a batch of drops comes from, for [`Plugin::on_loot`] (the engine's `NativeLootDrops`).
#[derive(Clone, Debug)]
pub struct Loot {
    /// The loot table (`minecraft:blocks/diamond_ore`, `minecraft:entities/zombie`, `minecraft:chests/...`);
    /// empty for a dying player's inventory and a leashed mob's lead.
    pub table: String,
    pub dimension: String,
    pub origin: [f64; 3],
    /// A broken block's position.
    pub block: Option<[i32; 3]>,
    /// The entity whose drops these are: `(id, kind)` (`minecraft:player` for a dying player).
    pub entity: Option<(u32, String)>,
    /// The player who caused it (who broke the block, killed the mob, opened the chest) or the dying player.
    pub player: Option<u128>,
    /// The tool used (a block broken by a player), item id.
    pub tool: Option<String>,
    /// Blown up (explosion radius).
    pub explosion: Option<f32>,
}

struct Command {
    names: Vec<String>,
    usages: Vec<String>,
    level: u8,
    run: CmdFn,
}

#[derive(Default)]
struct Shared {
    console: VecDeque<String>,
    tells: VecDeque<(u128, String)>,
    later: Vec<Task>,
    next_task: u64,
    tick: u64,
    players: Vec<(i32, u128)>,
    /// last tick's full player records: hooks that run while the server's own player list is busy (typed commands) read these
    cache: Vec<NativeServerPlayer>,
    config_dir: Option<PathBuf>,
    store: Option<Map<String, Value>>,
}

pub struct Plugin {
    id: String,
    commands: Vec<Command>,
    join: Vec<JoinFn>,
    quit: Vec<JoinFn>,
    brk: Vec<BreakFn>,
    sign: Vec<SignFn>,
    pickup: Vec<PickupFn>,
    portal: Vec<PortalFn>,
    interact: Vec<InteractFn>,
    attack: Vec<AttackFn>,
    damage: Vec<DamageFn>,
    tame: Vec<TameFn>,
    login: Vec<LoginFn>,
    death: Vec<DeathFn>,
    death_drops: Vec<DeathDropsFn>,
    loot: Vec<LootFn>,
    craft: Vec<CraftFn>,
    teleport: Vec<TeleportFn>,
    fire: Vec<FireFn>,
    plugin_msg: Vec<PluginMessageFn>,
    trade_offer: Vec<TradeOfferFn>,
    advancement: Vec<AdvancementFn>,
    menu_click: Vec<MenuClickFn>,
    menu_close: Vec<MenuCloseFn>,
    chat: Vec<ChatFn>,
    action: Vec<ActionFn>,
    explosion: Vec<ExplosionFn>,
    cmd_filter: Vec<CommandFilterFn>,
    every: Vec<(u64, TickFn)>,
    shared: Mutex<Shared>,
}

impl Plugin {
    pub fn new(id: &str) -> Self {
        Self {
            id: id.into(),
            commands: vec![],
            join: vec![],
            quit: vec![],
            brk: vec![],
            sign: vec![],
            pickup: vec![],
            portal: vec![],
            interact: vec![],
            attack: vec![],
            damage: vec![],
            tame: vec![],
            login: vec![],
            death: vec![],
            death_drops: vec![],
            loot: vec![],
            craft: vec![],
            teleport: vec![],
            fire: vec![],
            plugin_msg: vec![],
            trade_offer: vec![],
            advancement: vec![],
            menu_click: vec![],
            menu_close: vec![],
            chat: vec![],
            action: vec![],
            explosion: vec![],
            cmd_filter: vec![],
            every: vec![],
            shared: Default::default(),
        }
    }
    /// `usage` is `"name <arg:kind> [opt:kind]"`; kinds: word text integer number boolean player item block_pos.
    /// `level` is the op level needed (0 = everyone, 2 = ops, as `default: op`). Paper: `plugin.yml commands:` + `onCommand`.
    /// The console can always run it (level 4); then `ctx.try_player()` is None.
    pub fn command(
        mut self,
        usage: &str,
        level: u8,
        run: impl Fn(&mut Ctx, &Args) -> CmdResult + Send + Sync + 'static,
    ) -> Self {
        let name = usage.split_whitespace().next().unwrap_or("").to_owned();
        self.commands.push(Command {
            names: vec![name],
            usages: vec![usage.into()],
            level,
            run: Box::new(run),
        });
        self
    }
    /// Second usage line for the last command, e.g. `.also("unmute <player:player>")`-style overloads
    /// with the SAME first word (different argument shapes).
    pub fn usage(mut self, usage: &str) -> Self {
        if let Some(c) = self.commands.last_mut() {
            c.usages.push(usage.into());
        }
        self
    }
    /// Paper: `aliases: [pt]` in plugin.yml. Applies to the last `command`.
    pub fn alias(mut self, name: &str) -> Self {
        if let Some(c) = self.commands.last_mut() {
            let base = c.usages[0].clone();
            let rest = base.split_once(' ').map(|x| x.1).unwrap_or("");
            c.usages.push(format!("{name} {rest}").trim().to_owned());
            c.names.push(name.into());
        }
        self
    }
    /// Paper: `PlayerJoinEvent`.
    pub fn on_join(mut self, f: impl Fn(&mut Ctx) + Send + Sync + 'static) -> Self {
        self.join.push(Box::new(f));
        self
    }
    /// Paper: `PlayerQuitEvent`. The player may already be gone: use `try_player()`.
    pub fn on_quit(mut self, f: impl Fn(&mut Ctx) + Send + Sync + 'static) -> Self {
        self.quit.push(Box::new(f));
        self
    }
    /// Paper: `PlayerLoginEvent` / `AsyncPlayerPreLoginEvent`. Return `Some(reason)` to refuse. The player
    /// is not placed yet: no `try_player()`, you get `(uuid, name)`. Vanilla's own checks (whitelist,
    /// bans, full server) already ran. Tell nobody from here: the reason is the message.
    pub fn on_login(
        mut self,
        f: impl Fn(&mut Ctx, u128, &str) -> Option<String> + Send + Sync + 'static,
    ) -> Self {
        self.login.push(Box::new(f));
        self
    }
    /// Paper: `AsyncChatEvent` / `AsyncPlayerChatEvent` (cancel only; the text cannot be rewritten, a signed
    /// message would stop verifying). Needs the `player_chat` hook, which is NOT in upstream Horizon yet: it
    /// is in the local fork patch `patches/player-chat-hook.patch`. Host-less: keep your own state in a
    /// `static Mutex`, filled from other handlers (join, commands).
    pub fn on_chat(mut self, f: impl Fn(&Chat) -> ChatVerdict + Send + Sync + 'static) -> Self {
        self.chat.push(Box::new(f));
        self
    }
    /// Refuse player actions (return true to cancel): see [`Action`]. Paper: cancelling `PlayerInteractEvent` etc.
    pub fn on_action(
        mut self,
        f: impl Fn(&mut Ctx, &Action) -> bool + Send + Sync + 'static,
    ) -> Self {
        self.action.push(Box::new(f));
        self
    }
    /// Paper: `EntityExplodeEvent`/`BlockExplodeEvent` (`blockList().remove(..)`): remove positions from the
    /// list to keep those blocks. Fires for every explosion (creeper, TNT, bed, ...).
    pub fn on_explosion(
        mut self,
        f: impl Fn(&mut Ctx, &str, [f64; 3], &mut Vec<[i32; 3]>) + Send + Sync + 'static,
    ) -> Self {
        self.explosion.push(Box::new(f));
        self
    }
    /// Look at every command a player types (vanilla, plugin or unknown) before it runs: `(ctx, player_name, line)`.
    /// The server's player list is busy then, so `ctx.try_player()` is the record from the start of the tick. Return
    /// `Some(text)` to cancel it and tell the player `text` (empty: silently). Paper:
    /// `PlayerCommandPreprocessEvent`. The raw line (with arguments) is yours: do not log it.
    pub fn on_command_line(
        mut self,
        f: impl Fn(&mut Ctx, &str, &str) -> Option<String> + Send + Sync + 'static,
    ) -> Self {
        self.cmd_filter.push(Box::new(f));
        self
    }
    /// Paper: `PlayerDeathEvent`. `cause` is the damage type id when known.
    pub fn on_death(mut self, f: impl Fn(&mut Ctx, Option<&str>) + Send + Sync + 'static) -> Self {
        self.death.push(Box::new(f));
        self
    }
    /// Paper: `PlayerDeathEvent#getDrops()`. `(ctx, dimension, drop_point, drops)`: `ctx.player()` is the dying player; remove
    /// stacks from `drops` to keep them (a grave, soulbound items), add or change stacks to drop others. Not
    /// called when `keep_inventory` is on. Needs the engine hook of horizon-rust/horizon#8919.
    pub fn on_death_drops(
        mut self,
        f: impl Fn(&mut Ctx, &str, [f64; 3], &mut Vec<ItemStack>) + Send + Sync + 'static,
    ) -> Self {
        self.death_drops.push(Box::new(f));
        self
    }
    /// Every batch of drops the engine makes from a loot table or a dying entity (Paper: `BlockDropItemEvent`,
    /// `EntityDeathEvent#getDrops`, `LootGenerateEvent`): `(ctx, where_from, drops)`; change, add or remove stacks.
    /// A dying player's inventory comes here too (`entity` is `minecraft:player`), after [`Plugin::on_death_drops`].
    /// `ctx.player_uuid()` is `loot.player`; `ctx.try_player()` is that player's record when they are online.
    pub fn on_loot(
        mut self,
        f: impl Fn(&mut Ctx, &Loot, &mut Vec<ItemStack>) + Send + Sync + 'static,
    ) -> Self {
        self.loot.push(Box::new(f));
        self
    }
    /// Paper: `PrepareItemCraftEvent` (`getInventory().getMatrix()` / `setResult`): `(ctx, table_pos, matrix, result)`
    /// after vanilla matched a recipe (or found none): `matrix` is the 4 inventory-grid or 9 table slots, row by row;
    /// change, replace or clear `result`, which is what the player sees and takes. Heard on every grid change.
    /// `ctx.player()` is the crafting player (record from the start of the tick when the live list is busy).
    /// A shift-click that crafts several times re-matches inside the click: only the first result went through here.
    /// Needs the engine hook `item_craft_result` (engine-patches/item-craft-result.patch).
    pub fn on_craft(
        mut self,
        f: impl Fn(&mut Ctx, Option<[i32; 3]>, &[Option<ItemStack>], &mut Option<ItemStack>)
        + Send
        + Sync
        + 'static,
    ) -> Self {
        self.craft.push(Box::new(f));
        self
    }
    /// Paper: `PlayerTeleportEvent` (cancellable) for command and spectator-menu teleports. Return true to keep the
    /// player where they are. Portals ([`Plugin::on_portal`]), pearls and respawns are not heard here.
    /// Uses upstream `player_teleport`; its Host-free callback receives a [`TeleportCtx`].
    pub fn on_teleport(
        mut self,
        f: impl Fn(&mut TeleportCtx, &Teleport) -> bool + Send + Sync + 'static,
    ) -> Self {
        self.teleport.push(Box::new(f));
        self
    }
    /// Paper: `BlockIgniteEvent` + `BlockBurnEvent`: return true to stop the fire there (no fire placed / the block
    /// does not burn away). Host-less (inside the level tick): keep what you need in a `static`, filled from a tick.
    pub fn on_fire(mut self, f: impl Fn(&Fire) -> bool + Send + Sync + 'static) -> Self {
        self.fire.push(Box::new(f));
        self
    }
    /// Paper: `PluginMessageListener.onPluginMessageReceived`: `(ctx, channel, data)` from any client (Fabric mods'
    /// channels, `minecraft:register`). `ctx.player_uuid()` is the sender; no player state is swapped in, so Host calls
    /// that need "no player" work here. Answer with `ctx.host.send_custom_payload_to_player(uuid, channel, &bytes)`.
    /// Needs the engine hook `plugin_message`.
    pub fn on_plugin_message(
        mut self,
        f: impl Fn(&mut Ctx, &str, &[u8]) + Send + Sync + 'static,
    ) -> Self {
        self.plugin_msg.push(Box::new(f));
        self
    }
    /// Paper: `PlayerTradeEvent#getTrade()` / `VillagerAcquireTradeEvent` (mutable recipe): after each trade
    /// with a villager or wandering trader, `(ctx, trader_kind, offer)` where `offer` is the LIVE offer in the
    /// state the trade left it in (`uses` counted); change `uses`, `max_uses`, `demand`, `special_price_diff`,
    /// `price_multiplier`, `xp`. `ctx` has no player. Needs the engine hook `merchant_offer_traded`.
    pub fn on_trade_offer(
        mut self,
        f: impl Fn(&mut Ctx, &str, &mut MerchantOffer) + Send + Sync + 'static,
    ) -> Self {
        self.trade_offer.push(Box::new(f));
        self
    }
    /// Paper: `PlayerAdvancementDoneEvent`: `(ctx, advancement_id)` after the player completed an advancement
    /// (`minecraft:story/mine_stone`). The event has no frame (task/goal/challenge): look the id up yourself. In this
    /// hook the player is not swapped in: use `ctx.player_uuid()` and `ctx.tell`.
    pub fn on_advancement(mut self, f: impl Fn(&mut Ctx, &str) + Send + Sync + 'static) -> Self {
        self.advancement.push(Box::new(f));
        self
    }
    /// Paper: `InventoryClickEvent` on a menu this plugin opened with `ctx.host.open_menu(uuid, rows, title, items)`:
    /// `(ctx, window, slot, button, mode)`. `slot` counts the menu's own slots first, then the player's inventory
    /// (-999 outside); `mode` is `ClickType`'s id. The click moved nothing (the menu is display-only): answer with
    /// `ctx.host.set_menu_items(uuid, items)` or `exchange_items`. `ctx.player_uuid()` is the clicker; their
    /// state is NOT swapped in. Heard for every plugin's menus: check `window` against what `open_menu` returned.
    pub fn on_menu_click(
        mut self,
        f: impl Fn(&mut Ctx, i32, i32, i32, i32) + Send + Sync + 'static,
    ) -> Self {
        self.menu_click.push(Box::new(f));
        self
    }
    /// Paper: `InventoryCloseEvent` for a menu opened with `open_menu`: `(ctx, window)`.
    pub fn on_menu_close(mut self, f: impl Fn(&mut Ctx, i32) + Send + Sync + 'static) -> Self {
        self.menu_close.push(Box::new(f));
        self
    }
    /// Paper: `BlockBreakEvent`. Return true to cancel the break.
    pub fn on_block_break(
        mut self,
        f: impl Fn(&mut Ctx, [i32; 3]) -> bool + Send + Sync + 'static,
    ) -> Self {
        self.brk.push(Box::new(f));
        self
    }
    /// Paper: `EntityPickupItemEvent` / `PlayerPickupExperienceEvent`: a player is about to take `count` of `item`
    /// from the ground (an experience orb is `minecraft:experience_orb`). Return true to leave it there.
    pub fn on_pickup(
        mut self,
        f: impl Fn(&mut Ctx, &str, u32) -> bool + Send + Sync + 'static,
    ) -> Self {
        self.pickup.push(Box::new(f));
        self
    }
    /// Paper: `SignChangeEvent`: a player finished editing a sign (`(ctx, dimension, pos, lines)`, the side's four
    /// lines as plain text). Rewrite `lines` to change what is stored; return true to cancel the edit.
    pub fn on_sign_change(
        mut self,
        f: impl Fn(&mut Ctx, &str, [i32; 3], &mut Vec<String>) -> bool + Send + Sync + 'static,
    ) -> Self {
        self.sign.push(Box::new(f));
        self
    }
    /// Paper: `PlayerInteractEvent` with `RIGHT_CLICK_BLOCK` (hook `right_click_block`): `(pos, face)`.
    /// Return true to cancel. Left click and air clicks are separate hooks (`left_click_block`, `use_item`).
    pub fn on_interact(
        mut self,
        f: impl Fn(&mut Ctx, [i32; 3], [i32; 3]) -> bool + Send + Sync + 'static,
    ) -> Self {
        self.interact.push(Box::new(f));
        self
    }
    /// Paper: `EntityDamageByEntityEvent` where the damager is the acting player (hook `attack_entity`):
    /// `(entity_id, entity_type)`. Return true to cancel the attack.
    pub fn on_attack(
        mut self,
        f: impl Fn(&mut Ctx, i32, &str) -> bool + Send + Sync + 'static,
    ) -> Self {
        self.attack.push(Box::new(f));
        self
    }
    /// Paper: `EntityDamageEvent` for living entities (hook `living_incoming_damage`). This hook has no
    /// `Host`, so the closure only sees [`Damage`]. Return true to cancel the damage.
    pub fn on_damage(mut self, f: impl Fn(&Damage) -> bool + Send + Sync + 'static) -> Self {
        self.damage.push(Box::new(f));
        self
    }
    /// Paper: `EntityTameEvent`: `(entity_type, owner_uuid)`.
    pub fn on_tame(
        mut self,
        f: impl Fn(&mut Ctx, &str, Option<u128>) + Send + Sync + 'static,
    ) -> Self {
        self.tame.push(Box::new(f));
        self
    }
    /// Paper: `PlayerTeleportEvent` with cause END_PORTAL/NETHER_PORTAL (and `PlayerPortalEvent`).
    /// `cause` is `nether_portal`, `end_portal` or `end_gateway`; dimensions are ids such as
    /// `minecraft:the_end`. Return true to cancel the travel.
    pub fn on_portal(
        mut self,
        f: impl Fn(&mut Ctx, &str, &str, &str) -> bool + Send + Sync + 'static,
    ) -> Self {
        self.portal.push(Box::new(f));
        self
    }
    /// Paper: `runTaskTimer(plugin, f, 0, ticks)`. Runs at the start of every `ticks`-th server tick.
    pub fn every(mut self, ticks: u64, f: impl Fn(&mut Ctx) + Send + Sync + 'static) -> Self {
        self.every.push((ticks.max(1), Box::new(f)));
        self
    }
    pub fn id(&self) -> &str {
        &self.id
    }
    fn pickup_flow(&self, host: &mut dyn Host, player: u128, item: &str, count: u32) -> Flow {
        if self.pickup.is_empty() {
            return Flow::Continue;
        }
        let p = host.server_players().into_iter().find(|p| p.uuid == player);
        let mut cancel = false;
        for f in &self.pickup {
            let mut ctx = Ctx {
                host,
                uuid: Some(player),
                player: p.clone(),
                plugin_id: &self.id,
                shared: &self.shared,
            };
            cancel |= f(&mut ctx, item, count);
        }
        if cancel { Flow::Cancel } else { Flow::Continue }
    }
    /// `line` names one of this plugin's commands (directly or as `execute as <player> run <cmd>`).
    fn claims(&self, line: &str) -> bool {
        let line = execute_as(line).map(|(_, rest)| rest).unwrap_or(line);
        line.split_whitespace()
            .next()
            .is_some_and(|n| self.find(n.trim_start_matches('/')).is_some())
    }
    /// Run a console line when it is this plugin's command (as the named player for `execute as <p> run ..`).
    fn run_relayed(&self, host: &mut dyn Host, line: &str) -> bool {
        if !self.claims(line) {
            return false;
        }
        match execute_as(line) {
            Some((who, rest)) => {
                let Some(p) = host
                    .server_players()
                    .into_iter()
                    .find(|p| p.name.eq_ignore_ascii_case(who))
                else {
                    return true;
                };
                self.run_command(host, Some(p), rest.trim_start_matches('/'))
            }
            None => self.run_command(host, None, line.trim_start_matches('/')),
        }
    }
    fn find(&self, name: &str) -> Option<&Command> {
        self.commands
            .iter()
            .find(|c| c.names.iter().any(|n| n.eq_ignore_ascii_case(name)))
    }
    fn run_command(
        &self,
        host: &mut dyn Host,
        player: Option<NativeServerPlayer>,
        line: &str,
    ) -> bool {
        let mut words = line.split_whitespace();
        let Some(name) = words.next() else {
            return false;
        };
        let Some(cmd) = self.find(name) else {
            return false;
        };
        let args = Args(words.map(str::to_owned).collect());
        let mut ctx = Ctx {
            host,
            uuid: player.as_ref().map(|p| p.uuid),
            player,
            plugin_id: &self.id,
            shared: &self.shared,
        };
        // A panic in a plugin must not take the whole server down (it did: `ctx.player()` from the console).
        let r =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (cmd.run)(&mut ctx, &args)));
        match r {
            Ok(Err(e)) => ctx.host.feedback(if e.contains('§') {
                legacy(&e)
            } else {
                Text::colored(e, 0xFF5555)
            }),
            Ok(Ok(())) => {}
            Err(p) => {
                let msg = p
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()))
                    .unwrap_or_default();
                eprintln!("[hplug:{}] command /{name} panicked: {msg}", self.id);
                ctx.host.feedback(Text::colored(
                    "That command failed (plugin error, see the server log).",
                    0xFF5555,
                ));
            }
        }
        true
    }
}

/// `§a`-style legacy colour codes (Paper plugins use them a lot) to a [`Text`]. Only colours are
/// kept (0-9, a-f); `§l §o §n §m §k` and `§r` end the current colour run.
pub fn legacy(s: &str) -> Text {
    if !s.contains('§') {
        return Text::literal(s);
    }
    const COLORS: [u32; 16] = [
        0x000000, 0x0000AA, 0x00AA00, 0x00AAAA, 0xAA0000, 0xAA00AA, 0xFFAA00, 0xAAAAAA, 0x555555,
        0x5555FF, 0x55FF55, 0x55FFFF, 0xFF5555, 0xFF55FF, 0xFFFF55, 0xFFFFFF,
    ];
    let mut out = Text::empty();
    let (mut run, mut color): (String, Option<u32>) = (String::new(), None);
    let flush = |out: &mut Text, run: &mut String, color: Option<u32>| {
        if !run.is_empty() {
            let t = match color {
                Some(c) => Text::colored(std::mem::take(run), c),
                None => Text::literal(std::mem::take(run)),
            };
            out.push(t);
        }
    };
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '§' {
            if let Some(code) = chars.next() {
                flush(&mut out, &mut run, color);
                color = code.to_digit(16).map(|d| COLORS[d as usize]);
            }
        } else {
            run.push(c);
        }
    }
    flush(&mut out, &mut run, color);
    out
}

/// Where a permission is asked (LuckPerms contexts) and when (temporary nodes). Set by [`Ctx::has_perm`] and the
/// prefix lookup for the player asked about; `world` is the Bukkit-style world name or dimension id.
#[derive(Clone, Debug, Default)]
pub struct PermEnv {
    pub world: Option<String>,
    pub server: String,
    pub now_secs: u64,
}
thread_local! {
    static PERM_ENV: std::cell::RefCell<Option<PermEnv>> = const { std::cell::RefCell::new(None) };
}
/// Evaluate `f` with `env` as the permission context (nested calls keep the outer one afterwards).
pub fn with_perm_env<R>(env: PermEnv, f: impl FnOnce() -> R) -> R {
    let old = PERM_ENV.with(|e| e.replace(Some(env)));
    let r = f();
    PERM_ENV.with(|e| *e.borrow_mut() = old);
    r
}
/// The context of `uuid` now: their dimension when online, the `server` name of permissions.json (LuckPerms
/// `server` in config.yml; default `global`).
pub fn perm_env_for(host: &dyn Host, perms: &Value, uuid: u128) -> PermEnv {
    PermEnv {
        world: host
            .server_players()
            .into_iter()
            .find(|p| p.uuid == uuid)
            .map(|p| p.dimension),
        server: perms
            .get("server")
            .and_then(|s| s.as_str())
            .unwrap_or("global")
            .to_owned(),
        now_secs: now_ms() / 1000,
    }
}
/// A Bukkit world name for a dimension: the vanilla three as Paper names them, an extra level by its path.
fn bukkit_world_names(dim: &str) -> Vec<String> {
    let mut v = vec![dim.to_lowercase()];
    match dim {
        "minecraft:overworld" => v.push("world".into()),
        "minecraft:the_nether" => v.push("world_nether".into()),
        "minecraft:the_end" => v.push("world_the_end".into()),
        d => v.push(d.rsplit(':').next().unwrap_or(d).to_lowercase()),
    }
    v
}
/// Does an entry (a node, a parent group) apply in the current [`PermEnv`]? A plain string always does; an object
/// `{"expiry": unix_secs, "context": {"world": "..", "server": ".."}}` only before its expiry and where every
/// context key matches (LuckPerms: several values of one key are any-of, given as a list).
fn entry_applies(e: &Value) -> bool {
    let Some(o) = e.as_object() else { return true };
    let env = PERM_ENV
        .with(|x| x.borrow().clone())
        .unwrap_or_else(|| PermEnv {
            now_secs: now_ms() / 1000,
            server: "global".into(),
            world: None,
        });
    if let Some(exp) = o.get("expiry").and_then(|x| x.as_u64())
        && exp > 0
        && exp <= env.now_secs
    {
        return false;
    }
    let Some(c) = o.get("context").and_then(|c| c.as_object()) else {
        return true;
    };
    c.iter().all(|(k, want)| {
        let wants: Vec<String> = match want {
            Value::Array(a) => a
                .iter()
                .filter_map(|x| x.as_str().map(str::to_lowercase))
                .collect(),
            Value::String(s) => vec![s.to_lowercase()],
            _ => return true,
        };
        match k.as_str() {
            "world" => env
                .world
                .as_deref()
                .is_some_and(|w| bukkit_world_names(w).iter().any(|n| wants.contains(n))),
            "server" => wants.iter().any(|w| *w == env.server.to_lowercase()),
            _ => false, // a context Horizon does not know (gamemode, dimension-type...) never matches
        }
    })
}
/// `(negated, node)` of a node entry that applies: `"node"`, `"-node"` or `{"node": .., "value": bool, ...}`.
fn entry_node(e: &Value) -> Option<(bool, &str)> {
    if !entry_applies(e) {
        return None;
    }
    let (raw, value) = match e {
        Value::String(s) => (s.as_str(), true),
        Value::Object(o) => (
            o.get("node")?.as_str()?,
            o.get("value").and_then(|v| v.as_bool()).unwrap_or(true),
        ),
        _ => return None,
    };
    match raw.strip_prefix('-') {
        Some(rest) => Some((value, rest)),
        None => Some((!value, raw)),
    }
}
/// The group of a membership entry that applies: `"group"` or `{"group": .., "expiry", "context"}`.
fn entry_group(e: &Value) -> Option<String> {
    if !entry_applies(e) {
        return None;
    }
    match e {
        Value::String(s) => Some(s.clone()),
        Value::Object(o) => o.get("group").and_then(|g| g.as_str()).map(str::to_owned),
        _ => None,
    }
}

/// Does `nodes` (a JSON list of strings) decide `node`? `Some(true)` granted, `Some(false)` negated (`-node`),
/// `None` silent. Exact entries beat wildcards; a negation beats a grant at the same specificity.
fn node_verdict(list: Option<&Value>, node: &str) -> Option<bool> {
    let mut best: Option<(usize, bool)> = None;
    for (neg, g) in list
        .and_then(|l| l.as_array())
        .into_iter()
        .flatten()
        .filter_map(entry_node)
    {
        let score = if g == node {
            usize::MAX
        } else if g == "*" {
            0
        } else if let Some(pre) = g.strip_suffix(".*") {
            if node.starts_with(pre) && node[pre.len()..].starts_with('.') {
                pre.len() + 1
            } else {
                continue;
            }
        } else {
            continue;
        };
        let val = !neg;
        best = match best {
            None => Some((score, val)),
            Some((s, _)) if score > s => Some((score, val)),
            Some((s, _)) if score == s && !val => Some((s, false)),
            keep => keep,
        };
    }
    best.map(|(_, v)| v)
}

/// A group's own nodes then its parents' (depth-first, `inherit`), cycle-safe. A group is either a list of nodes
/// (old format) or `{"nodes": [...], "inherit": [...], "prefix": "...", "weight": n}`.
fn group_verdict(v: &Value, group: &str, node: &str, seen: &mut Vec<String>) -> Option<bool> {
    if seen.iter().any(|s| s == group) || seen.len() > 32 {
        return None;
    }
    seen.push(group.to_owned());
    let g = v.get("groups")?.get(group)?;
    let nodes = if g.is_array() {
        Some(g)
    } else {
        g.get("nodes")
    };
    if let Some(r) = node_verdict(nodes, node) {
        return Some(r);
    }
    for parent in g
        .get("inherit")
        .and_then(|i| i.as_array())
        .into_iter()
        .flatten()
        .filter_map(entry_group)
    {
        if let Some(r) = group_verdict(v, &parent, node, seen) {
            return Some(r);
        }
    }
    None
}

/// The groups a player is in (their entry's `groups`, else `["default"]`).
pub fn player_groups(v: &Value, uuid: u128, name: &str) -> Vec<String> {
    let entry = player_entry(v, uuid, name);
    let listed: Vec<String> = entry
        .and_then(|e| e.get("groups"))
        .and_then(|g| g.as_array())
        .into_iter()
        .flatten()
        .filter_map(entry_group)
        .collect();
    if listed.is_empty() {
        vec!["default".into()]
    } else {
        listed
    }
}

fn dashed(uuid: u128) -> String {
    format!(
        "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
        (uuid >> 96) as u32,
        (uuid >> 80) as u16,
        (uuid >> 64) as u16,
        (uuid >> 48) as u16,
        uuid & 0xFFFF_FFFF_FFFF
    )
}

fn player_entry<'v>(v: &'v Value, uuid: u128, name: &str) -> Option<&'v Value> {
    let players = v.get("players")?;
    players
        .get(dashed(uuid))
        .or_else(|| players.get(name.to_lowercase()))
}

/// `permissions.json`: `{"default": [..], "groups": {..}, "players": {"<uuid>|<name>": [..] | {"nodes": [..], "groups": [..]}}}`.
/// Order: the player's own nodes, then their groups (in listed order, with inheritance), then the top-level `default`
/// list and group `default`. Unknown = not granted.
/// [`Ctx::has_perm`]'s file lookup without the op shortcut (for `/lp ... permission check`).
pub fn permission_matches(v: &Value, uuid: u128, name: &str, node: &str) -> bool {
    perm_matches(v, uuid, name, node)
}

fn perm_matches(v: &Value, uuid: u128, name: &str, node: &str) -> bool {
    perm_verdict(v, uuid, name, node).unwrap_or(false)
}

/// What permissions.json says about `node` for a player: Some(true/false) when a node (theirs, a group's or the
/// `default` list) decides it, None when nothing does. For nodes granted by default (Bukkit `default: true`, e.g.
/// `voicechat.speak`): allowed unless this is Some(false).
pub fn perm_verdict(v: &Value, uuid: u128, name: &str, node: &str) -> Option<bool> {
    let entry = player_entry(v, uuid, name);
    let own = entry.and_then(|e| {
        if e.is_array() {
            Some(e)
        } else {
            e.get("nodes")
        }
    });
    if let Some(r) = node_verdict(own, node) {
        return Some(r);
    }
    let mut groups = player_groups(v, uuid, name);
    if !groups.iter().any(|g| g == "default") {
        groups.push("default".into());
    }
    for g in &groups {
        if let Some(r) = group_verdict(v, g, node, &mut Vec::new()) {
            return Some(r);
        }
    }
    node_verdict(v.get("default"), node)
}

/// The chat/tab prefix of a player: the highest-`weight` group with a `prefix` (LuckPerms meta), or their own `prefix`.
pub fn player_prefix(v: &Value, uuid: u128, name: &str) -> Option<String> {
    if let Some(p) = player_entry(v, uuid, name)
        .and_then(|e| e.get("prefix"))
        .and_then(|p| p.as_str())
    {
        return Some(p.to_owned());
    }
    let mut best: Option<(i64, String)> = None;
    let mut stack = player_groups(v, uuid, name);
    let mut seen = Vec::new();
    while let Some(g) = stack.pop() {
        if seen.contains(&g) || seen.len() > 64 {
            continue;
        }
        seen.push(g.clone());
        let Some(gv) = v.get("groups").and_then(|gs| gs.get(&g)) else {
            continue;
        };
        if let Some(p) = gv.get("prefix").and_then(|p| p.as_str()) {
            let w = gv.get("weight").and_then(|w| w.as_i64()).unwrap_or(0);
            if best.as_ref().is_none_or(|(bw, _)| w > *bw) {
                best = Some((w, p.to_owned()));
            }
        }
        stack.extend(
            gv.get("inherit")
                .and_then(|i| i.as_array())
                .into_iter()
                .flatten()
                .filter_map(entry_group),
        );
    }
    best.map(|(_, p)| p)
}

/// LuckPerms meta of a player: their own `meta.<key>` (or `suffix`), else the highest-weight group's (with
/// inheritance). `key` "suffix" reads the `suffix` field.
pub fn player_meta(v: &Value, uuid: u128, name: &str, key: &str) -> Option<String> {
    let field = |e: &Value| -> Option<String> {
        if key == "suffix" || key == "prefix" {
            e.get(key).and_then(|p| p.as_str()).map(str::to_owned)
        } else {
            e.get("meta").and_then(|m| m.get(key)).map(|x| {
                x.as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| x.to_string())
            })
        }
    };
    if let Some(p) = player_entry(v, uuid, name).and_then(field) {
        return Some(p);
    }
    let mut best: Option<(i64, String)> = None;
    let mut stack = player_groups(v, uuid, name);
    if !stack.iter().any(|g| g == "default") {
        stack.insert(0, "default".into());
    }
    let mut seen = Vec::new();
    while let Some(g) = stack.pop() {
        if seen.contains(&g) || seen.len() > 64 {
            continue;
        }
        seen.push(g.clone());
        let Some(gv) = v.get("groups").and_then(|gs| gs.get(&g)) else {
            continue;
        };
        if let Some(p) = field(gv) {
            let w = gv.get("weight").and_then(|w| w.as_i64()).unwrap_or(0);
            if best.as_ref().is_none_or(|(bw, _)| w > *bw) {
                best = Some((w, p));
            }
        }
        stack.extend(
            gv.get("inherit")
                .and_then(|i| i.as_array())
                .into_iter()
                .flatten()
                .filter_map(entry_group),
        );
    }
    best.map(|(_, p)| p)
}

/// A player's nickname from `<config>/hplug/nicknames.json` (`{"<dashed uuid>": "Nick"}`), set by essentials' `/nick`.
pub fn nickname(host: &dyn Host, uuid: u128) -> Option<String> {
    let dir = host.config_dir().ok()?;
    let v: Value = serde_json::from_str(
        &std::fs::read_to_string(dir.join("hplug").join("nicknames.json")).ok()?,
    )
    .ok()?;
    v.get(dashed(uuid))?
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// Set or clear (None) a nickname in `nicknames.json`. Call [`Ctx::refresh_display_name`] afterwards.
pub fn set_nickname_file(host: &dyn Host, uuid: u128, nick: Option<&str>) -> Result<(), String> {
    let dir = host.config_dir()?.join("hplug");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join("nicknames.json");
    let mut v: Map<String, Value> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    match nick {
        Some(n) => {
            v.insert(dashed(uuid), Value::String(n.to_owned()));
        }
        None => {
            v.remove(&dashed(uuid));
        }
    }
    std::fs::write(
        path,
        serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

/// The display tags plugins put before a player's name (a clan tag), in `<config>/hplug/display_tags.json`
/// `{dashed-uuid: {plugin: tag}}`, joined in plugin-name order. Set with [`Ctx::set_display_tag`].
pub fn display_tags(host: &dyn Host, uuid: u128) -> String {
    let Some(v) = host
        .config_dir()
        .ok()
        .and_then(|d| std::fs::read_to_string(d.join("hplug").join("display_tags.json")).ok())
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
    else {
        return String::new();
    };
    let Some(m) = v.get(dashed(uuid)).and_then(|m| m.as_object()) else {
        return String::new();
    };
    let mut keys: Vec<&String> = m.keys().collect();
    keys.sort();
    keys.into_iter().filter_map(|k| m[k].as_str()).collect()
}

/// An item as SNBT text (`{id:"minecraft:stone",count:3,components:{...}}`), lossless, for plugin stores.
pub fn item_to_snbt(stack: &ItemStack) -> String {
    horizon_world::snbt_text::to_snbt(&horizon_client::save::item_to_nbt(stack))
}
/// The item of [`item_to_snbt`]'s text (also a Bukkit-YAML item rebuilt as SNBT). None when it is not one.
pub fn item_from_snbt(text: &str) -> Option<ItemStack> {
    let (tag, _) = horizon_world::nbt::parse_snbt(text).ok()?;
    horizon_client::save::item_from_nbt(&tag)
}

/// Vault replacement: the server's money is the rewards plugin's Shards (`config/hplug/rewards.json`). Reading
/// takes the file (rewards writes it on every change); changing goes through rewards' own console commands
/// (`reward take|give`, next tick), so its cache stays the one truth. Check [`economy::balance`] first: a take
/// never goes below 0.
pub mod economy {
    use super::{Ctx, Value, dashed};
    /// The Shards of `uuid` (0 when unknown).
    pub fn balance(ctx: &Ctx, uuid: u128) -> u64 {
        ctx.host
            .config_dir()
            .ok()
            .and_then(|d| std::fs::read_to_string(d.join("hplug").join("rewards.json")).ok())
            .and_then(|t| super::json::from_str::<Value>(&t).ok())
            .and_then(|v| v.get("balances")?.get(dashed(uuid))?.as_u64())
            .unwrap_or(0)
    }
    /// Take `amount` from `uuid` when they have it (true), recorded in rewards' history with `reason`.
    pub fn charge(ctx: &mut Ctx, uuid: u128, amount: u64, reason: &str) -> bool {
        if amount == 0 {
            return true;
        }
        if balance(ctx, uuid) < amount {
            return false;
        }
        ctx.console(format!("reward take {} {amount} {reason}", dashed(uuid)));
        true
    }
    /// Give `amount` to `uuid`.
    pub fn pay(ctx: &mut Ctx, uuid: u128, amount: u64, reason: &str) {
        if amount > 0 {
            ctx.console(format!("reward give {} {amount} {reason}", dashed(uuid)));
        }
    }
}

/// A uuid in the dashed form used in Paper's files (`usercache.json`, `ops.json`, plugin data).
pub fn uuid_dashed(uuid: u128) -> String {
    dashed(uuid)
}

/// Parse a dashed or plain hex uuid.
pub fn parse_uuid(s: &str) -> Option<u128> {
    let h: String = s.chars().filter(|c| *c != '-').collect();
    if h.len() != 32 {
        return None;
    }
    u128::from_str_radix(&h, 16).ok()
}

/// Read `<config>/hplug/permissions.json` (empty object when missing or broken).
pub fn read_permissions(host: &dyn Host) -> Value {
    host.config_dir()
        .ok()
        .and_then(|d| std::fs::read_to_string(d.join("hplug").join("permissions.json")).ok())
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| Value::Object(Map::new()))
}

/// Write `<config>/hplug/permissions.json` (pretty, atomically through a temp file).
pub fn write_permissions(host: &dyn Host, v: &Value) -> Result<(), String> {
    let dir = host.config_dir()?.join("hplug");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let tmp = dir.join("permissions.json.tmp");
    std::fs::write(
        &tmp,
        serde_json::to_string_pretty(v).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, dir.join("permissions.json")).map_err(|e| e.to_string())
}

/// A clickable link (Paper/Adventure `<click:open_url:..>`): the label opens `url` in the player's browser.
/// Underlined and aqua. 26.x component format (`click_event`).
pub fn link(label: &str, url: &str) -> Text {
    Text::colored(label, 0x55FFFF)
        .underlined()
        .with_component_fields(vec![(
            "click_event".to_owned(),
            json::json!({"action": "open_url", "url": url}),
        )])
}

/// True for a Bedrock player that joined through Geyser + Floodgate: their forwarded UUID is
/// `00000000-0000-0000-xxxx-xxxxxxxxxxxx` (high 64 bits zero; Floodgate uses `...-0009-<xuid>`).
pub fn is_floodgate(uuid: u128) -> bool {
    uuid >> 64 == 0
}

/// True for a Mojang-account UUID (version 4). Offline-mode UUIDs are version 3.
pub fn is_premium(uuid: u128) -> bool {
    !is_floodgate(uuid) && (uuid >> 76) & 0xF == 4
}

/// Post `text` to a Discord channel with a bot token, from a thread (`curl`; needs curl on the host). Failures are
/// logged to stderr, never raised. Paper plugins used a java.net.http client for this.
pub fn discord_post(token: &str, channel: &str, text: &str) {
    if token.is_empty() || channel.is_empty() {
        return;
    }
    let (token, channel, body) = (
        token.to_owned(),
        channel.to_owned(),
        json::json!({"content": text}).to_string(),
    );
    std::thread::spawn(move || {
        use std::io::Write;
        let child = std::process::Command::new("curl")
            .args([
                "-s",
                "-m",
                "10",
                "-o",
                "/dev/null",
                "-w",
                "%{http_code}",
                "-X",
                "POST",
                &format!("https://discord.com/api/v10/channels/{channel}/messages"),
                "-H",
                &format!("Authorization: Bot {token}"),
                "-H",
                "Content-Type: application/json",
                "-H",
                "User-Agent: DiscordBot (horizons-plugin, 1.0)",
                "--data-binary",
                "@-",
            ])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn();
        match child {
            Ok(mut c) => {
                let _ = c.stdin.take().map(|mut s| s.write_all(body.as_bytes()));
                match c.wait_with_output() {
                    Ok(o) if !o.stdout.starts_with(b"2") => eprintln!(
                        "[hplug] Discord post got HTTP {}",
                        String::from_utf8_lossy(&o.stdout)
                    ),
                    Err(e) => eprintln!("[hplug] Discord post failed: {e}"),
                    _ => {}
                }
            }
            Err(e) => eprintln!("[hplug] Discord post failed (curl missing?): {e}"),
        }
    });
}

/// Milliseconds since the Unix epoch. Paper: `System.currentTimeMillis()`.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// What a handler gets. `player` is the acting player (None for console commands and tick tasks).
pub struct Ctx<'a> {
    pub host: &'a mut dyn Host,
    player: Option<NativeServerPlayer>,
    uuid: Option<u128>,
    plugin_id: &'a str,
    shared: &'a Mutex<Shared>,
}

impl Ctx<'_> {
    /// The id of the plugin this handler belongs to (its store is `config/hplug/<id>.json`).
    pub fn plugin_id(&self) -> &str {
        self.plugin_id
    }
    /// The acting player. Panics for console commands and tick tasks; use [`Ctx::try_player`] there.
    pub fn player(&self) -> &NativeServerPlayer {
        self.player.as_ref().expect("hplug: no acting player in this handler (console command or tick task); use try_player()")
    }
    /// The player's uuid even when the full record is missing: hooks that run without the player's
    /// state swapped in (portal, quit, death) can leave `try_player()` empty, but they always know who.
    pub fn player_uuid(&self) -> Option<u128> {
        self.uuid
    }
    pub fn try_player(&self) -> Option<&NativeServerPlayer> {
        self.player.as_ref()
    }
    /// Paper: `sender instanceof Player`. False for the console.
    pub fn is_player(&self) -> bool {
        self.player.is_some()
    }
    /// Paper: `sender.getName()` ("CONSOLE" without a player).
    pub fn sender_name(&self) -> String {
        self.player
            .as_ref()
            .map(|p| p.name.clone())
            .unwrap_or_else(|| "CONSOLE".into())
    }
    /// Op level of the sender (console = 4). Paper: `sender.hasPermission(..)` ~ `op_level() >= 2`.
    pub fn op_level(&self) -> u8 {
        self.player.as_ref().map(|p| p.permission).unwrap_or(4)
    }
    /// Command feedback (inside a command handler). `§a` colour codes work. Paper: `sender.sendMessage`.
    pub fn reply(&mut self, msg: impl AsRef<str>) {
        self.host.feedback(legacy(msg.as_ref()));
    }
    /// Kick a player with a reason (queued console `kick`, runs at the next tick start). Paper: `player.kick(..)`.
    pub fn kick(&mut self, name: &str, reason: &str) {
        self.console(format!("kick {name} {reason}"));
    }
    /// Title + subtitle for a player (queued console `title`). `fade_in`, `stay`, `fade_out` in ticks.
    /// Paper: `player.showTitle(..)`. Plain text (no `§` codes); the title is red-bold, the subtitle yellow.
    pub fn title(&mut self, name: &str, title: &str, subtitle: &str, stay: u32) {
        let esc = |t: &str| t.replace('\\', "").replace('"', "'");
        self.console(format!("title {name} times 0 {stay} 10"));
        self.console(format!(
            "title {name} subtitle {{\"text\":\"{}\",\"color\":\"yellow\"}}",
            esc(subtitle)
        ));
        self.console(format!(
            "title {name} title {{\"text\":\"{}\",\"color\":\"red\",\"bold\":true}}",
            esc(title)
        ));
    }
    /// Command feedback with a ready-made [`Message`] (see [`link`], [`legacy`]).
    pub fn reply_text(&mut self, msg: Message) {
        self.host.feedback(msg);
    }
    /// Text over the player's hotbar. Paper: `player.sendActionBar`.
    pub fn action_bar(&mut self, uuid: u128, msg: impl AsRef<str>) -> bool {
        self.host.action_bar(uuid, legacy(msg.as_ref()))
    }
    /// Message to any online player by uuid. Paper: `player.sendMessage`.
    /// Some hooks (portal, damage, death, quit...) run without the player's state swapped in, and then the
    /// game refuses a direct tell. hplug retries such a message at the next tick start, so the return value
    /// is true when the message was sent or queued.
    pub fn tell(&mut self, uuid: u128, msg: impl AsRef<str>) -> bool {
        if self.host.tell(uuid, legacy(msg.as_ref())) {
            return true;
        }
        self.shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .tells
            .push_back((uuid, msg.as_ref().to_owned()));
        true
    }
    /// Paper: `Bukkit.broadcastMessage`.
    pub fn broadcast(&mut self, msg: impl AsRef<str>) {
        self.host.broadcast(legacy(msg.as_ref()));
    }
    pub fn online(&self) -> Vec<NativeServerPlayer> {
        self.host.server_players()
    }
    /// Paper: `Bukkit.getPlayerExact(name)` (case-insensitive here). Online players only.
    pub fn find_online(&self, name: &str) -> Option<NativeServerPlayer> {
        self.host
            .server_players()
            .into_iter()
            .find(|p| p.name.eq_ignore_ascii_case(name))
    }
    /// The server folder (beside server.properties, usercache.json, ops.json). Paper: `Bukkit.getWorldContainer()`.
    pub fn server_dir(&self) -> Option<PathBuf> {
        self.host
            .world_dir()
            .and_then(|w| w.parent().map(|p| p.to_path_buf()))
    }
    /// Paper: `player.isOp()` / `hasPermission(..)` for ops, also for a player who is not placed yet
    /// (login) or offline: reads `ops.json` (level >= 2 counts).
    pub fn is_op(&self, uuid: u128) -> bool {
        let Some(dir) = self.server_dir() else {
            return false;
        };
        let Ok(text) = std::fs::read_to_string(dir.join("ops.json")) else {
            return false;
        };
        let Ok(v) = serde_json::from_str::<Value>(&text) else {
            return false;
        };
        v.as_array().into_iter().flatten().any(|e| {
            let u = e
                .get("uuid")
                .and_then(|x| x.as_str())
                .and_then(|x| u128::from_str_radix(&x.replace('-', ""), 16).ok());
            u == Some(uuid) && e.get("level").and_then(|l| l.as_u64()).unwrap_or(4) >= 2
        })
    }
    /// Paper: `Bukkit.getOfflinePlayer(name).getUniqueId()` for players who joined before (usercache.json).
    pub fn uuid_for_name(&self, name: &str) -> Option<(u128, String)> {
        let text = std::fs::read_to_string(self.server_dir()?.join("usercache.json")).ok()?;
        let v: Value = serde_json::from_str(&text).ok()?;
        v.as_array()?.iter().find_map(|e| {
            let n = e.get("name")?.as_str()?;
            if !n.eq_ignore_ascii_case(name) {
                return None;
            }
            let u = u128::from_str_radix(&e.get("uuid")?.as_str()?.replace('-', ""), 16).ok()?;
            Some((u, n.to_owned()))
        })
    }
    /// Paper: `player.teleport(new Location(...))` (same dimension). Runs at the next tick start
    /// because `Host::run_command` is refused inside a player's hook.
    pub fn teleport(&mut self, name: &str, x: f64, y: f64, z: f64) {
        self.console(format!("tp {name} {x:.2} {y:.2} {z:.2}"));
    }
    /// Paper: `player.teleport(location)` into any dimension, with a look: `execute in <dim> run tp ...` at the next
    /// tick start. The engine's `player_teleport` hook hears it (cause `command`), so a plugin may refuse it.
    pub fn teleport_to(
        &mut self,
        name: &str,
        dimension: &str,
        pos: [f64; 3],
        look: Option<[f32; 2]>,
    ) {
        let rot = look
            .map(|[y, p]| format!(" {y:.1} {p:.1}"))
            .unwrap_or_default();
        self.console(format!(
            "execute in {dimension} run tp {name} {:.3} {:.3} {:.3}{rot}",
            pos[0], pos[1], pos[2]
        ));
    }
    /// The name other players see for `uuid` (chat, Tab list): the permission prefix (`permissions.json` group or
    /// player `prefix`, see [`Ctx::has_perm`]) + the nickname from `<config>/hplug/nicknames.json` or the real name.
    /// Applied with `Host::set_nickname` at the next tick (it is refused while a player's state is swapped in).
    /// Every plugin that changes a prefix or nickname calls this; nothing else should call `set_nickname`.
    /// What an online player holds in their main hand (a full stack: components included).
    pub fn main_hand_item(&self, uuid: u128) -> Option<ItemStack> {
        let p = self
            .host
            .server_players()
            .into_iter()
            .find(|p| p.uuid == uuid)?;
        let e = self.host.entity(p.entity_id as u32)?;
        let l = self.host.living(e)?;
        self.host
            .living_hand_item(l, horizon_client::mod_api::Hand::Main)
    }
    /// What `uuid` wears or holds in `slot` (online players). Paper: `getEquipment().getItem(slot)`.
    pub fn equipment_of(&self, uuid: u128, slot: EquipmentSlot) -> Option<ItemStack> {
        let p = self
            .host
            .server_players()
            .into_iter()
            .find(|p| p.uuid == uuid)?;
        let l = self.host.living(self.host.entity(p.entity_id as u32)?)?;
        match slot {
            EquipmentSlot::MainHand => self
                .host
                .living_hand_item(l, horizon_client::mod_api::Hand::Main),
            EquipmentSlot::OffHand => self
                .host
                .living_hand_item(l, horizon_client::mod_api::Hand::Off),
            s => self.host.living_equipment(l, s),
        }
    }
    /// Put `stack` (None: empty) in `uuid`'s `slot`. Paper: `getEquipment().setItem(slot, stack)`. Call from a tick or
    /// `run_later`, not inside the player's own command (their state is swapped in there).
    pub fn set_equipment_of(
        &mut self,
        uuid: u128,
        slot: EquipmentSlot,
        stack: Option<ItemStack>,
    ) -> bool {
        let Some(p) = self
            .host
            .server_players()
            .into_iter()
            .find(|p| p.uuid == uuid)
        else {
            return false;
        };
        let Some(l) = self
            .host
            .entity(p.entity_id as u32)
            .and_then(|e| self.host.living(e))
        else {
            return false;
        };
        self.host.living_set_equipment(l, slot, stack)
    }
    /// Set or clear (None) this plugin's display tag for `uuid` (shown before the rank prefix and name in chat
    /// and the Tab list, e.g. a clan tag), then refresh the name.
    pub fn set_display_tag(
        &mut self,
        uuid: u128,
        name: &str,
        tag: Option<&str>,
    ) -> Result<(), String> {
        let dir = self.host.config_dir()?.join("hplug");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let path = dir.join("display_tags.json");
        let mut v: Value = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_else(|| Value::Object(Map::new()));
        if !v.is_object() {
            v = Value::Object(Map::new());
        }
        let key = dashed(uuid);
        let entry = v
            .as_object_mut()
            .unwrap()
            .entry(key.clone())
            .or_insert_with(|| Value::Object(Map::new()));
        if !entry.is_object() {
            *entry = Value::Object(Map::new());
        }
        let m = entry.as_object_mut().unwrap();
        match tag {
            Some(t) => {
                m.insert(self.plugin_id.to_owned(), Value::String(t.to_owned()));
            }
            None => {
                m.remove(self.plugin_id);
            }
        }
        if m.is_empty() {
            v.as_object_mut().unwrap().remove(&key);
        }
        let tmp = dir.join("display_tags.json.tmp");
        std::fs::write(
            &tmp,
            serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
        self.refresh_display_name(uuid, name);
        Ok(())
    }
    pub fn refresh_display_name(&mut self, uuid: u128, name: &str) {
        let name = name.to_owned();
        self.run_later(1, move |c| {
            let perms = read_permissions(&*c.host);
            let env = perm_env_for(&*c.host, &perms, uuid);
            let (prefix, suffix) = with_perm_env(env, || {
                (
                    player_prefix(&perms, uuid, &name).unwrap_or_default(),
                    player_meta(&perms, uuid, &name, "suffix").unwrap_or_default(),
                )
            });
            let nick = nickname(&*c.host, uuid);
            let tags = display_tags(&*c.host, uuid);
            let shown = format!("{tags}{prefix}{}{suffix}", nick.as_deref().unwrap_or(&name));
            let arg = if shown == name {
                None
            } else {
                Some(shown.as_str())
            };
            c.host.set_nickname(uuid, arg);
        });
    }
    /// Set the skin with upstream's profile mutation and refresh APIs.
    pub fn set_player_skin(&mut self, uuid: u128, value: &str, signature: Option<&str>) -> bool {
        let property = (!value.is_empty()).then_some((value, signature));
        self.host.set_profile_property(uuid, "textures", property) && self.host.refresh_player(uuid)
    }
    /// Grant flight without changing a player who is already flying.
    pub fn set_may_fly(&mut self, uuid: u128, allow: bool) -> bool {
        let flying = self
            .host
            .server_players()
            .into_iter()
            .find(|p| p.uuid == uuid)
            .is_some_and(|p| p.flying);
        self.host.set_flight(uuid, allow, flying)
    }
    /// Run a console command (op level 4) at the next tick start. Paper: `dispatchCommand(console, ..)`.
    pub fn console(&mut self, line: impl Into<String>) {
        self.shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .console
            .push_back(line.into());
    }
    /// Paper: `runTaskLater`. Returns a handle for [`Ctx::cancel`].
    pub fn run_later(
        &mut self,
        ticks: u64,
        f: impl Fn(&mut Ctx) + Send + Sync + 'static,
    ) -> TaskId {
        self.schedule(ticks.max(1), None, Arc::new(f))
    }
    /// Paper: `runTaskTimer(plugin, f, delay, period)`. First run after `delay` ticks, then every `period`.
    pub fn run_repeating(
        &mut self,
        delay: u64,
        period: u64,
        f: impl Fn(&mut Ctx) + Send + Sync + 'static,
    ) -> TaskId {
        self.schedule(delay.max(1), Some(period.max(1)), Arc::new(f))
    }
    fn schedule(
        &mut self,
        delay: u64,
        period: Option<u64>,
        run: Arc<dyn Fn(&mut Ctx) + Send + Sync>,
    ) -> TaskId {
        let mut s = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        s.next_task += 1;
        let id = s.next_task;
        let at = s.tick + delay;
        s.later.push(Task {
            at,
            id,
            period,
            run,
        });
        TaskId(id)
    }
    /// Paper: `BukkitTask#cancel`. False if the task already ran (one-shot) or was cancelled.
    pub fn cancel(&mut self, task: TaskId) -> bool {
        let mut s = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        let n = s.later.len();
        s.later.retain(|t| t.id != task.0);
        s.later.len() != n
    }
    /// Paper: `player.hasPermission(node)`. Ops (level >= 2, `ops.json`) have every node. Everybody else
    /// needs the node in `<config_dir>/hplug/permissions.json` (LuckPerms-like, managed by the `perms` plugin's `/lp`):
    /// `{"default": ["a.b"], "groups": {"default": {"nodes": [..]}, "vip": {"inherit": ["default"], "nodes": ["x.*", "-x.y"],
    /// "prefix": "§6[VIP] ", "weight": 10}}, "players": {"<uuid with dashes or lowercase name>": {"groups": ["vip"], "nodes": [..]}}}`.
    /// The old shape (`"players": {"name": ["x.*"]}`) still works. `*` and `prefix.*` wildcards, `-node` negation.
    /// Reloaded on each call (small file, command-rate use).
    pub fn has_perm(&self, uuid: u128, name: &str, node: &str) -> bool {
        if self.is_op(uuid)
            || self
                .player
                .as_ref()
                .is_some_and(|p| p.uuid == uuid && p.permission >= 2)
        {
            return true;
        }
        let v = read_permissions(&*self.host);
        let env = perm_env_for(&*self.host, &v, uuid);
        with_perm_env(env, || perm_matches(&v, uuid, name, node))
    }
    /// A permission that players have unless it is set false (Bukkit `default: true`): `voicechat.speak`. Ops too.
    pub fn has_default_perm(&self, uuid: u128, name: &str, node: &str) -> bool {
        let v = read_permissions(&*self.host);
        let env = perm_env_for(&*self.host, &v, uuid);
        with_perm_env(env, || perm_verdict(&v, uuid, name, node)) != Some(false)
    }
    /// LuckPerms meta value `key` of a player (`/lp user X meta set key value`), in their current context.
    pub fn perm_meta(&self, uuid: u128, name: &str, key: &str) -> Option<String> {
        let v = read_permissions(&*self.host);
        let env = perm_env_for(&*self.host, &v, uuid);
        with_perm_env(env, || player_meta(&v, uuid, name, key))
    }
    pub fn tick(&self) -> u64 {
        self.shared.lock().unwrap_or_else(|e| e.into_inner()).tick
    }
    pub fn give(&mut self, item: &str, count: u32) -> u32 {
        self.host.give_items(item, count)
    }
    /// Server log line. Paper: `getLogger().info`.
    pub fn log(&self, msg: impl AsRef<str>) {
        println!("[{}] {}", self.plugin_id, msg.as_ref());
    }

    // ---- persistence: one JSON object per plugin in <config_dir>/hplug/<id>.json ----
    fn store_path(&self) -> Result<PathBuf, String> {
        let mut p = self.host.config_dir()?;
        p.push("hplug");
        std::fs::create_dir_all(&p).map_err(|e| e.to_string())?;
        p.push(format!("{}.json", self.plugin_id));
        Ok(p)
    }
    fn load_store(&self) {
        let mut s = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        if s.store.is_none() {
            let m = self
                .store_path()
                .ok()
                .and_then(|p| std::fs::read_to_string(p).ok())
                .and_then(|t| serde_json::from_str::<Map<String, Value>>(&t).ok())
                .unwrap_or_default();
            s.store = Some(m);
        }
    }
    /// Paper: `getConfig().get(path)` / a data file.
    pub fn get(&self, key: &str) -> Option<Value> {
        self.load_store();
        self.shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .store
            .as_ref()
            .unwrap()
            .get(key)
            .cloned()
    }
    /// Paper: `getConfig().set(path, v); saveConfig()`. Written to disk immediately.
    pub fn set(&mut self, key: &str, value: Value) -> Result<(), String> {
        self.load_store();
        let text = {
            let mut s = self.shared.lock().unwrap_or_else(|e| e.into_inner());
            let m = s.store.as_mut().unwrap();
            m.insert(key.into(), value);
            serde_json::to_string_pretty(m).map_err(|e| e.to_string())?
        };
        std::fs::write(self.store_path()?, text).map_err(|e| e.to_string())
    }
    /// Remove a key (and save). Paper: `getConfig().set(path, null)`.
    pub fn remove(&mut self, key: &str) -> Result<(), String> {
        self.load_store();
        let text = {
            let mut s = self.shared.lock().unwrap_or_else(|e| e.into_inner());
            let m = s.store.as_mut().unwrap();
            m.remove(key);
            serde_json::to_string_pretty(m).map_err(|e| e.to_string())?
        };
        std::fs::write(self.store_path()?, text).map_err(|e| e.to_string())
    }
}

impl Plugin {
    fn player_death_drops(&self, host: &mut dyn Host, event: &mut LootDropsEvent<'_>) {
        {
            // only a dying player's inventory (entity type minecraft:player and the player named)
            let (Some(uuid), Some((_, "minecraft:player"))) =
                (event.context.player, event.context.entity)
            else {
                return;
            };
            // the dying player may be missing from the live list: fall back to last tick's record (like player_died)
            let player = host
                .server_players()
                .into_iter()
                .find(|p| p.uuid == uuid)
                .or_else(|| {
                    self.shared
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .cache
                        .iter()
                        .find(|p| p.uuid == uuid)
                        .cloned()
                });
            for f in &self.death_drops {
                f(
                    &mut Ctx {
                        host,
                        uuid: Some(uuid),
                        player: player.clone(),
                        plugin_id: &self.id,
                        shared: &self.shared,
                    },
                    event.context.dimension,
                    event.context.origin,
                    event.drops,
                );
            }
        }
    }
    fn run_actions(&self, host: &mut dyn Host, action: Action) -> Flow {
        if self.action.is_empty() {
            return Flow::Continue;
        }
        let player = host.acting_player();
        let mut cancel = false;
        for f in &self.action {
            cancel |= f(
                &mut Ctx {
                    host,
                    uuid: player.as_ref().map(|p| p.uuid),
                    player: player.clone(),
                    plugin_id: &self.id,
                    shared: &self.shared,
                },
                &action,
            );
        }
        if cancel { Flow::Cancel } else { Flow::Continue }
    }
    fn refresh_players(&self, host: &mut dyn Host) {
        let all = host.server_players();
        let config_dir = host.config_dir().ok();
        let table: Vec<(i32, u128)> = all
            .iter()
            .filter(|p| p.entity_id != 0)
            .map(|p| (p.entity_id, p.uuid))
            .collect();
        let mut s = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        s.players = table;
        s.cache = all;
        s.config_dir = config_dir;
    }
}

/// Runs a hook body so a panic in plugin code is logged and answered with `default` (no cancel) instead of
/// unwinding into the game and killing the server. Poisoned plugin mutexes stay poisoned: a plugin that panics
/// while holding its own lock will keep failing, loudly, in the log, but the server keeps running.
fn guard<R>(id: &str, hook: &str, default: R, f: impl FnOnce() -> R) -> R {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(r) => r,
        Err(p) => {
            let msg = p
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_default();
            eprintln!("[hplug:{id}] hook {hook} panicked: {msg}");
            default
        }
    }
}

/// Console lines waiting for the plugin that owns their command, with the last tick they may wait to.
static RELAY: Mutex<Vec<(String, u64)>> = Mutex::new(Vec::new());
fn relay() -> std::sync::MutexGuard<'static, Vec<(String, u64)>> {
    RELAY.lock().unwrap_or_else(|e| e.into_inner())
}
/// `execute as <name> run <rest>` -> (name, rest).
fn execute_as(line: &str) -> Option<(&str, &str)> {
    let rest = line.strip_prefix("execute as ")?;
    let (who, rest) = rest.split_once(' ')?;
    Some((who, rest.strip_prefix("run ")?))
}

impl Hooks for Plugin {
    fn register_commands(&self, _host: &mut dyn Host, event: &mut RegisterCommandsEvent<'_>) {
        guard(&self.id, "register_commands", (), || {
            for c in &self.commands {
                for u in &c.usages {
                    event.commands.push(if c.level == 0 {
                        NativeCommand::new(u)
                    } else {
                        NativeCommand::op(u, c.level)
                    });
                }
            }
        })
    }

    fn perform_command(&self, host: &mut dyn Host, event: &mut PerformCommandEvent<'_>) -> Flow {
        guard(&self.id, "perform_command", Flow::Continue, || {
            if self.run_command(host, Some(event.player.clone()), event.command) {
                Flow::Cancel
            } else {
                Flow::Continue
            }
        })
    }

    fn perform_console_command(
        &self,
        host: &mut dyn Host,
        event: &mut PerformConsoleCommandEvent<'_>,
    ) -> Flow {
        guard(&self.id, "perform_console_command", Flow::Continue, || {
            if self.run_command(host, None, event.command) {
                Flow::Cancel
            } else {
                Flow::Continue
            }
        })
    }

    fn player_logged_in(&self, host: &mut dyn Host, event: &mut PlayerLoggedInEvent<'_>) {
        guard(&self.id, "player_logged_in", (), || {
            let player = host
                .server_players()
                .into_iter()
                .find(|p| p.uuid == event.player);
            self.refresh_players(host);
            let Some(player) = player else {
                eprintln!(
                    "[hplug:{}] {} joined but is not in server_players(); join handlers skipped",
                    self.id, event.name
                );
                return;
            };
            for f in &self.join {
                f(&mut Ctx {
                    host,
                    uuid: Some(player.uuid),
                    player: Some(player.clone()),
                    plugin_id: &self.id,
                    shared: &self.shared,
                });
            }
        })
    }

    fn player_logged_out(&self, host: &mut dyn Host, event: &mut PlayerLoggedOutEvent<'_>) {
        guard(&self.id, "player_logged_out", (), || {
            self.refresh_players(host);
            self.shared
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .players
                .retain(|(_, u)| *u != event.player);
            let player = host
                .server_players()
                .into_iter()
                .find(|p| p.uuid == event.player);
            for f in &self.quit {
                f(&mut Ctx {
                    host,
                    uuid: Some(event.player),
                    player: player.clone(),
                    plugin_id: &self.id,
                    shared: &self.shared,
                });
            }
        })
    }

    fn player_login(&self, host: &mut dyn Host, event: &mut PlayerLoginEvent<'_>) -> Flow {
        guard(&self.id, "player_login", Flow::Continue, || {
            for f in &self.login {
                if let Some(reason) = f(
                    &mut Ctx {
                        host,
                        uuid: None,
                        player: None,
                        plugin_id: &self.id,
                        shared: &self.shared,
                    },
                    event.player,
                    event.name,
                ) {
                    *event.reason = Some(Text::literal(reason));
                    return Flow::Cancel;
                }
            }
            Flow::Continue
        })
    }

    fn player_died(&self, host: &mut dyn Host, event: &mut PlayerDiedEvent<'_>) {
        guard(&self.id, "player_died", (), || {
            let player = host
                .server_players()
                .into_iter()
                .find(|p| p.uuid == event.player);
            for f in &self.death {
                f(
                    &mut Ctx {
                        host,
                        uuid: Some(event.player),
                        player: player.clone(),
                        plugin_id: &self.id,
                        shared: &self.shared,
                    },
                    event.cause,
                );
            }
        })
    }

    fn loot_drops(&self, host: &mut dyn Host, event: &mut LootDropsEvent<'_>) {
        guard(&self.id, "loot_drops", (), || {
            if !self.death_drops.is_empty() {
                self.player_death_drops(host, event);
            }
            if self.loot.is_empty() {
                return;
            }
            let c = event.context;
            let loot = Loot {
                table: c.table.to_owned(),
                dimension: c.dimension.to_owned(),
                origin: c.origin,
                block: c.block.map(|(pos, _)| pos),
                entity: c.entity.map(|(id, kind)| (id, kind.to_owned())),
                player: c.player,
                tool: c.tool.map(|s| s.item.to_string()),
                explosion: c.explosion,
            };
            let player = c.player.and_then(|u| {
                host.server_players()
                    .into_iter()
                    .find(|p| p.uuid == u)
                    .or_else(|| {
                        self.shared
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .cache
                            .iter()
                            .find(|p| p.uuid == u)
                            .cloned()
                    })
            });
            for f in &self.loot {
                f(
                    &mut Ctx {
                        host,
                        uuid: loot.player,
                        player: player.clone(),
                        plugin_id: &self.id,
                        shared: &self.shared,
                    },
                    &loot,
                    event.drops,
                );
            }
        })
    }

    fn block_burn(&self, _level: &dyn LevelHost, event: &mut BlockBurnEvent) -> Flow {
        guard(&self.id, "block_burn", Flow::Continue, || {
            let f = Fire {
                dimension: dim_id(event.dimension),
                pos: [event.pos.x, event.pos.y, event.pos.z],
                cause: "burn",
                player: None,
            };
            if self.fire.iter().any(|h| h(&f)) {
                Flow::Cancel
            } else {
                Flow::Continue
            }
        })
    }

    fn block_ignite(&self, _level: &dyn LevelHost, event: &mut BlockIgniteEvent<'_>) -> Flow {
        guard(&self.id, "block_ignite", Flow::Continue, || {
            let f = Fire {
                dimension: dim_id(event.dimension),
                pos: [event.pos.x, event.pos.y, event.pos.z],
                cause: event.cause,
                player: event.player,
            };
            if self.fire.iter().any(|h| h(&f)) {
                Flow::Cancel
            } else {
                Flow::Continue
            }
        })
    }

    fn player_teleport(&self, event: &mut PlayerTeleportEvent<'_>) -> Flow {
        guard(&self.id, "player_teleport", Flow::Continue, || {
            if self.teleport.is_empty() {
                return Flow::Continue;
            }
            let player = self
                .shared
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .cache
                .iter()
                .find(|p| p.uuid == event.player)
                .cloned();
            let tp = Teleport {
                player: event.player,
                name: event.name,
                from: event.from,
                to: *event.to,
                from_dimension: event.from_dimension,
                to_dimension: event.to_dimension,
                cause: event.cause,
            };
            let mut cancel = false;
            for f in &self.teleport {
                cancel |= f(
                    &mut TeleportCtx {
                        player: player.clone(),
                        dimension: event.from_dimension,
                        plugin_id: &self.id,
                        shared: &self.shared,
                    },
                    &tp,
                );
            }
            if cancel { Flow::Cancel } else { Flow::Continue }
        })
    }

    fn custom_payload_from_client(
        &self,
        host: &mut dyn Host,
        event: &mut CustomPayloadFromClientEvent<'_>,
    ) {
        guard(&self.id, "plugin_message", (), || {
            if self.plugin_msg.is_empty() {
                return;
            }
            let player = host
                .server_players()
                .into_iter()
                .find(|p| p.uuid == event.player);
            for f in &self.plugin_msg {
                f(
                    &mut Ctx {
                        host,
                        uuid: Some(event.player),
                        player: player.clone(),
                        plugin_id: &self.id,
                        shared: &self.shared,
                    },
                    event.channel,
                    event.data,
                );
            }
        })
    }

    fn item_craft_result(&self, host: &mut dyn Host, event: &mut ItemCraftResultEvent<'_>) {
        guard(&self.id, "item_craft_result", (), || {
            if self.craft.is_empty() {
                return;
            }
            let uuid = event.player;
            let player = host
                .server_players()
                .into_iter()
                .find(|p| p.uuid == uuid)
                .or_else(|| {
                    self.shared
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .cache
                        .iter()
                        .find(|p| p.uuid == uuid)
                        .cloned()
                });
            for f in &self.craft {
                f(
                    &mut Ctx {
                        host,
                        uuid: Some(uuid),
                        player: player.clone(),
                        plugin_id: &self.id,
                        shared: &self.shared,
                    },
                    event.pos,
                    event.matrix,
                    event.result,
                );
            }
        })
    }

    fn merchant_offer_traded(&self, host: &mut dyn Host, event: &mut MerchantOfferTradedEvent<'_>) {
        guard(&self.id, "merchant_offer_traded", (), || {
            for f in &self.trade_offer {
                f(
                    &mut Ctx {
                        host,
                        uuid: None,
                        player: None,
                        plugin_id: &self.id,
                        shared: &self.shared,
                    },
                    event.kind,
                    event.offer,
                );
            }
        })
    }

    fn player_advancement_done(
        &self,
        host: &mut dyn Host,
        event: &mut PlayerAdvancementDoneEvent<'_>,
    ) {
        guard(&self.id, "player_advancement_done", (), || {
            let player = host
                .server_players()
                .into_iter()
                .find(|p| p.uuid == event.player);
            for f in &self.advancement {
                f(
                    &mut Ctx {
                        host,
                        uuid: Some(event.player),
                        player: player.clone(),
                        plugin_id: &self.id,
                        shared: &self.shared,
                    },
                    event.advancement,
                );
            }
        })
    }

    fn menu_clicked(&self, host: &mut dyn Host, event: &mut MenuClickedEvent) {
        guard(&self.id, "menu_clicked", (), || {
            for f in &self.menu_click {
                f(
                    &mut Ctx {
                        host,
                        uuid: Some(event.uuid),
                        player: None,
                        plugin_id: &self.id,
                        shared: &self.shared,
                    },
                    event.window,
                    event.slot,
                    event.button,
                    event.mode,
                );
            }
        })
    }

    fn menu_closed(&self, host: &mut dyn Host, event: &mut MenuClosedEvent) {
        guard(&self.id, "menu_closed", (), || {
            for f in &self.menu_close {
                f(
                    &mut Ctx {
                        host,
                        uuid: Some(event.uuid),
                        player: None,
                        plugin_id: &self.id,
                        shared: &self.shared,
                    },
                    event.window,
                );
            }
        })
    }

    fn player_portal(&self, host: &mut dyn Host, event: &mut PlayerPortalEvent<'_>) -> Flow {
        guard(&self.id, "player_portal", Flow::Continue, || {
            let player = host
                .server_players()
                .into_iter()
                .find(|p| p.uuid == event.player);
            let mut cancel = false;
            for f in &self.portal {
                let mut ctx = Ctx {
                    host,
                    uuid: Some(event.player),
                    player: player.clone(),
                    plugin_id: &self.id,
                    shared: &self.shared,
                };
                cancel |= f(
                    &mut ctx,
                    event.from_dimension,
                    event.to_dimension,
                    event.cause,
                );
            }
            if cancel { Flow::Cancel } else { Flow::Continue }
        })
    }

    fn destroy_block(&self, host: &mut dyn Host, event: &mut DestroyBlockEvent) -> Flow {
        guard(&self.id, "destroy_block", Flow::Continue, || {
            let player = host.acting_player();
            let mut cancel = false;
            for f in &self.brk {
                let mut ctx = Ctx {
                    host,
                    uuid: player.as_ref().map(|p| p.uuid),
                    player: player.clone(),
                    plugin_id: &self.id,
                    shared: &self.shared,
                };
                cancel |= f(&mut ctx, event.pos);
            }
            if cancel { Flow::Cancel } else { Flow::Continue }
        })
    }

    fn right_click_block(&self, host: &mut dyn Host, event: &mut RightClickBlockEvent) -> Flow {
        guard(&self.id, "right_click_block", Flow::Continue, || {
            let player = host.acting_player();
            let mut cancel = false;
            for f in &self.interact {
                let mut ctx = Ctx {
                    host,
                    uuid: player.as_ref().map(|p| p.uuid),
                    player: player.clone(),
                    plugin_id: &self.id,
                    shared: &self.shared,
                };
                cancel |= f(&mut ctx, event.pos, event.face);
            }
            if cancel { Flow::Cancel } else { Flow::Continue }
        })
    }

    fn item_picked_up(&self, host: &mut dyn Host, event: &mut ItemPickedUpEvent<'_>) -> Flow {
        guard(&self.id, "item_picked_up", Flow::Continue, || {
            self.pickup_flow(host, event.player, event.item, event.count)
        })
    }
    fn experience_orb_pickup(
        &self,
        host: &mut dyn Host,
        event: &mut ExperienceOrbPickupEvent<'_>,
    ) -> Flow {
        guard(&self.id, "experience_orb_pickup", Flow::Continue, || {
            self.pickup_flow(host, event.player, "minecraft:experience_orb", 1)
        })
    }
    fn sign_changed(&self, host: &mut dyn Host, event: &mut SignChangedEvent<'_>) -> Flow {
        guard(&self.id, "sign_changed", Flow::Continue, || {
            if self.sign.is_empty() {
                return Flow::Continue;
            }
            let player = host
                .server_players()
                .into_iter()
                .find(|p| p.uuid == event.player);
            let mut cancel = false;
            for f in &self.sign {
                let mut ctx = Ctx {
                    host,
                    uuid: Some(event.player),
                    player: player.clone(),
                    plugin_id: &self.id,
                    shared: &self.shared,
                };
                cancel |= f(&mut ctx, event.dimension, event.pos, event.lines);
            }
            if cancel { Flow::Cancel } else { Flow::Continue }
        })
    }
    fn explosion_detonate(&self, host: &mut dyn Host, event: &mut ExplosionDetonateEvent<'_>) {
        guard(&self.id, "explosion_detonate", (), || {
            for f in &self.explosion {
                f(
                    &mut Ctx {
                        host,
                        uuid: None,
                        player: None,
                        plugin_id: &self.id,
                        shared: &self.shared,
                    },
                    event.dimension,
                    event.center,
                    event.blocks,
                );
            }
        })
    }
    fn use_item_on(&self, host: &mut dyn Host, event: &mut UseItemOnEvent<'_>) -> Flow {
        guard(&self.id, "use_item_on", Flow::Continue, || {
            self.run_actions(
                host,
                Action::UseItemOn {
                    pos: event.pos,
                    face: event.face,
                    item: event.stack.item.to_string(),
                },
            )
        })
    }
    fn use_item(&self, host: &mut dyn Host, event: &mut UseItemEvent<'_>) -> Flow {
        guard(&self.id, "use_item", Flow::Continue, || {
            self.run_actions(
                host,
                Action::UseItem {
                    item: event.stack.item.to_string(),
                },
            )
        })
    }
    fn interact_entity(&self, host: &mut dyn Host, event: &mut InteractEntityEvent<'_>) -> Flow {
        guard(&self.id, "interact_entity", Flow::Continue, || {
            self.run_actions(
                host,
                Action::InteractEntity {
                    entity: event.entity,
                    kind: event.kind.to_owned(),
                },
            )
        })
    }
    fn fill_bucket(&self, host: &mut dyn Host, event: &mut FillBucketEvent<'_>) -> Flow {
        guard(&self.id, "fill_bucket", Flow::Continue, || {
            self.run_actions(host, Action::FillBucket { pos: event.pos })
        })
    }
    fn left_click_block(&self, host: &mut dyn Host, event: &mut LeftClickBlockEvent) -> Flow {
        guard(&self.id, "left_click_block", Flow::Continue, || {
            self.run_actions(host, Action::LeftClickBlock { pos: event.pos })
        })
    }
    fn container_opened(&self, host: &mut dyn Host, event: &mut ContainerOpenedEvent<'_>) -> Flow {
        guard(&self.id, "container_opened", Flow::Continue, || {
            if self.action.is_empty() {
                return Flow::Continue;
            }
            let player = host
                .server_players()
                .into_iter()
                .find(|p| p.uuid == event.player);
            let a = Action::ContainerOpen {
                kind: event.kind.to_owned(),
            };
            let mut cancel = false;
            for f in &self.action {
                cancel |= f(
                    &mut Ctx {
                        host,
                        uuid: Some(event.player),
                        player: player.clone(),
                        plugin_id: &self.id,
                        shared: &self.shared,
                    },
                    &a,
                );
            }
            if cancel { Flow::Cancel } else { Flow::Continue }
        })
    }
    fn player_command(&self, host: &mut dyn Host, event: &mut PlayerCommandEvent<'_>) -> Flow {
        guard(&self.id, "player_command", Flow::Continue, || {
            // Capture the command's live player before the Host-free teleport callback.
            let current = host
                .acting_player()
                .filter(|p| p.uuid == event.player)
                .or_else(|| {
                    host.server_players()
                        .into_iter()
                        .find(|p| p.uuid == event.player)
                });
            if let Some(current) = current {
                let mut s = self.shared.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(p) = s.cache.iter_mut().find(|p| p.uuid == current.uuid) {
                    *p = current;
                } else {
                    s.cache.push(current);
                }
            }
            if self.cmd_filter.is_empty() {
                return Flow::Continue;
            }
            let player = host
                .server_players()
                .into_iter()
                .find(|p| p.uuid == event.player)
                .or_else(|| {
                    self.shared
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .cache
                        .iter()
                        .find(|p| p.uuid == event.player)
                        .cloned()
                });
            for f in &self.cmd_filter {
                let r = f(
                    &mut Ctx {
                        host,
                        uuid: Some(event.player),
                        player: player.clone(),
                        plugin_id: &self.id,
                        shared: &self.shared,
                    },
                    event.name,
                    event.command.trim_start_matches('/'),
                );
                if let Some(text) = r {
                    if !text.is_empty() {
                        event.replies.push(legacy(&text));
                    }
                    return Flow::Cancel;
                }
            }
            Flow::Continue
        })
    }

    fn attack_entity(&self, host: &mut dyn Host, event: &mut AttackEntityEvent<'_>) -> Flow {
        guard(&self.id, "attack_entity", Flow::Continue, || {
            let player = host.acting_player();
            let mut cancel = false;
            for f in &self.attack {
                let mut ctx = Ctx {
                    host,
                    uuid: player.as_ref().map(|p| p.uuid),
                    player: player.clone(),
                    plugin_id: &self.id,
                    shared: &self.shared,
                };
                cancel |= f(&mut ctx, event.entity, event.kind);
            }
            if cancel { Flow::Cancel } else { Flow::Continue }
        })
    }

    fn player_chat(&self, event: &mut PlayerChatEvent<'_>) -> Flow {
        guard(&self.id, "player_chat", Flow::Continue, || {
            let c = Chat {
                player: event.player,
                name: event.name,
                message: event.message,
            };
            for f in &self.chat {
                if let ChatVerdict::Cancel(reply) = f(&c) {
                    if let Some(r) = reply {
                        event.replies.push(legacy(&r));
                    }
                    return Flow::Cancel;
                }
            }
            Flow::Continue
        })
    }

    fn living_incoming_damage(&self, event: &mut LivingIncomingDamageEvent<'_>) -> Flow {
        guard(&self.id, "living_incoming_damage", Flow::Continue, || {
            let (victim_player, attacker_player) = {
                let s = self.shared.lock().unwrap_or_else(|e| e.into_inner());
                let find = |id: i32| {
                    if id == 0 {
                        None
                    } else {
                        s.players.iter().find(|(e, _)| *e == id).map(|(_, u)| *u)
                    }
                };
                (find(event.entity), find(event.causing))
            };
            let d = Damage {
                entity: event.entity,
                kind: event.kind,
                damage_type: event.damage_type,
                attacker: event.causing,
                amount: event.amount,
                victim_player,
                attacker_player,
                shared: &self.shared,
            };
            if self.damage.iter().any(|f| f(&d)) {
                Flow::Cancel
            } else {
                Flow::Continue
            }
        })
    }

    fn entity_tame(&self, host: &mut dyn Host, event: &mut EntityTameEvent<'_>) {
        guard(&self.id, "entity_tame", (), || {
            for f in &self.tame {
                f(
                    &mut Ctx {
                        host,
                        uuid: None,
                        player: None,
                        plugin_id: &self.id,
                        shared: &self.shared,
                    },
                    event.kind,
                    event.owner,
                );
            }
        })
    }

    fn server_tick(&self, host: &mut dyn Host, event: &mut ServerTickEvent<'_>) {
        guard(&self.id, "server_tick", (), || {
            if !matches!(event.phase, ServerTickPhase::ServerTickStart { .. }) {
                return;
            }
            let tells: Vec<(u128, String)> = self
                .shared
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .tells
                .drain(..)
                .collect();
            for (u, m) in tells {
                if !host.tell(u, legacy(&m)) {
                    eprintln!(
                        "[hplug:{}] could not deliver a queued message to {u:032x} (offline?): {m}",
                        self.id
                    );
                }
            }
            self.refresh_players(host);
            let (tick, lines, due) = {
                let mut s = self.shared.lock().unwrap_or_else(|e| e.into_inner());
                s.tick += 1;
                let tick = s.tick;
                let lines: Vec<String> = s.console.drain(..).collect();
                let (due, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut s.later)
                    .into_iter()
                    .partition(|t| t.at <= tick);
                s.later = rest;
                // repeating tasks go back in the queue before they run, so they can cancel themselves
                for t in &due {
                    if let Some(p) = t.period {
                        s.later.push(Task {
                            at: tick + p,
                            id: t.id,
                            period: t.period,
                            run: t.run.clone(),
                        });
                    }
                }
                (tick, lines, due)
            };
            for l in lines {
                // A preceding queued command can change mode/permissions before a synchronous
                // upstream teleport hook, which cannot consult Host itself.
                self.refresh_players(host);
                // this plugin's own command, then vanilla's, then (through the relay) another plugin's
                if self.run_relayed(host, &l) {
                    continue;
                }
                if !host.run_command(&l) {
                    relay().push((l, tick + 2));
                }
            }
            // console lines other plugins queued for a command of this one (`reward take ..` from clans, `lp ..` from
            // rewards); unclaimed ones expire after a few plugins' ticks
            let claimed: Vec<String> = {
                let mut q = relay();
                let mut mine = Vec::new();
                q.retain(|(l, until)| {
                    if self.claims(l) {
                        mine.push(l.clone());
                        return false;
                    }
                    if tick > *until {
                        eprintln!("[hplug] console command failed: {l}");
                        return false;
                    }
                    true
                });
                mine
            };
            for l in claimed {
                self.run_relayed(host, &l);
            }
            for t in due {
                (t.run)(&mut Ctx {
                    host,
                    uuid: None,
                    player: None,
                    plugin_id: &self.id,
                    shared: &self.shared,
                });
            }
            for (n, f) in &self.every {
                if tick % n == 0 {
                    f(&mut Ctx {
                        host,
                        uuid: None,
                        player: None,
                        plugin_id: &self.id,
                        shared: &self.shared,
                    });
                }
            }
        })
    }
}

#[cfg(test)]
mod snbt_tests {
    /// The shape tools/migrate_clans.py writes for a Bukkit-YAML chest item (synthetic data).
    #[test]
    fn bukkit_item_snbt_round_trips() {
        let text = r#"{id:"minecraft:pink_shulker_box",count:1,components:{"minecraft:container":[{item:{components:{"minecraft:stored_enchantments":{"minecraft:protection":4}},id:"minecraft:enchanted_book"},slot:0}],"minecraft:custom_name":"'Loot'"}}"#;
        let s = super::item_from_snbt(text).expect("parses");
        assert_eq!(&*s.item, "minecraft:pink_shulker_box");
        assert_eq!(s.count, 1);
        let back = super::item_to_snbt(&s);
        assert!(back.contains("enchanted_book"), "{back}");
        let again = super::item_from_snbt(&back).expect("re-parses");
        assert_eq!(super::item_to_snbt(&again), back);
        let plain = super::item_from_snbt(r#"{id:"minecraft:diamond",count:12}"#).unwrap();
        assert_eq!(plain.count, 12);
    }

    /// Every chest stack of a migrated clans.json parses (`HPLUG_CLANS_JSON=<path> cargo test -p hplug`); skipped
    /// without the variable, so no real data is needed in git.
    #[test]
    fn migrated_chests_parse() {
        let Ok(p) = std::env::var("HPLUG_CLANS_JSON") else {
            return;
        };
        let v: super::Value = super::json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap();
        let (mut ok, mut bad) = (0, Vec::new());
        for (k, slots) in v["chests"].as_object().unwrap() {
            for s in slots.as_array().unwrap().iter().filter_map(|s| s.as_str()) {
                match super::item_from_snbt(s) {
                    Some(_) => ok += 1,
                    None => bad.push(format!("{k}: {}", &s[..s.len().min(80)])),
                }
            }
        }
        eprintln!("{ok} stacks parsed");
        assert!(bad.is_empty(), "{bad:?}");
    }
    /// Optional private real-copy proof, no engine builds on netcup:
    /// HPLUG_MERCHANTLINK_JSON=<scratch>/merchantlink.json cargo test -p hplug migrated_merchantlink_items_round_trip
    /// Run an already built matching SDK test executable directly when available.
    #[test]
    fn migrated_merchantlink_items_round_trip() {
        let Ok(path) = std::env::var("HPLUG_MERCHANTLINK_JSON") else {
            return;
        };
        let data: super::Value =
            super::json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let mut checked = 0;
        let mut check = |item: &super::Value| {
            let text = item["snbt"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| format!("{{id:{},count:{}}}", item["item"], item["count"]));
            let (raw, used) = horizon_world::nbt::parse_snbt(&text).expect("item SNBT parses");
            assert_eq!(used, text.len(), "trailing data on item {checked}");
            let stack = super::item_from_snbt(&text).expect("engine decodes item");
            assert_eq!(&*stack.item, item["item"].as_str().unwrap());
            assert_eq!(stack.count as u64, item["count"].as_u64().unwrap());
            let saved = horizon_client::save::item_to_nbt(&stack);
            // Strict: a normalised/dropped component also requires explicit review.
            assert!(
                raw == saved,
                "item {checked} changed during engine round trip; inspect privately"
            );
            let text2 = super::item_to_snbt(&stack);
            let again = super::item_from_snbt(&text2).expect("saved item re-parses");
            assert_eq!(super::item_to_snbt(&again), text2);
            checked += 1;
        };
        for profile in data["state"]["profiles"].as_object().unwrap().values() {
            for chest in profile["supply_chests"]
                .as_array()
                .unwrap()
                .iter()
                .chain((!profile["return_chest"].is_null()).then_some(&profile["return_chest"]))
            {
                for item in chest["contents"].as_array().unwrap() {
                    check(item);
                }
            }
            for listing in profile["listings"].as_object().unwrap().values() {
                check(&listing["product"]);
                if !listing["cost"].is_null() {
                    check(&listing["cost"]);
                }
            }
        }
        assert!(checked > 0, "no MerchantLink items checked");
        eprintln!("{checked} MerchantLink stacks round-tripped");
    }
}

/// Context for upstream's synchronous teleport hook, which has no Host access.
/// Player facts come from the last server tick; messages and display refreshes run next tick.
pub struct TeleportCtx<'a> {
    player: Option<NativeServerPlayer>,
    dimension: &'a str,
    plugin_id: &'a str,
    shared: &'a Mutex<Shared>,
}
impl TeleportCtx<'_> {
    pub fn try_player(&self) -> Option<&NativeServerPlayer> {
        self.player.as_ref()
    }
    fn read(&self, name: &str) -> Value {
        let dir = self
            .shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .config_dir
            .clone();
        dir.and_then(|d| std::fs::read_to_string(d.join("hplug").join(format!("{name}.json"))).ok())
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_else(|| json::json!({}))
    }
    pub fn get(&self, key: &str) -> Option<Value> {
        self.read(self.plugin_id).get(key).cloned()
    }
    pub fn has_perm(&self, uuid: u128, name: &str, node: &str) -> bool {
        if self
            .player
            .as_ref()
            .is_some_and(|p| p.uuid == uuid && p.permission >= 2)
        {
            return true;
        }
        let perms = self.read("permissions");
        let env = PermEnv {
            world: Some(self.dimension.to_owned()),
            server: perms
                .get("server")
                .and_then(Value::as_str)
                .unwrap_or("global")
                .to_owned(),
            now_secs: now_ms() / 1000,
        };
        with_perm_env(env, || perm_matches(&perms, uuid, name, node))
    }
    pub fn tell(&mut self, uuid: u128, text: impl AsRef<str>) {
        self.shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .tells
            .push_back((uuid, text.as_ref().to_owned()));
    }
    pub fn refresh_display_name(&mut self, uuid: u128, name: &str) {
        let name = name.to_owned();
        let mut s = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        s.next_task += 1;
        let (id, at) = (s.next_task, s.tick + 1);
        s.later.push(Task {
            at,
            id,
            period: None,
            run: Arc::new(move |c| c.refresh_display_name(uuid, &name)),
        });
    }
}
