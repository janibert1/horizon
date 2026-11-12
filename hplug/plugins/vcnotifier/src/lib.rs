//! Port of nl.jdries.vcnotifier.VCNotifier: after joining, tell Java players (after ~70 s) and Bedrock players
//! (names starting with ".", after 8 s) about Simple Voice Chat / voice.jdries.nl.
//! Java source: /home/jan/vc-notifier/src/main/java/nl/jdries/vcnotifier/VCNotifier.java
//! NOT ported (no Horizon equivalent, see PAPER_MAPPING.md):
//!  - Simple Voice Chat API: since 06/11 the `voicechat` plugin is the SVC server; a Java player who already has
//!    the mod (asked for a voice secret or announced the channels) gets no tip, as the Java did with the SVC API.
//!  - /tell override: `tell`, `msg`, `w` are vanilla names, Horizon rejects re-registering them, and Horizon's
//!    own /tell already works without a signed chat session (docs/server.md).
//!  - join/quit message suppression for `server.silentjoin` (no permission nodes; would need the `broadcast_message` hook).
//!  - the attack notifier (needs entity-id -> player mapping in the host-less `living_incoming_damage` hook).
//!
//! Delays are read from config/hplug/vcnotifier.json (`java-delay-ticks`, default 1400; `bedrock-delay-ticks`, default 160).
use hplug::Plugin;

const JAVA_MSG: &str = "§7🎤 §eTip:§7 Horizons SMP has §fproximity voice chat§7!\n§7Install §b§nSimple Voice Chat§r§7 (modrinth.com/plugin/simple-voice-chat) to hear and talk to nearby players in-game.";
const BEDROCK_MSG: &str = "§7🎤 §eTip:§7 Horizons SMP has §fvoice chat for Bedrock players§7!\n§7Open §bvoice.jdries.nl§7 in your browser and allow microphone access to join.";

pub fn plugin() -> Plugin {
    Plugin::new("vcnotifier").on_join(|ctx| {
        let (uuid, bedrock) = (ctx.player().uuid, ctx.player().name.starts_with('.'));
        let ticks = if bedrock {
            ctx.get("bedrock-delay-ticks").and_then(|v| v.as_u64()).unwrap_or(160)
        } else {
            ctx.get("java-delay-ticks").and_then(|v| v.as_u64()).unwrap_or(1400)
        };
        ctx.run_later(ticks, move |ctx| {
            // `if (!player.isOnline()) return;` : tell() reports an offline player by queueing, so check first
            if ctx.online().iter().any(|p| p.uuid == uuid) && (bedrock || !hplug_plugin_voicechat::has_voicechat(uuid)) {
                ctx.tell(uuid, if bedrock { BEDROCK_MSG } else { JAVA_MSG });
            }
        });
    })
}
