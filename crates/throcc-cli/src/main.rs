mod synthetic;
mod unit;

use std::borrow::Cow;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use throcc_client_core::account;
use throcc_client_core::call::Call;
use throcc_client_core::control::{Link, Requester};
use throcc_client_core::keystore::{IdentityMissing, Keystore, StartedOver};
use throcc_proto::ids::RoomId;
use throcc_proto::invite::InviteCode;
use throcc_proto::messages::{Event, Presence, Request, Response, Room, Welcome};
use tokio::io::{AsyncBufReadExt, BufReader};

#[derive(Parser)]
#[command(about = "Headless throcc client")]
struct Cli {
    /// Directory for this client's keys, servers and settings. Without it the identity key is in
    /// the OS keychain and the rest in the platform config directory.
    #[arg(long, global = true)]
    profile: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Joins a server with an invite code and pins its key.
    ///
    /// Without CODE the code comes from THROCC_INVITE, or else from stdin; `-` reads stdin.
    Enroll {
        address: String,
        /// The invite code. Given here, other local users can read it (`ps`, /proc) and it
        /// lands in shell history; prefer THROCC_INVITE or stdin.
        code: Option<String>,
    },
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
    /// Starts a new identity after the keychain lost yours, forgetting every server: joining
    /// them again needs new invites. Changes nothing while the keychain has your identity.
    ///
    /// Asks you to type `start over` first.
    StartOver {
        /// Starts over without asking, for scripts.
        #[arg(long)]
        yes: bool,
    },
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
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .init();
    match run(Cli::parse()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("Error: {}", shown(&format!("{e:#}")));
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> Result<()> {
    let mut keystore = match (&cli.command, &cli.profile) {
        (Command::Devices, _) => {
            let (inputs, outputs) = throcc_client_core::media::devices::list();
            println!("inputs:\n  {}", inputs.join("\n  "));
            println!("outputs:\n  {}", outputs.join("\n  "));
            return Ok(());
        }
        (Command::StartOver { .. }, Some(_)) => {
            bail!("start-over is for the identity in the OS keychain; a profile keeps its own")
        }
        (Command::StartOver { yes }, None) => return start_over(*yes).await,
        (_, Some(dir)) => Keystore::open_in(dir)?,
        (_, None) => Keystore::open_default().map_err(offer_start_over)?,
    };

    match cli.command {
        Command::Enroll { address, code } => {
            let code = invite_code(code).await?;
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
            run_command(&link.requester, &link.welcome, &format!("join {room}")).await?;
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
        Command::Devices => unreachable!("listed before the keystore opens"),
        Command::StartOver { .. } => unreachable!("done before the keystore opens"),
        Command::SendSynthetic {
            address,
            room,
            track,
            size,
            rate,
            count,
            with_audio,
        } => {
            track.check_with_audio(with_audio)?;
            track.check_size(size)?;
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
            track.check_with_audio(with_audio)?;
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

/// Runs a call with the real client until the connection ends or Ctrl-C.
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
    use throcc_client_core::verified::VerifiedStore;
    let options = ClientOptions {
        audio,
        input_device: input,
        output_device: output,
        ..Default::default()
    };
    let verified = VerifiedStore::load(keystore.dir()).unwrap_or_else(|e| {
        eprintln!("Couldn't read who you verified ({e:#}), so everyone shows as not verified.");
        VerifiedStore::empty(keystore.dir())
    });
    let client = Client::connect(address, keystore, None, options, verified)?;
    let state = client.state();
    let rooms = state.rooms.iter().map(|r| (r.id, r.name.as_str()));
    let room = find_room(rooms, room)?;
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
                    println!("speaking: {}", shown(&names.join(", ")));
                    speaking = now;
                }
            }
            Ok(ClientEvent::Notice(notice)) => println!("* {}", shown(&notice)),
            Ok(ClientEvent::Kicked(reason)) => bail!("disconnected: {reason}"),
            Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
            Err(_) => return Ok(()),
        }
    }
}

/// The invite code from the argument, else THROCC_INVITE, else a line of stdin.
async fn invite_code(arg: Option<String>) -> Result<InviteCode> {
    let code = match given_code(arg, std::env::var("THROCC_INVITE").ok()) {
        Some(code) => code,
        None => {
            let unechoed = std::io::stdin()
                .is_terminal()
                .then(|| {
                    eprint!("invite code: ");
                    Unechoed::start()
                })
                .flatten();
            let mut line = String::new();
            let read = BufReader::new(tokio::io::stdin())
                .read_line(&mut line)
                .await;
            if let Some(unechoed) = unechoed {
                drop(unechoed);
                // The Enter that ended the line wasn't shown either.
                eprintln!();
            }
            read?;
            line
        }
    };
    Ok(code.parse()?)
}

/// Terminal echo turned off while a secret is typed, and back on when dropped. Uses `stty`,
/// so where that is missing (Windows, say) the typing shows as before.
struct Unechoed;

impl Unechoed {
    fn start() -> Option<Self> {
        stty("-echo").then_some(Self)
    }
}

impl Drop for Unechoed {
    fn drop(&mut self) {
        stty("echo");
    }
}

/// Runs `stty` on our terminal; true if it worked.
fn stty(setting: &str) -> bool {
    cfg!(unix)
        && std::process::Command::new("stty")
            .arg(setting)
            .stdin(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
}

/// The code given without reading stdin: the argument unless it is `-`, else the environment.
fn given_code(arg: Option<String>, env: Option<String>) -> Option<String> {
    match arg {
        Some(arg) if arg == "-" => None,
        Some(arg) => Some(arg),
        None => env,
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
    let room = find_room(room_names(&link.welcome.rooms), room)?;
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
                    println!("error: {}", shown(&format!("{e:#}")));
                }
            }
            event = link.events.recv() => {
                let Some(event) = event else { bail!("disconnected") };
                print_event(&event);
                follow(&mut link.welcome, &event);
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
        "join" => Request::SetRoom {
            room: Some(find_room(room_names(&welcome.rooms), rest)?),
            password: None,
        },
        "leave" => Request::SetRoom {
            room: None,
            password: None,
        },
        "invite" => Request::CreateInvite,
        "users" => Request::ListUsers,
        "create-room" => Request::CreateRoom {
            name: rest.to_owned(),
            password: None,
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
            println!("invite {} (expires at unix time {expires})", shown(&code))
        }
        Response::Users(users) => {
            for user in users {
                let invited_by = user.invited_by.map_or("-".to_owned(), |u| u.0.to_string());
                println!(
                    "{:>4} {:<20} {:<8} invited by {invited_by}",
                    user.id.0,
                    shown(&user.name),
                    format!("{:?}", user.role)
                );
            }
        }
        other => println!("{other:?}"),
    }
    Ok(())
}

/// Whether `query` is `name`, ignoring case, or the numeric `id`.
fn names(query: &str, name: &str, id: u64) -> bool {
    name.eq_ignore_ascii_case(query) || id.to_string() == query
}

fn room_names(rooms: &[Room]) -> impl Iterator<Item = (RoomId, &str)> {
    rooms.iter().map(|r| (r.id, r.name.as_str()))
}

fn find_room<'a>(
    rooms: impl IntoIterator<Item = (RoomId, &'a str)>,
    query: &str,
) -> Result<RoomId> {
    let query = query.trim();
    rooms
        .into_iter()
        .find(|(id, name)| names(query, name, id.0))
        .map(|(id, _)| id)
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
        println!(
            "{:>4} {:<20} {}",
            room.id.0,
            shown(&room.name),
            shown(&people.join(", "))
        );
    }
}

/// Keeps the sign-in snapshot's rooms and people current, so `rooms` and `join` see changes.
fn follow(welcome: &mut Welcome, event: &Event) {
    match event {
        Event::PresenceChanged(p) => {
            match welcome.presence.iter_mut().find(|old| old.user == p.user) {
                Some(old) => *old = p.clone(),
                None => welcome.presence.push(p.clone()),
            }
        }
        Event::PresenceEnded(user) => welcome.presence.retain(|p| p.user != *user),
        Event::RoomCreated(room) => welcome.rooms.push(room.clone()),
        Event::RoomRenamed { room, name } => {
            if let Some(r) = welcome.rooms.iter_mut().find(|r| r.id == *room) {
                r.name = name.clone();
            }
        }
        Event::RoomLocked { room, locked } => {
            if let Some(r) = welcome.rooms.iter_mut().find(|r| r.id == *room) {
                r.locked = *locked;
            }
        }
        Event::RoomDeleted(room) => welcome.rooms.retain(|r| r.id != *room),
        _ => {}
    }
}

fn print_event(event: &Event) {
    match event {
        Event::PresenceChanged(p) => println!(
            "* {} is in room {} (mic {}, deafened {}, sharing {})",
            shown(&p.name),
            p.room.map_or(0, |r| r.0),
            p.mic,
            p.deafened,
            p.sharing
        ),
        Event::PresenceEnded(user) => println!("* user {} left", user.0),
        Event::RoomCreated(room) => {
            println!("* room {} created: {}", room.id.0, shown(&room.name))
        }
        Event::RoomRenamed { room, name } => {
            println!("* room {} renamed to {}", room.0, shown(name))
        }
        Event::RoomDeleted(room) => println!("* room {} deleted", room.0),
        Event::RoomLocked { room, locked } => println!("* room {} locked: {locked}", room.0),
        Event::RoomState { room, epoch, peers } => {
            println!(
                "* room {} is at epoch {} with {} peers",
                room.0,
                epoch.0,
                peers.len()
            )
        }
        Event::InviteBurned { code_id } => {
            println!(
                "* invite {}-… was burned after 3 wrong tries",
                shown(code_id)
            )
        }
        Event::Kicked { reason } => println!("* disconnected: {}", shown(reason)),
        other => println!("* {other:?}"),
    }
}

const NOTHING_CHANGED: &str = "your identity is in the keychain; nothing was changed";
const START_OVER_CONFIRMATION: &str = "start over";

/// Starts over, but only while the keychain still lacks the identity and once the user
/// confirmed at a terminal, or `confirmed` beforehand.
async fn start_over(confirmed: bool) -> Result<()> {
    if !confirmed && !std::io::stdin().is_terminal() {
        bail!("start-over asks for confirmation at a terminal; pass --yes to skip it");
    }
    match Keystore::open_default() {
        Ok(_) => {
            eprintln!("{NOTHING_CHANGED}");
            return Ok(());
        }
        Err(e) if !e.is::<IdentityMissing>() => return Err(e),
        Err(e) => eprintln!("{e:#}."),
    }
    if !confirmed {
        eprint!(
            "Starting over makes a new identity and forgets every server; joining them again \
             needs new invites. Type `{START_OVER_CONFIRMATION}` to go on: "
        );
        let mut line = String::new();
        BufReader::new(tokio::io::stdin())
            .read_line(&mut line)
            .await?;
        if line.trim() != START_OVER_CONFIRMATION {
            bail!("not confirmed; nothing was changed");
        }
    }
    let (_, started) = Keystore::start_over()?;
    eprintln!(
        "{}",
        match started {
            StartedOver::Recovered => NOTHING_CHANGED,
            StartedOver::NewIdentity =>
                "started a new identity and forgot every server; join them again with new invites",
        }
    );
    Ok(())
}

fn offer_start_over(e: anyhow::Error) -> anyhow::Error {
    if !e.is::<IdentityMissing>() {
        return e;
    }
    anyhow::anyhow!(
        "{e:#}. If it is gone for good, run `throcc-cli start-over`: that starts a new identity \
         and forgets every server, and joining them again needs new invites"
    )
}

/// `text` from the server, made safe to print: control characters (escape sequences
/// included), bidi overrides and line separators become `\u{..}`, so a server can neither
/// drive the terminal nor fake or hide lines of output.
fn shown(text: &str) -> Cow<'_, str> {
    if !text.chars().any(moves_terminal) {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if moves_terminal(c) {
            out.extend(c.escape_unicode());
        } else {
            out.push(c);
        }
    }
    Cow::Owned(out)
}

fn moves_terminal(c: char) -> bool {
    c.is_control()
        || matches!(c,
            '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{2028}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

#[cfg(test)]
mod tests {
    use super::*;
    use throcc_proto::ids::{Epoch, Role, UserId};
    use throcc_proto::messages::{Placed, ServerProfile};

    fn welcome() -> Welcome {
        Welcome {
            me: UserId(1),
            role: Role::Admin,
            rooms: vec![],
            presence: vec![],
            placed: Placed {
                room: None,
                epoch: Epoch(0),
                tracks: None,
                peers: vec![],
                audiences: Vec::new(),
            },
            server: ServerProfile::default(),
        }
    }

    fn presence(user: u64, room: Option<RoomId>) -> Presence {
        Presence {
            user: UserId(user),
            name: format!("user {user}"),
            avatar: None,
            room,
            mic: false,
            deafened: false,
            sharing: false,
            role: Role::User,
        }
    }

    #[test]
    fn starting_over_is_its_own_command_and_never_rides_along() {
        let parse = |args: &[&str]| Cli::try_parse_from([&["throcc-cli"], args].concat());
        assert!(parse(&["--start-over", "rooms", "voice.example.net"]).is_err());
        assert!(parse(&["rooms", "voice.example.net", "--start-over"]).is_err());
        assert!(matches!(
            parse(&["start-over"]).unwrap().command,
            Command::StartOver { yes: false }
        ));
        assert!(matches!(
            parse(&["start-over", "--yes"]).unwrap().command,
            Command::StartOver { yes: true }
        ));
    }

    #[test]
    fn rooms_created_after_sign_in_can_be_joined() {
        let mut welcome = welcome();
        let room = Room {
            id: RoomId(7),
            name: "Foo".into(),
            locked: false,
        };
        follow(&mut welcome, &Event::RoomCreated(room));
        assert_eq!(
            find_room(room_names(&welcome.rooms), "foo").unwrap(),
            RoomId(7)
        );

        let renamed = Event::RoomRenamed {
            room: RoomId(7),
            name: "Bar".into(),
        };
        follow(&mut welcome, &renamed);
        assert_eq!(
            find_room(room_names(&welcome.rooms), "Bar").unwrap(),
            RoomId(7)
        );

        follow(&mut welcome, &Event::RoomDeleted(RoomId(7)));
        assert!(find_room(room_names(&welcome.rooms), "7").is_err());
    }

    #[test]
    fn server_text_cannot_reach_the_terminal() {
        assert!(matches!(shown("Room 1 — ok"), Cow::Borrowed("Room 1 — ok")));
        assert_eq!(shown("\x1b]52;c;cm0=\x07x"), "\\u{1b}]52;c;cm0=\\u{7}x");
        assert_eq!(shown("a\u{9b}2Jb"), "a\\u{9b}2Jb");
        assert_eq!(shown("a\nb\rc\u{7f}"), "a\\u{a}b\\u{d}c\\u{7f}");
        assert_eq!(shown("x\u{202E}y\u{2028}z"), "x\\u{202e}y\\u{2028}z");
        assert_eq!(
            shown("\u{061C}\u{200E}\u{200F}\u{2066}\u{2069}"),
            "\\u{61c}\\u{200e}\\u{200f}\\u{2066}\\u{2069}"
        );
    }

    #[test]
    fn invite_code_prefers_the_argument_then_the_environment() {
        let env = || Some("ENV-CODE".to_owned());
        assert_eq!(
            given_code(Some("ARG".into()), env()).as_deref(),
            Some("ARG")
        );
        assert_eq!(given_code(None, env()).as_deref(), Some("ENV-CODE"));
        assert_eq!(given_code(Some("-".into()), env()), None);
        assert_eq!(given_code(None, None), None);
    }

    #[test]
    fn presence_follows_people() {
        let mut welcome = welcome();
        follow(&mut welcome, &Event::PresenceChanged(presence(2, None)));
        follow(
            &mut welcome,
            &Event::PresenceChanged(presence(2, Some(RoomId(3)))),
        );
        assert_eq!(welcome.presence, [presence(2, Some(RoomId(3)))]);

        follow(&mut welcome, &Event::PresenceEnded(UserId(2)));
        assert!(welcome.presence.is_empty());
    }
}
