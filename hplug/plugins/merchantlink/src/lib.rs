//! Port of nl.jdries.merchantlink.MerchantLinkPlugin (Paper, Java):
//! Player-run merchant shops: stationary invulnerable villager owned by player,
//! linked to physical supply and return chests, with barter pricing, chest protection,
//! full trade simulation with rollback, and server chest GUI.
#![allow(unused_imports, unused_variables, dead_code)]
use hplug::{json, Action, Ctx, Damage, Direction, ItemStack, Message, Plugin};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;

const PREFIX: &str = "§6[MerchantLink] §r";
const MAX_CHEST_SLOTS: usize = 27;
const MAX_STACK_SIZE: u32 = 64;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct StoredPos {
    pub dim: String,
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct ChestPos {
    pub dim: String,
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LinkKind {
    Supply,
    Return,
}

impl LinkKind {
    pub fn display(&self) -> &'static str {
        match self {
            LinkKind::Supply => "Supply",
            LinkKind::Return => "Return",
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct StoredItem {
    pub item: String,
    pub count: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct StoredChest {
    pub pos: ChestPos,
    pub kind: LinkKind,
    #[serde(default)]
    pub contents: Vec<StoredItem>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct StoredListing {
    pub item_key: String,
    pub product: StoredItem,
    pub cost: Option<StoredItem>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct MerchantProfile {
    pub owner_uuid: String,
    pub owner_name: String,
    pub merchant_pos: Option<StoredPos>,
    #[serde(default)]
    pub supply_chests: Vec<StoredChest>,
    pub return_chest: Option<StoredChest>,
    #[serde(default)]
    pub listings: HashMap<String, StoredListing>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub profiles: HashMap<String, MerchantProfile>,
}

#[derive(Clone, Debug)]
pub enum MenuKind {
    Shop { owner_uuid: String, listing_keys: Vec<String> },
    Manage { owner_uuid: String, listing_keys: Vec<String> },
}

#[derive(Clone, Debug)]
pub struct OpenSession {
    pub window_id: i32,
    pub kind: MenuKind,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);
static PENDING_LINKS: Mutex<Option<HashMap<u128, LinkKind>>> = Mutex::new(None);
static OPEN_MENUS: Mutex<Option<HashMap<u128, OpenSession>>> = Mutex::new(None);

fn pending_links() -> std::sync::MutexGuard<'static, Option<HashMap<u128, LinkKind>>> {
    let mut g = PENDING_LINKS.lock().unwrap_or_else(|e| e.into_inner());
    if g.is_none() { *g = Some(HashMap::new()); }
    g
}

fn open_menus() -> std::sync::MutexGuard<'static, Option<HashMap<u128, OpenSession>>> {
    let mut g = OPEN_MENUS.lock().unwrap_or_else(|e| e.into_inner());
    if g.is_none() { *g = Some(HashMap::new()); }
    g
}

fn uuid_key(uuid: u128) -> String {
    format!("{uuid:032x}")
}

fn with_state<R>(ctx: &Ctx, f: impl FnOnce(&mut State) -> R) -> R {
    let mut g = STATE.lock().unwrap_or_else(|e| e.into_inner());
    if g.is_none() {
        *g = Some(ctx.get("state").and_then(|v| json::from_value(v).ok()).unwrap_or_default());
    }
    f(g.as_mut().unwrap())
}

fn save_state(ctx: &mut Ctx) {
    let val = STATE.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(|s| json::to_value(s).unwrap_or_default());
    if let Some(v) = val {
        let _ = ctx.set("state", v);
    }
}

fn get_profile<'a>(state: &'a mut State, uuid: u128, name: &str) -> &'a mut MerchantProfile {
    let k = uuid_key(uuid);
    state.profiles.entry(k.clone()).or_insert_with(|| MerchantProfile {
        owner_uuid: k,
        owner_name: name.to_string(),
        merchant_pos: None,
        supply_chests: vec![],
        return_chest: None,
        listings: HashMap::new(),
    })
}

fn find_profile_by_name_or_uuid<'a>(state: &'a mut State, identifier: &str) -> Option<&'a mut MerchantProfile> {
    let lower = identifier.to_lowercase();
    let clean_uuid = lower.replace('-', "");
    state.profiles.values_mut().find(|p| {
        p.owner_uuid == clean_uuid || p.owner_name.eq_ignore_ascii_case(&lower)
    })
}

fn count_stock(profile: &MerchantProfile, item_id: &str) -> u32 {
    let mut total = 0;
    for chest in &profile.supply_chests {
        for stack in &chest.contents {
            if stack.item == item_id {
                total += stack.count;
            }
        }
    }
    total
}

fn can_return_chest_fit(chest: &StoredChest, item_id: &str, count: u32) -> bool {
    let mut remaining = count;
    // Try to stack with existing stacks of the same item
    for stack in &chest.contents {
        if stack.item == item_id && stack.count < MAX_STACK_SIZE {
            let room = MAX_STACK_SIZE - stack.count;
            let moved = room.min(remaining);
            remaining -= moved;
            if remaining == 0 {
                return true;
            }
        }
    }
    // Check remaining empty slots
    let empty_slots = MAX_CHEST_SLOTS.saturating_sub(chest.contents.len());
    let needed_slots = remaining.div_ceil(MAX_STACK_SIZE) as usize;
    needed_slots <= empty_slots
}

fn add_to_chest(chest: &mut StoredChest, item_id: &str, count: u32) {
    let mut remaining = count;
    for stack in &mut chest.contents {
        if stack.item == item_id && stack.count < MAX_STACK_SIZE {
            let room = MAX_STACK_SIZE - stack.count;
            let moved = room.min(remaining);
            stack.count += moved;
            remaining -= moved;
            if remaining == 0 {
                return;
            }
        }
    }
    while remaining > 0 {
        let chunk = remaining.min(MAX_STACK_SIZE);
        chest.contents.push(StoredItem { item: item_id.to_string(), count: chunk });
        remaining -= chunk;
    }
}

fn remove_from_supply_chests(profile: &mut MerchantProfile, item_id: &str, count: u32) -> u32 {
    let mut remaining = count;
    for chest in &mut profile.supply_chests {
        for stack in &mut chest.contents {
            if stack.item == item_id {
                let moved = stack.count.min(remaining);
                stack.count -= moved;
                remaining -= moved;
                if remaining == 0 {
                    break;
                }
            }
        }
        chest.contents.retain(|s| s.count > 0);
        if remaining == 0 {
            break;
        }
    }
    count - remaining
}

/// Core trade logic with complete simulation and rollback.
fn perform_purchase(
    ctx: &mut Ctx,
    buyer_uuid: u128,
    owner_uuid: &str,
    item_or_key: &str,
    quantity: u32,
) -> Result<String, String> {
    if quantity == 0 {
        return Err("Quantity must be greater than zero.".into());
    }

    // Step 1: Pre-simulation checks on merchant state
    let (product_item, product_count, cost_item, cost_count, return_pos) = with_state(ctx, |s| {
        let prof = s.profiles.get_mut(owner_uuid)
            .ok_or_else(|| "Merchant profile not found.".to_string())?;

        let return_chest = prof.return_chest.as_ref()
            .ok_or_else(|| "This merchant has no return chest configured.".to_string())?;

        let listing = prof.listings.get(item_or_key)
            .or_else(|| prof.listings.values().find(|l| l.product.item == item_or_key))
            .ok_or_else(|| format!("No listing found for '{item_or_key}'."))?;

        let cost = listing.cost.as_ref()
            .ok_or_else(|| "This listing has no barter price set.".to_string())?;

        let total_product = (listing.product.count as u64) * (quantity as u64);
        let total_cost = (cost.count as u64) * (quantity as u64);
        if total_product > u32::MAX as u64 || total_cost > u32::MAX as u64 {
            return Err("Bulk trade amount is too large.".into());
        }

        let total_product = total_product as u32;
        let total_cost = total_cost as u32;

        let available = count_stock(prof, &listing.product.item);
        if available < total_product {
            return Err(format!("Insufficient stock for bulk purchase ({available}/{total_product} available)."));
        }

        if !can_return_chest_fit(return_chest, &cost.item, total_cost) {
            return Err("Return chest cannot hold payment for that bulk purchase.".into());
        }

        Ok((
            listing.product.item.clone(),
            total_product,
            cost.item.clone(),
            total_cost,
            return_chest.pos.clone(),
        ))
    })?;

    // Step 2: Pre-simulation checks on buyer inventory
    let buyer_has = ctx.host.count_items(&cost_item);
    if buyer_has < cost_count {
        return Err("You do not have the required barter items.".into());
    }

    let room = ctx.host.room_for(&product_item);
    if room < product_count {
        return Err("You do not have enough inventory space.".into());
    }

    // Step 3: Atomic execution with rollback
    let taken = ctx.host.take_items(&cost_item, cost_count);
    if taken < cost_count {
        // Rollback buyer payment
        if taken > 0 {
            ctx.host.give_items(&cost_item, taken);
        }
        return Err("Trade failed while applying inventory changes (rollback applied).".into());
    }

    let given = ctx.host.give_items(&product_item, product_count);
    if given < product_count {
        // Rollback buyer: take back given items and return payment
        if given > 0 {
            ctx.host.take_items(&product_item, given);
        }
        ctx.host.give_items(&cost_item, cost_count);
        return Err("Trade failed while applying inventory changes (rollback applied).".into());
    }

    // Step 4: Mutate merchant chests & persist
    with_state(ctx, |s| {
        if let Some(prof) = s.profiles.get_mut(owner_uuid) {
            remove_from_supply_chests(prof, &product_item, product_count);
            if let Some(ret) = &mut prof.return_chest {
                add_to_chest(ret, &cost_item, cost_count);
            }
        }
    });
    save_state(ctx);

    // Also attempt inserting into physical return chest in world if block exists
    ctx.host.insert_item([return_pos.x, return_pos.y, return_pos.z], Direction::Up, ItemStack::new(&cost_item, cost_count));

    Ok(format!("Purchased {product_count}x {product_item} for {cost_count}x {cost_item}."))
}

fn open_shop_gui(ctx: &mut Ctx, customer_uuid: u128, owner_uuid: &str) -> Option<i32> {
    let (title_text, items, keys) = with_state(ctx, |s| {
        let prof = s.profiles.get(owner_uuid)?;
        let title = format!("{}'s Merchant", prof.owner_name);
        let mut display_items: Vec<Option<ItemStack>> = Vec::with_capacity(54);
        let mut keys: Vec<String> = Vec::new();

        let mut sorted_listings: Vec<&StoredListing> = prof.listings.values().collect();
        sorted_listings.sort_by(|a, b| a.product.item.cmp(&b.product.item));

        for listing in sorted_listings {
            if listing.cost.is_some() {
                keys.push(listing.item_key.clone());
                display_items.push(Some(ItemStack::new(&listing.product.item, listing.product.count.max(1))));
            }
        }
        while display_items.len() < 54 {
            display_items.push(None);
        }
        Some((title, display_items, keys))
    })?;

    let win_opt = ctx.host.open_menu(customer_uuid, 6, Message::literal(&title_text), items);
    if let Some(w) = win_opt {
        open_menus().as_mut().unwrap().insert(customer_uuid, OpenSession {
            window_id: w,
            kind: MenuKind::Shop { owner_uuid: owner_uuid.to_string(), listing_keys: keys },
        });
    }
    win_opt
}

fn open_manage_gui(ctx: &mut Ctx, owner_uuid: u128) -> Option<i32> {
    let (items, keys) = with_state(ctx, |s| {
        let k = uuid_key(owner_uuid);
        let prof = s.profiles.get(&k)?;
        let mut display_items: Vec<Option<ItemStack>> = Vec::with_capacity(54);
        for _ in 0..54 { display_items.push(None); }

        // Slot 10: Supply chest 1
        if !prof.supply_chests.is_empty() {
            display_items[10] = Some(ItemStack::new("minecraft:chest", 1));
        }
        // Slot 12: Supply chest 2
        if prof.supply_chests.len() > 1 {
            display_items[12] = Some(ItemStack::new("minecraft:chest", 2));
        }
        // Slot 14: Return chest
        if prof.return_chest.is_some() {
            display_items[14] = Some(ItemStack::new("minecraft:ender_chest", 1));
        }
        // Slot 16: Remove merchant villager
        if prof.merchant_pos.is_some() {
            display_items[16] = Some(ItemStack::new("minecraft:villager_spawn_egg", 1));
        }

        let mut sorted_listings: Vec<&StoredListing> = prof.listings.values().collect();
        sorted_listings.sort_by(|a, b| a.product.item.cmp(&b.product.item));
        let mut keys = Vec::new();
        for (i, listing) in sorted_listings.into_iter().enumerate().take(27) {
            keys.push(listing.item_key.clone());
            display_items[27 + i] = Some(ItemStack::new(&listing.product.item, listing.product.count.max(1)));
        }

        Some((display_items, keys))
    })?;

    let win_opt = ctx.host.open_menu(owner_uuid, 6, Message::literal("MerchantLink Manage"), items);
    if let Some(w) = win_opt {
        open_menus().as_mut().unwrap().insert(owner_uuid, OpenSession {
            window_id: w,
            kind: MenuKind::Manage { owner_uuid: uuid_key(owner_uuid), listing_keys: keys },
        });
    }
    win_opt
}

fn handle_menu_click_event(ctx: &mut Ctx, clicker_uuid: u128, window: i32, slot: i32, button: i32, mode: i32) {
    let session = {
        let menus = open_menus();
        menus.as_ref().unwrap().get(&clicker_uuid).cloned()
    };

    let Some(session) = session else { return };
    if session.window_id != window { return; }

    match session.kind {
        MenuKind::Shop { owner_uuid, listing_keys } => {
            if slot < 0 || (slot as usize) >= listing_keys.len() {
                return;
            }
            let key = &listing_keys[slot as usize];
            let quantity: u32 = if mode == 1 {
                64
            } else if button == 1 {
                8
            } else {
                1
            };

            match perform_purchase(ctx, clicker_uuid, &owner_uuid, key, quantity) {
                Ok(msg) => { ctx.tell(clicker_uuid, format!("{PREFIX}{msg}")); }
                Err(err) => { ctx.tell(clicker_uuid, format!("{PREFIX}§c{err}")); }
            };

            // Refresh menu display after purchase
            let updated_items = with_state(ctx, |s| {
                let prof = s.profiles.get(&owner_uuid)?;
                let mut display_items: Vec<Option<ItemStack>> = Vec::with_capacity(54);
                for key in &listing_keys {
                    if let Some(l) = prof.listings.get(key) {
                        display_items.push(Some(ItemStack::new(&l.product.item, l.product.count.max(1))));
                    } else {
                        display_items.push(None);
                    }
                }
                while display_items.len() < 54 { display_items.push(None); }
                Some(display_items)
            });
            if let Some(items) = updated_items {
                ctx.host.set_menu_items(clicker_uuid, items);
            }
        }
        MenuKind::Manage { owner_uuid, listing_keys } => {
            if slot == 10 {
                // Unlink supply chest 1
                with_state(ctx, |s| {
                    if let Some(p) = s.profiles.get_mut(&owner_uuid)
                        && !p.supply_chests.is_empty() {
                            p.supply_chests.remove(0);
                        }
                });
                save_state(ctx);
                ctx.tell(clicker_uuid, format!("{PREFIX}Supply chest 1 unlinked."));
                open_manage_gui(ctx, clicker_uuid);
            } else if slot == 12 {
                // Unlink supply chest 2
                with_state(ctx, |s| {
                    if let Some(p) = s.profiles.get_mut(&owner_uuid)
                        && p.supply_chests.len() > 1 {
                            p.supply_chests.remove(1);
                        }
                });
                save_state(ctx);
                ctx.tell(clicker_uuid, format!("{PREFIX}Supply chest 2 unlinked."));
                open_manage_gui(ctx, clicker_uuid);
            } else if slot == 14 {
                // Unlink return chest
                with_state(ctx, |s| {
                    if let Some(p) = s.profiles.get_mut(&owner_uuid) {
                        p.return_chest = None;
                    }
                });
                save_state(ctx);
                ctx.tell(clicker_uuid, format!("{PREFIX}Return chest unlinked."));
                open_manage_gui(ctx, clicker_uuid);
            } else if slot == 16 {
                // Remove merchant
                with_state(ctx, |s| {
                    if let Some(p) = s.profiles.get_mut(&owner_uuid) {
                        p.merchant_pos = None;
                    }
                });
                save_state(ctx);
                ctx.console(format!("execute in minecraft:overworld run kill @e[type=villager,tag=ml_{owner_uuid}]"));
                ctx.tell(clicker_uuid, format!("{PREFIX}Merchant removed."));
                ctx.host.close_menu(clicker_uuid);
            } else if slot >= 27 {
                let idx = (slot - 27) as usize;
                if idx < listing_keys.len() {
                    let key = &listing_keys[idx];
                    if mode == 4 {
                        // Drop key (Q) -> delete listing
                        with_state(ctx, |s| {
                            if let Some(p) = s.profiles.get_mut(&owner_uuid) {
                                p.listings.remove(key);
                            }
                        });
                        save_state(ctx);
                        ctx.tell(clicker_uuid, format!("{PREFIX}Listing removed."));
                        open_manage_gui(ctx, clicker_uuid);
                    } else {
                        ctx.tell(clicker_uuid, format!("{PREFIX}Set price for {key} with /ml price {key} <cost_item> <cost_count>"));
                    }
                }
            }
        }
    }
}

/// (dimension, chest position) of every linked chest, for the host-less fire hooks.
static FIRE_GUARD: std::sync::Mutex<Vec<(String, [i32; 3])>> = std::sync::Mutex::new(Vec::new());

pub fn plugin() -> Plugin {
    Plugin::new("merchantlink")
        .command("ml [arg1:word] [arg2:word] [arg3:word] [arg4:word] [arg5:word]", 0, |ctx, args| {
            let Some(player) = ctx.try_player().cloned() else {
                return Err("This command can only be used by players.".into());
            };
            let sub = args.get(0).map(|s| s.to_lowercase()).unwrap_or_default();
            match sub.as_str() {
                "spawn" => {
                    let (already_spawned, pos_desc) = with_state(ctx, |s| {
                        let prof = get_profile(s, player.uuid, &player.name);
                        if let Some(pos) = &prof.merchant_pos {
                            (true, format!("{:.1}, {:.1}, {:.1}", pos.x, pos.y, pos.z))
                        } else {
                            let pos = StoredPos {
                                dim: player.dimension.clone(),
                                x: player.position[0],
                                y: player.position[1],
                                z: player.position[2],
                            };
                            prof.merchant_pos = Some(pos);
                            (false, String::new())
                        }
                    });

                    if already_spawned {
                        ctx.reply(format!("{PREFIX}You already have a merchant active at {pos_desc}."));
                        return Ok(());
                    }

                    save_state(ctx);
                    let tag = format!("ml_{:032x}", player.uuid);
                    let cmd = format!(
                        "execute in {} run summon villager {:.2} {:.2} {:.2} {{NoAI:1b,Invulnerable:1b,NoGravity:1b,Silent:1b,Tags:[\"{}\"],CustomName:'\"{} Merchant\"'}}",
                        player.dimension, player.position[0], player.position[1], player.position[2], tag, player.name
                    );
                    ctx.console(cmd);
                    ctx.reply(format!("{PREFIX}Merchant spawned."));
                }
                "remove" | "despawn" => {
                    let had_merchant = with_state(ctx, |s| {
                        let prof = get_profile(s, player.uuid, &player.name);
                        if prof.merchant_pos.is_some() {
                            prof.merchant_pos = None;
                            true
                        } else {
                            false
                        }
                    });

                    if !had_merchant {
                        ctx.reply(format!("{PREFIX}You do not have a merchant."));
                        return Ok(());
                    }

                    save_state(ctx);
                    let tag = format!("ml_{:032x}", player.uuid);
                    let cmd = format!("execute in {} run kill @e[tag={tag}]", player.dimension);
                    ctx.console(cmd);
                    ctx.reply(format!("{PREFIX}Merchant removed."));
                }
                "link" => {
                    let kind_str = args.get(1).map(|s| s.to_lowercase()).unwrap_or_default();
                    let kind = match kind_str.as_str() {
                        "supply" => LinkKind::Supply,
                        "return" => LinkKind::Return,
                        _ => return Err("Usage: /ml link <supply|return>".into()),
                    };
                    pending_links().as_mut().unwrap().insert(player.uuid, kind);
                    ctx.reply(format!("{PREFIX}Left-click the chest to link as your {} chest.", kind.display()));
                }
                "unlink" => {
                    let target = args.get(1).map(|s| s.to_lowercase()).unwrap_or_default();
                    let changed = with_state(ctx, |s| {
                        let prof = get_profile(s, player.uuid, &player.name);
                        match target.as_str() {
                            "supply1" | "supply" if !prof.supply_chests.is_empty() => {
                                prof.supply_chests.remove(0);
                                true
                            }
                            "supply2" if prof.supply_chests.len() > 1 => {
                                prof.supply_chests.remove(1);
                                true
                            }
                            "return" if prof.return_chest.is_some() => {
                                prof.return_chest = None;
                                true
                            }
                            _ => false,
                        }
                    });
                    if changed {
                        save_state(ctx);
                        ctx.reply(format!("{PREFIX}{target} chest unlinked."));
                    } else {
                        ctx.reply(format!("{PREFIX}No matching linked chest found for {target}."));
                    }
                }
                "price" => {
                    let product = args.get(1).ok_or("Usage: /ml price <product_item> <cost_item> <cost_count> [product_count]")?;
                    let cost = args.get(2).ok_or("Usage: /ml price <product_item> <cost_item> <cost_count> [product_count]")?;
                    let cost_count: u32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(1);
                    let product_count: u32 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(1);

                    with_state(ctx, |s| {
                        let prof = get_profile(s, player.uuid, &player.name);
                        prof.listings.insert(product.clone(), StoredListing {
                            item_key: product.clone(),
                            product: StoredItem { item: product.clone(), count: product_count },
                            cost: Some(StoredItem { item: cost.clone(), count: cost_count }),
                        });
                    });
                    save_state(ctx);
                    ctx.reply(format!("{PREFIX}Price set: {product_count}x {product} costs {cost_count}x {cost}."));
                }
                "stock" => {
                    let action = args.get(1).map(|s| s.to_lowercase()).unwrap_or_default();
                    match action.as_str() {
                        "add" => {
                            let target_chest = args.get(2).map(|s| s.to_lowercase()).unwrap_or_default();
                            let item = args.get(3).ok_or("Usage: /ml stock add <supply1|supply2> <item> <count>")?;
                            let count: u32 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(1);

                            let res = with_state(ctx, |s| {
                                let prof = get_profile(s, player.uuid, &player.name);
                                let chest_idx = if target_chest == "supply2" { 1 } else { 0 };
                                if chest_idx >= prof.supply_chests.len() {
                                    return Err("Supply chest not linked.".to_string());
                                }
                                add_to_chest(&mut prof.supply_chests[chest_idx], item, count);
                                // Automatically absorb listing
                                prof.listings.entry(item.clone()).or_insert_with(|| StoredListing {
                                    item_key: item.clone(),
                                    product: StoredItem { item: item.clone(), count: 1 },
                                    cost: None,
                                });
                                Ok(())
                            });
                            res?;
                            save_state(ctx);
                            ctx.reply(format!("{PREFIX}Added {count}x {item} to {target_chest}."));
                        }
                        "clear" => {
                            with_state(ctx, |s| {
                                let prof = get_profile(s, player.uuid, &player.name);
                                for c in &mut prof.supply_chests {
                                    c.contents.clear();
                                }
                            });
                            save_state(ctx);
                            ctx.reply(format!("{PREFIX}Cleared supply chest stock."));
                        }
                        _ => {
                            let stock_info = with_state(ctx, |s| {
                                let prof = get_profile(s, player.uuid, &player.name);
                                let mut lines = vec![];
                                for (i, c) in prof.supply_chests.iter().enumerate() {
                                    let items_desc: Vec<String> = c.contents.iter().map(|it| format!("{}x {}", it.count, it.item)).collect();
                                    lines.push(format!("Supply {}: [{}]", i + 1, items_desc.join(", ")));
                                }
                                if let Some(r) = &prof.return_chest {
                                    let items_desc: Vec<String> = r.contents.iter().map(|it| format!("{}x {}", it.count, it.item)).collect();
                                    lines.push(format!("Return: [{}]", items_desc.join(", ")));
                                }
                                lines.join(" | ")
                            });
                            ctx.reply(format!("{PREFIX}Stock: {stock_info}"));
                        }
                    }
                }
                "buy" => {
                    let merchant_target = args.get(1).ok_or("Usage: /ml buy <merchantOwner> <item> [amount]")?;
                    let item_target = args.get(2).ok_or("Usage: /ml buy <merchantOwner> <item> [amount]")?;
                    let amount: u32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(1);

                    let owner_uuid = with_state(ctx, |s| {
                        find_profile_by_name_or_uuid(s, merchant_target).map(|p| p.owner_uuid.clone())
                    }).ok_or_else(|| format!("Merchant '{merchant_target}' not found."))?;

                    let res = perform_purchase(ctx, player.uuid, &owner_uuid, item_target, amount);
                    match res {
                        Ok(msg) => ctx.reply(format!("{PREFIX}{msg}")),
                        Err(err) => return Err(err),
                    }
                }
                "manage" => {
                    if open_manage_gui(ctx, player.uuid).is_some() {
                        ctx.reply(format!("{PREFIX}Management GUI opened."));
                    } else {
                        ctx.reply(format!("{PREFIX}Manage: /ml link, /ml price, /ml stock, /ml remove"));
                    }
                }
                "shop" => {
                    let merchant_target = args.get(1).unwrap_or(&player.name);
                    let owner_uuid = with_state(ctx, |s| {
                        find_profile_by_name_or_uuid(s, merchant_target).map(|p| p.owner_uuid.clone())
                    }).ok_or_else(|| format!("Merchant '{merchant_target}' not found."))?;

                    if open_shop_gui(ctx, player.uuid, &owner_uuid).is_some() {
                        ctx.reply(format!("{PREFIX}Shop GUI opened."));
                    } else {
                        ctx.reply(format!("{PREFIX}Shop for {merchant_target} opened."));
                    }
                }
                "test" => {
                    let test_action = args.get(1).map(|s| s.to_lowercase()).unwrap_or_default();
                    match test_action.as_str() {
                        "click" => {
                            let slot: i32 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
                            let button: i32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(0);
                            let mode: i32 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(0);

                            let window = {
                                let menus = open_menus();
                                menus.as_ref().unwrap().get(&player.uuid).map(|s| s.window_id)
                            }.ok_or("No open menu session to click.")?;

                            handle_menu_click_event(ctx, player.uuid, window, slot, button, mode);
                        }
                        "fill_return" => {
                            // Fills return chest with 27 full stacks of cobblestone to simulate full return chest
                            with_state(ctx, |s| {
                                let prof = get_profile(s, player.uuid, &player.name);
                                if let Some(ret) = &mut prof.return_chest {
                                    ret.contents.clear();
                                    for _ in 0..27 {
                                        ret.contents.push(StoredItem { item: "minecraft:cobblestone".into(), count: 64 });
                                    }
                                }
                            });
                            save_state(ctx);
                            ctx.reply(format!("{PREFIX}Return chest filled to maximum capacity."));
                        }
                        _ => return Err("Usage: /ml test click <slot> [button] [mode] | /ml test fill_return".into()),
                    }
                }
                "status" => {
                    let desc = with_state(ctx, |s| {
                        let prof = get_profile(s, player.uuid, &player.name);
                        match &prof.merchant_pos {
                            Some(pos) => format!("Merchant active at {:.1}, {:.1}, {:.1} ({}) with {} supply chests, {} return chest, {} listings.",
                                pos.x, pos.y, pos.z, pos.dim, prof.supply_chests.len(), if prof.return_chest.is_some() { "1" } else { "0" }, prof.listings.len()),
                            None => "You do not have a merchant. Use /ml spawn to spawn one.".to_string(),
                        }
                    });
                    ctx.reply(format!("{PREFIX}{desc}"));
                }
                _ => {
                    ctx.reply(format!("{PREFIX}Commands: /ml spawn, /ml remove, /ml link supply|return, /ml unlink, /ml price, /ml stock, /ml buy, /ml manage, /ml status"));
                }
            }
            Ok(())
        })
        .on_action(|ctx, a| {
            let Some(player) = ctx.try_player().cloned() else { return false };
            match a {
                Action::LeftClickBlock { pos } => {
                    let pending = pending_links().as_mut().unwrap().remove(&player.uuid);
                    let Some(kind) = pending else { return false };

                    let block = ctx.host.block(*pos);
                    if !block.starts_with("minecraft:chest") && !block.starts_with("minecraft:trapped_chest") {
                        ctx.tell(player.uuid, format!("{PREFIX}You must left-click a chest to link it."));
                        pending_links().as_mut().unwrap().insert(player.uuid, kind);
                        return true;
                    }

                    let chest_pos = ChestPos {
                        dim: player.dimension.clone(),
                        x: pos[0],
                        y: pos[1],
                        z: pos[2],
                    };

                    let res = with_state(ctx, |s| {
                        // Check if already linked to this or any merchant
                        for p in s.profiles.values() {
                            for sc in &p.supply_chests {
                                if sc.pos == chest_pos {
                                    return Err(if p.owner_uuid == uuid_key(player.uuid) {
                                        "That chest is already linked to your merchant — left-click a different chest to link instead."
                                    } else {
                                        "That chest is already linked to another merchant — left-click a different chest to link instead."
                                    });
                                }
                            }
                            if let Some(rc) = &p.return_chest
                                && rc.pos == chest_pos {
                                    return Err(if p.owner_uuid == uuid_key(player.uuid) {
                                        "That chest is already linked to your merchant — left-click a different chest to link instead."
                                    } else {
                                        "That chest is already linked to another merchant — left-click a different chest to link instead."
                                    });
                                }
                        }

                        let prof = get_profile(s, player.uuid, &player.name);
                        match kind {
                            LinkKind::Return => {
                                prof.return_chest = Some(StoredChest {
                                    pos: chest_pos,
                                    kind: LinkKind::Return,
                                    contents: vec![],
                                });
                                Ok("Return chest linked.")
                            }
                            LinkKind::Supply => {
                                if prof.supply_chests.len() >= 2 {
                                    // Replace oldest
                                    prof.supply_chests.remove(0);
                                }
                                prof.supply_chests.push(StoredChest {
                                    pos: chest_pos,
                                    kind: LinkKind::Supply,
                                    contents: vec![],
                                });
                                Ok("Supply chest linked.")
                            }
                        }
                    });

                    match res {
                        Ok(msg) => {
                            save_state(ctx);
                            ctx.tell(player.uuid, format!("{PREFIX}{msg}"));
                        }
                        Err(err) => {
                            ctx.tell(player.uuid, format!("{PREFIX}{err}"));
                            pending_links().as_mut().unwrap().insert(player.uuid, kind);
                        }
                    }
                    true
                }
                Action::InteractEntity { entity, kind } => {
                    if kind == "minecraft:villager" {
                        // Check if player clicked a merchant
                        let matched_merchant = with_state(ctx, |s| {
                            // Find merchant near player
                            s.profiles.values().find_map(|p| {
                                if let Some(pos) = &p.merchant_pos
                                    && pos.dim == player.dimension {
                                        let dx = pos.x - player.position[0];
                                        let dy = pos.y - player.position[1];
                                        let dz = pos.z - player.position[2];
                                        if (dx*dx + dy*dy + dz*dz) < 36.0 {
                                            return Some(p.owner_uuid.clone());
                                        }
                                    }
                                None
                            })
                        });

                        if let Some(owner_uuid) = matched_merchant {
                            if owner_uuid == uuid_key(player.uuid) {
                                open_manage_gui(ctx, player.uuid);
                            } else {
                                open_shop_gui(ctx, player.uuid, &owner_uuid);
                            }
                            return true;
                        }
                    }
                    false
                }
                _ => false,
            }
        })
        .on_block_break(|ctx, pos| {
            let Some(player) = ctx.try_player().cloned() else { return false };
            let my_key = uuid_key(player.uuid);

            let check = with_state(ctx, |s| {
                for (k, prof) in &mut s.profiles {
                    for (i, c) in prof.supply_chests.iter().enumerate() {
                        if c.pos.x == pos[0] && c.pos.y == pos[1] && c.pos.z == pos[2] && c.pos.dim == player.dimension {
                            return if k == &my_key {
                                Some((true, LinkKind::Supply, i, k.clone()))
                            } else {
                                Some((false, LinkKind::Supply, i, k.clone()))
                            };
                        }
                    }
                    if let Some(rc) = &prof.return_chest
                        && rc.pos.x == pos[0] && rc.pos.y == pos[1] && rc.pos.z == pos[2] && rc.pos.dim == player.dimension {
                            return if k == &my_key {
                                Some((true, LinkKind::Return, 0, k.clone()))
                            } else {
                                Some((false, LinkKind::Return, 0, k.clone()))
                            };
                        }
                }
                None
            });

            if let Some((is_owner, kind, idx, owner_key)) = check {
                if !is_owner {
                    ctx.tell(player.uuid, format!("{PREFIX}No permission."));
                    return true;
                }
                with_state(ctx, |s| {
                    if let Some(prof) = s.profiles.get_mut(&owner_key) {
                        if kind == LinkKind::Supply {
                            if idx < prof.supply_chests.len() {
                                prof.supply_chests.remove(idx);
                            }
                        } else {
                            prof.return_chest = None;
                        }
                    }
                });
                save_state(ctx);
                ctx.tell(player.uuid, format!("{PREFIX}{} chest unlinked.", kind.display()));
            }
            false
        })
        // fire protection (06/11, was a gap): no fire placed at or next to a linked chest, nothing there burns away
        .on_fire(|f| {
            let linked = FIRE_GUARD.lock().unwrap_or_else(|e| e.into_inner());
            linked.iter().any(|(d, c)| *d == f.dimension && (0..3).all(|i| (c[i] - f.pos[i]).abs() <= 1))
        })
        .on_explosion(|ctx, dim, _center, blocks| {
            blocks.retain(|b| {
                let is_linked = with_state(ctx, |s| {
                    s.profiles.values().any(|p| {
                        p.supply_chests.iter().any(|c| c.pos.x == b[0] && c.pos.y == b[1] && c.pos.z == b[2] && c.pos.dim == dim)
                            || p.return_chest.as_ref().is_some_and(|c| c.pos.x == b[0] && c.pos.y == b[1] && c.pos.z == b[2] && c.pos.dim == dim)
                    })
                });
                !is_linked
            });
        })
        .on_menu_click(|ctx, window, slot, button, mode| {
            if let Some(uuid) = ctx.player_uuid() {
                handle_menu_click_event(ctx, uuid, window, slot, button, mode);
            }
        })
        .on_menu_close(|ctx, window| {
            if let Some(uuid) = ctx.player_uuid() {
                let mut menus = open_menus();
                if let Some(m) = menus.as_mut()
                    && let Some(sess) = m.get(&uuid)
                        && sess.window_id == window {
                            m.remove(&uuid);
                        }
            }
        })
        .every(20, |ctx| {
            // the fire hooks have no Host: keep the linked chest positions in a static for them
            let chests: Vec<(String, [i32; 3])> = with_state(ctx, |s| {
                s.profiles.values().flat_map(|p| {
                    p.supply_chests.iter().map(|c| &c.pos).chain(p.return_chest.as_ref().map(|c| &c.pos)).map(|c| (c.dim.clone(), [c.x, c.y, c.z])).collect::<Vec<_>>()
                }).collect()
            });
            *FIRE_GUARD.lock().unwrap_or_else(|e| e.into_inner()) = chests;
            let snaps: Vec<(String, String, StoredPos)> = with_state(ctx, |s| {
                s.profiles.values()
                    .filter_map(|p| p.merchant_pos.as_ref().map(|pos| (p.owner_uuid.clone(), pos.dim.clone(), pos.clone())))
                    .collect()
            });
            for (uuid_str, dim, pos) in snaps {
                let cmd = format!(
                    "execute in {} run tp @e[tag=ml_{}] {:.2} {:.2} {:.2}",
                    dim, uuid_str, pos.x, pos.y, pos.z
                );
                ctx.console(cmd);
            }
        })
        .on_attack(|_ctx, _entity_id, kind| {
            if kind == "minecraft:villager" {
                return true;
            }
            false
        })
        .on_damage(|d| {
            if d.kind == "minecraft:villager" {
                return true;
            }
            false
        })
}
