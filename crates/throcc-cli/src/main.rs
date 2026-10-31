mod synthetic;

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use throcc_client_core::account;
use throcc_client_core::call::Call;
use throcc_client_core::control::{Link, Requester};
use throcc_client_core::keystore::Keystore;
use throcc_proto::ids::RoomId;
use throcc_proto::invite::InviteCode;
use throcc_proto::messages::{Event, Presence, Request, Response, Room, Welcome};
use tokio::io::{AsyncBufReadExt, BufReader};

#[derive(Parser)]
#[command(about = "Headless throcc client")]
struct Cli {
    /// Directory holding this client's keys. Defaults to the platform data directory.
    #[arg(long, global = true)]
    profile: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Joins a server with an invite code and pins its key.
    Enroll { address: String, code: String },
    /// Connects and reads commands from stdin, printing events as they arrive.
    Connect { address: String },
    /// Lists rooms and who is in them.
    Rooms { address: String },
    /// Joins a room, then reads commands from stdin.
    Join { address: String, room: String },
    /// Connects and leaves to the lobby.
    Leave { address: String },
    /// Mints an invite code.
    Invite { address: String },
    /// Lists every user (admins only).
    Users { address: String },
    /// Creates a room (managers and admins).
    CreateRoom { address: String, name: String },
    /// Sets your display name.
    Name { address: String, name: String },
    /// Holds a voice call in a room until Ctrl-C, printing who speaks.
    Call {
        address: String,
        room: String,
        /// Starts muted.
        #[arg(long)]
        muted: bool,
        /// Opens no audio devices.
        #[arg(long)]
        no_audio: bool,
        /// Input device name, from `devices`.
        #[arg(long)]
        input: Option<String>,
        /// Output device name, from `devices`.
        #[arg(long)]
        output: Option<String>,
    },
    /// Lists audio devices.
    Devices,
    /// Joins a room and sends fake access units through the encrypted media path.
    SendSynthetic {
        address: String,
        room: String,
        #[arg(long, value_enum, default_value = "video")]
        track: synthetic::Track,
        /// Bytes per unit.
        #[arg(long, default_value_t = 2500)]
        size: usize,
        /// Units per second.
        #[arg(long, default_value_t = 1000)]
        rate: u32,
        #[arg(long, default_value_t = 10_000)]
        count: u32,
        /// Also sends 50 mic frames per second.
        #[arg(long)]
        with_audio: bool,
    },
    /// Joins a room and receives another member's units, checking each one.
    Recv {
        address: String,
        room: String,
        /// Name or user id of the sender.
        #[arg(long)]
        from: String,
        #[arg(long, value_enum, default_value = "video")]
        track: synthetic::Track,
        #[arg(long, default_value_t = 10_000)]
        count: u32,
        /// Also receives the sender's mic.
        #[arg(long)]
        with_audio: bool,
        /// Seconds without a unit before giving up.
        #[arg(long, default_value_t = 10)]
        idle: u64,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .init();
    let cli = Cli::parse();
    let mut keystore = match &cli.profile {
        Some(dir) => Keystore::open_in(dir)?,
        None => Keystore::open_default()?,
    };

    match cli.command {
        Command::Enroll { address, code } => {
            let code: InviteCode = code.parse()?;
            let link = account::enroll(&mut keystore, &address, &code).await?;
            println!(
                "enrolled as user {} ({:?})",
                link.welcome.me.0, link.welcome.role
            );
            println!("server fingerprint {}", link.connected.fingerprint);
            link.connected.close().await;
        }
        Command::Connect { address } => {
            let link = sign_in(&keystore, &address).await?;
            print_rooms(&link.welcome.rooms, &link.welcome.presence);
            interactive(link).await?;
        }
        Command::Rooms { address } => one_shot(&keystore, &address, "rooms").await?,
        Command::Join { address, room } => {
            let link = sign_in(&keystore, &address).await?;
            let room = find_room(&link.welcome.rooms, &room)?;
            run_command(&link.requester, &link.welcome, &format!("join {}", room.0)).await?;
            interactive(link).await?;
        }
        Command::Leave { address } => one_shot(&keystore, &address, "leave").await?,
        Command::Invite { address } => one_shot(&keystore, &address, "invite").await?,
        Command::Users { address } => one_shot(&keystore, &address, "users").await?,
        Command::CreateRoom { address, name } => {
            one_shot(&keystore, &address, &format!("create-room {name}")).await?
        }
        Command::Name { address, name } => {
            one_shot(&keystore, &address, &format!("name {name}")).await?
        }
        Command::Call {
            address,
            room,
            muted,
            no_audio,
            input,
            output,
        } => {
            tokio::task::spawn_blocking(move || {
                voice_call(&address, &room, keystore, muted, !no_audio, input, output)
            })
            .await??;
        }
        Command::Devices => {
            let (inputs, outputs) = throcc_client_core::media::devices::list();
            println!("inputs:\n  {}", inputs.join("\n  "));
            println!("outputs:\n  {}", outputs.join("\n  "));
        }
        Command::SendSynthetic {
            address,
            room,
            track,
            size,
            rate,
            count,
            with_audio,
        } => {
            let (mut call, room) = start_call(&keystore, &address, &room).await?;
            let options = synthetic::SendOptions {
                track,
                size,
                rate,
                count,
                with_audio,
            };
            let sent = synthetic::send(&mut call, room, options).await;
            call.close().await;
            sent?;
        }
        Command::Recv {
            address,
            room,
            from,
            track,
            count,
            with_audio,
            idle,
        } => {
            let (mut call, room) = start_call(&keystore, &address, &room).await?;
            let options = synthetic::RecvOptions {
                from: &from,
                track,
                count,
                with_audio,
                idle: std::time::Duration::from_secs(idle),
            };
            let received = synthetic::recv(&mut call, room, options).await;
            call.close().await;
            received?;
        }
    }
    Ok(())
}

/// Runs a call with the real client until stdin closes or Ctrl-C.
fn voice_call(
    address: &str,
    room: &str,
    keystore: Keystore,
    muted: bool,
    audio: bool,
    input: Option<String>,
    output: Option<String>,
) -> Result<()> {
    use throcc_client_core::client::{
        Client, ClientApi, ClientOptions, Command as ClientCommand, Event as ClientEvent,
    };
    let options = ClientOptions {
        audio,
        input_device: input,
        output_device: output,
        ..Default::default()
    };
    let client = Client::connect(address, keystore, None, options)?;
    let state = client.state();
    let room = find_room(
        &state
            .rooms
            .iter()
            .map(|r| Room {
                id: r.id,
                name: r.name.clone(),
            })
            .collect::<Vec<_>>(),
        room,
    )?;
    let mut events = client.events();
    client.command(ClientCommand::Join(Some(room)));
    client.command(ClientCommand::SetMic(!muted));
    println!("in the call; press Ctrl-C to leave");
    let mut speaking = std::collections::BTreeSet::new();
    loop {
        match events.blocking_recv() {
            Ok(ClientEvent::State(state)) => {
                let now: std::collections::BTreeSet<String> = state
                    .people
                    .values()
                    .filter(|p| p.room == Some(room) && p.speaking)
                    .map(|p| p.name.clone())
                    .collect();
                if now != speaking {
                    let names: Vec<&str> = now.iter().map(String::as_str).collect();
                    println!("speaking: {}", names.join(", "));
                    speaking = now;
                }
            }
            Ok(ClientEvent::Notice(notice)) => println!("* {notice}"),
            Ok(ClientEvent::Kicked(reason)) => bail!("disconnected: {reason}"),
            Ok(_) => {}
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
            Err(_) => return Ok(()),
        }
    }
}

async fn sign_in(keystore: &Keystore, address: &str) -> Result<Link> {
    let link = account::sign_in(keystore, address, None)
        .await
        .inspect_err(|e| {
            if let Some((expected, actual)) = e.pin_mismatch() {
                eprintln!(
                    "The server's key changed.\n  you trusted  {expected}\n  it now has   {actual}"
                );
                eprintln!("Re-enroll with a fresh invite code to trust the new key.");
            }
        })?;
    Ok(link)
}

async fn start_call(keystore: &Keystore, address: &str, room: &str) -> Result<(Call, RoomId)> {
    let link = sign_in(keystore, address).await?;
    let room = find_room(&link.welcome.rooms, room)?;
    Ok((Call::start(link, keystore)?, room))
}

async fn one_shot(keystore: &Keystore, address: &str, command: &str) -> Result<()> {
    let link = sign_in(keystore, address).await?;
    let result = run_command(&link.requester, &link.welcome, command).await;
    link.connected.close().await;
    result
}

async fn interactive(mut link: Link) -> Result<()> {
    let result = read_commands(&mut link).await;
    link.connected.close().await;
    result
}

async fn read_commands(link: &mut Link) -> Result<()> {
    println!(
        "commands: rooms, join <room>, leave, invite, users, create-room <name>, name <name>, quit"
    );
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    loop {
        tokio::select! {
            line = lines.next_line() => {
                let Some(line) = line? else { return Ok(()) };
                let line = line.trim();
                if line == "quit" {
                    return Ok(());
                }
                if !line.is_empty() && let Err(e) = run_command(&link.requester, &link.welcome, line).await {
                    println!("error: {e:#}");
                }
            }
            event = link.events.recv() => {
                let Some(event) = event else { bail!("disconnected") };
                print_event(&event);
                if let Event::Kicked { .. } = event {
                    return Ok(());
                }
            }
        }
    }
}

async fn run_command(requester: &Requester, welcome: &Welcome, line: &str) -> Result<()> {
    let (word, rest) = line.split_once(' ').unwrap_or((line, ""));
    let request = match word {
        "rooms" => {
            print_rooms(&welcome.rooms, &welcome.presence);
            return Ok(());
        }
        "join" => Request::SetRoom(Some(find_room(&welcome.rooms, rest)?)),
        "leave" => Request::SetRoom(None),
        "invite" => Request::CreateInvite,
        "users" => Request::ListUsers,
        "create-room" => Request::CreateRoom {
            name: rest.to_owned(),
        },
        "name" => Request::SetProfile {
            name: rest.to_owned(),
            avatar: None,
        },
        other => bail!("unknown command {other}"),
    };
    match requester.request(request).await? {
        Response::Ok => println!("ok"),
        Response::Err { code, message } => bail!("{message} ({code:?})"),
        Response::Placed(placed) => match placed.room {
            Some(room) => println!(
                "in room {} at epoch {}, {} peers, mic track {}",
                room.0,
                placed.epoch.0,
                placed.peers.len(),
                placed.tracks.map_or(0, |t| t.mic.0)
            ),
            None => println!("in the lobby"),
        },
        Response::Invite { code, expires } => {
            println!("invite {code} (expires at unix time {expires})")
        }
        Response::Users(users) => {
            for user in users {
                let invited_by = user.invited_by.map_or("-".to_owned(), |u| u.0.to_string());
                println!(
                    "{:>4} {:<20} {:<8} invited by {invited_by}",
                    user.id.0,
                    user.name,
                    format!("{:?}", user.role)
                );
            }
        }
        other => println!("{other:?}"),
    }
    Ok(())
}

fn find_room(rooms: &[Room], query: &str) -> Result<RoomId> {
    let query = query.trim();
    rooms
        .iter()
        .find(|r| r.name.eq_ignore_ascii_case(query) || r.id.0.to_string() == query)
        .map(|r| r.id)
        .with_context(|| format!("no room called {query}"))
}

fn print_rooms(rooms: &[Room], presence: &[Presence]) {
    if rooms.is_empty() {
        println!("no rooms yet");
    }
    for room in rooms {
        let people: Vec<&str> = presence
            .iter()
            .filter(|p| p.room == Some(room.id))
            .map(|p| p.name.as_str())
            .collect();
        println!("{:>4} {:<20} {}", room.id.0, room.name, people.join(", "));
    }
}

fn print_event(event: &Event) {
    match event {
        Event::PresenceChanged(p) => println!(
            "* {} is in room {} (mic {}, deafened {}, sharing {})",
            p.name,
            p.room.map_or(0, |r| r.0),
            p.mic,
            p.deafened,
            p.sharing
        ),
        Event::PresenceEnded(user) => println!("* user {} left", user.0),
        Event::RoomCreated(room) => println!("* room {} created: {}", room.id.0, room.name),
        Event::RoomRenamed { room, name } => println!("* room {} renamed to {name}", room.0),
        Event::RoomDeleted(room) => println!("* room {} deleted", room.0),
        Event::RoomState { room, epoch, peers } => {
            println!(
                "* room {} is at epoch {} with {} peers",
                room.0,
                epoch.0,
                peers.len()
            )
        }
        Event::InviteBurned { code_id } => {
            println!("* invite {code_id}-… was burned after 3 wrong tries")
        }
        Event::Kicked { reason } => println!("* disconnected: {reason}"),
        other => println!("* {other:?}"),
    }
}
