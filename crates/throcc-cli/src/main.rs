use std::io::BufRead as _;
use std::net::{Ipv6Addr, ToSocketAddrs as _};
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::Parser;
use throcc_client_core::{Client, Command, Event, Keystore, Welcome};
use throcc_proto::{Role, RoomId};
use tracing_subscriber::EnvFilter;

const HELP: &str = "commands: room <id>|none, invite [user|manager|admin], quit";

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
    let mut words = line.split_whitespace();
    match (words.next(), words.next()) {
        (None, _) => Err(HELP.to_string()),
        (Some("quit"), _) => Ok(None),
        (Some("room"), Some("none")) => Ok(Some(Command::SetRoom(None))),
        (Some("room"), Some(id)) => id
            .parse()
            .map(|id| Some(Command::SetRoom(Some(RoomId(id)))))
            .map_err(|_| format!("{id} is not a room id")),
        (Some("room"), None) => Err("room takes an id, or none".to_string()),
        (Some("invite"), role) => role
            .map_or(Ok(Role::User), parse_role)
            .map(|role| Some(Command::CreateInvite { role, ttl_secs: 0 })),
        (Some(other), _) => Err(format!("{other} is not a command. {HELP}")),
    }
}

fn parse_role(word: &str) -> std::result::Result<Role, String> {
    match word {
        "user" => Ok(Role::User),
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
            command("invite manager"),
            Ok(Some(Command::CreateInvite {
                role: Role::Manager,
                ttl_secs: 0
            }))
        );
        assert_eq!(command("quit"), Ok(None));
        assert!(command("room later").is_err());
        assert!(command("dance").is_err());
    }
}
