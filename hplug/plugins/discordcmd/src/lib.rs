//! Port of discord-cmd (Paper, 27 lines): `/discord` prints the invite link as a clickable message.
use hplug::{Message, Plugin, link};

const INVITE: &str = "https://discord.gg/zPZBPbQgx";

pub fn plugin() -> Plugin {
    Plugin::new("discordcmd").command("discord", 0, |ctx, _| {
        let mut msg = Message::colored("💬 Join us on Discord: ", 0xAAAAAA);
        msg.push(link("discord.gg/zPZBPbQgx", INVITE));
        ctx.reply_text(msg);
        Ok(())
    })
}
