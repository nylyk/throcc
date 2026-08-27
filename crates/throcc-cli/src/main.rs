use std::io::BufRead as _;
use std::net::{Ipv6Addr, ToSocketAddrs as _};
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::Parser;
use throcc_client_core::{Client, Command, Event, Keystore, Welcome};
use throcc_proto::{Role, RoomId};
use tracing_subscriber::EnvFilter;

const HELP: &str = "commands: room <id>|none, create <name>, rename <id> <name>, \
delete <id>, invite [user|manager|admin], quit";

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

    let mut events = client.events();
    std::thread::spawn(move || {
        while let Ok(event) = events.blocking_recv() {
            match event {
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
            Ok(Some(command)) => client.command(command)?,
            Ok(None) => break,
            Err(message) => println!("{message}"),
        }
    }

    client.shutdown();
    Ok(())
}

/// The parsed command, or `None` when the line ends the session.
fn command(line: &str) -> std::result::Result<Option<Command>, String> {
    let (word, rest) = split_word(line.trim());
    match word {
        "" => Err(HELP.to_string()),
        "quit" => Ok(None),
        "room" if rest == "none" => Ok(Some(Command::SetRoom(None))),
        "room" => parse_room(rest).map(|room| Some(Command::SetRoom(Some(room)))),
        "create" => match rest {
            "" => Err("create takes a name".to_string()),
            name => Ok(Some(Command::CreateRoom {
                name: name.to_string(),
            })),
        },
        "rename" => match split_word(rest) {
            (_, "") => Err("rename takes an id and a name".to_string()),
            (id, name) => parse_room(id).map(|room| {
                Some(Command::RenameRoom {
                    room,
                    name: name.to_string(),
                })
            }),
        },
        "delete" => parse_room(rest).map(|room| Some(Command::DeleteRoom(room))),
        "invite" => parse_role(rest).map(|role| Some(Command::CreateInvite { role, ttl_secs: 0 })),
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

    #[test]
    fn commands_parse() {
        assert_eq!(command("room none"), Ok(Some(Command::SetRoom(None))));
        assert_eq!(
            command("room 7"),
            Ok(Some(Command::SetRoom(Some(RoomId(7)))))
        );
        assert_eq!(
            command("create the lounge"),
            Ok(Some(Command::CreateRoom {
                name: "the lounge".into()
            }))
        );
        assert_eq!(
            command("rename 7 the lounge"),
            Ok(Some(Command::RenameRoom {
                room: RoomId(7),
                name: "the lounge".into()
            }))
        );
        assert_eq!(
            command("delete 7"),
            Ok(Some(Command::DeleteRoom(RoomId(7))))
        );
        assert_eq!(
            command("invite manager"),
            Ok(Some(Command::CreateInvite {
                role: Role::Manager,
                ttl_secs: 0
            }))
        );
        assert_eq!(command("quit"), Ok(None));
        assert!(command("room later").is_err());
        assert!(command("create").is_err());
        assert!(command("rename 7").is_err());
        assert!(command("dance").is_err());
    }
}
