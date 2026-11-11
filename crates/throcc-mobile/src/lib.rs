mod messages;
mod state;

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use anyhow::{Context, Result, anyhow, bail};
use throcc_client_core::avatars;
use throcc_client_core::blocked::BlockedStore;
use throcc_client_core::client::{self, Client, ClientApi, MockClient, UiCommand, UiEvent};
use throcc_client_core::keystore::{self, Keystore};
use throcc_client_core::profile::Profile as CoreProfile;
use throcc_client_core::server_look::ServerLook;
use throcc_client_core::settings::{
    ACCENT, CUSTOM_THEME, DEFAULT_PALETTE, PRESET_ACCENTS, Settings,
};
use throcc_client_core::state::BLOCKED_NAME;
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
use tokio::sync::watch;

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

/// Hears of every change while connected. Calls arrive on a thread of the core's.
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

/// An accent colour by name, as 0xRRGGBB.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct Accent {
    pub name: String,
    pub rgb: u32,
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
    listener: Arc<Mutex<Option<Arc<dyn Listener>>>>,
    connected: Mutex<Option<Connected>>,
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
        if let Some(connected) = &*lock(&self.connected) {
            connected.profile.send_replace(profile.clone());
            connected.client.command(UiCommand::SetProfile(profile));
        }
        Ok(())
    }

    /// The accents to pick from. The first is the default.
    pub fn accents(&self) -> Vec<Accent> {
        let accents = PRESET_ACCENTS.iter().map(|(name, rgb)| Accent {
            name: (*name).to_owned(),
            rgb: *rgb,
        });
        accents.collect()
    }

    /// The accent in use: a preset, or a colour of the user's own under the name "Custom".
    pub fn accent(&self) -> Accent {
        let settings = lock(&self.settings);
        let appearance = &settings.appearance;
        let custom = || {
            let rgb = appearance.palette.as_ref()?.get(ACCENT)?;
            let rgb = u32::from_str_radix(rgb.trim_start_matches('#'), 16).ok()?;
            Some((CUSTOM_THEME, rgb))
        };
        let (name, rgb) = match appearance.theme.as_deref() {
            Some(CUSTOM_THEME) => custom(),
            Some(name) => PRESET_ACCENTS
                .iter()
                .find(|(preset, _)| preset.eq_ignore_ascii_case(name))
                .copied(),
            None => None,
        }
        .unwrap_or(PRESET_ACCENTS[0]);
        Accent {
            name: name.to_owned(),
            rgb,
        }
    }

    /// Picks the preset accent called `name`.
    pub fn set_accent(&self, name: String) -> Result<(), CoreError> {
        if !PRESET_ACCENTS.iter().any(|(preset, _)| *preset == name) {
            return Err(anyhow!("there is no accent called {name}").into());
        }
        self.change_settings(|settings| settings.appearance.theme = Some(name))
    }

    /// Picks an accent of the user's own, as 0xRRGGBB.
    pub fn set_custom_accent(&self, rgb: u32) -> Result<(), CoreError> {
        let mut palette = DEFAULT_PALETTE;
        palette[ACCENT] = rgb & 0xFF_FFFF;
        let palette = palette.iter().map(|rgb| format!("#{rgb:06X}")).collect();
        self.change_settings(|settings| {
            settings.appearance.theme = Some(CUSTOM_THEME.to_owned());
            settings.appearance.palette = Some(palette);
        })
    }

    /// The servers this install has joined.
    pub fn servers(&self) -> Result<Vec<KnownServer>, CoreError> {
        if self.mock {
            return Ok(sample_servers());
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

    /// Takes the server at `address` off the list without leaving it.
    pub fn forget_server(&self, address: String) -> Result<(), CoreError> {
        if self.connected_to(&address) {
            self.disconnect();
        }
        if !self.mock {
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
        self.disconnect();
        let (profile, kept_profile) = watch::channel(profile);
        let (verified, blocked) = (self.verified.clone(), self.blocked.clone());
        let client: Arc<dyn ClientApi> = if self.mock {
            Arc::new(MockClient::at(&address, verified, blocked))
        } else {
            let invite = match invite {
                Some(code) => Some(parse_invite(&code)?),
                None => None,
            };
            let keystore = Keystore::open_default()?;
            let connected = Client::connect(
                &address,
                keystore,
                invite.as_ref(),
                kept_profile,
                None,
                verified,
                blocked,
            );
            Arc::new(connected.map_err(anyhow::Error::from)?)
        };
        self.forward_events(&client);
        let state = CoreState::from(&*client.state());
        *lock(&self.connected) = Some(Connected { client, profile });
        self.tell_listener(state);
        Ok(())
    }

    /// Ends the connection, staying a member of the server.
    pub fn disconnect(&self) {
        let Some(connected) = lock(&self.connected).take() else {
            return;
        };
        drop(connected);
        self.tell_listener(CoreState::default());
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
        Arc::new(Self {
            mock,
            settings: Mutex::new(settings),
            verified,
            blocked,
            listener: Arc::new(Mutex::new(None)),
            connected: Mutex::new(None),
        })
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

    /// Gives the listener `state`, with no lock held while it runs.
    fn tell_listener(&self, state: CoreState) {
        let listener = lock(&self.listener).clone();
        if let Some(listener) = listener {
            listener.state(state);
        }
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
        let known = self.servers().unwrap_or_default().into_iter();
        known
            .filter_map(|server| {
                Some((server.address, messages::fingerprint(&server.fingerprint)?))
            })
            .collect()
    }

    /// Passes what `client` says on to the listener until the client is gone.
    fn forward_events(&self, client: &Arc<dyn ClientApi>) {
        let mut events = client.events();
        let listener = self.listener.clone();
        std::thread::spawn(move || {
            loop {
                let event = match events.blocking_recv() {
                    Ok(event) => event,
                    Err(RecvError::Lagged(_)) => continue,
                    Err(RecvError::Closed) => return,
                };
                let Some(listener) = lock(&listener).clone() else {
                    continue;
                };
                match event {
                    UiEvent::State(state) => listener.state(CoreState::from(&*state)),
                    other => {
                        if let Some(event) = Event::from_core(other) {
                            listener.event(event);
                        }
                    }
                }
            }
        });
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
    fn the_mock_keeps_a_profile_and_an_accent_in_memory() {
        let core = Core::mock(false);
        assert_eq!(core.profile(), None);
        assert!(core.connect("owls.thro.cc".into(), None).is_err());
        let profile = Profile {
            name: "  mara ".into(),
            picture: None,
        };
        core.set_profile(profile).unwrap();
        assert_eq!(core.profile().unwrap().name, "mara");

        assert_eq!(core.accent(), core.accents()[0]);
        core.set_accent("Tide".into()).unwrap();
        assert_eq!(core.accent().rgb, 0x5CC8E8);
        core.set_custom_accent(0x12_3456).unwrap();
        let custom = core.accent();
        assert_eq!((custom.name.as_str(), custom.rgb), ("Custom", 0x12_3456));
        assert!(core.set_accent("Nope".into()).is_err());
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
