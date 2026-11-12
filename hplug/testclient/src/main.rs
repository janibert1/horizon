//! hp-client: log into a Horizon (or vanilla) offline server, run a script, assert on what comes back.
//!
//!   hp-client --server 127.0.0.1:25570 --name Tester --script "wait 3; cmd hello; expect Hello; cmd sethome; expect Home saved; break 0 -61 0; expect-log broke block"
//!
//! Script ops (separated by `;`):  wait N (seconds, keeps answering the server) | cmd TEXT (a slash command)
//!   | chat TEXT | expect TEXT (substring of a chat line that arrives within 10 s) | break X Y Z (start-destroy)
//!   | chat TEXT (unsigned chat message) | expect-none TEXT (no chat line with TEXT within 3 s)
//!   | interact-last (right click the last added entity) | trade N (in the open merchant window: select offer N, shift-click the result)
//!   | pos (print position) | expect-pos X Y Z TOL (position within TOL blocks of target within 10 s)
//! `--protocol 26.2`: join as a client of that older release (see `join`).
//! `--expect-refused TEXT`: the login must be refused with a reason containing TEXT (no script runs).
//! Exit code 0 when every `expect*` passed, 1 otherwise. Chat lines print as `CHAT: ...`.
use horizon_protocol::{
    PROTOCOL_VERSION,
    buf::{Reader, Writer},
    connection::Connection,
    crypto::offline_uuid,
    ids,
    packets::{
        Empty, configuration,
        handshake::{self, Intention},
        login::{self, ServerHello},
        play,
    },
};
use std::io::{BufRead, Write};
use std::time::{Duration, Instant};

fn arg(name: &str) -> Option<String> {
    let a: Vec<String> = std::env::args().collect();
    a.iter()
        .position(|x| x == name)
        .and_then(|i| a.get(i + 1).cloned())
}
fn raw(id: i32, f: impl FnOnce(&mut Writer)) -> Vec<u8> {
    let mut w = Writer::new();
    w.varint(id);
    f(&mut w);
    w.data
}
fn e(x: impl std::fmt::Display) -> String {
    x.to_string()
}

fn join(host: &str, port: u16, name: &str) -> Result<Connection, String> {
    let mut c = Connection::connect(host, port, Duration::from_secs(10)).map_err(e)?;
    // `--protocol 26.2`: speak an older release on the wire (horizon-protocol's adapter renumbers and rewrites every
    // packet both ways), to test a translating proxy (Velocity + ViaBackwards) or Horizon's `accept_older_clients`.
    use horizon_protocol::adapter::ServerVersion;
    let version = match arg("--protocol").as_deref() {
        None | Some("26.3") | Some("current") => ServerVersion::Current,
        Some(v) => ServerVersion::ALL
            .into_iter()
            .find(|s| s.name() == v || s.protocol().to_string() == v)
            .ok_or_else(|| {
                format!(
                    "--protocol {v}: known: {}",
                    ServerVersion::ALL
                        .iter()
                        .map(|s| s.name())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })?,
    };
    c.adapt(version);
    let protocol = if version == ServerVersion::Current {
        PROTOCOL_VERSION
    } else {
        version.protocol()
    };
    if version != ServerVersion::Current {
        println!("speaking {} (protocol {protocol})", version.name());
    }
    c.send(
        &handshake::Serverbound::Intention(Intention {
            protocol,
            host: host.into(),
            port,
            intent: handshake::LOGIN,
        })
        .encode(),
    )
    .map_err(e)?;
    c.send(
        &login::Serverbound::Hello(ServerHello {
            name: name.into(),
            uuid: offline_uuid(name),
        })
        .encode(),
    )
    .map_err(e)?;
    loop {
        let (id, body) = c.recv().map_err(e)?;
        match login::Clientbound::decode(id, &body).map_err(e)? {
            login::Clientbound::Compression(p) => c.set_compression(p.threshold),
            login::Clientbound::LoginFinished(_) => {
                c.send(&login::Serverbound::LoginAcknowledged(Empty).encode())
                    .map_err(e)?;
                break;
            }
            login::Clientbound::Disconnect(p) => return Err(format!("refused: {}", p.reason_json)),
            login::Clientbound::Hello(_) => return Err("server is in online mode".into()),
            _ => {}
        }
    }
    c.send(
        &configuration::Serverbound::ClientInformation(configuration::ClientInformation::new(4))
            .encode(),
    )
    .map_err(e)?;
    use ids::configuration::clientbound as cb;
    loop {
        let (id, body) = c.recv().map_err(e)?;
        if ![
            cb::SELECT_KNOWN_PACKS,
            cb::KEEP_ALIVE,
            cb::PING,
            cb::CODE_OF_CONDUCT,
            cb::COOKIE_REQUEST,
            cb::DISCONNECT,
            cb::FINISH_CONFIGURATION,
        ]
        .contains(&id)
        {
            continue;
        }
        let reply = match configuration::Clientbound::decode(id, &body).map_err(e)? {
            configuration::Clientbound::SelectKnownPacks(p) => {
                configuration::Serverbound::SelectKnownPacks(p)
            }
            configuration::Clientbound::KeepAlive(p) => configuration::Serverbound::KeepAlive(p),
            configuration::Clientbound::Ping(p) => configuration::Serverbound::Pong(p),
            configuration::Clientbound::CodeOfConduct(_) => {
                configuration::Serverbound::AcceptCodeOfConduct(Empty)
            }
            configuration::Clientbound::CookieRequest(p) => {
                configuration::Serverbound::CookieResponse(configuration::CookieResponse {
                    key: p.key,
                    payload: None,
                })
            }
            configuration::Clientbound::Disconnect(_) => {
                return Err("disconnected during configuration".into());
            }
            configuration::Clientbound::FinishConfiguration(_) => {
                c.send(&configuration::Serverbound::FinishConfiguration(Empty).encode())
                    .map_err(e)?;
                return Ok(c);
            }
            _ => continue,
        };
        c.send(&reply.encode()).map_err(e)?;
    }
}

struct State {
    chat: Vec<String>,
    pos: [f64; 3],
    seen_tp: u32,
    last_move: Instant,
    /// id of the most recently added entity (for `interact-last`)
    entities: Vec<(i32, i32, [f64; 3])>,
    /// open container window id (0 = none) and its last state id (for `trade`)
    container: i32,
    state_id: i32,
}

/// Pump packets for up to `d`, answering the server; returns early when `stop` says so.
fn pump(
    c: &mut Connection,
    st: &mut State,
    d: Duration,
    stop: &dyn Fn(&State) -> bool,
) -> Result<(), String> {
    use ids::play::clientbound as cb;
    use ids::play::serverbound as sb;
    let until = Instant::now() + d;
    c.reader
        .set_read_timeout(Some(Duration::from_millis(50)))
        .ok();
    while Instant::now() < until && !stop(st) {
        // A real client reports its position every tick; the server only runs block-contact logic
        // (portals, pressure plates) for players it hears from.
        if st.seen_tp > 0 && st.last_move.elapsed() >= Duration::from_millis(50) {
            st.last_move = Instant::now();
            c.send(
                &play::Serverbound::MovePlayerPos(play::MovePlayerPos {
                    x: st.pos[0],
                    y: st.pos[1],
                    z: st.pos[2],
                    flags: 0,
                })
                .encode(),
            )
            .map_err(e)?;
            c.send(&raw(sb::CLIENT_TICK_END, |_| {})).map_err(e)?;
        }
        let (id, body) = match c.recv() {
            Ok(x) => x,
            Err(err) if is_timeout(&err.to_string()) => continue,
            Err(err) => return Err(format!("connection lost: {err}")),
        };
        let mut r = Reader::new(&body);
        match id {
            cb::CUSTOM_PAYLOAD => {
                if arg("--plugin-stdio").is_some() {
                    let channel = r.string(32767).map_err(e)?;
                    let hex: String = r.rest().iter().map(|b| format!("{b:02x}")).collect();
                    println!("PLUGIN {channel} {hex}");
                    std::io::stdout().flush().map_err(e)?;
                }
            }
            cb::KEEP_ALIVE => {
                if let Ok(v) = r.i64() {
                    c.send(&raw(sb::KEEP_ALIVE, |w| w.i64(v))).map_err(e)?;
                }
            }
            cb::PING => {
                if let Ok(v) = r.i32() {
                    c.send(&raw(sb::PONG, |w| w.i32(v))).map_err(e)?;
                }
            }
            cb::PLAYER_CHAT => {
                if let Ok(play::Clientbound::PlayerChat(p)) = play::Clientbound::decode(id, &body) {
                    let t = format!("<chat {:08x}> {}", (p.sender >> 96) as u32, p.content);
                    println!("CHAT: {t}");
                    st.chat.push(t);
                }
            }
            cb::SYSTEM_CHAT => {
                if let Ok(play::Clientbound::SystemChat(p)) = play::Clientbound::decode(id, &body)
                    && !p.overlay
                {
                    let t = horizon_protocol::text::plain(&p.content, &Default::default());
                    println!("CHAT: {t}");
                    st.chat.push(t);
                }
            }
            cb::PLAYER_POSITION => {
                if let Ok(play::Clientbound::PlayerPosition(p)) =
                    play::Clientbound::decode(id, &body)
                {
                    let prev = st.pos;
                    let pos: [f64; 3] = std::array::from_fn(|i| {
                        if p.relatives & (1 << i) != 0 {
                            prev[i] + p.position[i]
                        } else {
                            p.position[i]
                        }
                    });
                    st.pos = pos;
                    st.seen_tp += 1;
                    println!("POS: {:.2} {:.2} {:.2}", pos[0], pos[1], pos[2]);
                    c.send(
                        &play::Serverbound::AcceptTeleportation(play::AcceptTeleportation {
                            id: p.teleport,
                            x: pos[0],
                            y: pos[1],
                            z: pos[2],
                            yaw: p.yaw,
                            pitch: p.pitch,
                        })
                        .encode(),
                    )
                    .map_err(e)?;
                    if st.seen_tp == 1 {
                        // "the level is loaded": the server ignores moves, portals and actions until it hears this
                        c.send(&raw(sb::PLAYER_LOADED, |_| {})).map_err(e)?;
                    }
                }
            }
            cb::ADD_ENTITY => {
                if let Ok(play::Clientbound::AddEntity(p)) = play::Clientbound::decode(id, &body) {
                    if std::env::var_os("HP_ENTITIES").is_some() {
                        println!(
                            "ENTITY: {} kind {} at {:.1} {:.1} {:.1}",
                            p.id, p.kind, p.position[0], p.position[1], p.position[2]
                        );
                    }
                    st.entities.push((p.id, p.kind, p.position));
                }
            }
            cb::MAP_ITEM_DATA => {
                // a drawn map arrived: "MAP <id> <patch width>x<height>" goes into the chat list so `expect` can see it
                if let Ok(play::Clientbound::MapItemData(p)) = play::Clientbound::decode(id, &body)
                {
                    let t = format!(
                        "MAP {} {}",
                        p.map_id,
                        if p.patch.is_some() {
                            "with-pixels"
                        } else {
                            "no-pixels"
                        }
                    );
                    println!("CHAT: {t}");
                    st.chat.push(t);
                }
            }
            cb::OPEN_SCREEN => {
                if let Ok(play::Clientbound::OpenScreen(p)) = play::Clientbound::decode(id, &body) {
                    st.container = p.container;
                    st.state_id = 0;
                    println!("SCREEN: container {} menu {}", p.container, p.menu);
                }
            }
            cb::CONTAINER_SET_CONTENT => {
                if let Ok(play::Clientbound::ContainerSetContent(p)) =
                    play::Clientbound::decode(id, &body)
                    && p.container == st.container
                {
                    st.state_id = p.state;
                }
            }
            cb::CONTAINER_SET_SLOT => {
                if let Ok(play::Clientbound::ContainerSetSlot(p)) =
                    play::Clientbound::decode(id, &body)
                    && p.container == st.container
                {
                    st.state_id = p.state;
                }
            }
            cb::CHUNK_BATCH_FINISHED => {
                c.send(
                    &play::Serverbound::ChunkBatchReceived(play::ChunkBatchReceived {
                        chunks_per_tick: 4.0,
                    })
                    .encode(),
                )
                .map_err(e)?;
            }
            cb::DISCONNECT => {
                return Err(format!(
                    "server disconnected us: {}",
                    String::from_utf8_lossy(&body)
                        .chars()
                        .filter(|c| !c.is_control() || *c == '\n')
                        .collect::<String>()
                ));
            }
            _ => {}
        }
    }
    Ok(())
}
fn is_timeout(s: &str) -> bool {
    let s = s.to_lowercase();
    s.contains("timed out") || s.contains("would block") || s.contains("temporarily unavailable")
}

/// A bounded transport bridge: the Python SVC client owns all voice codecs and assertions.
/// Input: `plugin CHANNEL HEX`, `cmd TEXT`, `quit`; output: `PLUGIN CHANNEL HEX`.
fn plugin_bridge(c: &mut Connection, st: &mut State, seconds: f64) -> Result<(), String> {
    let (tx, rx) = std::sync::mpsc::sync_channel(64);
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let deadline = Instant::now() + Duration::from_secs_f64(seconds.clamp(1.0, 600.0));
    while Instant::now() < deadline {
        match rx.try_recv() {
            Ok(line) if line == "quit" => return Ok(()),
            Ok(line) => {
                if let Some(rest) = line.strip_prefix("plugin ") {
                    let (channel, hex) = rest.split_once(' ').unwrap_or((rest, ""));
                    if !hex.is_ascii() || hex.len() % 2 != 0 || hex.len() > 65536 {
                        return Err("invalid plugin hex length".into());
                    }
                    let data = (0..hex.len())
                        .step_by(2)
                        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).map_err(e))
                        .collect::<Result<Vec<_>, _>>()?;
                    c.send(&raw(ids::play::serverbound::CUSTOM_PAYLOAD, |w| {
                        w.string(channel);
                        w.data.extend_from_slice(&data);
                    }))
                    .map_err(e)?;
                } else if let Some(command) = line.strip_prefix("cmd ") {
                    c.send(
                        &play::Serverbound::ChatCommand(play::ChatCommand {
                            command: command.into(),
                        })
                        .encode(),
                    )
                    .map_err(e)?;
                } else {
                    return Err("expected plugin CHANNEL HEX, cmd TEXT or quit".into());
                }
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => return Ok(()),
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }
        pump(c, st, Duration::from_millis(20), &|_| false)?;
    }
    Err("plugin bridge time limit reached".into())
}

fn main() {
    let server = arg("--server").unwrap_or("127.0.0.1:25565".into());
    let name = arg("--name").unwrap_or("Tester".into());
    let script = arg("--script").unwrap_or_default();
    let (host, port) = server
        .rsplit_once(':')
        .map(|(h, p)| (h.to_owned(), p.parse().unwrap_or(25565)))
        .unwrap_or((server.clone(), 25565));
    let refused = arg("--expect-refused");
    let mut c = match (join(&host, port, &name), &refused) {
        (Ok(c), None) => c,
        (Ok(mut c), Some(t)) => {
            // Mods refuse in play state (an in-game disconnect), after configuration.
            let mut st = State {
                chat: vec![],
                pos: [0.0; 3],
                seen_tp: 0,
                last_move: Instant::now(),
                entities: vec![],
                container: 0,
                state_id: 0,
            };
            let r = pump(&mut c, &mut st, Duration::from_secs(8), &|_| false);
            let ok = matches!(&r, Err(m) if m.contains(t.as_str()));
            println!(
                "{} expect-refused {t:?} ({})",
                if ok { "PASS" } else { "FAIL" },
                match &r {
                    Err(m) => m.clone(),
                    Ok(()) => "the server let the player in".into(),
                }
            );
            println!("RESULT: {}", if ok { "PASS" } else { "FAIL" });
            std::process::exit(if ok { 0 } else { 1 })
        }
        (Err(err), None) => {
            eprintln!("FAIL join: {err}");
            std::process::exit(1)
        }
        (Err(err), Some(t)) => {
            let ok = err.replace("\\n", "\n").contains(t.as_str());
            println!(
                "{} expect-refused {t:?} (reason: {err})",
                if ok { "PASS" } else { "FAIL" }
            );
            println!("RESULT: {}", if ok { "PASS" } else { "FAIL" });
            std::process::exit(if ok { 0 } else { 1 })
        }
    };
    let mut st = State {
        chat: vec![],
        pos: [0.0; 3],
        seen_tp: 0,
        last_move: Instant::now(),
        entities: vec![],
        container: 0,
        state_id: 0,
    };
    if let Some(seconds) = arg("--plugin-stdio") {
        let result = plugin_bridge(&mut c, &mut st, seconds.parse().unwrap_or(120.0));
        if let Err(err) = result {
            eprintln!("FAIL bridge: {err}");
            std::process::exit(1);
        }
        return;
    }
    let mut failed = 0;
    // let the join settle: first teleport + chat lines
    let _ = pump(&mut c, &mut st, Duration::from_secs(3), &|_| false);
    for op in script.split(';').map(str::trim).filter(|s| !s.is_empty()) {
        let (verb, rest) = op.split_once(' ').unwrap_or((op, ""));
        let res: Result<(), String> = match verb {
            "wait" => pump(
                &mut c,
                &mut st,
                Duration::from_secs_f64(rest.parse().unwrap_or(1.0)),
                &|_| false,
            ),
            "cmd" => {
                println!("> /{rest}");
                c.send(
                    &play::Serverbound::ChatCommand(play::ChatCommand {
                        command: rest.into(),
                    })
                    .encode(),
                )
                .map_err(e)
            }
            "break" => {
                let v: Vec<i32> = rest
                    .split_whitespace()
                    .filter_map(|x| x.parse().ok())
                    .collect();
                if v.len() != 3 {
                    Err("break X Y Z".into())
                } else {
                    println!("> break {v:?}");
                    c.send(
                        &play::Serverbound::PlayerAction(play::PlayerAction {
                            action: 0,
                            pos: [v[0], v[1], v[2]],
                            direction: 1,
                            sequence: 1,
                        })
                        .encode(),
                    )
                    .map_err(e)
                }
            }
            "use" => {
                let v: Vec<i32> = rest
                    .split_whitespace()
                    .filter_map(|x| x.parse().ok())
                    .collect();
                if v.len() != 3 {
                    Err("use X Y Z (right click the top of that block with the held item)".into())
                } else {
                    println!("> use {v:?}");
                    c.send(
                        &play::Serverbound::UseItemOn(play::UseItemOn {
                            hand: 0,
                            pos: [v[0], v[1], v[2]],
                            face: 1,
                            cursor: [0.5, 1.0, 0.5],
                            inside: false,
                            world_border: false,
                            sequence: 2,
                        })
                        .encode(),
                    )
                    .map_err(e)
                }
            }
            "slot" => {
                println!("> slot {rest}");
                c.send(
                    &play::Serverbound::SetCarriedItem(play::SetCarriedItem {
                        slot: rest.trim().parse().unwrap_or(0),
                    })
                    .encode(),
                )
                .map_err(e)
            }
            "interact-last" => {
                // right click the entity that was added last (a villager summoned just before)
                // (the last one added within 4 blocks of the player: natural spawns come too)
                let me = st.pos;
                let near = st
                    .entities
                    .iter()
                    .rev()
                    .find(|e| (0..3).map(|k| (e.2[k] - me[k]).powi(2)).sum::<f64>() < 16.0)
                    .map(|x| x.0);
                match near {
                    None => Err("interact-last: no entity was added yet".into()),
                    Some(id) => {
                        println!("> interact entity {id}");
                        c.send(
                            &play::Serverbound::Interact(play::Interact {
                                id,
                                hand: 0,
                                location: [0.0, 0.0, 0.0],
                                secondary: false,
                            })
                            .encode(),
                        )
                        .map_err(e)
                        .and_then(|_| pump(&mut c, &mut st, Duration::from_millis(800), &|_| false))
                    }
                }
            }
            "trade" => {
                // in the open merchant window: select offer N, then shift-click the result slot (one trade)
                if st.container == 0 {
                    Err("trade: no window is open (interact-last first)".into())
                } else {
                    let n: i32 = rest.trim().parse().unwrap_or(0);
                    println!("> trade offer {n}");
                    c.send(&play::Serverbound::SelectTrade(play::SelectTrade { item: n }).encode())
                        .map_err(e)
                        .and_then(|_| pump(&mut c, &mut st, Duration::from_millis(600), &|_| false))
                        .and_then(|_| {
                            c.send(
                                &play::Serverbound::ContainerClick(play::ContainerClick {
                                    container: st.container,
                                    state: st.state_id,
                                    slot: 2,
                                    button: 0,
                                    input: 1,
                                    changed: vec![],
                                    carried: None,
                                })
                                .encode(),
                            )
                            .map_err(e)
                        })
                        .and_then(|_| pump(&mut c, &mut st, Duration::from_millis(800), &|_| false))
                }
            }
            "pos" => {
                println!(
                    "POS NOW: {:.2} {:.2} {:.2}",
                    st.pos[0], st.pos[1], st.pos[2]
                );
                Ok(())
            }
            "expect" => {
                let start = st.chat.len().saturating_sub(5);
                let want = rest.to_owned();
                let r = pump(&mut c, &mut st, Duration::from_secs(10), &|s| {
                    s.chat[start..].iter().any(|l| l.contains(&want))
                });
                if r.is_ok()
                    && st.chat[start.min(st.chat.len())..]
                        .iter()
                        .any(|l| l.contains(rest))
                {
                    println!("PASS expect {rest:?}");
                    Ok(())
                } else {
                    println!("FAIL expect {rest:?}");
                    failed += 1;
                    r
                }
            }
            "expect-pos" => {
                let v: Vec<f64> = rest
                    .split_whitespace()
                    .filter_map(|x| x.parse().ok())
                    .collect();
                if v.len() != 4 {
                    Err("expect-pos X Y Z TOL".into())
                } else {
                    let near = |s: &State| (0..3).all(|i| (s.pos[i] - v[i]).abs() <= v[3]);
                    let r = pump(&mut c, &mut st, Duration::from_secs(30), &near);
                    if near(&st) {
                        println!("PASS expect-pos {rest}");
                        Ok(())
                    } else {
                        println!("FAIL expect-pos {rest} (at {:?})", st.pos);
                        failed += 1;
                        r
                    }
                }
            }
            "chat" => {
                println!("> chat {rest}");
                let ts = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(1);
                c.send(
                    &play::Serverbound::Chat(play::Chat {
                        message: rest.into(),
                        timestamp: ts,
                        salt: 0,
                        signature: None,
                        last_seen: play::LastSeenUpdate {
                            offset: 0,
                            acknowledged: [0; 3],
                            checksum: 0,
                        },
                    })
                    .encode(),
                )
                .map_err(e)
            }
            "expect-none" => {
                // nothing containing TEXT may arrive in the next 3 seconds
                let from = st.chat.len();
                let want = rest.to_owned();
                let _ = pump(&mut c, &mut st, Duration::from_secs(3), &|_| false);
                if st.chat[from..].iter().any(|l| l.contains(&want)) {
                    println!("FAIL expect-none {rest:?}");
                    failed += 1;
                } else {
                    println!("PASS expect-none {rest:?}");
                }
                Ok(())
            }
            other => Err(format!("unknown script op {other:?}")),
        };
        if let Err(err) = res {
            eprintln!("script error at {op:?}: {err}");
            std::process::exit(1)
        }
    }
    println!(
        "{}",
        if failed == 0 {
            "RESULT: PASS"
        } else {
            "RESULT: FAIL"
        }
    );
    std::process::exit(if failed == 0 { 0 } else { 1 });
}
