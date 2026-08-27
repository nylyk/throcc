use std::io::BufRead as _;
use std::net::{Ipv6Addr, ToSocketAddrs as _};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::Parser;
use throcc_client_core::{Client, Command, Event, Keystore, MediaSender, Welcome};
use throcc_proto::{Role, RoomId, Tracks};
use tracing_subscriber::EnvFilter;

const HELP: &str = "commands: room <id>|none, mic on|off, mute on|off, create <name>, \
rename <id> <name>, delete <id>, invite [user|manager|admin], quit";

#[derive(Parser, Debug)]
#[command(
    name = "throcc-cli",
    version,
    about = "Headless voice + screenshare client"
)]
struct Args {
    /// The server's domain or IP address.
    server: String,

    #[arg(long, default_value_t = throcc_proto::DEFAULT_PORT)]
    port: u16,

    /// A one-time code that enrolls this client's identity key. It is needed only
    /// on the first connection.
    #[arg(long)]
    invite: Option<String>,

    /// The keystore's location.
    #[arg(long)]
    keystore: Option<PathBuf>,

    /// Send counted dummy packets on every track of the room this client enters,
    /// so the transport can be watched without a codec in the way.
    #[arg(long)]
    synthetic: bool,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();

    let keystore = Keystore::open(args.keystore)?;
    tracing::info!(keystore = %keystore.path().display(), "loaded keystore");

    if args.server.contains(':') && args.server.parse::<Ipv6Addr>().is_err() {
        bail!(
            "{} is not a host name or IP address; the port goes in --port",
            args.server
        );
    }

    let authority = format_authority(&args.server, args.port);
    let address = authority
        .to_socket_addrs()
        .with_context(|| format!("resolving {authority}"))?
        .next()
        .ok_or_else(|| anyhow::anyhow!("{authority} resolved to no addresses"))?;

    let client = match Client::connect(address, &authority, keystore, args.invite) {
        Ok(client) => client,
        Err(throcc_client_core::Error::PinMismatch {
            server,
            pinned,
            presented,
        }) => {
            bail!(
                "the key for {server} has changed.\n  pinned:    {pinned}\n  presented: {presented}\n\
                 \nIf the server was rebuilt this is expected; re-accept by removing that entry \
                 from the keystore. If it was not, stop."
            );
        }
        Err(throcc_client_core::Error::Rejected(error)) => {
            bail!("{error}.\nEnroll this client with --invite <CODE>.");
        }
        Err(e) => return Err(e).context("connecting"),
    };
    tracing::info!(server = %authority, "connected");
    report(client.welcome());

    let tracks = Arc::new(Mutex::new(client.welcome().placed.tracks.clone()));
    if args.synthetic {
        let sending = client.media();
        let tracks = tracks.clone();
        std::thread::spawn(move || send_synthetic(&sending, &tracks));
    }

    let mut events = client.events();
    let seen_tracks = tracks.clone();
    std::thread::spawn(move || {
        while let Ok(event) = events.blocking_recv() {
            match event {
                Event::Media {
                    media_id,
                    timestamp,
                    bytes,
                } => tracing::info!(
                    %media_id,
                    ?timestamp,
                    bytes = bytes.len(),
                    counter = counter_of(&bytes),
                    "a unit arrived"
                ),
                Event::Placed(placed) => {
                    tracing::info!(
                        room = ?placed.room,
                        epoch = %placed.epoch,
                        tracks = ?placed.tracks,
                        peers = placed.peers.len(),
                        "placed"
                    );
                    *seen_tracks.lock().expect("tracks mutex poisoned") = placed.tracks;
                }
                Event::UserEntered { room, epoch, peer } => {
                    tracing::info!(%room, %epoch, user = %peer.user, "a user entered")
                }
                Event::UserExited { room, epoch, user } => {
                    tracing::info!(%room, %epoch, %user, "a user left")
                }
                Event::RoomCreated(room) => {
                    tracing::info!(id = %room.id, name = %room.name, "a room was created")
                }
                Event::RoomRenamed { room, name } => {
                    tracing::info!(id = %room, %name, "a room was renamed")
                }
                Event::RoomDeleted(room) => tracing::info!(id = %room, "a room was deleted"),
                Event::Invited { code, expires } => {
                    tracing::info!(%code, expires, "minted an invite")
                }
                Event::Failed { message } => tracing::info!(%message, "request failed"),
                Event::Disconnected { reason } => {
                    tracing::info!(%reason, "disconnected");
                    break;
                }
            }
        }
    });

    println!("{HELP}");
    for line in std::io::stdin().lock().lines() {
        match command(&line?) {
            Ok(Some(Spoken::ToServer(command))) => client.command(command)?,
            Ok(Some(Spoken::Microphone(on))) => match on {
                true => match client.start_microphone(None) {
                    Ok(()) => println!("microphone open"),
                    Err(e) => println!("{e}"),
                },
                false => {
                    client.stop_microphone();
                    println!("microphone closed");
                }
            },
            Ok(Some(Spoken::Muted(muted))) => {
                client.set_microphone_muted(muted);
                println!("microphone {}", if muted { "muted" } else { "live" });
            }
            Ok(None) => break,
            Err(message) => println!("{message}"),
        }
    }

    client.shutdown();
    Ok(())
}

/// One counted unit per track every 20 ms: a small one on the microphone id, and
/// one that has to fragment on the share id.
fn send_synthetic(sending: &MediaSender, tracks: &Mutex<Option<Tracks>>) {
    let mut counter: u64 = 0;
    loop {
        counter += 1;
        let current = tracks.lock().expect("tracks mutex poisoned").clone();
        if let Some(tracks) = current {
            sending.send(tracks.mic, &counted(counter, 80), None, false);
            for share in &tracks.shares {
                sending.send(share.video, &counted(counter, 3_000), Some(90), false);
                if let Some(audio) = share.audio {
                    sending.send(audio, &counted(counter, 160), Some(90), false);
                }
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn counted(counter: u64, len: usize) -> Vec<u8> {
    let mut unit = counter.to_be_bytes().to_vec();
    unit.resize(len.max(size_of::<u64>()), 0);
    unit
}

fn counter_of(unit: &[u8]) -> u64 {
    unit.get(..size_of::<u64>())
        .and_then(|counter| counter.try_into().ok())
        .map_or(0, u64::from_be_bytes)
}

/// What a typed line asks for. Only some of it reaches the server: the
/// microphone is local.
#[derive(Debug, PartialEq)]
enum Spoken {
    ToServer(Command),
    Microphone(bool),
    Muted(bool),
}

/// The parsed line, or `None` when it ends the session.
fn command(line: &str) -> std::result::Result<Option<Spoken>, String> {
    let (word, rest) = split_word(line.trim());
    match word {
        "" => Err(HELP.to_string()),
        "quit" => Ok(None),
        "mic" => parse_switch(rest).map(|on| Some(Spoken::Microphone(on))),
        "mute" => parse_switch(rest).map(|muted| Some(Spoken::Muted(muted))),
        "room" if rest == "none" => Ok(Some(Spoken::ToServer(Command::SetRoom(None)))),
        "room" => parse_room(rest).map(|room| Some(Spoken::ToServer(Command::SetRoom(Some(room))))),
        "create" => match rest {
            "" => Err("create takes a name".to_string()),
            name => Ok(Some(Spoken::ToServer(Command::CreateRoom {
                name: name.to_string(),
            }))),
        },
        "rename" => match split_word(rest) {
            (_, "") => Err("rename takes an id and a name".to_string()),
            (id, name) => parse_room(id).map(|room| {
                Some(Spoken::ToServer(Command::RenameRoom {
                    room,
                    name: name.to_string(),
                }))
            }),
        },
        "delete" => parse_room(rest).map(|room| Some(Spoken::ToServer(Command::DeleteRoom(room)))),
        "invite" => parse_role(rest).map(|role| {
            Some(Spoken::ToServer(Command::CreateInvite {
                role,
                ttl_secs: 0,
            }))
        }),
        other => Err(format!("{other} is not a command. {HELP}")),
    }
}

/// The first word and whatever follows it, both trimmed.
fn split_word(line: &str) -> (&str, &str) {
    match line.split_once(char::is_whitespace) {
        Some((word, rest)) => (word, rest.trim()),
        None => (line, ""),
    }
}

fn parse_room(word: &str) -> std::result::Result<RoomId, String> {
    word.parse()
        .map(RoomId)
        .map_err(|_| format!("{word} is not a room id"))
}

fn parse_switch(word: &str) -> std::result::Result<bool, String> {
    match word {
        "on" => Ok(true),
        "off" => Ok(false),
        other => Err(format!("{other} is not on or off")),
    }
}

fn parse_role(word: &str) -> std::result::Result<Role, String> {
    match word {
        "" | "user" => Ok(Role::User),
        "manager" => Ok(Role::Manager),
        "admin" => Ok(Role::Admin),
        other => Err(format!("{other} is not a role")),
    }
}

fn format_authority(host: &str, port: u16) -> String {
    if host.parse::<Ipv6Addr>().is_ok() {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

fn report(welcome: &Welcome) {
    println!("you are user {} with role {:?}", welcome.me, welcome.role);
    for user in &welcome.users {
        let name = if user.name.is_empty() {
            "<unnamed>"
        } else {
            &user.name
        };
        println!("  user {} {name} ({:?})", user.id, user.role);
    }
    if welcome.rooms.is_empty() {
        println!("no rooms exist yet");
    }
    for room in &welcome.rooms {
        println!("  room {} {}", room.id, room.name);
    }
    match welcome.placed.room {
        None => println!("you are in no room"),
        Some(room) => println!("you are in room {room}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brackets_only_ipv6_addresses() {
        assert_eq!(format_authority("::1", 8476), "[::1]:8476");
        assert_eq!(format_authority("1.2.3.4", 8476), "1.2.3.4:8476");
        assert_eq!(format_authority("example.com", 8476), "example.com:8476");
    }

    fn to_server(command: Command) -> std::result::Result<Option<Spoken>, String> {
        Ok(Some(Spoken::ToServer(command)))
    }

    #[test]
    fn commands_parse() {
        assert_eq!(command("room none"), to_server(Command::SetRoom(None)));
        assert_eq!(
            command("room 7"),
            to_server(Command::SetRoom(Some(RoomId(7))))
        );
        assert_eq!(
            command("create the lounge"),
            to_server(Command::CreateRoom {
                name: "the lounge".into()
            })
        );
        assert_eq!(
            command("rename 7 the lounge"),
            to_server(Command::RenameRoom {
                room: RoomId(7),
                name: "the lounge".into()
            })
        );
        assert_eq!(
            command("delete 7"),
            to_server(Command::DeleteRoom(RoomId(7)))
        );
        assert_eq!(
            command("invite manager"),
            to_server(Command::CreateInvite {
                role: Role::Manager,
                ttl_secs: 0
            })
        );
        assert_eq!(command("mic on"), Ok(Some(Spoken::Microphone(true))));
        assert_eq!(command("mute off"), Ok(Some(Spoken::Muted(false))));
        assert_eq!(command("quit"), Ok(None));
        assert!(command("room later").is_err());
        assert!(command("create").is_err());
        assert!(command("rename 7").is_err());
        assert!(command("mic").is_err());
        assert!(command("dance").is_err());
    }
}
