mod messages;
mod safety;
mod state;
mod voice;

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc};

use std::collections::HashMap;

use anyhow::{Context, Result, anyhow};
use throcc_client_core::account::SignInError;
use throcc_client_core::blocked::BlockedStore;
use throcc_client_core::client::{self, Client, ClientApi, MockClient, UiCommand, UiEvent};
use throcc_client_core::install;
use throcc_client_core::keystore::{self, Keystore};
use throcc_client_core::media::video::View;
use throcc_client_core::media::voice::AudioEngine;
use throcc_client_core::picture;
use throcc_client_core::profile::{
    self as core_profile, NameCheck as NameCheckCore, Profile as CoreProfile,
};
use throcc_client_core::safety as core_safety;
use throcc_client_core::settings::{QUIETEST_GATE_DBFS, Settings};
use throcc_client_core::state::{self as core_state, BLOCKED_NAME, State as CoreStateInner};
use throcc_client_core::theme;
use throcc_client_core::verified::VerifiedStore;
use throcc_proto::ids::{Hash, UserId};
use throcc_proto::invite::InviteCode;
use throcc_proto::messages::{MAX_AVATAR_BYTES, MAX_NAME_CHARS, MAX_PASSWORD_CHARS};
use throcc_proto::permissions;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{broadcast, watch};

pub use messages::{Command, Event, NameChange, PictureChange, User};
pub use safety::SafetyCheck;
pub use state::{
    Connection, ConnectionNotice, CoreState, CurrentRoom, Health, JoinAction, KeyTrust, KeyWarning,
    NoticeAction, Person, Role, Room, Server, Share, Sound, SoundLevel,
};
pub use voice::{MicLevel, NoiseSuppression, Voice};

/// `ShareView.nativeWindow` in the Android app: the `ANativeWindow` behind a `Surface`, with a
/// reference the caller hands on to `Core::set_share_view`. 0 if it has none.
///
/// # Safety
/// Only the JVM calls this, with its environment and a `Surface`.
#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
pub unsafe extern "system" fn Java_cc_thro_throcc_ShareView_nativeWindow(
    env: *mut std::ffi::c_void,
    _class: *mut std::ffi::c_void,
    surface: *mut std::ffi::c_void,
) -> i64 {
    #[link(name = "android")]
    unsafe extern "C" {
        fn ANativeWindow_fromSurface(
            env: *mut std::ffi::c_void,
            surface: *mut std::ffi::c_void,
        ) -> *mut std::ffi::c_void;
    }
    // SAFETY: the JVM calls this with its environment and a live `Surface`, and the window
    // comes back with a reference of its own.
    unsafe { ANativeWindow_fromSurface(env, surface) as usize as i64 }
}

uniffi::setup_scaffolding!();

/// Why the core couldn't do what an app asked.
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum CoreError {
    /// The server's key isn't the one this install trusted for its address: it was
    /// reinstalled, or something pretends to be it. A fresh invite proves the new key;
    /// `Core::trust_server_key` takes it on the user's word.
    #[error("the server's key changed")]
    PinMismatch { expected: Vec<u8>, actual: Vec<u8> },
    /// Our identity is gone from the keychain, so no server we joined knows us.
    /// `Core::start_over` makes a new one once the user agrees.
    #[error("your identity is missing from this device")]
    IdentityMissing,
    /// Anything else, in words for the user.
    #[error("{reason}")]
    Failed { reason: String },
}

impl From<anyhow::Error> for CoreError {
    fn from(error: anyhow::Error) -> Self {
        Self::Failed {
            reason: format!("{error:#}"),
        }
    }
}

impl CoreError {
    /// A failed sign-in to `address`, explained as every client explains it.
    fn sign_in(error: &SignInError, address: &str) -> Self {
        if let Some((expected, actual)) = error.pin_mismatch() {
            return Self::PinMismatch {
                expected: expected.0.to_vec(),
                actual: actual.0.to_vec(),
            };
        }
        if error.identity_missing() {
            return Self::IdentityMissing;
        }
        Self::Failed {
            reason: error.explain(address),
        }
    }
}

/// What starting over did.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum StartedOver {
    /// The keychain had the identity after all; the servers stay on the list.
    Recovered,
    /// A new identity, and no servers on the list.
    NewIdentity,
}

/// Why something this install keeps couldn't be read, so the app started without it.
#[derive(Clone, Debug, Default, PartialEq, Eq, uniffi::Record)]
pub struct StoreProblems {
    pub settings: Option<String>,
    pub verified: Option<String>,
    pub blocked: Option<String>,
}

/// The colours every client mixes from a theme's eight, each as 0xRRGGBB. The muted, accent
/// and status colours read as text on every surface here.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ThemeColors {
    pub bg: u32,
    pub panel: u32,
    pub raised: u32,
    pub control: u32,
    pub line: u32,
    pub selected: u32,
    pub danger: u32,
    pub text: u32,
    pub muted: u32,
    pub accent: u32,
    pub warning: u32,
    pub good: u32,
    pub bad: u32,
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
    problems: Mutex<StoreProblems>,
    /// The servers the mock has joined.
    sample_servers: Mutex<Vec<install::KnownServer>>,
    /// Carries a profile saved from the start of a connection on to its session, which gives
    /// it to the server.
    profile_watch: Mutex<Option<watch::Sender<CoreProfile>>>,
    /// The views each share is shown in, newest last: the newest shows it, and the one before
    /// takes over when it goes.
    share_views: Mutex<HashMap<u64, Vec<(u64, View)>>>,
    next_view: AtomicU64,
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
    address: String,
    client: Arc<dyn ClientApi>,
    /// What the room's sound runs through. The mock has none.
    audio: Option<Arc<AudioEngine>>,
}

#[uniffi::export]
impl Core {
    /// The core over what this install keeps in the directories set with `set_app_dirs`.
    /// A file it can't read is left out, and `store_problems` says why.
    #[uniffi::constructor]
    pub fn new() -> Arc<Self> {
        let (settings, settings_problem) = install::load_settings();
        let (verified, verified_problem) = install::load_verified();
        let (blocked, blocked_problem) = install::load_blocked();
        let core = Self::with(false, settings, verified, blocked);
        *lock(&core.problems) = StoreProblems {
            settings: settings_problem,
            verified: verified_problem,
            blocked: blocked_problem,
        };
        core
    }

    /// Why what this install keeps couldn't be read or kept, where it couldn't.
    pub fn store_problems(&self) -> StoreProblems {
        lock(&self.problems).clone()
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
    /// picture is one `prepare_picture` made.
    pub fn set_profile(&self, profile: Profile) -> Result<(), CoreError> {
        let name = match core_profile::check_name(&profile.name) {
            NameCheckCore::Fine(name) => name,
            NameCheckCore::Empty => {
                return Err(anyhow!("Names need at least one visible character.").into());
            }
            refused => return Err(anyhow!(refused.problem().unwrap_or_default()).into()),
        };
        if let Some(problem) = profile
            .picture
            .as_deref()
            .and_then(core_profile::picture_problem)
        {
            return Err(anyhow!(problem).into());
        }
        let profile = CoreProfile {
            name,
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

    /// Picks the preset theme called `name`, keeping the Custom theme's colours as they
    /// stand.
    pub fn set_theme(&self, name: String) -> Result<(), CoreError> {
        let (name, _) =
            theme::preset(&name).with_context(|| format!("there is no theme called {name}"))?;
        self.change_settings(|settings| theme::pick(&mut settings.appearance, name))
    }

    /// Picks the Custom theme with `palette` as its colours.
    pub fn set_custom_theme(&self, palette: Palette) -> Result<(), CoreError> {
        self.change_settings(|settings| {
            theme::pick_custom(&mut settings.appearance, palette.colors());
        })
    }

    /// The servers this install has joined.
    pub fn servers(&self) -> Vec<KnownServer> {
        self.known_servers()
            .into_iter()
            .map(|server| KnownServer {
                address: server.address,
                fingerprint: server.fingerprint.0.to_vec(),
                name: server.name,
                picture: showable(server.picture),
            })
            .collect()
    }

    /// Trusts `actual` as the key of the server at `address` from now on, as the user chose
    /// to when it changed.
    pub fn trust_server_key(&self, address: String, actual: Vec<u8>) -> Result<(), CoreError> {
        let actual = messages::fingerprint(&actual).context("that is no server key")?;
        if !self.mock {
            Keystore::default_known_servers()?.trust(&address, actual)?;
        }
        Ok(())
    }

    /// Makes a new identity and forgets every server, as the user chose to after the keychain
    /// lost the old one, unless the keychain holds it again by now. It blocks until done.
    pub fn start_over(&self) -> Result<StartedOver, CoreError> {
        if self.mock {
            lock(&self.sample_servers).clear();
            return Ok(StartedOver::NewIdentity);
        }
        let (_, started) = Keystore::start_over()?;
        Ok(match started {
            keystore::StartedOver::Recovered => StartedOver::Recovered,
            keystore::StartedOver::NewIdentity => StartedOver::NewIdentity,
        })
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
        let (kept_profile, profile_changes) = watch::channel(profile);
        *lock(&self.profile_watch) = Some(kept_profile);
        let (verified, blocked) = (self.verified.clone(), self.blocked.clone());
        let mut audio = None;
        let client: Arc<dyn ClientApi> = if self.mock {
            Arc::new(MockClient::at(&address, verified, blocked))
        } else {
            let keystore = Keystore::open_default()
                .map_err(|e| CoreError::sign_in(&SignInError::Store(e), &address))?;
            let voice = lock(&self.settings).audio.voice;
            let engine = Arc::new(AudioEngine::start(voice).context("starting the audio")?);
            audio = Some(engine.clone());
            let connected = Client::connect(
                &address,
                keystore,
                invite.as_ref(),
                profile_changes,
                Some(engine),
                verified,
                blocked,
            );
            Arc::new(connected.map_err(|e| CoreError::sign_in(&e, &address))?)
        };
        let events = client.events();
        let state = CoreState::from(&*client.state());
        let connection = self.connection.load(Ordering::SeqCst);
        for (user, views) in lock(&self.share_views).iter() {
            let newest = views.last().map(|(_, view)| view.clone());
            client.set_share_view(UserId(*user), newest);
        }
        *lock(&self.connected) = Some(Connected {
            address,
            client,
            audio,
        });
        let _ = self.told.send((connection, Told::State(Box::new(state))));
        self.forward(events, connection);
        if let Some(audio) = self.audio() {
            audio.apply(lock(&self.settings).audio.voice);
        }
        Ok(())
    }

    /// Ends the connection, staying a member of the server. It waits for a `connect` under
    /// way, and then up to three seconds for the goodbye to the server.
    pub fn disconnect(&self) {
        let _switching = lock(&self.switching);
        self.end_connection();
    }

    /// Opens the speaker, and the mic `with_mic`, for the room we are in. The app sets up
    /// the platform's audio session first. It blocks until the devices run.
    pub fn open_audio(&self, with_mic: bool) -> Result<(), CoreError> {
        let Some(audio) = self.audio() else {
            return Ok(());
        };
        #[cfg(any(target_os = "ios", target_os = "android"))]
        audio.open_devices(with_mic)?;
        #[cfg(not(any(target_os = "ios", target_os = "android")))]
        let _ = (audio, with_mic);
        Ok(())
    }

    /// Closes the speaker and the mic, which frees them for other apps. It blocks until
    /// they are closed.
    pub fn close_audio(&self) {
        #[cfg(any(target_os = "ios", target_os = "android"))]
        if let Some(audio) = self.audio() {
            audio.close_devices();
        }
    }

    pub fn voice(&self) -> Voice {
        Voice::of(&lock(&self.settings).audio.voice)
    }

    /// Applies `voice` to the call at once, and keeps it when `keep`. A slider keeps it only
    /// once let go.
    pub fn set_voice(&self, voice: Voice, keep: bool) -> Result<(), CoreError> {
        if keep {
            self.change_settings(|settings| voice.apply_to(&mut settings.audio.voice))?;
        } else {
            voice.apply_to(&mut lock(&self.settings).audio.voice);
        }
        if let Some(audio) = self.audio() {
            audio.apply(lock(&self.settings).audio.voice);
        }
        Ok(())
    }

    /// The mic level while a call has the mic open. The mock shows a sample level.
    pub fn mic_level(&self) -> Option<MicLevel> {
        if self.mock {
            return Some(MicLevel {
                dbfs: -32.0,
                gate_open: true,
            });
        }
        MicLevel::of(self.audio()?.mic_level())
    }

    pub fn command(&self, command: Command) {
        if let Some(command) = command.into_core() {
            self.tell_client(command);
        }
    }

    /// Watches `user`'s share in place of any other: a phone shows one share at a time.
    pub fn watch_only(&self, user: u64) {
        let Some(state) = self.client_state() else {
            return;
        };
        let watching = state.current.as_ref().map(|c| c.watching.clone());
        for watched in watching.unwrap_or_default() {
            if watched.0 != user {
                self.tell_client(UiCommand::Unwatch(watched));
            }
        }
        self.tell_client(UiCommand::Watch(UserId(user)));
    }

    /// Shows `user`'s share in a view of the app's from now on, in place of the one it showed
    /// in, until `hide_share` with the number this returns. On Android `view` is the
    /// `ANativeWindow*` that `ShareView.nativeWindow` returns for a surface, on iOS an
    /// `AVSampleBufferDisplayLayer` passed retained. Either way the core takes over that
    /// reference. The view may come before the share is watched; the share shows once it is.
    // Desktops have no views, so there the rest never runs.
    #[cfg_attr(
        not(any(target_os = "android", target_os = "ios")),
        allow(unreachable_code, unused_variables)
    )]
    pub fn show_share(&self, user: u64, view: u64) -> u64 {
        // SAFETY: the app hands over a live view and gives up its reference, as documented.
        let Some(view) = (unsafe { View::from_raw(view) }) else {
            return 0;
        };
        let number = self.next_view.fetch_add(1, Ordering::Relaxed) + 1;
        lock(&self.share_views)
            .entry(user)
            .or_default()
            .push((number, view.clone()));
        if let Some(connected) = &*lock(&self.connected) {
            connected.client.set_share_view(UserId(user), Some(view));
        }
        number
    }

    /// Stops showing `user`'s share in the view `show_share` numbered `number`, which is going
    /// away. The view shown before it takes over, if there is one still.
    // Desktops have no views, so there the rest never runs.
    #[cfg_attr(
        not(any(target_os = "android", target_os = "ios")),
        allow(unreachable_code, unused_variables)
    )]
    pub fn hide_share(&self, user: u64, number: u64) {
        let mut views = lock(&self.share_views);
        let Some(shown) = views.get_mut(&user) else {
            return;
        };
        let Some(index) = shown.iter().position(|(n, _)| *n == number) else {
            return;
        };
        let was_newest = index + 1 == shown.len();
        shown.remove(index);
        let newest = shown.last().map(|(_, view)| view.clone());
        if shown.is_empty() {
            views.remove(&user);
        }
        drop(views);
        if was_newest && let Some(connected) = &*lock(&self.connected) {
            connected.client.set_share_view(UserId(user), newest);
        }
    }

    /// The safety-number comparison with `user`, as their profile shows it.
    pub fn safety_check(self: Arc<Self>, user: u64) -> Arc<SafetyCheck> {
        Arc::new(SafetyCheck {
            core: self,
            check: Mutex::new(core_safety::SafetyCheck::new(UserId(user))),
        })
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

    /// The picture behind `hash`, if it is one a phone may show. One the core lacks is
    /// fetched, and `Event::AvatarReady` says when it has arrived.
    pub fn avatar(&self, hash: Vec<u8>) -> Option<Vec<u8>> {
        let hash = Hash(hash.as_slice().try_into().ok()?);
        let connected = lock(&self.connected);
        let picture = connected.as_ref()?.client.avatar(hash);
        showable(picture)
    }

    /// Everyone verified on this install.
    pub fn verified(&self) -> Vec<VerifiedPerson> {
        let servers = self.known_servers();
        let people = install::verified_people(&self.verified, &self.blocked, &servers);
        let people = people.into_iter().map(|person| VerifiedPerson {
            key: person.key.0.to_vec(),
            name: person.name,
            servers: person.servers,
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
        let servers = self.known_servers();
        let connected = lock(&self.connected);
        let fetched = |hash| connected.as_ref()?.client.avatar(hash);
        let people = install::blocked_people(&self.blocked, &servers, fetched);
        let people = people.into_iter().map(|listed| BlockedPerson {
            key: listed.person.key.0.to_vec(),
            name: listed.person.name,
            picture: showable(listed.picture),
            server: listed.server,
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
            problems: Mutex::default(),
            sample_servers: Mutex::new(if mock {
                install::sample_servers()
            } else {
                Vec::new()
            }),
            profile_watch: Mutex::new(None),
            share_views: Mutex::default(),
            next_view: AtomicU64::new(0),
            listener,
            told,
            connection,
            switching: Mutex::new(()),
            connected: Mutex::new(None),
        })
    }

    /// Ends the connection there is, after which the listener hears nothing more of it.
    fn end_connection(&self) {
        *lock(&self.profile_watch) = None;
        let Some(connected) = lock(&self.connected).take() else {
            return;
        };
        let next = self.connection.fetch_add(1, Ordering::SeqCst) + 1;
        drop(connected);
        let _ = self.told.send((next, Told::State(Box::default())));
    }

    /// Gives `profile` to the session, which gives it to the server, even while it
    /// connects. The mock takes it as a command.
    fn pass_on(&self, profile: CoreProfile) {
        if let Some(watch) = &*lock(&self.profile_watch) {
            watch.send_replace(profile.clone());
        }
        if self.mock {
            self.tell_client(UiCommand::SetProfile(profile));
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

    fn audio(&self) -> Option<Arc<AudioEngine>> {
        lock(&self.connected).as_ref()?.audio.clone()
    }

    fn connected_to(&self, address: &str) -> bool {
        let connected = lock(&self.connected);
        connected.as_ref().is_some_and(|c| c.address == address)
    }

    fn tell_client(&self, command: UiCommand) {
        if let Some(connected) = &*lock(&self.connected) {
            connected.client.command(command);
        }
    }

    /// The servers this install has joined, or the sample ones.
    fn known_servers(&self) -> Vec<install::KnownServer> {
        if self.mock {
            lock(&self.sample_servers).clone()
        } else {
            install::known_servers()
        }
    }

    fn client_state(&self) -> Option<Arc<CoreStateInner>> {
        Some(lock(&self.connected).as_ref()?.client.state())
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

/// The quietest voice sensitivity (dBFS), and the bottom of the level meter.
#[uniffi::export]
pub fn quietest_gate_dbfs() -> f32 {
    QUIETEST_GATE_DBFS
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
    match core_profile::check_name(&typed) {
        NameCheckCore::Empty => NameCheck::Empty,
        NameCheckCore::Fine(name) => NameCheck::Fine { name },
        NameCheckCore::TooLong => NameCheck::TooLong,
        NameCheckCore::ShowsNothing => NameCheck::ShowsNothing,
        NameCheckCore::HidesText => NameCheck::HidesText,
    }
}

/// Why a server would refuse `typed` as a name, in words for the user, if it would.
#[uniffi::export]
pub fn name_problem(typed: String) -> Option<String> {
    core_profile::check_name(&typed).problem()
}

/// Why a server would refuse `typed` as a room password, in words for the user, if it would.
#[uniffi::export]
pub fn password_problem(typed: String) -> Option<String> {
    core_profile::password_problem(&typed)
}

/// Turns a picture the user chose into one any server takes: a small centred square. The app
/// first turns formats only it reads, such as HEIC, into PNG.
#[uniffi::export]
pub fn prepare_picture(file: Vec<u8>) -> Result<Vec<u8>, CoreError> {
    picture::prepare(&file).map_err(|reason| CoreError::Failed { reason })
}

/// The gain for a volume slider at `loudness`, from 0 (silent) to 2 where 1 plays the sound
/// as it arrives, the same curve on every client.
#[uniffi::export]
pub fn loudness_to_gain(loudness: f32) -> f32 {
    core_state::loudness_to_gain(loudness)
}

/// Where a volume slider sits for `gain`.
#[uniffi::export]
pub fn gain_to_loudness(gain: f32) -> f32 {
    core_state::gain_to_loudness(gain)
}

/// The colours mixed from `palette`, as every client mixes them.
#[uniffi::export]
pub fn theme_colors(palette: Palette) -> ThemeColors {
    let c = theme::Colors::of(palette.colors().map(|rgb| rgb & 0xFF_FFFF));
    ThemeColors {
        bg: c.bg,
        panel: c.panel,
        raised: c.raised,
        control: c.control,
        line: c.line,
        selected: c.selected,
        danger: c.danger,
        text: c.text,
        muted: c.muted,
        accent: c.accent,
        warning: c.warning,
        good: c.good,
        bad: c.bad,
    }
}

/// `color`, as 0xRRGGBB, from typed hex: `#rrggbb`, `rrggbb` or `#rgb`.
#[uniffi::export]
pub fn parse_color(typed: String) -> Option<u32> {
    theme::parse_hex(&typed)
}

/// `color`, as 0xRRGGBB, as `#RRGGBB`.
#[uniffi::export]
pub fn format_color(color: u32) -> String {
    theme::format_hex(color & 0xFF_FFFF)
}

/// What to tell the user about `connection`, as `CoreState::notice` says it. For samples
/// that show a connection of their own.
#[uniffi::export]
pub fn connection_notice(connection: Connection) -> Option<ConnectionNotice> {
    core_state::ConnectionStatus::from(connection)
        .notice()
        .map(ConnectionNotice::from)
}

/// The letter a picture stands in with for `name`.
#[uniffi::export]
pub fn initial(name: String) -> String {
    core_state::initial(&name)
}

/// Whether a server takes `typed` as a room password.
#[uniffi::export]
pub fn is_valid_password(typed: String) -> bool {
    core_profile::password_problem(&typed).is_none()
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
    code.trim().parse().ok().context("that is no invite code")
}

/// `picture`, if it is one a phone may show: a PNG or JPEG as profiles are made, read by
/// the platform's own decoder.
fn showable(picture: Option<Arc<[u8]>>) -> Option<Vec<u8>> {
    picture
        .filter(|bytes| picture::showable(bytes))
        .map(|bytes| bytes.to_vec())
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
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
        let listed = core.servers();
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
        let room = core.state().rooms[0].id;
        core.command(Command::Join { room: Some(room) });
        next(&states, "us in the room", |s| {
            s.current.as_ref().is_some_and(|c| c.room == room)
        });
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
        // Picking a preset keeps Custom's colours as they stood: the default theme's here.
        assert_eq!(core.custom_palette(), core.themes()[0].palette);
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
