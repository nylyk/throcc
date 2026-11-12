mod messages;
mod state;

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc};

use anyhow::{Context, Result, anyhow, bail};
use throcc_client_core::avatars;
use throcc_client_core::blocked::BlockedStore;
use throcc_client_core::client::{self, Client, ClientApi, MockClient, UiCommand, UiEvent};
use throcc_client_core::keystore::{self, Keystore};
use throcc_client_core::profile::Profile as CoreProfile;
use throcc_client_core::server_look::ServerLook;
use throcc_client_core::settings::Settings;
use throcc_client_core::state::BLOCKED_NAME;
use throcc_client_core::theme;
use throcc_client_core::verified::VerifiedStore;
use throcc_proto::Fingerprint;
use throcc_proto::ids::{Hash, UserId};
use throcc_proto::invite::InviteCode;
use throcc_proto::messages::{
    MAX_AVATAR_BYTES, MAX_NAME_CHARS, MAX_PASSWORD_CHARS, shows_something, valid_name,
    valid_password,
};
use throcc_proto::permissions;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{broadcast, watch};

pub use messages::{Command, Event, NameChange, PictureChange, User};
pub use state::{
    Connection, CoreState, CurrentRoom, Health, KeyWarning, Person, Role, Room, Server, Share,
    Sound, SoundLevel,
};

uniffi::setup_scaffolding!();

/// Why the core couldn't do what an app asked, in words for the user.
#[derive(Debug, thiserror::Error, uniffi::Error)]
#[uniffi(flat_error)]
pub enum CoreError {
    #[error("{0:#}")]
    Failed(anyhow::Error),
}

impl From<anyhow::Error> for CoreError {
    fn from(error: anyhow::Error) -> Self {
        Self::Failed(error)
    }
}

/// Hears of every change while connected. Calls arrive in order, one at a time, on a
/// thread of the core's.
#[uniffi::export(with_foreign)]
pub trait Listener: Send + Sync {
    fn state(&self, state: CoreState);
    fn event(&self, event: Event);
}

/// Our name and picture, which every server we join gets.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct Profile {
    pub name: String,
    pub picture: Option<Vec<u8>>,
}

/// A server this install has joined, as it looked when last connected.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct KnownServer {
    pub address: String,
    pub fingerprint: Vec<u8>,
    pub name: Option<String>,
    pub picture: Option<Vec<u8>>,
}

/// A theme by name with its colours.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ThemeChoice {
    pub name: String,
    pub palette: Palette,
}

/// The eight colours a theme is made of, each as 0xRRGGBB.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Record)]
pub struct Palette {
    pub bg: u32,
    pub panel: u32,
    pub text: u32,
    pub muted: u32,
    pub accent: u32,
    pub warning: u32,
    pub good: u32,
    pub bad: u32,
}

impl Palette {
    fn colors(self) -> [u32; 8] {
        [
            self.bg,
            self.panel,
            self.text,
            self.muted,
            self.accent,
            self.warning,
            self.good,
            self.bad,
        ]
    }
}

impl From<[u32; 8]> for Palette {
    fn from([bg, panel, text, muted, accent, warning, good, bad]: [u32; 8]) -> Self {
        Self {
            bg,
            panel,
            text,
            muted,
            accent,
            warning,
            good,
            bad,
        }
    }
}

impl From<(&str, [u32; 8])> for ThemeChoice {
    fn from((name, colors): (&str, [u32; 8])) -> Self {
        Self {
            name: name.to_owned(),
            palette: Palette::from(colors),
        }
    }
}

/// Someone verified on this install.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct VerifiedPerson {
    pub key: Vec<u8>,
    /// The name they had when verified.
    pub name: String,
    /// The servers they were seen on, by address where the server is still known.
    pub servers: Vec<String>,
}

/// Someone blocked on this install, as they looked when blocked.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct BlockedPerson {
    pub key: Vec<u8>,
    pub name: String,
    pub picture: Option<Vec<u8>>,
    /// The server they were blocked on, by address where it is still known.
    pub server: Option<String>,
}

/// What a typed name is to a server.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum NameCheck {
    Empty,
    /// The name as a server keeps it, trimmed.
    Fine {
        name: String,
    },
    TooLong,
    ShowsNothing,
    HidesText,
}

/// The limits every server holds a profile and a room to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Record)]
pub struct Limits {
    pub name_chars: u32,
    pub password_chars: u32,
    pub picture_bytes: u32,
}

/// What a role may do on its server.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Record)]
pub struct Permissions {
    pub manage_rooms: bool,
    pub list_users: bool,
    pub edit_server: bool,
    /// How many invites it may have open, or `None` for no cap.
    pub invite_cap: Option<u32>,
}

/// The core for one app: what the install keeps, and the connection to one server at a time.
#[derive(uniffi::Object)]
pub struct Core {
    mock: bool,
    settings: Mutex<Settings>,
    verified: VerifiedStore,
    blocked: BlockedStore,
    /// The servers the mock has joined.
    sample_servers: Mutex<Vec<KnownServer>>,
    listener: Arc<Mutex<Option<Arc<dyn Listener>>>>,
    /// Queues what the listener hears, each with the number of the connection it is from.
    told: mpsc::Sender<(u64, Told)>,
    /// The number of the connection the listener hears, which the end of one raises.
    connection: Arc<AtomicU64>,
    /// Held while the connection is made or ended, so that happens one at a time.
    switching: Mutex<()>,
    connected: Mutex<Option<Connected>>,
}

/// What the listener hears.
enum Told {
    State(Box<CoreState>),
    Event(Event),
}

struct Connected {
    client: Arc<dyn ClientApi>,
    /// Carries a profile saved while the client runs to its session.
    profile: watch::Sender<CoreProfile>,
}

#[uniffi::export]
impl Core {
    /// The core over what this install keeps in the directories set with `set_app_dirs`.
    #[uniffi::constructor]
    pub fn new() -> Result<Arc<Self>, CoreError> {
        let dir = keystore::config_dir()?;
        let settings = Settings::load(&dir).context("reading the settings")?;
        let verified = VerifiedStore::load(&dir).context("reading who you verified")?;
        let blocked = BlockedStore::load(&dir).context("reading who you blocked")?;
        Ok(Self::with(false, settings, verified, blocked))
    }

    /// A core that keeps nothing and connects to sample servers, with our profile set once
    /// `onboarded`.
    #[uniffi::constructor]
    pub fn mock(onboarded: bool) -> Arc<Self> {
        let profile = onboarded.then(|| CoreProfile {
            name: "nori".into(),
            picture: client::sample_picture(UserId(1)).map(Arc::from),
        });
        let settings = Settings {
            profile,
            ..Settings::default()
        };
        let (verified, blocked) = (VerifiedStore::in_memory(), BlockedStore::in_memory());
        Self::with(true, settings, verified, blocked)
    }

    pub fn set_listener(&self, listener: Option<Arc<dyn Listener>>) {
        *lock(&self.listener) = listener;
    }

    pub fn profile(&self) -> Option<Profile> {
        let settings = lock(&self.settings);
        settings.profile.as_ref().map(|profile| Profile {
            name: profile.name.clone(),
            picture: profile.picture.as_ref().map(|bytes| bytes.to_vec()),
        })
    }

    /// Keeps our name and picture, and gives them to the server we are connected to. The
    /// picture is an image file of at most 64 KiB.
    pub fn set_profile(&self, profile: Profile) -> Result<(), CoreError> {
        let name = valid_name(&profile.name).context("that name can't be used")?;
        if profile
            .picture
            .as_ref()
            .is_some_and(|picture| picture.len() > MAX_AVATAR_BYTES)
        {
            return Err(anyhow!("the picture is larger than 64 KiB").into());
        }
        let profile = CoreProfile {
            name: name.to_owned(),
            picture: profile.picture.map(Arc::from),
        };
        self.change_settings(|settings| settings.profile = Some(profile.clone()))?;
        self.pass_on(profile);
        Ok(())
    }

    /// The preset themes to pick from. The first is the default.
    pub fn themes(&self) -> Vec<ThemeChoice> {
        theme::PRESETS.into_iter().map(ThemeChoice::from).collect()
    }

    /// The theme in use: a preset, or the user's own colours under the Custom theme's name.
    pub fn theme(&self) -> ThemeChoice {
        theme::picked(&lock(&self.settings).appearance).into()
    }

    /// The colours the Custom theme starts from: the user's own once saved, else the
    /// picked theme's.
    pub fn custom_palette(&self) -> Palette {
        theme::custom(&lock(&self.settings).appearance).into()
    }

    /// Picks the preset theme called `name`.
    pub fn set_theme(&self, name: String) -> Result<(), CoreError> {
        let (name, _) =
            theme::preset(&name).with_context(|| format!("there is no theme called {name}"))?;
        self.change_settings(|settings| settings.appearance.theme = Some(name.to_owned()))
    }

    /// Picks the Custom theme with `palette` as its colours.
    pub fn set_custom_theme(&self, palette: Palette) -> Result<(), CoreError> {
        let colors = theme::to_hex(palette.colors().map(|rgb| rgb & 0xFF_FFFF));
        self.change_settings(|settings| {
            settings.appearance.theme = Some(theme::CUSTOM.to_owned());
            settings.appearance.palette = Some(colors);
        })
    }

    /// The servers this install has joined.
    pub fn servers(&self) -> Result<Vec<KnownServer>, CoreError> {
        if self.mock {
            return Ok(lock(&self.sample_servers).clone());
        }
        let (dir, cache) = (keystore::config_dir()?, keystore::cache_dir()?);
        let known = Keystore::default_known_servers()?;
        let servers = known.iter().map(|(address, fingerprint)| {
            let look = ServerLook::load(&dir, &fingerprint).unwrap_or_default();
            KnownServer {
                address: address.to_owned(),
                fingerprint: fingerprint.0.to_vec(),
                name: look.name,
                picture: look
                    .avatar
                    .and_then(|hash| avatars::load(&cache, hash))
                    .map(|bytes| bytes.to_vec()),
            }
        });
        Ok(servers.collect())
    }

    /// Takes the server at `address` off the list without leaving it. It blocks as
    /// [`Self::disconnect`] does.
    pub fn forget_server(&self, address: String) -> Result<(), CoreError> {
        let _switching = lock(&self.switching);
        if self.connected_to(&address) {
            self.end_connection();
        }
        if self.mock {
            lock(&self.sample_servers).retain(|server| server.address != address);
        } else {
            Keystore::default_known_servers()?.forget(&address)?;
        }
        Ok(())
    }

    /// Signs in to the server at `address`, joining it first with `invite`, and waits for
    /// its answer. It takes the place of any connection there was.
    pub fn connect(&self, address: String, invite: Option<String>) -> Result<(), CoreError> {
        let Some(profile) = lock(&self.settings).profile.clone() else {
            return Err(anyhow!("set your name first").into());
        };
        let invite = invite.map(|code| parse_invite(&code)).transpose()?;
        let _switching = lock(&self.switching);
        self.end_connection();
        let (kept_profile, profile_changes) = watch::channel(profile.clone());
        let (verified, blocked) = (self.verified.clone(), self.blocked.clone());
        let client: Arc<dyn ClientApi> = if self.mock {
            Arc::new(MockClient::at(&address, verified, blocked))
        } else {
            let keystore = Keystore::open_default()?;
            let connected = Client::connect(
                &address,
                keystore,
                invite.as_ref(),
                profile_changes,
                None,
                verified,
                blocked,
            );
            Arc::new(connected.map_err(anyhow::Error::from)?)
        };
        let events = client.events();
        let state = CoreState::from(&*client.state());
        let connection = self.connection.load(Ordering::SeqCst);
        *lock(&self.connected) = Some(Connected {
            client,
            profile: kept_profile,
        });
        let _ = self.told.send((connection, Told::State(Box::new(state))));
        self.forward(events, connection);
        let saved = lock(&self.settings).profile.clone();
        if let Some(saved_meanwhile) = saved.filter(|saved| *saved != profile) {
            self.pass_on(saved_meanwhile);
        }
        Ok(())
    }

    /// Ends the connection, staying a member of the server. It waits for a `connect` under
    /// way, and then up to three seconds for the goodbye to the server.
    pub fn disconnect(&self) {
        let _switching = lock(&self.switching);
        self.end_connection();
    }

    pub fn command(&self, command: Command) {
        let Some(command) = command.into_core() else {
            return;
        };
        if let Some(connected) = &*lock(&self.connected) {
            connected.client.command(command);
        }
    }

    pub fn state(&self) -> CoreState {
        match &*lock(&self.connected) {
            Some(connected) => CoreState::from(&*connected.client.state()),
            None => CoreState::default(),
        }
    }

    /// The 60-digit safety number with whoever holds the identity key `key`.
    pub fn safety_number(&self, key: Vec<u8>) -> Option<String> {
        let key = key.as_slice().try_into().ok()?;
        let connected = lock(&self.connected);
        Some(connected.as_ref()?.client.safety_number(&key))
    }

    /// The picture behind `hash`. One the core lacks is fetched, and `Event::AvatarReady`
    /// says when it has arrived.
    pub fn avatar(&self, hash: Vec<u8>) -> Option<Vec<u8>> {
        let hash = Hash(hash.as_slice().try_into().ok()?);
        let connected = lock(&self.connected);
        let picture = connected.as_ref()?.client.avatar(hash)?;
        Some(picture.to_vec())
    }

    /// Everyone verified on this install.
    pub fn verified(&self) -> Vec<VerifiedPerson> {
        let addresses = self.addresses();
        let people = self.verified.list().into_iter().map(|person| {
            let servers = person
                .servers
                .iter()
                .map(|server| address_of(&addresses, server));
            VerifiedPerson {
                key: person.key.0.to_vec(),
                name: person.name,
                servers: servers.collect(),
            }
        });
        people.collect()
    }

    /// Takes the identity key `key` off the verified ones, on every server.
    pub fn unverify(&self, key: Vec<u8>) -> Result<(), CoreError> {
        let key = messages::fingerprint(&key).context("that is no identity key")?;
        self.verified.unverify(&key)?;
        self.tell_client(UiCommand::VerifiedChanged);
        Ok(())
    }

    /// Everyone blocked on this install.
    pub fn blocked(&self) -> Vec<BlockedPerson> {
        let addresses = self.addresses();
        let cache = keystore::cache_dir().ok().filter(|_| !self.mock);
        let connected = lock(&self.connected);
        let fetched = |hash| connected.as_ref()?.client.avatar(hash);
        let people = self.blocked.list().into_iter().map(|person| {
            let cached = person
                .avatar
                .zip(cache.as_ref())
                .and_then(|(hash, cache)| avatars::load(cache, hash))
                .or_else(|| person.avatar.and_then(fetched));
            BlockedPerson {
                key: person.key.0.to_vec(),
                name: person.name,
                picture: cached.map(|bytes| bytes.to_vec()),
                server: person.server.map(|server| address_of(&addresses, &server)),
            }
        });
        people.collect()
    }

    /// Takes the block off whoever holds the identity key `key`, on every server.
    pub fn unblock(&self, key: Vec<u8>) -> Result<(), CoreError> {
        let key = messages::fingerprint(&key).context("that is no identity key")?;
        self.blocked.unblock(&key)?;
        self.tell_client(UiCommand::BlockedChanged);
        Ok(())
    }
}

impl Core {
    fn with(
        mock: bool,
        settings: Settings,
        verified: VerifiedStore,
        blocked: BlockedStore,
    ) -> Arc<Self> {
        let listener = Arc::new(Mutex::new(None));
        let connection = Arc::new(AtomicU64::new(0));
        let (told, queue) = mpsc::channel();
        std::thread::spawn({
            let (listener, connection) = (listener.clone(), connection.clone());
            move || tell(&queue, &connection, &listener)
        });
        Arc::new(Self {
            mock,
            settings: Mutex::new(settings),
            verified,
            blocked,
            sample_servers: Mutex::new(if mock { sample_servers() } else { Vec::new() }),
            listener,
            told,
            connection,
            switching: Mutex::new(()),
            connected: Mutex::new(None),
        })
    }

    /// Ends the connection there is, after which the listener hears nothing more of it.
    fn end_connection(&self) {
        let Some(connected) = lock(&self.connected).take() else {
            return;
        };
        let next = self.connection.fetch_add(1, Ordering::SeqCst) + 1;
        drop(connected);
        let _ = self.told.send((next, Told::State(Box::default())));
    }

    /// Gives `profile` to the server we are connected to, and to the session for later
    /// sign-ins.
    fn pass_on(&self, profile: CoreProfile) {
        if let Some(connected) = &*lock(&self.connected) {
            connected.profile.send_replace(profile.clone());
            connected.client.command(UiCommand::SetProfile(profile));
        }
    }

    /// Changes the settings and keeps them, in memory only for the mock.
    fn change_settings(&self, change: impl FnOnce(&mut Settings)) -> Result<(), CoreError> {
        let mut settings = lock(&self.settings);
        let mut changed = settings.clone();
        change(&mut changed);
        if !self.mock {
            changed
                .save(&keystore::config_dir()?)
                .context("saving the settings")?;
        }
        *settings = changed;
        Ok(())
    }

    fn connected_to(&self, address: &str) -> bool {
        let connected = lock(&self.connected);
        let state = connected.as_ref().map(|connected| connected.client.state());
        state.is_some_and(|state| state.server.as_ref().is_some_and(|s| s.address == address))
    }

    fn tell_client(&self, command: UiCommand) {
        if let Some(connected) = &*lock(&self.connected) {
            connected.client.command(command);
        }
    }

    /// The known servers' addresses by fingerprint.
    fn addresses(&self) -> Vec<(String, Fingerprint)> {
        if self.mock {
            let servers = lock(&self.sample_servers);
            let known = servers.iter().filter_map(|server| {
                Some((
                    server.address.clone(),
                    messages::fingerprint(&server.fingerprint)?,
                ))
            });
            return known.collect();
        }
        let Ok(known) = Keystore::default_known_servers() else {
            return Vec::new();
        };
        let known = known.iter();
        known
            .map(|(address, fingerprint)| (address.to_owned(), fingerprint))
            .collect()
    }

    /// Queues what the client behind `events` says, as news of `connection`, until that
    /// client is gone.
    fn forward(&self, mut events: broadcast::Receiver<UiEvent>, connection: u64) {
        let told = self.told.clone();
        std::thread::spawn(move || {
            loop {
                let news = match events.blocking_recv() {
                    Ok(UiEvent::State(state)) => {
                        Some(Told::State(Box::new(CoreState::from(&*state))))
                    }
                    Ok(event) => Event::from_core(event).map(Told::Event),
                    Err(RecvError::Lagged(_)) => None,
                    Err(RecvError::Closed) => return,
                };
                if let Some(news) = news
                    && told.send((connection, news)).is_err()
                {
                    return;
                }
            }
        });
    }
}

/// Gives the listener what `queue` holds of the current connection, in order, until the
/// core is gone.
fn tell(
    queue: &mpsc::Receiver<(u64, Told)>,
    current: &AtomicU64,
    listener: &Mutex<Option<Arc<dyn Listener>>>,
) {
    for (connection, told) in queue {
        if connection != current.load(Ordering::SeqCst) {
            continue;
        }
        let Some(listener) = lock(listener).clone() else {
            continue;
        };
        // UniFFI turns an exception the app's listener throws into a panic.
        let _ = catch_unwind(AssertUnwindSafe(|| match told {
            Told::State(state) => listener.state(*state),
            Told::Event(event) => listener.event(event),
        }));
    }
}

/// Sets where the app keeps its data and what it can fetch again. The app calls this once,
/// before anything else.
#[cfg(any(target_os = "ios", target_os = "android"))]
#[uniffi::export]
pub fn set_app_dirs(data: String, cache: String) {
    keystore::set_app_dirs(data.into(), cache.into());
}

#[uniffi::export]
pub fn limits() -> Limits {
    Limits {
        name_chars: MAX_NAME_CHARS as u32,
        password_chars: MAX_PASSWORD_CHARS as u32,
        picture_bytes: MAX_AVATAR_BYTES as u32,
    }
}

/// The name a blocked person is shown under.
#[uniffi::export]
pub fn blocked_name() -> String {
    BLOCKED_NAME.to_owned()
}

/// The name of the theme that holds the user's own colours.
#[uniffi::export]
pub fn custom_theme_name() -> String {
    theme::CUSTOM.to_owned()
}

/// Whether `palette` makes a light theme.
#[uniffi::export]
pub fn is_light_theme(palette: Palette) -> bool {
    theme::is_light(palette.bg)
}

/// What `typed` is as a name, as every server judges it.
#[uniffi::export]
pub fn check_name(typed: String) -> NameCheck {
    if typed.trim().is_empty() {
        return NameCheck::Empty;
    }
    match valid_name(&typed) {
        Some(name) => NameCheck::Fine {
            name: name.to_owned(),
        },
        None if typed.trim().chars().count() > MAX_NAME_CHARS => NameCheck::TooLong,
        None if !shows_something(&typed) => NameCheck::ShowsNothing,
        None => NameCheck::HidesText,
    }
}

/// Whether a server takes `typed` as a room password.
#[uniffi::export]
pub fn is_valid_password(typed: String) -> bool {
    valid_password(&typed)
}

/// Whether `typed` has the shape of an invite code.
#[uniffi::export]
pub fn is_invite_code(typed: String) -> bool {
    parse_invite(&typed).is_ok()
}

/// A key or fingerprint as groups of four hex digits.
#[uniffi::export]
pub fn fingerprint_text(key: Vec<u8>) -> String {
    messages::fingerprint(&key).map_or_else(String::new, |key| key.to_string())
}

/// The first two groups of [`fingerprint_text`].
#[uniffi::export]
pub fn fingerprint_short(key: Vec<u8>) -> String {
    messages::fingerprint(&key).map_or_else(String::new, |key| key.short())
}

#[uniffi::export]
pub fn permissions_of(role: Role) -> Permissions {
    let role = role.into();
    Permissions {
        manage_rooms: permissions::can_manage_rooms(role),
        list_users: permissions::can_list_users(role),
        edit_server: permissions::can_edit_server(role),
        invite_cap: permissions::invite_cap(role),
    }
}

#[uniffi::export]
pub fn can_kick_from_room(actor: Role, target: Role) -> bool {
    permissions::can_kick_from_room(actor.into(), target.into())
}

#[uniffi::export]
pub fn can_remove(actor: Role, target: Role) -> bool {
    permissions::can_remove(actor.into(), target.into())
}

#[uniffi::export]
pub fn can_set_role(actor: Role, target: Role, new: Role) -> bool {
    permissions::can_set_role(actor.into(), target.into(), new.into())
}

fn parse_invite(code: &str) -> Result<InviteCode> {
    match code.trim().parse() {
        Ok(code) => Ok(code),
        Err(_) => bail!("that is no invite code"),
    }
}

fn address_of(addresses: &[(String, Fingerprint)], server: &Fingerprint) -> String {
    let known = addresses.iter().find(|(_, known)| known == server);
    known.map_or_else(|| server.short(), |(address, _)| address.clone())
}

/// The servers the mock has joined.
fn sample_servers() -> Vec<KnownServer> {
    let server = |address: &str, key: u8, name: &str, picture: Option<&[u8]>| KnownServer {
        address: address.to_owned(),
        fingerprint: vec![key; 32],
        name: Some(name.to_owned()),
        picture: picture.map(<[u8]>::to_vec),
    };
    vec![
        server(
            "owls.thro.cc",
            0x2c,
            "Night Owls",
            Some(client::SAMPLE_SERVER_PICTURE),
        ),
        server("192.168.1.20", 0x5e, "LAN Party", None),
        server("192.0.2.40", 0x91, "Survival SMP", None),
    ]
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;

    struct Recorder(Mutex<mpsc::Sender<CoreState>>);

    impl Listener for Recorder {
        fn state(&self, state: CoreState) {
            lock(&self.0).send(state).ok();
        }

        fn event(&self, _event: Event) {}
    }

    fn next(
        states: &mpsc::Receiver<CoreState>,
        what: &str,
        ready: impl Fn(&CoreState) -> bool,
    ) -> CoreState {
        loop {
            let state = states
                .recv_timeout(Duration::from_secs(5))
                .unwrap_or_else(|_| panic!("no state with {what}"));
            if ready(&state) {
                return state;
            }
        }
    }

    #[test]
    fn the_mock_connects_and_tells_the_listener_of_every_change() {
        let core = Core::mock(true);
        let (sender, states) = mpsc::channel();
        core.set_listener(Some(Arc::new(Recorder(Mutex::new(sender)))));
        core.connect("owls.thro.cc".into(), None).unwrap();
        let connected = next(&states, "a connection", |s| {
            s.connection == Connection::Connected
        });
        assert_eq!(connected.server.unwrap().address, "owls.thro.cc");
        assert!(!connected.people.is_empty());

        let room = connected.rooms[0].id;
        core.command(Command::Join { room: Some(room) });
        next(&states, "us in the room", |s| {
            s.current.as_ref().is_some_and(|c| c.room == room)
        });

        let other = connected
            .people
            .iter()
            .find(|p| Some(p.user) != connected.me);
        let other = other.unwrap().user;
        core.command(Command::Block { user: other });
        let blocked = next(&states, "them blocked", |s| {
            s.people.iter().any(|p| p.user == other && p.blocked)
        });
        assert!(blocked.people.iter().any(|p| p.user == other));
        assert_eq!(core.blocked().len(), 1);
        core.unblock(core.blocked()[0].key.clone()).unwrap();
        next(&states, "them unblocked", |s| {
            s.people.iter().any(|p| p.user == other && !p.blocked)
        });

        core.disconnect();
        next(&states, "no connection", |s| {
            s.connection == Connection::Disconnected && s.server.is_none()
        });
    }

    #[test]
    fn a_block_outlasts_the_connection_and_a_forgotten_server_leaves_the_list() {
        let core = Core::mock(true);
        let (sender, states) = mpsc::channel();
        core.set_listener(Some(Arc::new(Recorder(Mutex::new(sender)))));
        assert!(
            core.connect("owls.thro.cc".into(), Some("no".into()))
                .is_err()
        );
        core.connect("owls.thro.cc".into(), None).unwrap();
        let connected = next(&states, "a connection", |s| s.server.is_some());
        let other = connected
            .people
            .iter()
            .find(|p| Some(p.user) != connected.me);
        let other = other.unwrap().user;
        core.command(Command::Block { user: other });
        next(&states, "them blocked", |s| {
            s.people.iter().any(|p| p.user == other && p.blocked)
        });

        core.connect("owls.thro.cc".into(), None).unwrap();
        next(&states, "no connection", |s| s.server.is_none());
        let again = states.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(again.people.iter().any(|p| p.user == other && p.blocked));

        core.forget_server("owls.thro.cc".into()).unwrap();
        next(&states, "no connection", |s| s.server.is_none());
        let listed = core.servers().unwrap();
        assert!(listed.iter().all(|server| server.address != "owls.thro.cc"));
        assert_eq!(listed.len(), 2);
    }

    struct PanicsOnce(Mutex<Option<mpsc::Sender<CoreState>>>, Recorder);

    impl Listener for PanicsOnce {
        fn state(&self, state: CoreState) {
            if lock(&self.0).take().is_some() {
                panic!("the listener threw");
            }
            self.1.state(state);
        }

        fn event(&self, _event: Event) {}
    }

    #[test]
    fn a_listener_that_throws_still_hears_what_follows() {
        let core = Core::mock(true);
        let (sender, states) = mpsc::channel();
        let recorder = Recorder(Mutex::new(sender.clone()));
        let listener = PanicsOnce(Mutex::new(Some(sender)), recorder);
        core.set_listener(Some(Arc::new(listener)));
        core.connect("owls.thro.cc".into(), None).unwrap();
        core.disconnect();
        next(&states, "no connection", |s| s.server.is_none());
    }

    #[test]
    fn the_mock_keeps_a_profile_and_a_theme_in_memory() {
        let core = Core::mock(false);
        assert_eq!(core.profile(), None);
        assert!(core.connect("owls.thro.cc".into(), None).is_err());
        let profile = Profile {
            name: "  mara ".into(),
            picture: None,
        };
        core.set_profile(profile).unwrap();
        assert_eq!(core.profile().unwrap().name, "mara");

        assert_eq!(core.theme(), core.themes()[0]);
        core.set_theme("tide".into()).unwrap();
        assert_eq!(core.theme().name, "Tide");
        assert_eq!(core.custom_palette().accent, 0x5CC8E8);
        let own = Palette {
            accent: 0x12_3456,
            ..core.theme().palette
        };
        core.set_custom_theme(own).unwrap();
        assert_eq!(core.theme().name, custom_theme_name());
        assert!(!is_light_theme(own));
        assert_eq!(core.theme().palette, own);
        assert_eq!(core.custom_palette(), own);
        core.set_theme("Paper".into()).unwrap();
        assert!(is_light_theme(core.theme().palette));
        assert_eq!(core.custom_palette(), own);
        assert!(core.set_theme("Nope".into()).is_err());
    }

    #[test]
    fn names_are_judged_as_servers_judge_them() {
        assert_eq!(check_name("  ".into()), NameCheck::Empty);
        let fine = NameCheck::Fine {
            name: "mara".into(),
        };
        assert_eq!(check_name(" mara ".into()), fine);
        assert_eq!(check_name("x".repeat(33)), NameCheck::TooLong);
        assert_eq!(check_name("\u{2800}".into()), NameCheck::ShowsNothing);
        assert_eq!(check_name("a\u{202E}b".into()), NameCheck::HidesText);
    }
}
