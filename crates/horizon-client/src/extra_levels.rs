//! Levels beyond vanilla's three: extra worlds a server owner or a mod adds (Bukkit's `WorldCreator`,
//! Multiverse's `mv create`, a datapack dimension's place in `MinecraftServer.levels`).
//!
//! An extra level has its own id (`namespace:path`), save folder (`dimensions/<namespace>/<path>/`, as every
//! level in 26.x), chunks, entities, block ticks and players, and its own generator: `void` (air, the
//! `minecraft:the_void` biome), `flat` (superflat layers) or `normal` (the Overworld's noise generator with its
//! own seed). Extra levels select a vanilla environment (normal, Nether or End), which supplies their
//! dimension type, shape, sky, beds, weather and mob rules. Nether and End portals do not lead out of an extra level.
//!
//! The table is append-only for the life of the process: a level's index (the game's `Dim`) never changes.
//! What is registered is saved in `<world>/horizon_levels.json` and registered again when the world opens,
//! before players are placed, so a player saved in an extra level comes back to it. Removing a level from the
//! file takes effect at the next start; its folder is left on disk.
use horizon_world::nbt::Tag;
use std::{path::Path, sync::RwLock};

/// Vanilla's levels, by index (0, 1, 2).
pub const VANILLA: [&str; 3] = [
    "minecraft:overworld",
    "minecraft:the_nether",
    "minecraft:the_end",
];

/// How an extra level's chunks are made.
#[derive(Clone, Debug, PartialEq)]
pub enum Generator {
    /// Air only, in `minecraft:the_void` (an arena, a lobby built by hand).
    Void,
    /// Superflat: `layers` bottom up (block id, height), in `biome`.
    Flat {
        biome: String,
        layers: Vec<(String, i32)>,
    },
    /// The Overworld's terrain generator; `seed` or the world's seed.
    Normal { seed: Option<i64> },
}

#[derive(Clone, Debug)]
pub struct ExtraLevel {
    pub id: &'static str,
    pub generator: Generator,
    /// The vanilla level whose dimension type and rules it has (Bukkit `World.Environment`): 0 Overworld,
    /// 1 Nether, 2 End. A `normal` generator makes that dimension's terrain.
    pub environment: u16,
}

static TABLE: RwLock<Vec<ExtraLevel>> = RwLock::new(Vec::new());

fn table() -> std::sync::RwLockReadGuard<'static, Vec<ExtraLevel>> {
    TABLE.read().unwrap_or_else(|e| e.into_inner())
}

/// The index of level `id`: 0..=2 for vanilla's, 3.. for extra ones.
pub fn index(id: &str) -> Option<u16> {
    if let Some(i) = VANILLA.iter().position(|v| *v == id) {
        return Some(i as u16);
    }
    table()
        .iter()
        .position(|l| l.id == id)
        .map(|i| (i + VANILLA.len()) as u16)
}

/// The id of the level at `index` (`minecraft:overworld` for an index nobody registered).
pub fn id(index: u16) -> &'static str {
    let i = index as usize;
    if i < VANILLA.len() {
        return VANILLA[i];
    }
    table().get(i - VANILLA.len()).map_or(VANILLA[0], |l| l.id)
}

/// The extra level at `index`, None for vanilla's.
pub fn get(index: u16) -> Option<ExtraLevel> {
    (index as usize)
        .checked_sub(VANILLA.len())
        .and_then(|i| table().get(i).cloned())
}

/// Every level's index, vanilla's first.
pub fn all() -> Vec<u16> {
    (0..(VANILLA.len() + table().len()) as u16).collect()
}

/// Every level's id, vanilla's first (the login packet's level list, command suggestions).
pub fn ids() -> Vec<String> {
    all().into_iter().map(|i| id(i).to_owned()).collect()
}

/// The extra levels' ids.
pub fn extra_ids() -> Vec<&'static str> {
    table().iter().map(|l| l.id).collect()
}

/// Whether `id` is an extra level.
pub fn is_extra(id: &str) -> bool {
    table().iter().any(|l| l.id == id)
}

/// The dimension type a level is shown with (`ClientboundLoginPacket` / `ClientboundRespawnPacket`):
/// an extra level is of the Overworld's type.
pub fn dimension_type(id: &str) -> &str {
    match table().iter().find(|l| l.id == id) {
        Some(l) => VANILLA[l.environment as usize],
        None => id,
    }
}

/// The environment (0 Overworld, 1 Nether, 2 End) of level `index`: its own for a vanilla level.
pub fn environment(index: u16) -> u16 {
    get(index).map_or(index.min(2), |l| l.environment)
}

/// `ResourceLocation` rules: `namespace:path`, lowercase a-z 0-9 `_ - .` (and `/` in the path).
pub fn valid_id(id: &str) -> bool {
    let Some((ns, path)) = id.split_once(':') else {
        return false;
    };
    let ok = |s: &str, slash: bool| {
        !s.is_empty()
            && s.chars().all(|c| {
                c.is_ascii_lowercase()
                    || c.is_ascii_digit()
                    || "_-.".contains(c)
                    || (slash && c == '/')
            })
    };
    ok(ns, false)
        && ok(path, true)
        && !path
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == "..")
}

/// Add an extra level (not yet opened: it opens when a player or a ticket enters it). Its index.
pub fn register(id: &str, generator: Generator) -> Result<u16, String> {
    register_with(id, generator, 0)
}

/// [`register`] with an environment (0 Overworld, 1 Nether, 2 End).
pub fn register_with(id: &str, generator: Generator, environment: u16) -> Result<u16, String> {
    if environment > 2 {
        return Err("environment is normal, nether or the_end".into());
    }
    if !valid_id(id) {
        return Err(format!("not a level id: {id} (namespace:path, lowercase)"));
    }
    if VANILLA.contains(&id) {
        return Err(format!("{id} is one of vanilla's levels"));
    }
    if let Generator::Flat { layers, .. } = &generator
        && layers.iter().any(|(_, h)| *h < 0 || *h > 4064)
    {
        return Err("a flat layer's height is 0 to 4064".into());
    }
    let mut t = TABLE.write().unwrap_or_else(|e| e.into_inner());
    if let Some(i) = t.iter().position(|l| l.id == id) {
        return Err(format!("{id} exists (index {})", i + VANILLA.len()));
    }
    if t.len() + VANILLA.len() >= u16::MAX as usize {
        return Err("too many levels".into());
    }
    let id: &'static str = Box::leak(id.to_owned().into_boxed_str());
    t.push(ExtraLevel {
        id,
        generator,
        environment,
    });
    Ok((t.len() - 1 + VANILLA.len()) as u16)
}

fn file(world: &Path) -> std::path::PathBuf {
    world.join("horizon_levels.json")
}

/// Register the levels of `<world>/horizon_levels.json` (missing file: none). Levels already registered are
/// skipped. The number registered, and a message for each entry that could not be read.
pub fn load(world: &Path) -> (usize, Vec<String>) {
    let Ok(text) = std::fs::read_to_string(file(world)) else {
        return (0, Vec::new());
    };
    let v: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => return (0, vec![format!("horizon_levels.json: {e}")]),
    };
    let (mut n, mut problems) = (0, Vec::new());
    for e in v
        .get("levels")
        .and_then(|l| l.as_array())
        .into_iter()
        .flatten()
    {
        let Some(id) = e.get("id").and_then(|x| x.as_str()) else {
            problems.push("horizon_levels.json: a level without an id".into());
            continue;
        };
        if index(id).is_some() {
            continue;
        }
        match generator_from_json(e)
            .and_then(|g| environment_from_json(e).and_then(|env| register_with(id, g, env)))
        {
            Ok(_) => n += 1,
            Err(m) => problems.push(format!("horizon_levels.json: {id}: {m}")),
        }
    }
    (n, problems)
}

/// Write every extra level to `<world>/horizon_levels.json` (through a temporary file).
pub fn save(world: &Path) -> std::io::Result<()> {
    let levels: Vec<serde_json::Value> = table()
        .iter()
        .map(|l| {
            let mut v = to_json(l.id, &l.generator);
            v["environment"] =
                serde_json::json!(["normal", "nether", "the_end"][l.environment as usize]);
            v
        })
        .collect();
    let text = serde_json::to_string_pretty(&serde_json::json!({ "levels": levels }))
        .unwrap_or_else(|_| "{}".into());
    let tmp = world.join("horizon_levels.json.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(tmp, file(world))
}

fn to_json(id: &str, g: &Generator) -> serde_json::Value {
    match g {
        Generator::Void => serde_json::json!({"id": id, "generator": "void"}),
        Generator::Flat { biome, layers } => serde_json::json!({
            "id": id, "generator": "flat", "biome": biome,
            "layers": layers.iter().map(|(b, h)| serde_json::json!({"block": b, "height": h})).collect::<Vec<_>>(),
        }),
        Generator::Normal { seed } => {
            serde_json::json!({"id": id, "generator": "normal", "seed": seed})
        }
    }
}

/// `"environment": "normal" | "nether" | "the_end"` (default normal; Bukkit names and dimension ids too).
pub fn environment_from_json(e: &serde_json::Value) -> Result<u16, String> {
    match e
        .get("environment")
        .and_then(|x| x.as_str())
        .unwrap_or("normal")
        .to_ascii_lowercase()
        .as_str()
    {
        "normal" | "overworld" | "minecraft:overworld" => Ok(0),
        "nether" | "the_nether" | "minecraft:the_nether" => Ok(1),
        "the_end" | "end" | "minecraft:the_end" => Ok(2),
        o => Err(format!("unknown environment {o}")),
    }
}

/// `{"generator": "void" | "flat" | "normal", "biome", "layers": [{"block", "height"}], "seed"}`.
pub fn generator_from_json(e: &serde_json::Value) -> Result<Generator, String> {
    match e
        .get("generator")
        .and_then(|g| g.as_str())
        .unwrap_or("void")
    {
        "void" => Ok(Generator::Void),
        "normal" => Ok(Generator::Normal {
            seed: e.get("seed").and_then(|s| s.as_i64()),
        }),
        "flat" => {
            let layers = match e.get("layers").and_then(|l| l.as_array()) {
                Some(list) => list
                    .iter()
                    .map(|l| {
                        let block = l
                            .get("block")
                            .and_then(|b| b.as_str())
                            .ok_or("a layer without a block")?;
                        let height = l.get("height").and_then(|h| h.as_i64()).unwrap_or(1) as i32;
                        let block = if block.contains(':') {
                            block.to_owned()
                        } else {
                            format!("minecraft:{block}")
                        };
                        Ok((block, height))
                    })
                    .collect::<Result<Vec<_>, String>>()?,
                // the Classic Flat preset
                None => vec![
                    ("minecraft:bedrock".into(), 1),
                    ("minecraft:dirt".into(), 2),
                    ("minecraft:grass_block".into(), 1),
                ],
            };
            let biome = e
                .get("biome")
                .and_then(|b| b.as_str())
                .unwrap_or("minecraft:plains")
                .to_owned();
            Ok(Generator::Flat { biome, layers })
        }
        other => Err(format!("unknown generator {other} (void, flat or normal)")),
    }
}

/// A superflat or void level's generator as the world-gen settings `FlatTerrain::from_settings` reads
/// (`dimensions.minecraft:overworld.generator`, type `minecraft:flat`). None for `normal`.
pub fn flat_settings(g: &Generator) -> Option<Tag> {
    let (biome, layers): (&str, Vec<Tag>) = match g {
        Generator::Void => ("minecraft:the_void", Vec::new()),
        Generator::Flat { biome, layers } => (
            biome.as_str(),
            layers
                .iter()
                .map(|(b, h)| {
                    Tag::compound([
                        ("block", Tag::String(b.as_str().into())),
                        ("height", Tag::Int(*h)),
                    ])
                })
                .collect(),
        ),
        Generator::Normal { .. } => return None,
    };
    let settings = Tag::compound([
        ("biome", Tag::String(biome.into())),
        ("layers", Tag::List(layers)),
        ("features", Tag::Byte(0)),
        ("lakes", Tag::Byte(0)),
        ("structure_overrides", Tag::List(Vec::new())),
    ]);
    let generator = Tag::compound([
        ("type", Tag::String("minecraft:flat".into())),
        ("settings", settings),
    ]);
    Some(Tag::compound([(
        "dimensions",
        Tag::compound([(
            "minecraft:overworld",
            Tag::compound([("generator", generator)]),
        )]),
    )]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_checked() {
        assert!(valid_id("horizons:arena"));
        assert!(valid_id("multiverse:worlds/creative"));
        assert!(!valid_id("Arena"));
        assert!(!valid_id("horizons:"));
        assert!(!valid_id("horizons:../x"));
    }

    #[test]
    fn environments_are_read() {
        assert_eq!(environment_from_json(&serde_json::json!({})).unwrap(), 0);
        assert_eq!(
            environment_from_json(&serde_json::json!({"environment": "NETHER"})).unwrap(),
            1
        );
        assert_eq!(
            environment_from_json(&serde_json::json!({"environment": "the_end"})).unwrap(),
            2
        );
        assert!(environment_from_json(&serde_json::json!({"environment": "moon"})).is_err());
        assert_eq!(environment(1), 1);
    }

    #[test]
    fn generators_round_trip() {
        let e = serde_json::json!({"id": "t:flat", "generator": "flat", "layers": [{"block": "stone", "height": 3}]});
        let g = generator_from_json(&e).unwrap();
        assert_eq!(
            g,
            Generator::Flat {
                biome: "minecraft:plains".into(),
                layers: vec![("minecraft:stone".into(), 3)]
            }
        );
        assert_eq!(generator_from_json(&to_json("t:flat", &g)).unwrap(), g);
        assert!(flat_settings(&Generator::Void).is_some());
        assert!(flat_settings(&Generator::Normal { seed: None }).is_none());
    }
}
