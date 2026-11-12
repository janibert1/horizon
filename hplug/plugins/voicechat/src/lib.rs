//! Simple Voice Chat, server half, for Horizon: players with the Simple Voice Chat client mod (Fabric/NeoForge,
//! compatibility version 21 = SVC 2.6.x for 26.x; config `compatibility_version`) talk by proximity, as with the SVC
//! Paper plugin. Wire format read from henkelmax/simple-voice-chat (common/.../net and voice/common):
//! * plugin channels (engine hook `plugin_message` + `Host::send_plugin_message`): `voicechat:request_secret` ->
//!   `voicechat:secret` (AES key, connection id, UDP port, codec, MTU, distance, keep-alive, groups, voice host),
//!   `voicechat:client_state` (muted/disabled) -> `voicechat:state` / `voicechat:states` / `voicechat:remove_state`.
//!   The server announces its channels with `minecraft:register` on join (Fabric only sends on registered channels).
//! * UDP (`port`, default 24454, `bind`, default 0.0.0.0): 0xFF, connection id, AES-128-GCM payload (12-byte IV,
//!   128-bit tag); Authenticate -> AuthenticateAck(nonce = HMAC-SHA256(connection id, address)), ConnectionCheck(nonce)
//!   -> ConnectionCheckAck, Mic -> PlayerSound to everyone in the same dimension within the distance (whisper: 24),
//!   KeepAlive both ways (a connection silent for 10 keep-alives is dropped).
//!   NOT (yet): groups (`enable_groups` is sent as false), the SVC server API for other plugins beyond
//!   `has_voicechat`/`is_connected`, spectator possession, voice categories (location sounds of plugins), recording
//!   permission (always allowed). `voicechat.speak` / `voicechat.listen` are honoured as SVC's defaults: everyone
//!   has them unless permissions.json sets the node false (refreshed every 2 s). Voice packets need the UDP port open in the firewall and, behind Velocity, `voice_host` set to the
//!   public host:port (the proxy does not carry UDP).
use aes_gcm::{Aes128Gcm, KeyInit, Nonce, aead::Aead};
use hmac::{Hmac, Mac};
use hplug::{Ctx, Plugin};
use std::collections::HashMap;
use std::net::{SocketAddr, UdpSocket};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const DEFAULT_COMPAT: i32 = 21;
const MAX_OPUS: usize = 1275;
const CHANNELS_FROM_CLIENT: &[&str] = &["voicechat:request_secret", "voicechat:client_state"];
const CHANNELS_TO_CLIENT: &[&str] = &[
    "voicechat:secret",
    "voicechat:state",
    "voicechat:states",
    "voicechat:remove_state",
];

// ---------- FriendlyByteBuf ----------
#[derive(Default)]
struct W(Vec<u8>);
impl W {
    fn u8(&mut self, b: u8) -> &mut Self {
        self.0.push(b);
        self
    }
    fn bool(&mut self, b: bool) -> &mut Self {
        self.u8(b as u8)
    }
    fn i32(&mut self, v: i32) -> &mut Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }
    fn i64(&mut self, v: i64) -> &mut Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }
    fn f32(&mut self, v: f32) -> &mut Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }
    fn f64(&mut self, v: f64) -> &mut Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }
    fn uuid(&mut self, u: u128) -> &mut Self {
        self.0.extend_from_slice(&u.to_be_bytes());
        self
    }
    fn varint(&mut self, mut v: u32) -> &mut Self {
        loop {
            if v & !0x7F == 0 {
                self.0.push(v as u8);
                return self;
            }
            self.0.push((v as u8 & 0x7F) | 0x80);
            v >>= 7;
        }
    }
    fn bytes(&mut self, b: &[u8]) -> &mut Self {
        self.varint(b.len() as u32);
        self.0.extend_from_slice(b);
        self
    }
    fn utf(&mut self, s: &str) -> &mut Self {
        self.bytes(s.as_bytes())
    }
    fn raw(&mut self, b: &[u8]) -> &mut Self {
        self.0.extend_from_slice(b);
        self
    }
}
struct R<'a>(&'a [u8]);
impl R<'_> {
    fn take(&mut self, n: usize) -> Option<&[u8]> {
        if self.0.len() < n {
            return None;
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Some(a)
    }
    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }
    fn bool(&mut self) -> Option<bool> {
        self.u8().map(|b| b != 0)
    }
    fn i32(&mut self) -> Option<i32> {
        self.take(4)
            .map(|b| i32::from_be_bytes(b.try_into().unwrap()))
    }
    fn i64(&mut self) -> Option<i64> {
        self.take(8)
            .map(|b| i64::from_be_bytes(b.try_into().unwrap()))
    }
    fn uuid(&mut self) -> Option<u128> {
        self.take(16)
            .map(|b| u128::from_be_bytes(b.try_into().unwrap()))
    }
    fn varint(&mut self) -> Option<u32> {
        let mut v = 0u32;
        for i in 0..5 {
            let b = self.u8()?;
            v |= ((b & 0x7F) as u32) << (7 * i);
            if b & 0x80 == 0 {
                return Some(v);
            }
        }
        None
    }
    fn bytes(&mut self, max: usize) -> Option<Vec<u8>> {
        let n = self.varint()? as usize;
        if n > max {
            return None;
        }
        self.take(n).map(<[u8]>::to_vec)
    }
}

fn random<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    if getrandom::getrandom(&mut b).is_err() {
        // never expected on Linux/macOS; fall back to std's OS-seeded hasher
        use std::hash::{BuildHasher, Hasher};
        for (i, c) in b.chunks_mut(8).enumerate() {
            let mut h = std::collections::hash_map::RandomState::new().build_hasher();
            h.write_usize(i);
            c.copy_from_slice(&h.finish().to_le_bytes()[..c.len()]);
        }
    }
    b
}

// ---------- shared state ----------
#[derive(Clone)]
struct Secret {
    player: u128,
    key: [u8; 16],
}
#[derive(Clone)]
struct Conn {
    addr: SocketAddr,
    last_ack: Instant,
}
#[derive(Clone, Default)]
struct PState {
    name: String,
    muted: bool,
    disabled: bool,
    disconnected: bool,
}
#[derive(Clone)]
struct Pos {
    dim: String,
    pos: [f64; 3],
}
#[derive(Default)]
struct Voice {
    socket: Option<Arc<UdpSocket>>,
    port: u16,
    distance: f64,
    whisper: f64,
    keep_alive_ms: u64,
    nonce_key: [u8; 32],
    secrets: HashMap<u128, Secret>,  // connection id -> secret
    by_player: HashMap<u128, u128>,  // player -> connection id
    conns: HashMap<u128, Conn>,      // player -> validated address
    states: HashMap<u128, PState>,   // every online player's SVC state
    compatible: HashMap<u128, bool>, // player -> sent a compatible request_secret
    announced: HashMap<u128, bool>, // player's client announced voicechat channels (minecraft:register)
    positions: HashMap<u128, Pos>,
    events: Vec<(u128, bool)>, // (player, connected) for the tick to broadcast
    no_speak: std::collections::HashSet<u128>, // `voicechat.speak` set false
    no_listen: std::collections::HashSet<u128>, // `voicechat.listen` set false
    started: bool,
}
static VOICE: Mutex<Option<Voice>> = Mutex::new(None);
fn vs<T>(f: impl FnOnce(&mut Voice) -> T) -> T {
    let mut g = VOICE.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(Voice::default))
}

/// The player has the Simple Voice Chat mod (it asked for a compatible secret, or announced the channels).
pub fn has_voicechat(uuid: u128) -> bool {
    vs(|v| {
        v.compatible.get(&uuid).copied().unwrap_or(false)
            || v.announced.get(&uuid).copied().unwrap_or(false)
    })
}
/// The player's voice connection is up (UDP validated).
pub fn is_connected(uuid: u128) -> bool {
    vs(|v| v.conns.contains_key(&uuid))
}

fn nonce(key: &[u8; 32], conn: u128, addr: &SocketAddr) -> [u8; 16] {
    let mut mac = <Hmac<sha2::Sha256> as Mac>::new_from_slice(key).expect("hmac key");
    mac.update(&conn.to_be_bytes());
    match addr {
        SocketAddr::V4(a) => mac.update(&a.ip().octets()),
        SocketAddr::V6(a) => mac.update(&a.ip().octets()),
    }
    mac.update(&(addr.port() as i32).to_be_bytes());
    let out = mac.finalize().into_bytes();
    out[..16].try_into().unwrap()
}
fn encrypt(key: &[u8; 16], plain: &[u8]) -> Option<Vec<u8>> {
    let iv: [u8; 12] = random();
    let c = Aes128Gcm::new_from_slice(key)
        .ok()?
        .encrypt(Nonce::from_slice(&iv), plain)
        .ok()?;
    let mut out = iv.to_vec();
    out.extend_from_slice(&c);
    Some(out)
}
fn decrypt(key: &[u8; 16], data: &[u8]) -> Option<Vec<u8>> {
    if data.len() < 12 + 16 {
        return None;
    }
    Aes128Gcm::new_from_slice(key)
        .ok()?
        .decrypt(Nonce::from_slice(&data[..12]), &data[12..])
        .ok()
}
/// Server -> client datagram: 0xFF, then the encrypted (type + body) as a byte array.
fn send_udp(sock: &UdpSocket, key: &[u8; 16], addr: SocketAddr, ty: u8, body: &[u8]) {
    let mut plain = vec![ty];
    plain.extend_from_slice(body);
    if let Some(enc) = encrypt(key, &plain) {
        let mut w = W::default();
        w.u8(0xFF).bytes(&enc);
        let _ = sock.send_to(&w.0, addr);
    }
}

fn udp_loop(sock: Arc<UdpSocket>) {
    let mut buf = [0u8; 4096];
    loop {
        let Ok((n, addr)) = sock.recv_from(&mut buf) else {
            continue;
        };
        let mut r = R(&buf[..n]);
        if r.u8() != Some(0xFF) {
            continue;
        }
        let Some(conn_id) = r.uuid() else { continue };
        let Some(enc) = r.bytes(2048) else { continue };
        let Some((secret, nonce_key)) =
            vs(|v| v.secrets.get(&conn_id).cloned().map(|s| (s, v.nonce_key)))
        else {
            continue;
        };
        let Some(plain) = decrypt(&secret.key, &enc) else {
            continue;
        };
        let mut p = R(&plain);
        let Some(ty) = p.u8() else { continue };
        match ty {
            0x5 => {
                // Authenticate -> AuthenticateAck(nonce)
                let n = nonce(&nonce_key, conn_id, &addr);
                send_udp(&sock, &secret.key, addr, 0x6, &n);
            }
            0x9 => {
                // ConnectionCheck(nonce) -> the connection is validated from this address
                let Some(got) = p.take(16) else { continue };
                if got != nonce(&nonce_key, conn_id, &addr) {
                    continue;
                }
                let fresh = vs(|v| {
                    let fresh = !v.conns.contains_key(&secret.player);
                    v.conns.insert(
                        secret.player,
                        Conn {
                            addr,
                            last_ack: Instant::now(),
                        },
                    );
                    if fresh {
                        v.events.push((secret.player, true));
                    }
                    fresh
                });
                if fresh {
                    eprintln!("[voicechat] {:032x} connected from {addr}", secret.player);
                }
                send_udp(&sock, &secret.key, addr, 0xA, &[]);
            }
            0x8 => vs(|v| {
                if let Some(c) = v.conns.get_mut(&secret.player)
                    && c.addr == addr
                {
                    c.last_ack = Instant::now();
                }
            }),
            0x1 => {
                // Mic: data, sequence, whispering
                let (Some(data), Some(seq), Some(whisper)) = (p.bytes(MAX_OPUS), p.i64(), p.bool())
                else {
                    continue;
                };
                let targets = vs(|v| {
                    let sender = secret.player;
                    if v.conns.get(&sender).is_none_or(|c| c.addr != addr) {
                        return Vec::new();
                    }
                    if v.states
                        .get(&sender)
                        .is_some_and(|s| s.disabled || s.disconnected)
                    {
                        return Vec::new();
                    }
                    if v.no_speak.contains(&sender) {
                        return Vec::new();
                    }
                    let Some(me) = v.positions.get(&sender).cloned() else {
                        return Vec::new();
                    };
                    let dist = if whisper { v.whisper } else { v.distance };
                    let range = (v.distance + 1.0).max(dist);
                    v.conns
                        .iter()
                        .filter(|(u, _)| **u != sender && !v.no_listen.contains(*u))
                        .filter_map(|(u, c)| {
                            let p = v.positions.get(u)?;
                            let d2 = (0..3).map(|i| (p.pos[i] - me.pos[i]).powi(2)).sum::<f64>();
                            let st = v.states.get(u)?;
                            (p.dim == me.dim
                                && d2 <= range * range
                                && !st.disabled
                                && !st.disconnected)
                                .then(|| {
                                    let key = v
                                        .by_player
                                        .get(u)
                                        .and_then(|cid| v.secrets.get(cid))
                                        .map(|s| s.key)?;
                                    Some((c.addr, key, dist as f32))
                                })?
                        })
                        .collect::<Vec<_>>()
                });
                for (to, key, dist) in targets {
                    let mut w = W::default();
                    w.uuid(secret.player)
                        .uuid(secret.player)
                        .bytes(&data)
                        .i64(seq)
                        .f32(dist)
                        .u8(if whisper { 0b1 } else { 0 });
                    send_udp(&sock, &key, to, 0x2, &w.0);
                }
            }
            _ => {} // Ping (0x7) and anything else: nothing to do on the server
        }
    }
}

fn keepalive_loop(sock: Arc<UdpSocket>) {
    loop {
        let every = vs(|v| v.keep_alive_ms).max(1000);
        std::thread::sleep(Duration::from_millis(every));
        let (send, dropped): (Vec<(SocketAddr, [u8; 16])>, Vec<u128>) = vs(|v| {
            let timeout = Duration::from_millis(every * 10);
            let dropped: Vec<u128> = v
                .conns
                .iter()
                .filter(|(_, c)| c.last_ack.elapsed() > timeout)
                .map(|(u, _)| *u)
                .collect();
            for u in &dropped {
                v.conns.remove(u);
                v.events.push((*u, false));
            }
            let send = v
                .conns
                .iter()
                .filter_map(|(u, c)| {
                    Some((
                        c.addr,
                        v.by_player.get(u).and_then(|cid| v.secrets.get(cid))?.key,
                    ))
                })
                .collect();
            (send, dropped)
        });
        for (addr, key) in send {
            send_udp(&sock, &key, addr, 0x8, &[]);
        }
        for u in dropped {
            eprintln!("[voicechat] {u:032x} timed out");
        }
    }
}

fn start(ctx: &Ctx) {
    if vs(|v| std::mem::replace(&mut v.started, true)) {
        return;
    }
    if !ctx.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true) {
        return;
    }
    let port = ctx.get("port").and_then(|v| v.as_u64()).unwrap_or(24454) as u16;
    let bind = ctx
        .get("bind")
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| "0.0.0.0".into());
    let sock = match UdpSocket::bind((bind.as_str(), port)) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            eprintln!("[voicechat] cannot bind UDP {bind}:{port}: {e}");
            return;
        }
    };
    vs(|v| {
        v.socket = Some(sock.clone());
        v.port = port;
        v.distance = ctx
            .get("max_voice_distance")
            .and_then(|x| x.as_f64())
            .unwrap_or(48.0);
        v.whisper = ctx
            .get("whisper_distance")
            .and_then(|x| x.as_f64())
            .unwrap_or(24.0);
        v.keep_alive_ms = ctx
            .get("keep_alive")
            .and_then(|x| x.as_u64())
            .unwrap_or(1000)
            .max(1000);
        v.nonce_key = random();
    });
    eprintln!("[voicechat] voice server on UDP {bind}:{port}");
    let s2 = sock.clone();
    std::thread::Builder::new()
        .name("voicechat-udp".into())
        .spawn(move || udp_loop(sock))
        .ok();
    std::thread::Builder::new()
        .name("voicechat-keepalive".into())
        .spawn(move || keepalive_loop(s2))
        .ok();
}

fn state_bytes(w: &mut W, uuid: u128, s: &PState) {
    let flags = (s.muted as u8) | ((s.disabled as u8) << 2) | ((s.disconnected as u8) << 4);
    w.u8(flags).uuid(uuid).utf(&s.name);
}
/// One player's state to every SVC client online.
fn broadcast_state(ctx: &mut Ctx, uuid: u128) {
    let (state, to) = vs(|v| {
        (
            v.states.get(&uuid).cloned(),
            v.compatible
                .iter()
                .filter(|(_, ok)| **ok)
                .map(|(u, _)| *u)
                .collect::<Vec<_>>(),
        )
    });
    let Some(state) = state else { return };
    let mut w = W::default();
    state_bytes(&mut w, uuid, &state);
    for r in to {
        let _ = ctx
            .host
            .send_custom_payload_to_player(r, "voicechat:state", &w.0);
    }
}

fn on_message(ctx: &mut Ctx, channel: &str, data: &[u8]) {
    let Some(player) = ctx.player_uuid() else {
        return;
    };
    match channel {
        "minecraft:register" => {
            let has = data
                .split(|b| *b == 0)
                .any(|c| c.starts_with(b"voicechat:"));
            vs(|v| {
                v.announced.insert(player, has);
            });
        }
        "voicechat:request_secret" => {
            let want = ctx
                .get("compatibility_version")
                .and_then(|v| v.as_i64())
                .unwrap_or(DEFAULT_COMPAT as i64) as i32;
            let Some(version) = R(data).i32() else { return };
            if version != want {
                vs(|v| {
                    v.compatible.insert(player, false);
                });
                ctx.tell(player, format!("§cYour Simple Voice Chat version is not compatible with this server (yours: {version}, server: {want}). Please install the matching version."));
                return;
            }
            let Some(port) = vs(|v| v.socket.as_ref().map(|_| v.port)) else {
                ctx.tell(player, "§cVoice chat is not running on this server.");
                return;
            };
            // one secret per player and session (SVC ignores a second request)
            let created = vs(|v| {
                if v.by_player.contains_key(&player) {
                    return None;
                }
                let key: [u8; 16] = random();
                let conn = u128::from_be_bytes(random::<16>());
                v.secrets.insert(conn, Secret { player, key });
                v.by_player.insert(player, conn);
                v.compatible.insert(player, true);
                Some((key, conn, v.distance))
            });
            let Some((key, conn, distance)) = created else {
                return;
            };
            let host = ctx
                .get("voice_host")
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_default();
            let mtu = ctx
                .get("mtu_size")
                .and_then(|v| v.as_i64())
                .unwrap_or(MAX_OPUS as i64)
                .clamp(512, 2048) as i32;
            let codec = match ctx
                .get("codec")
                .and_then(|v| v.as_str().map(str::to_uppercase))
                .as_deref()
            {
                Some("AUDIO") => 1,
                Some("RESTRICTED_LOWDELAY") => 2,
                _ => 0,
            };
            let keep = vs(|v| v.keep_alive_ms) as i32;
            let mut w = W::default();
            w.raw(&key)
                .uuid(conn)
                .i32(port as i32)
                .uuid(player)
                .u8(codec)
                .i32(mtu)
                .f64(distance)
                .i32(keep)
                .bool(false)
                .utf(&host)
                .bool(true);
            let _ = ctx
                .host
                .send_custom_payload_to_player(player, "voicechat:secret", &w.0);
            // everyone's state to the newcomer (PlayerStateManager.onPlayerCompatibilityCheckSucceeded)
            let all: Vec<(u128, PState)> = vs(|v| {
                v.states
                    .iter()
                    .filter(|(u, _)| **u != player)
                    .map(|(u, s)| (*u, s.clone()))
                    .collect()
            });
            let mut w = W::default();
            w.i32(all.len() as i32);
            for (u, s) in &all {
                state_bytes(&mut w, *u, s);
            }
            let _ = ctx
                .host
                .send_custom_payload_to_player(player, "voicechat:states", &w.0);
        }
        "voicechat:client_state" => {
            let mut r = R(data);
            let (Some(muted), Some(disabled)) = (r.bool(), r.bool()) else {
                return;
            };
            vs(|v| {
                if let Some(s) = v.states.get_mut(&player) {
                    s.muted = muted;
                    s.disabled = disabled;
                }
            });
            broadcast_state(ctx, player);
        }
        _ => {}
    }
}

pub fn plugin() -> Plugin {
    Plugin::new("voicechat")
        .command("voicechat [sub:word]", 0, |ctx, _| {
            let online = ctx.online();
            let (port, conns) = vs(|v| (v.socket.as_ref().map(|_| v.port), v.conns.len()));
            ctx.reply(format!(
                "§7Voice chat: {} §7connected §f{conns}§7/{}",
                match port {
                    Some(p) => format!("§aUDP {p}"),
                    None => "§cnot running".into(),
                },
                online.len()
            ));
            if ctx.op_level() >= 2 {
                for p in &online {
                    ctx.reply(format!(
                        "§7- {}: {}",
                        p.name,
                        if is_connected(p.uuid) {
                            "§aconnected"
                        } else if has_voicechat(p.uuid) {
                            "§emod, not connected"
                        } else {
                            "§8no mod"
                        }
                    ));
                }
            }
            Ok(())
        })
        .alias("vc")
        .on_plugin_message(on_message)
        .on_join(|ctx| {
            let p = ctx.player().clone();
            vs(|v| {
                v.states.insert(
                    p.uuid,
                    PState {
                        name: p.name.clone(),
                        disconnected: true,
                        ..Default::default()
                    },
                );
            });
            let u = p.uuid;
            // channel announcement + this player's (disconnected) state for the others: outside the player's hook
            ctx.run_later(1, move |c| {
                let mut reg = Vec::new();
                for (i, ch) in CHANNELS_FROM_CLIENT
                    .iter()
                    .chain(CHANNELS_TO_CLIENT)
                    .enumerate()
                {
                    if i > 0 {
                        reg.push(0);
                    }
                    reg.extend_from_slice(ch.as_bytes());
                }
                let _ = c
                    .host
                    .send_custom_payload_to_player(u, "minecraft:register", &reg);
                broadcast_state(c, u);
            });
        })
        .on_quit(|ctx| {
            let Some(u) = ctx.player_uuid() else { return };
            let to = vs(|v| {
                v.states.remove(&u);
                v.conns.remove(&u);
                if let Some(cid) = v.by_player.remove(&u) {
                    v.secrets.remove(&cid);
                }
                v.compatible.remove(&u);
                v.announced.remove(&u);
                v.positions.remove(&u);
                v.compatible
                    .iter()
                    .filter(|(_, ok)| **ok)
                    .map(|(p, _)| *p)
                    .collect::<Vec<_>>()
            });
            let mut w = W::default();
            w.uuid(u);
            let id = w.0;
            ctx.run_later(1, move |c| {
                for r in &to {
                    let _ = c
                        .host
                        .send_custom_payload_to_player(*r, "voicechat:remove_state", &id);
                }
            });
        })
        // SVC's `voicechat.speak` / `voicechat.listen` (granted by default: only a node set false takes them away)
        .every(40, |ctx| {
            let online = ctx.online();
            let (ns, nl): (
                std::collections::HashSet<u128>,
                std::collections::HashSet<u128>,
            ) = (
                online
                    .iter()
                    .filter(|p| !ctx.has_default_perm(p.uuid, &p.name, "voicechat.speak"))
                    .map(|p| p.uuid)
                    .collect(),
                online
                    .iter()
                    .filter(|p| !ctx.has_default_perm(p.uuid, &p.name, "voicechat.listen"))
                    .map(|p| p.uuid)
                    .collect(),
            );
            vs(|v| {
                v.no_speak = ns;
                v.no_listen = nl;
            });
        })
        .every(1, |ctx| {
            start(ctx);
            let online = ctx.online();
            let events = vs(|v| {
                v.positions = online
                    .iter()
                    .map(|p| {
                        (
                            p.uuid,
                            Pos {
                                dim: p.dimension.clone(),
                                pos: [p.position[0], p.position[1] + 1.62, p.position[2]],
                            },
                        )
                    })
                    .collect();
                let ev = std::mem::take(&mut v.events);
                for (u, connected) in &ev {
                    if let Some(s) = v.states.get_mut(u) {
                        s.disconnected = !connected;
                    }
                }
                ev
            });
            for (u, _) in events {
                broadcast_state(ctx, u);
            }
        })
}
