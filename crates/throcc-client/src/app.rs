use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, AppContext, Context, Entity, InteractiveElement, IntoElement, KeyBinding,
    ParentElement, PathPromptOptions, Render, RenderImage, SharedString, Styled, Subscription,
    Task, Window, WindowHandle, actions, div,
};
use gpui_component::input::{InputEvent, InputState};
use gpui_component::notification::Notification;
use gpui_component::slider::{SliderEvent, SliderState};
use gpui_component::{Root, WindowExt};
use throcc_client_core::account::SignInError;
use throcc_client_core::avatars;
use throcc_client_core::client::{self, Client, ClientApi, MockClient, UiCommand, UiEvent};
use throcc_client_core::keystore::{self, Keystore, StartedOver};
use throcc_client_core::media::devices;
use throcc_client_core::media::video::frame::VideoFrame;
use throcc_client_core::media::voice::AudioEngine;
use throcc_client_core::profile::Profile;
use throcc_client_core::server_look::ServerLook;
use throcc_client_core::settings::{Appearance, AudioSettings, Settings, Transmit};
use throcc_client_core::state::{
    ConnectionStatus, KeyWarning, ServerView, ShareChoice, Sound, SoundLevel, State,
};
use throcc_client_core::verified::VerifiedStore;
use throcc_proto::ids::{Hash, Role, RoomId, UserId};
use throcc_proto::invite::{self, InviteCode};
use throcc_proto::messages::{MAX_NAME_CHARS, User, shows_something, valid_name, valid_password};
use throcc_proto::{Fingerprint, permissions};
use tokio::sync::watch;

use crate::keep_display_awake::KeepDisplayAwake;
use crate::theme;
use crate::updates::{self, Release};
use crate::views;
use crate::views::common::{
    CONTROLS_LINGER, OVERLAY_LINGER, Typeset, UpdateNotification, hide_controls_after,
    notify_after, slider_gain, slider_value, update_notification, volume_slider,
};

actions!(throcc, [ToggleMic, ToggleDeafen, CloseDialog]);

const MUTE_KEY: &str = if cfg!(target_os = "macos") {
    "cmd-shift-m"
} else {
    "ctrl-shift-m"
};
const DEAFEN_KEY: &str = if cfg!(target_os = "macos") {
    "cmd-shift-d"
} else {
    "ctrl-shift-d"
};

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new(MUTE_KEY, ToggleMic, None),
        KeyBinding::new(DEAFEN_KEY, ToggleDeafen, None),
        KeyBinding::new("escape", CloseDialog, None),
    ]);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsPage {
    Profile,
    Audio,
    Appearance,
    Verified,
    About,
}

impl SettingsPage {
    pub const ALL: [Self; 5] = [
        Self::Profile,
        Self::Audio,
        Self::Appearance,
        Self::Verified,
        Self::About,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::Profile => "Profile",
            Self::Audio => "Audio",
            Self::Appearance => "Appearance",
            Self::Verified => "Verified People",
            Self::About => "About",
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            Self::Profile => "circle-user",
            Self::Audio => "audio-lines",
            Self::Appearance => "palette",
            Self::Verified => "shield-check",
            Self::About => "info",
        }
    }
}

/// Pages about the server we are connected to, apart from this machine's settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServerPage {
    Overview,
    Keys,
    Members,
}

impl ServerPage {
    pub const ALL: [Self; 3] = [Self::Overview, Self::Keys, Self::Members];

    pub fn title(self) -> &'static str {
        match self {
            Self::Overview => "Server",
            Self::Keys => "Privacy and Keys",
            Self::Members => "Members",
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            Self::Overview => "server",
            Self::Keys => "key-round",
            Self::Members => "users",
        }
    }
}

/// Whose picture a chosen file becomes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PictureFor {
    Profile,
    Server,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    /// Asks for the name, and optionally a picture, that every server will know us by.
    Onboarding,
    Join,
    Main,
    Settings(SettingsPage),
    Server(ServerPage),
}

/// How the last change on the profile or server page went.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SaveStatus {
    Saving,
    Saved,
    Failed(String),
}

/// A picture chosen or removed on a page and not saved yet.
pub struct PictureEdit {
    pub bytes: Option<Vec<u8>>,
    pub image: Option<Arc<RenderImage>>,
}

/// The unsaved picture of the profile or server page, and how its last save went.
#[derive(Default)]
pub struct PictureForm {
    pub edit: Option<PictureEdit>,
    pub status: Option<SaveStatus>,
    /// Counts picks, resets and saves, so a picture still decoding when one of them
    /// overtakes it is dropped.
    pick: u64,
}

impl PictureForm {
    pub fn saving(&self) -> bool {
        self.status == Some(SaveStatus::Saving)
    }

    fn reset(&mut self) {
        *self = Self {
            pick: self.pick + 1,
            ..Self::default()
        };
    }

    /// Drops the picture still decoding, if one is.
    fn outdate_pick(&mut self) {
        self.pick += 1;
    }

    /// Applies the picture prepared for pick number `pick`, unless something overtook it.
    fn picked(&mut self, pick: u64, prepared: Result<PictureEdit, String>) -> bool {
        if self.pick != pick {
            return false;
        }
        match prepared {
            Ok(edit) => self.edit = Some(edit),
            Err(message) => self.status = Some(SaveStatus::Failed(message)),
        }
        true
    }

    /// The picture Save would set: `Some(None)` removes it.
    fn change(&self) -> Option<Option<Vec<u8>>> {
        self.edit.as_ref().map(|edit| edit.bytes.clone())
    }
}

/// The menu of someone's sound and stream, open where it was asked for.
pub struct PersonMenu {
    pub user: UserId,
    /// The window the menu opened in, the only one that shows it.
    pub window: gpui::AnyWindowHandle,
    pub position: gpui::Point<gpui::Pixels>,
    pub voice: SoundLevelControl,
    pub share: SoundLevelControl,
}

impl PersonMenu {
    fn control_mut(&mut self, sound: Sound) -> &mut SoundLevelControl {
        match sound {
            Sound::Voice => &mut self.voice,
            Sound::Share => &mut self.share,
        }
    }
}

/// One of someone's sounds as the person menu last set it, with its volume slider.
pub struct SoundLevelControl {
    pub level: SoundLevel,
    pub slider: Entity<SliderState>,
    _changes: Subscription,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Dialog {
    Share,
    Connection,
    /// `user`'s profile, with our safety number for them. `shown` is the identity key the
    /// number on screen is for, with the number, once they presented one; `key_changed` once
    /// another key replaced it while open, until the user goes on to compare the new number.
    /// Marking waits for that, so a click meant for one number can't land on the next.
    Profile {
        user: UserId,
        shown: Option<(Fingerprint, String)>,
        key_changed: bool,
    },
    Remove {
        user: UserId,
        name: String,
        cascade: bool,
    },
    PinMismatch {
        address: String,
        expected: Fingerprint,
        actual: Fingerprint,
    },
    NewRoom,
    EditRoom(RoomId),
    RoomPassword {
        room: RoomId,
        error: Option<String>,
    },
    LeaveServer,
    AddServer,
    DeleteRoom(RoomId),
    /// Takes the server at this address off our list, keeping our membership.
    RemoveServer(String),
    /// The keychain lost the identity our servers know us by; offers to start over.
    IdentityMissing,
    OwnProfile,
}

/// How starting over after a lost identity goes, shown in its dialog.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum StartOver {
    #[default]
    NotStarted,
    /// Holds the dialog open until it finishes, as the keychain may be showing a prompt.
    Running,
    Done(StartedOver),
    Failed(String),
}

/// The widest the sidebar may be in `window` and still leave the room its minimum width.
fn widest_sidebar(window: &Window) -> gpui::Pixels {
    (window.viewport_size().width - theme::SERVER_BAR_WIDTH - theme::ROOM_MIN_WIDTH)
        .clamp(theme::SIDEBAR_MIN_WIDTH, theme::SIDEBAR_MAX_WIDTH)
}

/// A known server's name and picture as last seen, for while we aren't connected to it.
pub struct CachedLook {
    pub name: Option<String>,
    pub picture: Option<Arc<RenderImage>>,
}

/// Runs `f` on the app a moment from now, once the mock client has applied a sample's
/// commands.
fn after_a_moment(
    cx: &mut Context<ThroccApp>,
    f: impl FnOnce(&mut ThroccApp, &mut Context<ThroccApp>) + 'static,
) {
    cx.spawn(async move |this, cx| {
        cx.background_executor()
            .timer(Duration::from_millis(300))
            .await;
        this.update(cx, |app, cx| {
            f(app, cx);
            cx.notify();
        })
        .ok();
    })
    .detach();
}

/// `client` with every share showing a game.
fn with_sample_share(client: MockClient) -> Arc<MockClient> {
    let game = image::load_from_memory_with_format(
        include_bytes!("../assets/mock/share.jpg"),
        image::ImageFormat::Jpeg,
    );
    if let Ok(game) = game {
        let game = game.into_rgba8();
        let (width, height) = (game.width() as usize, game.height() as usize);
        client.show_in_shares(VideoFrame::from_rgba(width, height, game.as_raw()));
    }
    Arc::new(client)
}

/// The kept looks of `known` servers, by address.
fn cached_looks(known: &[(String, Fingerprint)]) -> HashMap<String, CachedLook> {
    let (Ok(dir), Ok(cache)) = (keystore::config_dir(), keystore::cache_dir()) else {
        return HashMap::new();
    };
    known
        .iter()
        .filter_map(|(address, fingerprint)| {
            let look = ServerLook::load(&dir, fingerprint).ok()?;
            let picture = look
                .avatar
                .and_then(|hash| avatars::load(&cache, hash))
                .as_deref()
                .and_then(crate::avatar::decode);
            Some((
                address.clone(),
                CachedLook {
                    name: look.name,
                    picture,
                },
            ))
        })
        .collect()
}

/// The verified people kept on this computer, with why they couldn't be read if they
/// couldn't, in which case nobody counts as verified.
fn load_verified() -> (VerifiedStore, Option<String>) {
    match keystore::config_dir() {
        Ok(dir) => {
            let verified = VerifiedStore::empty(&dir);
            let problem = read_verified(&verified);
            (verified, problem)
        }
        Err(e) => (VerifiedStore::in_memory(), Some(e.to_string())),
    }
}

/// Reads `verified` from its file again, returning why it couldn't be read if it couldn't.
fn read_verified(verified: &VerifiedStore) -> Option<String> {
    let e = verified.reload().err()?;
    tracing::warn!("no verified keys: {e:#}");
    Some(format!(
        "Couldn't read who you verified ({e}), so everyone shows as not verified."
    ))
}

fn known_servers() -> Vec<(String, Fingerprint)> {
    Keystore::default_known_servers()
        .map(|k| k.iter().map(|(a, f)| (a.to_owned(), f)).collect())
        .unwrap_or_default()
}

/// Decoded pictures kept before the ones nothing shown uses are dropped.
const PICTURES_KEPT: usize = 256;
const NAME_TOO_LONG: &str = "Names can be at most 32 characters.";
const NAME_HIDES_TEXT: &str = "Names can’t contain control or invisible characters.";
const NAME_SHOWS_NOTHING: &str = "Names need at least one visible character.";
const PASSWORD_TOO_LONG: &str = "Passwords can be at most 128 characters.";

/// A typed name: `Ok(None)` when empty, and why the server would refuse it if it would.
fn checked_name(typed: &str) -> Result<Option<String>, &'static str> {
    if typed.trim().is_empty() {
        return Ok(None);
    }
    match valid_name(typed) {
        Some(name) => Ok(Some(name.to_owned())),
        None if typed.trim().chars().count() > MAX_NAME_CHARS => Err(NAME_TOO_LONG),
        None if !shows_something(typed) => Err(NAME_SHOWS_NOTHING),
        None => Err(NAME_HIDES_TEXT),
    }
}

/// A typed password: `Ok(None)` when empty, and why the server would refuse it if it would.
fn checked_password(typed: &str) -> Result<Option<String>, &'static str> {
    if typed.is_empty() {
        Ok(None)
    } else if valid_password(typed) {
        Ok(Some(typed.to_owned()))
    } else {
        Err(PASSWORD_TOO_LONG)
    }
}

/// A name field's text, checked as [`checked_name`] does.
pub fn typed_name(field: &Entity<InputState>, cx: &App) -> Result<Option<String>, &'static str> {
    checked_name(&field.read(cx).value())
}

/// A password field's text, checked as [`checked_password`] does.
pub fn typed_password(
    field: &Entity<InputState>,
    cx: &App,
) -> Result<Option<String>, &'static str> {
    checked_password(&field.read(cx).value())
}

/// Puts `value` in `field`, and focuses it when `focus`.
fn set_field(
    field: &Entity<InputState>,
    value: impl Into<SharedString>,
    focus: bool,
    window: &mut Window,
    cx: &mut App,
) {
    let value: SharedString = value.into();
    field.update(cx, |field, cx| {
        field.set_value(value, window, cx);
        if focus {
            field.focus(window, cx);
        }
    });
}

/// Opens the devices `audio` names, or the system default for one that isn't among
/// `inputs` and `outputs`. Returns why audio doesn't work if it doesn't.
fn open_devices(
    engine: &AudioEngine,
    audio: &AudioSettings,
    (inputs, outputs): &DeviceNames,
) -> Option<String> {
    let present =
        |name: &Option<String>, all: &[String]| name.clone().filter(|name| all.contains(name));
    let input = present(&audio.input_device, inputs);
    let output = present(&audio.output_device, outputs);
    let e = engine
        .open_devices(input.as_deref(), output.as_deref())
        .err()?;
    tracing::warn!("no audio: {e:#}");
    Some(format!("Audio is unavailable: {e}"))
}

/// The input and the output device names.
pub type DeviceNames = (Vec<String>, Vec<String>);

pub struct ThroccApp {
    /// Running on sample data, which must leave this computer's settings and servers alone.
    mock: bool,
    pub client: Option<Arc<dyn ClientApi>>,
    pub state: Arc<State>,
    pub screen: Screen,
    pub dialog: Option<Dialog>,
    pub address: Entity<InputState>,
    pub code: Entity<InputState>,
    pub name: Entity<InputState>,
    pub room_name: Entity<InputState>,
    pub room_password: Entity<InputState>,
    pub server_name: Entity<InputState>,
    pub server_form: PictureForm,
    /// One hex field per palette colour, in [`theme::Palette::LABELS`] order.
    pub palette_fields: Vec<Entity<InputState>>,
    /// Pictures by hash, `None` while one decodes in the background or if it can't be shown.
    pub pictures: HashMap<Hash, Option<Arc<RenderImage>>>,
    pub joining: bool,
    /// Our profile as the join in progress pushes it, updated by saves until the join ends.
    joining_profile: Option<watch::Sender<Profile>>,
    /// Whether the join in progress was asked for on a settings page, so it leaves them.
    join_asked_in_settings: bool,
    pub mascot: views::mascot::Shown,
    /// Counts joins started, so a join that was overtaken by a newer one is ignored.
    join_attempt: u64,
    pub join_error: Option<String>,
    pub start_over: StartOver,
    pub profile_avatar: Option<Hash>,
    pub profile_form: PictureForm,
    pub known_servers: Vec<(String, Fingerprint)>,
    pub verified: VerifiedStore,
    /// Why the verified people couldn't be read or saved, the last time that failed.
    pub verified_problem: Option<String>,
    pub server_looks: HashMap<String, CachedLook>,
    pub users: Vec<User>,
    pub invite: Option<String>,
    /// Everything set on the settings pages, kept on this computer as it changes.
    pub settings: Settings,
    /// Why the settings couldn't be read or kept, the last time that failed.
    pub settings_problem: Option<String>,
    /// A newer client release, once the update check found one.
    pub newer_release: Option<Release>,
    /// The audio thread, from launch to exit.
    pub audio_engine: Arc<AudioEngine>,
    /// Why the audio devices don't work, if they don't.
    pub audio_problem: Option<String>,
    /// The device change under way, which the next one waits for.
    changing_devices: Option<Task<()>>,
    pub mic_test: bool,
    pub audio: views::settings::AudioControls,
    push_to_talk: Option<crate::hotkey::PushToTalkKey>,
    /// Whether Space is held down as the push-to-talk key.
    space_talking: bool,
    pub feeds: crate::video::Feeds,
    parts: HashMap<views::part::Part, Entity<views::part::AppPart>>,
    /// The parts drawn since the last render swept the others away.
    parts_drawn: HashSet<views::part::Part>,
    /// What the share dialog asks for, kept between shares.
    pub share_choice: ShareChoice,
    pub share_audio: bool,
    /// The share dialog's bitrate slider, over the indices of the offered bitrates.
    pub bitrate: Entity<SliderState>,
    _bitrate_changes: Subscription,
    /// The watched share shown large, with everyone else in a strip below it.
    pub focus: Option<UserId>,
    /// Whether our own share's self-view shows in our tile.
    pub preview_self: bool,
    pub popouts: HashMap<UserId, WindowHandle<Root>>,
    /// Held while someone else's share is watched here or in a pop-out.
    keep_display_awake: Option<KeepDisplayAwake>,
    /// The share filling the whole window, with the rest of the UI hidden.
    pub fullscreen: Option<UserId>,
    /// Until when the controls over the fullscreen share show.
    pub controls_until: Instant,
    /// Redraws once the controls over the fullscreen share are due to hide.
    hide_controls: Option<Task<()>>,
    /// Whether [`Self::enter_fullscreen`] made the window fullscreen, so leaving undoes only that.
    made_window_fullscreen: bool,
    /// The whole window's focus, so its key bindings work when no field has focus.
    root_focus: gpui::FocusHandle,
    pub sidebar_width: gpui::Pixels,
    /// The window's space around the room besides the sidebar, measured whenever the room is
    /// drawn.
    pub room_chrome: gpui::Size<gpui::Pixels>,
    pub person_menu: Option<PersonMenu>,
    /// The tile whose overlay shows, and until when once the pointer has left it.
    overlay: Option<(UserId, Option<Instant>)>,
    pub resizing_sidebar: bool,
    events: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl ThroccApp {
    pub fn new(
        mock: bool,
        settings: Settings,
        settings_problem: Option<String>,
        audio_engine: Arc<AudioEngine>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let address = cx.new(|cx| InputState::new(window, cx).placeholder("voice.example.net"));
        let code = cx.new(|cx| InputState::new(window, cx).placeholder("K7Q-M2XPD"));
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Your name"));
        let room_name = cx.new(|cx| InputState::new(window, cx).placeholder("Room name"));
        let room_password = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder("Password")
        });
        let server_name = cx.new(|cx| InputState::new(window, cx).placeholder("Server name"));
        let palette_fields: Vec<Entity<InputState>> = theme::custom(&settings.appearance)
            .colors()
            .iter()
            .map(|color| {
                cx.new(|cx| {
                    let mut field = InputState::new(window, cx);
                    field.set_value(theme::format_hex(*color), window, cx);
                    field
                })
            })
            .collect();
        let mut subscriptions: Vec<Subscription> = palette_fields
            .iter()
            .enumerate()
            .map(|(i, field)| {
                cx.subscribe_in(
                    field,
                    window,
                    move |app, field, event: &InputEvent, _, cx| {
                        if matches!(event, InputEvent::Change) {
                            let typed = field.read(cx).value().to_string();
                            app.set_palette_color(i, &typed, cx);
                        }
                    },
                )
            })
            .collect();
        subscriptions.extend([
            cx.subscribe_in(&code, window, Self::on_code_input),
            cx.on_focus_lost(window, |app, window, cx| window.focus(&app.root_focus, cx)),
            cx.observe_window_activation(window, |app, window, _| {
                if !window.is_window_active() {
                    app.release_space();
                }
            }),
            cx.subscribe_in(&name, window, Self::on_name_field),
            cx.subscribe(&server_name, Self::on_edit),
            cx.subscribe_in(&room_name, window, Self::on_room_field),
            cx.subscribe_in(&room_password, window, Self::on_room_field),
        ]);
        let (verified, verified_problem) = if mock {
            (VerifiedStore::in_memory(), None)
        } else {
            load_verified()
        };
        let known_servers = if mock {
            vec![
                ("owls.thro.cc".into(), Fingerprint([0x2c; 32])),
                ("192.168.1.20".into(), Fingerprint([0x5e; 32])),
                ("192.0.2.40".into(), Fingerprint([0x91; 32])),
            ]
        } else {
            known_servers()
        };
        let server_looks = if mock {
            let look = |name: &str, picture: &[u8]| CachedLook {
                name: Some(name.into()),
                picture: crate::avatar::decode(picture),
            };
            HashMap::from([
                (
                    "owls.thro.cc".to_owned(),
                    look("Night Owls", client::SAMPLE_SERVER_PICTURE),
                ),
                (
                    "192.168.1.20".to_owned(),
                    look("LAN Party", include_bytes!("../assets/mock/lan-party.jpg")),
                ),
                (
                    "192.0.2.40".to_owned(),
                    look(
                        "Survival SMP",
                        include_bytes!("../assets/mock/survival-smp.jpg"),
                    ),
                ),
            ])
        } else {
            cached_looks(&known_servers)
        };
        let share_choice = ShareChoice::default();
        let (bitrate, bitrate_changes) = Self::bitrate_slider(share_choice, cx);
        let audio = views::settings::AudioControls::new(&settings.audio.voice, window, cx);
        let mut app = Self {
            mock,
            client: None,
            state: Arc::new(State::default()),
            screen: Screen::Join,
            dialog: None,
            address,
            code,
            name,
            room_name,
            room_password,
            server_name,
            server_form: PictureForm::default(),
            palette_fields,
            pictures: HashMap::new(),
            joining: false,
            joining_profile: None,
            join_asked_in_settings: false,
            mascot: views::mascot::Shown::default(),
            join_attempt: 0,
            join_error: None,
            start_over: StartOver::NotStarted,
            profile_avatar: None,
            profile_form: PictureForm::default(),
            known_servers,
            verified,
            verified_problem,
            server_looks,
            users: Vec::new(),
            invite: None,
            settings,
            settings_problem,
            newer_release: None,
            audio_engine,
            audio_problem: None,
            changing_devices: None,
            mic_test: false,
            audio,
            push_to_talk: None,
            space_talking: false,
            feeds: crate::video::Feeds::default(),
            parts: HashMap::new(),
            parts_drawn: HashSet::new(),
            share_choice,
            share_audio: true,
            bitrate,
            _bitrate_changes: bitrate_changes,
            focus: None,
            preview_self: false,
            popouts: HashMap::new(),
            keep_display_awake: None,
            fullscreen: None,
            controls_until: Instant::now(),
            hide_controls: None,
            made_window_fullscreen: false,
            root_focus: cx.focus_handle(),
            sidebar_width: theme::SIDEBAR_WIDTH,
            room_chrome: gpui::size(
                theme::SERVER_BAR_WIDTH,
                theme::HEADER_HEIGHT + theme::FOOTER_HEIGHT,
            ),
            overlay: None,
            person_menu: None,
            resizing_sidebar: false,
            events: None,
            _subscriptions: subscriptions,
        };
        window.focus(&app.root_focus, cx);
        app.sync_push_to_talk();
        app.change_devices(window, cx);
        if mock {
            let profile = Profile {
                name: "nori".into(),
                picture: client::sample_picture(UserId(1)).map(Arc::from),
            };
            app.set_profile(profile, cx);
            let client = MockClient::new(app.verified.clone());
            app.attach(with_sample_share(client), window, cx);
        } else {
            match app.settings.profile.clone() {
                Some(profile) => app.set_profile(profile, cx),
                None => app.show_screen(Screen::Onboarding, window, cx),
            }
            if app.settings_problem.is_none() {
                app.look_for_update(window, cx);
            }
        }
        app
    }

    pub fn checks_for_updates(&self) -> bool {
        self.settings
            .check_for_updates
            .unwrap_or_else(updates::checked_by_default)
    }

    /// Asks GitHub for a newer release in the background, if the user wants that here, unless
    /// running on sample data, and announces one found in `window`.
    fn look_for_update(&mut self, window: &Window, cx: &mut Context<Self>) {
        if self.mock || !self.checks_for_updates() {
            return;
        }
        let window_handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            let found = cx
                .background_executor()
                .spawn(async { updates::newer_release() })
                .await;
            match found {
                Ok(newer) => {
                    cx.update_window(window_handle, |_, window, cx| {
                        this.update(cx, |app, cx| {
                            app.newer_release = newer.filter(|_| app.checks_for_updates());
                            if let Some(release) = &app.newer_release {
                                window.push_notification(update_notification(release), cx);
                            }
                            cx.notify();
                        })
                        .ok();
                    })
                    .ok();
                }
                Err(e) => tracing::info!("could not check for updates: {e:#}"),
            }
        })
        .detach();
    }

    /// Turns the update check on or off and keeps the choice.
    pub fn use_update_check(&mut self, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.change_settings(|s| s.check_for_updates = Some(on));
        self.newer_release = None;
        if !on {
            window.remove_notification::<UpdateNotification>(cx);
        }
        self.look_for_update(window, cx);
        cx.notify();
    }

    /// Keeps the invite field upper-case, Crockford-normalized and dashed as you type.
    fn on_code_input(
        &mut self,
        state: &Entity<InputState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !matches!(event, InputEvent::Change) {
            return;
        }
        let typed = state.read(cx).value().to_string();
        let formatted = invite::format_partial(&typed);
        if formatted != typed {
            state.update(cx, |s, cx| s.set_value(formatted, window, cx));
        }
    }

    pub fn command(&self, command: UiCommand) {
        if let Some(client) = &self.client {
            client.command(command);
        }
    }

    fn attach(&mut self, client: Arc<dyn ClientApi>, window: &mut Window, cx: &mut Context<Self>) {
        self.state = client.state();
        let mut events = client.events();
        self.client = Some(client);
        self.joining = false;
        self.joining_profile = None;
        self.join_error = None;
        self.dialog = None;
        if self.join_asked_in_settings || !matches!(self.screen, Screen::Settings(_)) {
            self.screen = Screen::Main;
            window.focus(&self.root_focus, cx);
        }
        if let Some(problem) = &self.audio_problem {
            window.push_notification(Notification::new().message(problem.clone()), cx);
        }
        let window_handle = window.window_handle();
        self.events = Some(cx.spawn(async move |this, cx| {
            loop {
                let event = match events.recv().await {
                    Ok(event) => event,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => return,
                };
                let updated = cx.update_window(window_handle, |_, window, cx| {
                    this.update(cx, |app, cx| app.on_event(event, window, cx))
                        .ok();
                });
                if updated.is_err() {
                    return;
                }
            }
        }));
    }

    fn on_event(&mut self, event: UiEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            UiEvent::State(state) => {
                self.state = state;
                if let Some(Dialog::RoomPassword { room, .. }) = self.dialog
                    && self.state.current.as_ref().is_some_and(|c| c.room == room)
                {
                    self.dialog = None;
                }
                self.close_unwatched_popouts(cx);
                self.sync_keep_display_awake(window, cx);
                if self
                    .person_menu
                    .as_ref()
                    .is_some_and(|m| self.state.person(m.user).is_none())
                {
                    self.person_menu = None;
                }
                if self.fullscreen.is_some_and(|user| !self.shows_stream(user)) {
                    self.exit_fullscreen(window, cx);
                }
            }
            UiEvent::Invite { code, .. } => self.invite = Some(code),
            UiEvent::Users(users) => self.users = users,
            UiEvent::Notice(text) => {
                window.push_notification(Notification::new().message(text), cx)
            }
            UiEvent::RemovedFromServer(reason) => {
                self.end_session(window, cx);
                self.join_error = Some(reason);
            }
            UiEvent::AvatarReady(_) => {}
            UiEvent::ProfileSaved => {}
            UiEvent::ProfileRefused(reason) => {
                let notice = format!("This server refused your profile: {reason}");
                window.push_notification(Notification::new().message(notice), cx)
            }
            UiEvent::ServerProfileSaved => {
                self.server_form.edit = None;
                self.server_form.status = Some(SaveStatus::Saved);
            }
            UiEvent::ServerProfileRefused(reason) => {
                self.server_form.status = Some(SaveStatus::Failed(reason))
            }
            UiEvent::PasswordRefused(reason) => {
                if let Some(Dialog::RoomPassword { error, .. }) = &mut self.dialog {
                    *error = Some(reason);
                }
            }
            UiEvent::LeftServer => self.end_session(window, cx),
        }
        self.follow_presented_key();
        cx.notify();
    }

    /// The decoded pictures of everyone in the state who has one and has arrived.
    pub fn pictures_of_people(
        &mut self,
        cx: &mut Context<Self>,
    ) -> HashMap<UserId, Arc<RenderImage>> {
        let state = self.state.clone();
        state
            .people
            .iter()
            .filter_map(|(user, p)| Some((*user, self.picture(p.avatar, cx)?)))
            .collect()
    }

    /// `part` of the window as a view the window reuses while only a share redraws, drawn
    /// with `draw` whenever the app changes.
    pub fn part(
        &mut self,
        part: views::part::Part,
        draw: impl Fn(&mut ThroccApp, &mut Window, &mut Context<ThroccApp>) -> AnyElement + 'static,
        cx: &mut Context<Self>,
    ) -> Entity<views::part::AppPart> {
        let draw: views::part::Draw = std::rc::Rc::new(draw);
        self.parts_drawn.insert(part);
        if let Some(view) = self.parts.get(&part).cloned() {
            view.update(cx, |view, _| view.set_draw(draw));
            return view;
        }
        let app = cx.entity();
        let view = cx.new(|cx| views::part::AppPart::new(&app, draw, cx));
        self.parts.insert(part, view.clone());
        view
    }

    /// Our name, avatar seed and picture: as the server shows us, or from our profile
    /// outside a server.
    pub fn own_look(&mut self, cx: &mut Context<Self>) -> (String, u64, Option<Arc<RenderImage>>) {
        let me = self.state.me;
        let person = me.and_then(|m| self.state.person(m));
        let name = person
            .map(|p| p.name.clone())
            .or_else(|| self.settings.profile.as_ref().map(|p| p.name.clone()))
            .unwrap_or_else(|| "You".into());
        let avatar = person.and_then(|p| p.avatar);
        let picture = self
            .picture(avatar, cx)
            .or_else(|| self.picture(self.profile_avatar, cx));
        (name, me.map_or(0, |m| m.0), picture)
    }

    /// The decoded profile picture behind `avatar`, once the client has it and it has decoded.
    pub fn picture(
        &mut self,
        avatar: Option<Hash>,
        cx: &mut Context<Self>,
    ) -> Option<Arc<RenderImage>> {
        let hash = avatar?;
        if let Some(picture) = self.pictures.get(&hash) {
            return picture.clone();
        }
        let bytes = self.client.as_ref()?.avatar(hash)?;
        self.decode_picture(hash, bytes, cx);
        None
    }

    /// Decodes `bytes` in the background and keeps the result as the picture behind `hash`.
    fn decode_picture(&mut self, hash: Hash, bytes: Arc<[u8]>, cx: &mut Context<Self>) {
        if self.pictures.contains_key(&hash) {
            return;
        }
        if self.pictures.len() >= PICTURES_KEPT {
            let in_use = self.state.avatars_in_use();
            let own = self.profile_avatar;
            self.pictures
                .retain(|hash, _| in_use.contains(hash) || own == Some(*hash));
        }
        self.pictures.insert(hash, None);
        cx.spawn(async move |this, cx| {
            let picture = cx
                .background_executor()
                .spawn(async move { crate::avatar::decode(&bytes) })
                .await;
            if picture.is_some() {
                this.update(cx, |app, cx| {
                    app.pictures.insert(hash, picture);
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    /// Switches every window to the theme called `name`, a preset or Custom, and keeps it.
    pub fn use_theme(&mut self, name: &'static str, cx: &mut Context<Self>) {
        let custom = theme::custom(&self.settings.appearance).to_hex();
        self.change_appearance(cx, |appearance| {
            appearance.theme = Some(name.into());
            appearance.palette = Some(custom);
        });
    }

    /// Changes the appearance, keeps it and shows it in every window.
    fn change_appearance(&mut self, cx: &mut Context<Self>, change: impl FnOnce(&mut Appearance)) {
        self.change_settings(|s| change(&mut s.appearance));
        theme::apply(&self.settings.appearance, cx);
    }

    /// Takes `key` off the verified people, on every server.
    pub fn unverify(&mut self, key: Fingerprint) {
        if let Err(e) = self.verified.unverify(&key) {
            tracing::warn!("could not save who is verified: {e:#}");
            self.verified_problem = Some(format!("Couldn't save who you verified: {e}"));
        }
        self.command(UiCommand::VerifiedChanged);
    }

    /// Makes the sidebar see-through or solid and keeps the choice for next time.
    pub fn use_translucent_sidebar(&mut self, translucent: bool, cx: &mut Context<Self>) {
        self.change_appearance(cx, |appearance| {
            appearance.translucent_sidebar = Some(translucent)
        });
    }

    /// Takes a typed hex colour into the Custom theme once it parses.
    fn set_palette_color(&mut self, index: usize, typed: &str, cx: &mut Context<Self>) {
        let Some(color) = theme::parse_hex(typed) else {
            return;
        };
        let mut colors = theme::custom(&self.settings.appearance).colors();
        if colors[index] == color {
            return;
        }
        colors[index] = color;
        let palette = theme::Palette::from_colors(colors).to_hex();
        self.change_appearance(cx, |appearance| appearance.palette = Some(palette));
    }

    /// The share filling the whole window, while it is still shown here.
    pub fn fullscreen_stream(&self) -> Option<UserId> {
        self.fullscreen.filter(|user| self.shows_stream(*user))
    }

    /// Whether `user`'s share is watched here, in this window.
    pub fn shows_stream(&self, user: UserId) -> bool {
        let sharing = self.state.person(user).is_some_and(|p| p.sharing.is_some());
        let watched = if Some(user) == self.state.me {
            self.preview_self
        } else {
            self.state
                .current
                .as_ref()
                .is_some_and(|c| c.watching.contains(&user))
        };
        sharing && watched && !self.popouts.contains_key(&user)
    }

    /// Fills the window with `user`'s share and makes the window fullscreen.
    pub fn enter_fullscreen(&mut self, user: UserId, window: &mut Window, cx: &mut Context<Self>) {
        self.fullscreen = Some(user);
        self.show_controls(window, cx);
        if !window.is_fullscreen() {
            window.toggle_fullscreen();
            self.made_window_fullscreen = true;
        }
    }

    pub fn exit_fullscreen(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.fullscreen = None;
        if std::mem::take(&mut self.made_window_fullscreen) && window.is_fullscreen() {
            window.toggle_fullscreen();
        }
        cx.notify();
    }

    /// Opens the person menu for `user` at `position` in `window`.
    pub fn open_person_menu(
        &mut self,
        user: UserId,
        position: gpui::Point<gpui::Pixels>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let Some(playback) = self.state.person(user).map(|p| p.playback) else {
            return;
        };
        self.person_menu = Some(PersonMenu {
            user,
            window: window.window_handle(),
            position,
            voice: Self::sound_level_control(Sound::Voice, playback.voice, cx),
            share: Self::sound_level_control(Sound::Share, playback.share, cx),
        });
        cx.notify();
    }

    fn sound_level_control(
        sound: Sound,
        level: SoundLevel,
        cx: &mut Context<Self>,
    ) -> SoundLevelControl {
        let slider = cx.new(|_| volume_slider(level.volume));
        let changes = cx.subscribe(&slider, move |app, _, event: &SliderEvent, cx| {
            let volume = slider_gain(event);
            app.change_playback(sound, |p| p.volume = volume, cx);
        });
        SoundLevelControl {
            level,
            slider,
            _changes: changes,
        }
    }

    /// Changes one sound of the person in the open menu, and plays it that way.
    pub fn change_playback(
        &mut self,
        sound: Sound,
        change: impl FnOnce(&mut SoundLevel),
        cx: &mut Context<Self>,
    ) {
        let Some(menu) = &mut self.person_menu else {
            return;
        };
        let user = menu.user;
        let control = menu.control_mut(sound);
        change(&mut control.level);
        let command = UiCommand::SetSoundLevel(user, sound, control.level);
        self.command(command);
        cx.notify();
    }

    pub fn close_person_menu(&mut self, cx: &mut Context<Self>) {
        self.person_menu = None;
        cx.notify();
    }

    /// Closes the person menu if it is open in `window`, and says whether it was.
    pub fn close_person_menu_in(&mut self, window: &Window, cx: &mut Context<Self>) -> bool {
        let closed = self
            .person_menu
            .take_if(|menu| menu.window == window.window_handle())
            .is_some();
        if closed {
            cx.notify();
        }
        closed
    }

    pub fn shows_overlay(&self, user: UserId) -> bool {
        self.overlay.is_some_and(|(shown, until)| {
            shown == user && until.is_none_or(|until| Instant::now() < until)
        })
    }

    /// Shows `user`'s tile overlay while the pointer is over it, and for a moment after.
    pub fn hover_tile(&mut self, user: UserId, hovered: bool, cx: &mut Context<Self>) {
        if hovered {
            self.overlay = Some((user, None));
        } else if self.overlay.is_some_and(|(shown, _)| shown == user) {
            self.overlay = Some((user, Some(Instant::now() + OVERLAY_LINGER)));
            notify_after(OVERLAY_LINGER, cx).detach();
        }
        cx.notify();
    }

    pub fn show_controls(&mut self, window: &Window, cx: &mut Context<Self>) {
        self.controls_until = Instant::now() + CONTROLS_LINGER;
        let fullscreen = |app: &Self| app.fullscreen_stream().is_some();
        self.hide_controls = Some(hide_controls_after(CONTROLS_LINGER, fullscreen, window, cx));
        cx.notify();
    }

    /// Changes the share dialog's choice and fits its bitrate slider to the bitrates offered.
    pub fn set_share_choice(&mut self, choice: ShareChoice, cx: &mut Context<Self>) {
        let offered_before = self.share_choice.bitrates();
        self.share_choice = choice;
        if choice.bitrates() != offered_before {
            (self.bitrate, self._bitrate_changes) = Self::bitrate_slider(choice, cx);
        }
        cx.notify();
    }

    fn bitrate_slider(
        choice: ShareChoice,
        cx: &mut Context<Self>,
    ) -> (Entity<SliderState>, Subscription) {
        let offered = choice.bitrates();
        let at = offered.iter().position(|k| *k == choice.kbps).unwrap_or(0);
        let slider = cx.new(|_| {
            SliderState::new()
                .min(0.)
                .max(offered.len().saturating_sub(1) as f32)
                .step(1.)
                .default_value(at as f32)
        });
        let changes = cx.subscribe(&slider, move |app, _, event: &SliderEvent, cx| {
            let at = slider_value(event).round().max(0.) as usize;
            if let Some(kbps) = offered.get(at) {
                app.share_choice.kbps = *kbps;
                cx.notify();
            }
        });
        (slider, changes)
    }

    /// Moves a watched share into a window of its own, showing its controls for
    /// `controls_for`.
    pub fn pop_out(&mut self, user: UserId, controls_for: Duration, cx: &mut Context<Self>) {
        if self.popouts.contains_key(&user) {
            return;
        }
        let name = self
            .state
            .person(user)
            .map_or("Someone".into(), |p| p.name.clone());
        let title = format!("{name}’s Screen");
        let app = cx.entity().downgrade();
        cx.defer(move |cx| {
            let opened = crate::popout::PopOut::open(app.clone(), user, title, controls_for, cx);
            let Some(window) = opened else {
                return;
            };
            app.update(cx, |app, cx| {
                app.popouts.insert(user, window);
                cx.notify();
            })
            .ok();
        });
        if self.focus == Some(user) {
            self.focus = None;
        }
        cx.notify();
    }

    pub fn close_popout(&mut self, user: UserId, cx: &mut Context<Self>) {
        if let Some(window) = self.popouts.remove(&user) {
            // gpui cannot update a window while it handles an event, and this may run in
            // the pop-out.
            cx.defer(move |cx| {
                window
                    .update(cx, |_, window, _| window.remove_window())
                    .ok();
            });
        }
        cx.notify();
    }

    fn close_all_popouts(&mut self, cx: &mut Context<Self>) {
        for user in self.popouts.keys().copied().collect::<Vec<_>>() {
            self.close_popout(user, cx);
        }
    }

    fn close_unwatched_popouts(&mut self, cx: &mut Context<Self>) {
        let state = &self.state;
        let gone: Vec<UserId> = self
            .popouts
            .keys()
            .filter(|user| {
                state
                    .current
                    .as_ref()
                    .is_none_or(|c| !c.watching.contains(user))
                    || state.person(**user).is_none_or(|p| p.sharing.is_none())
            })
            .copied()
            .collect();
        for user in gone {
            self.close_popout(user, cx);
        }
    }

    fn sync_keep_display_awake(&mut self, window: &Window, cx: &App) {
        let watching = self.state.current.as_ref().is_some_and(|c| {
            c.watching.iter().any(|user| {
                self.state
                    .person(*user)
                    .is_some_and(|p| p.sharing.is_some())
            })
        });
        if watching != self.keep_display_awake.is_some() {
            self.keep_display_awake = watching.then(|| KeepDisplayAwake::new(window, cx));
        }
    }

    /// Shows `user`'s share here: back from its pop-out, our own self-view, or someone
    /// else's stream.
    pub fn watch(&mut self, user: UserId, cx: &mut Context<Self>) {
        if self.popouts.contains_key(&user) {
            self.close_popout(user, cx);
        } else if Some(user) == self.state.me {
            self.preview_self = true;
        } else {
            self.command(UiCommand::Watch(user));
        }
        cx.notify();
    }

    /// Stops showing `user`'s share, leaving fullscreen if it fills the window.
    pub fn stop_watching(&mut self, user: UserId, window: &mut Window, cx: &mut Context<Self>) {
        if self.fullscreen_stream() == Some(user) {
            self.exit_fullscreen(window, cx);
        }
        if Some(user) == self.state.me {
            self.preview_self = false;
        } else {
            self.command(UiCommand::Unwatch(user));
        }
        cx.notify();
    }

    /// Whether we may create, rename and delete rooms.
    pub fn manages_rooms(&self) -> bool {
        self.state.role.is_some_and(permissions::can_manage_rooms)
    }

    pub fn set_dialog(&mut self, dialog: Option<Dialog>, cx: &mut Context<Self>) {
        self.dialog = dialog;
        cx.notify();
    }

    /// Closes the dialog the user dismissed, unless it waits for starting over to finish.
    pub fn dismiss_dialog(&mut self, cx: &mut Context<Self>) {
        if self.start_over != StartOver::Running {
            self.set_dialog(None, cx);
        }
    }

    fn show_identity_missing(&mut self) {
        self.start_over = StartOver::NotStarted;
        self.dialog = Some(Dialog::IdentityMissing);
    }

    /// Opens `user`'s profile, or our own when `user` is us.
    pub fn open_profile(&mut self, user: UserId, cx: &mut Context<Self>) {
        self.dialog = Some(if self.state.me == Some(user) {
            Dialog::OwnProfile
        } else {
            Dialog::Profile {
                user,
                shown: None,
                key_changed: false,
            }
        });
        self.follow_presented_key();
        cx.notify();
    }

    /// Lets the user mark the person in the profile again, now comparing the number
    /// that replaced the one they had.
    pub fn compare_again(&mut self, cx: &mut Context<Self>) {
        if let Some(Dialog::Profile { key_changed, .. }) = &mut self.dialog {
            *key_changed = false;
            cx.notify();
        }
    }

    /// Keeps the profile's number on the key its person presents now. Their key
    /// arrives with a room state, and the server may present another at any time, so this
    /// follows every event.
    fn follow_presented_key(&mut self) {
        let Some(Dialog::Profile {
            user,
            shown,
            key_changed,
        }) = &mut self.dialog
        else {
            return;
        };
        let Some(key) = self.state.person(*user).and_then(|p| p.identity_key) else {
            return;
        };
        let Some(client) = &self.client else {
            return;
        };
        if shown.as_ref().is_some_and(|(shown, _)| shown.0 == key) {
            return;
        }
        *key_changed |= shown.is_some();
        *shown = Some((Fingerprint(key), client.safety_number(&key)));
    }

    fn on_room_field(
        &mut self,
        _: &Entity<InputState>,
        event: &InputEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(event, InputEvent::Change) {
            cx.notify();
        }
        if matches!(event, InputEvent::PressEnter { .. }) {
            match self.dialog {
                Some(Dialog::NewRoom) => self.create_room(cx),
                Some(Dialog::EditRoom(_)) => self.save_room(cx),
                Some(Dialog::RoomPassword { .. }) => self.join_with_password(cx),
                _ => {}
            }
        }
    }

    /// Joins `room`, asking for its password first when it is locked and we don't manage
    /// rooms.
    pub fn join_room(&mut self, room: RoomId, window: &mut Window, cx: &mut Context<Self>) {
        if self.state.current.as_ref().is_some_and(|c| c.room == room) {
            return;
        }
        if self.state.room(room).is_some_and(|r| r.locked) && !self.manages_rooms() {
            set_field(&self.room_password, "", true, window, cx);
            self.set_dialog(Some(Dialog::RoomPassword { room, error: None }), cx);
            return;
        }
        self.command(UiCommand::Join(Some(room)));
    }

    pub fn join_with_password(&mut self, cx: &mut Context<Self>) {
        let Some(Dialog::RoomPassword { room, .. }) = self.dialog else {
            return;
        };
        if let Ok(Some(password)) = typed_password(&self.room_password, cx) {
            self.command(UiCommand::JoinWithPassword(room, password));
        }
    }

    pub fn can_join_with_password(&self, cx: &App) -> bool {
        typed_password(&self.room_password, cx).is_ok_and(|password| password.is_some())
    }

    pub fn open_new_room(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_room_dialog(Dialog::NewRoom, String::new(), window, cx);
    }

    pub fn open_edit_room(&mut self, room: RoomId, window: &mut Window, cx: &mut Context<Self>) {
        let name = self
            .state
            .room(room)
            .map(|r| r.name.clone())
            .unwrap_or_default();
        self.open_room_dialog(Dialog::EditRoom(room), name, window, cx);
    }

    /// Opens a room dialog with `name` in its name field and its password field empty.
    fn open_room_dialog(
        &mut self,
        dialog: Dialog,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        set_field(&self.room_password, "", true, window, cx);
        set_field(&self.room_name, name, true, window, cx);
        self.set_dialog(Some(dialog), cx);
    }

    /// The room dialog's typed name and password, unless the server would refuse either.
    fn room_fields(&self, cx: &App) -> Option<(Option<String>, Option<String>)> {
        Some((
            typed_name(&self.room_name, cx).ok()?,
            typed_password(&self.room_password, cx).ok()?,
        ))
    }

    pub fn can_create_room(&self, cx: &App) -> bool {
        self.room_fields(cx).is_some_and(|(name, _)| name.is_some())
    }

    pub fn can_save_room(&self, cx: &App) -> bool {
        self.room_fields(cx).is_some()
    }

    pub fn create_room(&mut self, cx: &mut Context<Self>) {
        let Some((Some(name), password)) = self.room_fields(cx) else {
            return;
        };
        self.command(UiCommand::CreateRoom(name, password));
        self.set_dialog(None, cx);
    }

    /// Renames the edited room, and locks it with the typed password if there is one.
    pub fn save_room(&mut self, cx: &mut Context<Self>) {
        let Some(Dialog::EditRoom(room)) = self.dialog else {
            return;
        };
        let Some((name, password)) = self.room_fields(cx) else {
            return;
        };
        if let Some(name) =
            name.filter(|name| self.state.room(room).is_none_or(|r| r.name != *name))
        {
            self.command(UiCommand::RenameRoom(room, name));
        }
        if password.is_some() {
            self.command(UiCommand::SetRoomPassword(room, password));
        }
        self.set_dialog(None, cx);
    }

    /// The profile Save on the profile page, or Continue on the onboarding screen, would keep:
    /// the typed name, if valid, with the unsaved picture or the kept one.
    fn edited_profile(&self, cx: &App) -> Option<Profile> {
        let name = typed_name(&self.name, cx).ok()??;
        let picture = match self.profile_form.change() {
            Some(bytes) => bytes.map(Arc::from),
            None => self
                .settings
                .profile
                .as_ref()
                .and_then(|p| p.picture.clone()),
        };
        Some(Profile { name, picture })
    }

    /// Whether Save on the profile page has a valid change to keep.
    pub fn profile_changed(&self, cx: &App) -> bool {
        self.edited_profile(cx)
            .is_some_and(|edited| self.settings.profile.as_ref() != Some(&edited))
    }

    pub fn save_profile(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(profile) = self
            .edited_profile(cx)
            .filter(|edited| self.settings.profile.as_ref() != Some(edited))
        else {
            return;
        };
        self.keep_profile(profile.clone(), window, cx);
        self.profile_form.status = Some(SaveStatus::Saved);
        self.command(UiCommand::SetProfile(profile));
        cx.notify();
    }

    /// Keeps the onboarding screen's profile and moves on to joining a server.
    pub fn finish_onboarding(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(profile) = self.edited_profile(cx) else {
            return;
        };
        self.keep_profile(profile, window, cx);
        self.show_screen(Screen::Join, window, cx);
    }

    /// Shows `profile` as ours and keeps it on this computer, telling the user if it couldn't
    /// be kept.
    fn keep_profile(&mut self, profile: Profile, window: &mut Window, cx: &mut Context<Self>) {
        self.profile_form.outdate_pick();
        self.profile_form.edit = None;
        if let Some(joining) = &self.joining_profile {
            joining.send_replace(profile.clone());
        }
        self.set_profile(profile, cx);
        self.save_settings();
        if let Some(problem) = self.settings_problem.clone() {
            window.push_notification(Notification::new().message(problem), cx);
        }
    }

    fn set_profile(&mut self, profile: Profile, cx: &mut Context<Self>) {
        self.profile_avatar = profile.avatar();
        if let (Some(hash), Some(bytes)) = (self.profile_avatar, &profile.picture) {
            self.decode_picture(hash, bytes.clone(), cx);
        }
        self.settings.profile = Some(profile);
    }

    pub fn change_settings(&mut self, change: impl FnOnce(&mut Settings)) {
        change(&mut self.settings);
        self.save_settings();
    }

    /// Writes the settings to disk, unless running on sample data.
    pub fn save_settings(&mut self) {
        if self.mock {
            return;
        }
        let saved = keystore::config_dir().and_then(|dir| self.settings.save(&dir));
        self.settings_problem = saved.err().map(|e| {
            tracing::warn!("saving the settings: {e:#}");
            format!("Couldn't keep your settings: {e}")
        });
    }

    /// The name Save on the server page would give the server, if it differs, and why the
    /// server would refuse it if it would.
    fn server_name_change(&self, cx: &App) -> Result<Option<Option<String>>, &'static str> {
        let typed = typed_name(&self.server_name, cx)?;
        let current = self.state.server.as_ref().and_then(|s| s.name.clone());
        Ok((typed != current).then_some(typed))
    }

    /// Whether Save on the server page has something to save that the server would take.
    pub fn server_changed(&self, cx: &App) -> bool {
        self.server_name_change(cx)
            .is_ok_and(|name| name.is_some() || self.server_form.edit.is_some())
    }

    pub fn save_server(&mut self, cx: &mut Context<Self>) {
        let Ok(name) = self.server_name_change(cx) else {
            return;
        };
        let picture = self.server_form.change();
        if name.is_none() && picture.is_none() {
            return;
        }
        self.server_form.outdate_pick();
        self.server_form.status = Some(SaveStatus::Saving);
        self.command(UiCommand::SetServerProfile { name, picture });
        cx.notify();
    }

    fn on_name_field(
        &mut self,
        _: &Entity<InputState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => cx.notify(),
            InputEvent::PressEnter { .. } if self.screen == Screen::Onboarding => {
                self.finish_onboarding(window, cx)
            }
            _ => {}
        }
    }

    fn on_edit(&mut self, _: Entity<InputState>, event: &InputEvent, cx: &mut Context<Self>) {
        if matches!(event, InputEvent::Change) {
            cx.notify();
        }
    }

    /// Shows `screen`, first filling in what its page needs.
    pub fn show_screen(&mut self, screen: Screen, window: &mut Window, cx: &mut Context<Self>) {
        match screen {
            Screen::Onboarding => {
                self.profile_form.reset();
                set_field(&self.name, "", true, window, cx);
            }
            Screen::Settings(SettingsPage::Profile) => {
                self.profile_form.reset();
                let name = self.settings.profile.as_ref().map(|p| p.name.clone());
                set_field(&self.name, name.unwrap_or_default(), false, window, cx);
            }
            Screen::Server(ServerPage::Overview) => {
                self.server_form.reset();
                let name = self.state.server.as_ref().and_then(|s| s.name.clone());
                set_field(
                    &self.server_name,
                    name.unwrap_or_default(),
                    false,
                    window,
                    cx,
                );
            }
            Screen::Server(ServerPage::Members)
                if self.state.role.is_some_and(permissions::can_list_users) =>
            {
                self.command(UiCommand::ListUsers);
            }
            _ => {}
        }
        self.screen = screen;
        cx.notify();
    }

    pub fn form(&self, target: PictureFor) -> &PictureForm {
        match target {
            PictureFor::Profile => &self.profile_form,
            PictureFor::Server => &self.server_form,
        }
    }

    fn form_mut(&mut self, target: PictureFor) -> &mut PictureForm {
        match target {
            PictureFor::Profile => &mut self.profile_form,
            PictureFor::Server => &mut self.server_form,
        }
    }

    /// Marks our or the server's picture for removal on the next save.
    pub fn remove_picture(&mut self, target: PictureFor, cx: &mut Context<Self>) {
        let form = self.form_mut(target);
        form.outdate_pick();
        form.edit = Some(PictureEdit {
            bytes: None,
            image: None,
        });
        form.status = None;
        cx.notify();
    }

    /// Asks for a picture file, shrinks it and sets it as our or the server's picture.
    pub fn choose_picture(&mut self, target: PictureFor, cx: &mut Context<Self>) {
        let form = self.form_mut(target);
        form.status = None;
        form.outdate_pick();
        let pick = form.pick;
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Use as Picture".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = chosen.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let prepared = cx
                .background_executor()
                .spawn(async move {
                    let file = std::fs::read(&path).map_err(|e| e.to_string())?;
                    let bytes = crate::avatar::prepare(&file)?;
                    Ok(PictureEdit {
                        image: crate::avatar::decode(&bytes),
                        bytes: Some(bytes),
                    })
                })
                .await;
            this.update(cx, |app, cx| {
                if app.form_mut(target).picked(pick, prepared) {
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// The server we are connected to, or still reconnecting to.
    pub fn connected_server(&self) -> Option<&ServerView> {
        let gave_up = matches!(self.state.connection, ConnectionStatus::Failed(_));
        self.client
            .as_ref()
            .filter(|_| !gave_up)
            .and(self.state.server.as_ref())
    }

    /// Shows the server at `address`, connecting to it first unless we already are.
    pub fn open_server(&mut self, address: String, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .connected_server()
            .is_some_and(|s| s.address == address)
        {
            if matches!(self.screen, Screen::Settings(_) | Screen::Server(_)) {
                self.screen = Screen::Main;
            }
            cx.notify();
            return;
        }
        self.screen = Screen::Join;
        if !self.joining || self.address.read(cx).value() != address {
            set_field(&self.address, address, false, window, cx);
            set_field(&self.code, "", false, window, cx);
            self.join(window, cx);
        }
        cx.notify();
    }

    pub fn open_add_server(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.join_error = None;
        set_field(&self.address, "", true, window, cx);
        set_field(&self.code, "", false, window, cx);
        self.set_dialog(Some(Dialog::AddServer), cx);
    }

    /// Connects to the server in the address field, leaving the one we are on first and
    /// abandoning any join still under way.
    pub fn join(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.join_asked_in_settings = matches!(self.screen, Screen::Settings(_));
        let address = self.address.read(cx).value().trim().to_owned();
        let code_text = self.code.read(cx).value().to_string();
        if address.is_empty() {
            self.join_error = Some("Enter the server’s address.".into());
            return;
        }
        let invite = if code_text.trim().is_empty() {
            None
        } else {
            match code_text.parse::<InviteCode>() {
                Ok(code) => Some(code),
                Err(e) => {
                    self.join_error = Some(e.to_string());
                    return;
                }
            }
        };
        if self.client.is_some() {
            let dialog = self.dialog.take();
            self.disconnect(window, cx);
            self.dialog = dialog;
        }
        if self.mock {
            let client = MockClient::at(&address, self.verified.clone());
            self.attach(with_sample_share(client), window, cx);
            return;
        }
        let Some(profile) = self.settings.profile.clone() else {
            return;
        };
        self.verified_problem = read_verified(&self.verified);
        self.joining = true;
        self.join_attempt += 1;
        let attempt = self.join_attempt;
        self.join_error = None;
        let audio = Some(self.audio_engine.clone());
        let window_handle = window.window_handle();
        let joined_as = profile.clone();
        let (joining_profile, profile) = watch::channel(profile);
        self.joining_profile = Some(joining_profile);
        let connecting = cx.background_executor().spawn({
            let address = address.clone();
            let verified = self.verified.clone();
            async move {
                let keystore = Keystore::open_default().map_err(SignInError::Store)?;
                Client::connect(
                    &address,
                    keystore,
                    invite.as_ref(),
                    profile,
                    audio,
                    verified,
                )
            }
        });
        cx.spawn(async move |this, cx| {
            let result = connecting.await;
            cx.update_window(window_handle, |_, window, cx| {
                this.update(cx, |app, cx| {
                    if result.is_ok() {
                        app.reload_known_servers();
                    }
                    if app.join_attempt != attempt {
                        return;
                    }
                    match result {
                        Ok(client) => {
                            app.attach(Arc::new(client), window, cx);
                            if let Some(kept) = app.settings.profile.clone()
                                && kept != joined_as
                            {
                                app.command(UiCommand::SetProfile(kept));
                            }
                        }
                        Err(e) => {
                            app.joining = false;
                            app.joining_profile = None;
                            match e.pin_mismatch() {
                                Some((expected, actual)) => {
                                    app.dialog = Some(Dialog::PinMismatch {
                                        address: address.clone(),
                                        expected,
                                        actual,
                                    });
                                }
                                None if e.identity_missing() => app.show_identity_missing(),
                                None => app.join_error = Some(views::join::explain(&e, &address)),
                            }
                        }
                    }
                    cx.notify();
                })
                .ok();
            })
            .ok();
        })
        .detach();
    }

    /// Trusts a server's new key after a pin mismatch, as the human chose to.
    pub fn trust_new_key(
        &mut self,
        address: &str,
        actual: Fingerprint,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.mock
            || Keystore::default_known_servers()
                .and_then(|mut known| known.trust(address, actual))
                .is_ok()
        {
            self.set_dialog(None, cx);
            self.join(window, cx);
        }
    }

    /// Looks for the identity again after the keychain seemed to have lost it, as the human
    /// may have unlocked or started their keychain since.
    pub fn try_again(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_dialog(None, cx);
        self.join(window, cx);
    }

    /// Forgets every known server and starts a new identity, as the human chose to after the
    /// keychain lost the old one, unless the keychain holds it again by now.
    pub fn start_over(&mut self, cx: &mut Context<Self>) {
        if self.start_over == StartOver::Running {
            return;
        }
        if self.mock {
            self.known_servers.clear();
            self.start_over = StartOver::Done(StartedOver::NewIdentity);
            cx.notify();
            return;
        }
        self.start_over = StartOver::Running;
        cx.notify();
        let starting = cx
            .background_executor()
            .spawn(async { Keystore::start_over().map(|(_, started)| started) });
        cx.spawn(async move |this, cx| {
            let started = starting.await;
            this.update(cx, |app, cx| {
                app.reload_known_servers();
                app.start_over = match started {
                    Ok(started) => StartOver::Done(started),
                    Err(e) => StartOver::Failed(format!("Couldn’t start over: {e:#}")),
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Disconnects from the server, keeping our membership.
    fn disconnect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.command(UiCommand::Disconnect);
        self.end_session(window, cx);
    }

    /// Drops everything that belonged to the server's session, shows the screen for no server
    /// unless a settings page is open, and reloads the known servers' looks.
    fn end_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_all_popouts(cx);
        self.exit_fullscreen(window, cx);
        self.client = None;
        self.events = None;
        self.keep_display_awake = None;
        self.invite = None;
        self.users.clear();
        self.dialog = None;
        self.person_menu = None;
        self.focus = None;
        self.preview_self = false;
        self.space_talking = false;
        self.audio_engine.push_to_talk(false);
        self.server_form.reset();
        if !matches!(self.screen, Screen::Settings(_)) {
            self.screen = Screen::Join;
        }
        self.reload_known_servers();
    }

    /// Takes the server at `address` off our list without leaving it, abandoning a join to
    /// it and a session that gave up on it.
    pub fn remove_server(&mut self, address: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.joining && self.address.read(cx).value() == address {
            self.joining = false;
            self.join_attempt += 1;
        }
        if self.mock {
            self.known_servers.retain(|(known, _)| known != address);
        } else if let Err(e) = Keystore::default_known_servers().and_then(|mut k| k.forget(address))
        {
            tracing::warn!("forgetting {address}: {e:#}");
        }
        let attached = self.state.server.as_ref().map(|s| s.address.as_str());
        if self.client.is_some() && attached == Some(address) {
            self.disconnect(window, cx);
        }
        self.reload_known_servers();
        self.set_dialog(None, cx);
    }

    fn reload_known_servers(&mut self) {
        if self.mock {
            return;
        }
        self.known_servers = known_servers();
        self.server_looks = cached_looks(&self.known_servers);
    }

    /// The name of the server at `address`: the live one while connected to it, else the
    /// one last seen.
    pub fn server_name(&self, address: &str) -> Option<String> {
        match self.connected_server().filter(|s| s.address == address) {
            Some(server) => server.name.clone(),
            None => self.server_looks.get(address)?.name.clone(),
        }
    }

    /// Opens a screen of the mock client by name, for screenshots: onboarding, onboardingerror,
    /// welcome, join, joinfailed, lost, personmenu, addserver, removeserver, room, testpattern,
    /// unwatched, alone, emptylobby, focus, fullscreen, popout, slow, slowpopout, slowfullscreen,
    /// share, connection, person, personverified, personchanged, keychange, ownprofile,
    /// pinmismatch, settings, appearance, verified, about, update, newroom, editroom,
    /// roompassword, deleteroom, profile, profileerror, server, keys, members, invites,
    /// removeuser, leaveserver, identitymissing, identityback or startedover. Any other name
    /// leaves the lobby.
    pub fn show_sample(&mut self, screen: &str, window: &mut Window, cx: &mut Context<Self>) {
        let room = RoomId(1);
        let joined = |app: &mut Self| {
            app.command(UiCommand::Join(Some(room)));
            app.command(UiCommand::SetMic(true));
            app.command(UiCommand::Watch(UserId(4)));
        };
        let offline = |app: &mut Self| {
            app.client = None;
            app.screen = Screen::Join;
        };
        let with_picture = |app: &mut Self, cx: &mut Context<Self>| {
            let profile = Profile {
                name: "nori".into(),
                picture: Some(Arc::from(crate::avatar::sample())),
            };
            app.set_profile(profile.clone(), cx);
            app.command(UiCommand::SetProfile(profile));
        };
        let slow = screen.starts_with("slow");
        let make_slow = |app: &mut Self| {
            Arc::make_mut(&mut app.state)
                .health
                .slow_shares
                .insert(UserId(4));
        };
        let show_overlay = |app: &mut Self| {
            app.overlay = Some((UserId(4), Some(Instant::now() + Duration::from_secs(60))));
        };
        let refuse_picture = |app: &mut Self| {
            app.profile_form.status = crate::avatar::prepare(&[]).err().map(SaveStatus::Failed);
        };
        match screen {
            "onboarding" | "onboardingerror" => {
                offline(self);
                self.known_servers.clear();
                self.settings.profile = None;
                self.profile_avatar = None;
                self.show_screen(Screen::Onboarding, window, cx);
                if screen == "onboardingerror" {
                    after_a_moment(cx, move |app, _| refuse_picture(app));
                }
            }
            "welcome" => {
                offline(self);
                self.known_servers.clear();
            }
            "join" => offline(self),
            "joinfailed" => {
                offline(self);
                self.join_error = Some(views::join::unreachable_hint("owls.thro.cc"));
            }
            "lost" => {
                joined(self);
                after_a_moment(cx, |app, _| {
                    Arc::make_mut(&mut app.state).connection = ConnectionStatus::Reconnecting {
                        attempt: 3,
                        reason: "timed out".into(),
                    };
                });
            }
            "personmenu" => {
                joined(self);
                self.open_person_menu(
                    UserId(4),
                    gpui::point(gpui::px(900.), gpui::px(420.)),
                    window,
                    cx,
                );
            }
            "addserver" => {
                offline(self);
                self.dialog = Some(Dialog::AddServer);
            }
            "removeserver" => {
                offline(self);
                self.dialog = Some(Dialog::RemoveServer("192.168.1.20".into()));
            }
            "room" => {
                joined(self);
                after_a_moment(cx, move |app, _| show_overlay(app));
            }
            "testpattern" => {
                let client = MockClient::new(self.verified.clone());
                self.attach(Arc::new(client), window, cx);
                joined(self);
            }
            "unwatched" => {
                joined(self);
                self.command(UiCommand::Unwatch(UserId(4)));
            }
            "alone" => self.command(UiCommand::Join(Some(RoomId(3)))),
            "emptylobby" => {
                if let Some(client) = &self.client {
                    for room in client.state().rooms.iter() {
                        self.command(UiCommand::DeleteRoom(room.id));
                    }
                }
            }
            "focus" => {
                joined(self);
                self.focus = Some(UserId(4));
                after_a_moment(cx, move |app, _| show_overlay(app));
            }
            "fullscreen" | "slowfullscreen" => {
                joined(self);
                after_a_moment(cx, move |app, _| {
                    app.fullscreen = Some(UserId(4));
                    app.controls_until = Instant::now() + Duration::from_secs(60);
                    if slow {
                        make_slow(app);
                    }
                });
            }
            "popout" | "slowpopout" => {
                joined(self);
                after_a_moment(cx, move |app, cx| {
                    if slow {
                        make_slow(app);
                    }
                    app.pop_out(UserId(4), Duration::from_secs(60), cx)
                });
            }
            "slow" => {
                joined(self);
                after_a_moment(cx, move |app, _| {
                    show_overlay(app);
                    make_slow(app);
                });
            }
            "share" => {
                joined(self);
                self.dialog = Some(Dialog::Share);
            }
            "connection" => {
                joined(self);
                self.dialog = Some(Dialog::Connection);
            }
            "person" => self.open_profile(UserId(4), cx),
            "personverified" => self.open_profile(UserId(2), cx),
            "personchanged" => {
                self.open_profile(UserId(4), cx);
                if let Some(Dialog::Profile { key_changed, .. }) = &mut self.dialog {
                    *key_changed = true;
                }
            }
            "keychange" => {
                self.open_profile(UserId(3), cx);
                after_a_moment(cx, |app, _| {
                    let state = Arc::make_mut(&mut app.state);
                    if let Some(key) = state.person(UserId(3)).and_then(|p| p.identity_key) {
                        state.key_warnings.push(KeyWarning {
                            user: UserId(3),
                            verified: Fingerprint([0x7c; 32]),
                            now: Fingerprint(key),
                        });
                    }
                });
            }
            "ownprofile" => {
                joined(self);
                with_picture(self, cx);
                self.dialog = Some(Dialog::OwnProfile);
            }
            "pinmismatch" => {
                offline(self);
                self.dialog = Some(Dialog::PinMismatch {
                    address: "owls.thro.cc".into(),
                    expected: Fingerprint([0x2c; 32]),
                    actual: Fingerprint([0xb8; 32]),
                });
            }
            "settings" => self.screen = Screen::Settings(SettingsPage::Audio),
            "appearance" => self.screen = Screen::Settings(SettingsPage::Appearance),
            "verified" => self.screen = Screen::Settings(SettingsPage::Verified),
            "about" => {
                self.screen = Screen::Settings(SettingsPage::About);
                self.newer_release = Some(Release::sample());
            }
            "update" => {
                let release = Release::sample();
                self.newer_release = Some(release.clone());
                cx.spawn_in(window, async move |_, cx| {
                    cx.update(|window, cx| {
                        window.push_notification(update_notification(&release), cx)
                    })
                    .ok();
                })
                .detach();
            }
            "newroom" => self.dialog = Some(Dialog::NewRoom),
            "editroom" => self.dialog = Some(Dialog::EditRoom(room)),
            "roompassword" => {
                self.dialog = Some(Dialog::RoomPassword {
                    room: RoomId(3),
                    error: Some("wrong password".into()),
                })
            }
            "deleteroom" => self.dialog = Some(Dialog::DeleteRoom(room)),
            "profile" | "profileerror" => {
                self.screen = Screen::Settings(SettingsPage::Profile);
                joined(self);
                with_picture(self, cx);
                if screen == "profileerror" {
                    after_a_moment(cx, move |app, _| refuse_picture(app));
                }
            }
            "server" => self.screen = Screen::Server(ServerPage::Overview),
            "keys" => {
                joined(self);
                self.screen = Screen::Server(ServerPage::Keys);
            }
            "members" => {
                self.screen = Screen::Server(ServerPage::Members);
                self.command(UiCommand::CreateInvite);
            }
            "invites" => {
                self.screen = Screen::Server(ServerPage::Members);
                self.command(UiCommand::CreateInvite);
                after_a_moment(cx, |app, _| {
                    Arc::make_mut(&mut app.state).role = Some(Role::User)
                });
            }
            "removeuser" => {
                self.screen = Screen::Server(ServerPage::Members);
                self.dialog = Some(Dialog::Remove {
                    user: UserId(4),
                    name: "pixelmoth".into(),
                    cascade: false,
                });
            }
            "leaveserver" => self.dialog = Some(Dialog::LeaveServer),
            "identitymissing" => {
                offline(self);
                self.show_identity_missing();
            }
            "identityback" => {
                offline(self);
                self.show_identity_missing();
                self.start_over = StartOver::Done(StartedOver::Recovered);
            }
            "startedover" => {
                offline(self);
                self.known_servers.clear();
                self.show_identity_missing();
                self.start_over = StartOver::Done(StartedOver::NewIdentity);
            }
            _ => {}
        }
        if let Some(client) = &self.client {
            self.state = client.state();
        }
        self.show_screen(self.screen, window, cx);
    }

    /// Registers or drops the global push-to-talk key to match the transmit mode.
    pub fn sync_push_to_talk(&mut self) {
        if !self.uses_push_to_talk() {
            if self.push_to_talk.take().is_some() {
                self.audio_engine.push_to_talk(false);
            }
            return;
        }
        if self.push_to_talk.is_some() {
            return;
        }
        let audio = self.audio_engine.clone();
        let changed = move |down| audio.push_to_talk(down);
        match crate::hotkey::PushToTalkKey::register(crate::hotkey::DEFAULT_KEY, changed) {
            Ok(key) => self.push_to_talk = Some(key),
            Err(e) => tracing::warn!(
                "no global push-to-talk key ({e:#}); hold Space in the window instead"
            ),
        }
    }

    fn uses_push_to_talk(&self) -> bool {
        self.settings.audio.voice.transmit == Transmit::PushToTalk
    }

    /// Whether Space should talk: in push-to-talk mode, with no dialog open and no text field
    /// taking the keys.
    fn push_to_talk_ready(&self, window: &Window) -> bool {
        self.dialog.is_none() && self.uses_push_to_talk() && self.root_focus.is_focused(window)
    }

    fn release_space(&mut self) {
        if std::mem::take(&mut self.space_talking) {
            self.audio_engine.push_to_talk(false);
        }
    }

    /// Lists the devices, opens the ones the settings name and offers them on the audio page.
    /// Sample data opens none.
    pub fn change_devices(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let engine = (!self.mock).then(|| self.audio_engine.clone());
        let audio = self.settings.audio.clone();
        let earlier = self.changing_devices.take();
        let window_handle = window.window_handle();
        self.changing_devices = Some(cx.spawn(async move |this, cx| {
            if let Some(earlier) = earlier {
                earlier.await;
            }
            let (names, opened) = cx
                .background_executor()
                .spawn(async move {
                    let names = devices::list();
                    let opened = engine.map(|engine| open_devices(&engine, &audio, &names));
                    (names, opened)
                })
                .await;
            cx.update_window(window_handle, |_, window, cx| {
                this.update(cx, |app, cx| {
                    app.audio
                        .show_devices(&names, &app.settings.audio, window, cx);
                    if let Some(problem) = opened {
                        app.audio_problem = problem;
                    }
                    cx.notify();
                })
                .ok();
            })
            .ok();
        }));
    }

    pub fn set_mic_test(&mut self, on: bool) {
        self.mic_test = on;
        self.audio_engine.mic_test(on);
    }

    /// Whether the push-to-talk key works while another app has focus.
    pub fn has_global_push_to_talk(&self) -> bool {
        self.push_to_talk.is_some()
    }

    fn toggle_mic(&mut self, _: &ToggleMic, _: &mut Window, _: &mut Context<Self>) {
        self.command(UiCommand::SetMic(!self.state.mic));
    }

    fn toggle_deafen(&mut self, _: &ToggleDeafen, _: &mut Window, _: &mut Context<Self>) {
        self.command(UiCommand::SetDeafened(!self.state.deafened));
    }

    fn close_dialog(&mut self, _: &CloseDialog, window: &mut Window, cx: &mut Context<Self>) {
        if self.close_person_menu_in(window, cx) {
            return;
        }
        if self.dialog.is_none() && self.fullscreen.is_some() {
            self.exit_fullscreen(window, cx);
        }
        self.dismiss_dialog(cx);
    }
}

impl Render for ThroccApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.feeds.sweep();
        let drawn = std::mem::take(&mut self.parts_drawn);
        self.parts.retain(|part, _| drawn.contains(part));
        self.sidebar_width = self.sidebar_width.min(widest_sidebar(window));
        self.mascot.start_render();
        let lost = match self.screen {
            Screen::Main => views::join::render_lost(self),
            _ => None,
        };
        let shows_lost = lost.is_some();
        let screen = match self.screen {
            Screen::Onboarding => views::onboarding::render(self, cx).into_any_element(),
            Screen::Join => views::join::render(self, cx).into_any_element(),
            Screen::Main => match lost {
                Some(lost) => lost.into_any_element(),
                None => views::main::render(self, window, cx).into_any_element(),
            },
            Screen::Settings(page) => views::settings::render(self, page, cx).into_any_element(),
            Screen::Server(page) => views::server::render(self, page, cx).into_any_element(),
        };
        let screen = if self.screen == Screen::Onboarding
            || self.fullscreen_stream().is_some() && !shows_lost
        {
            screen
        } else {
            div()
                .size_full()
                .flex()
                .child(
                    self.part(
                        views::part::Part::ServerBar,
                        |app, _, cx| views::server_bar::render(app, cx).into_any_element(),
                        cx,
                    )
                    .cached(
                        gpui::StyleRefinement::default()
                            .w(theme::SERVER_BAR_WIDTH)
                            .h_full()
                            .flex_none(),
                    ),
                )
                .child(div().flex_1().min_w_0().h_full().child(screen))
                .into_any_element()
        };
        div()
            .id("throcc")
            .key_context("Throcc")
            .track_focus(&self.root_focus)
            .on_action(cx.listener(Self::toggle_mic))
            .on_action(cx.listener(Self::toggle_deafen))
            .on_action(cx.listener(Self::close_dialog))
            .on_key_down(cx.listener(|app, event: &gpui::KeyDownEvent, window, _| {
                if event.keystroke.key == "space"
                    && !event.is_held
                    && app.push_to_talk_ready(window)
                {
                    app.space_talking = true;
                    app.audio_engine.push_to_talk(true);
                }
            }))
            .on_key_up(cx.listener(|app, event: &gpui::KeyUpEvent, _, _| {
                if event.keystroke.key == "space" {
                    app.release_space();
                }
            }))
            .size_full()
            .relative()
            .bg(theme::window_bg())
            .text_color(theme::text())
            .on_mouse_move(
                cx.listener(|app, event: &gpui::MouseMoveEvent, window, cx| {
                    if !app.resizing_sidebar {
                        return;
                    }
                    if event.pressed_button == Some(gpui::MouseButton::Left) {
                        app.sidebar_width = (event.position.x - theme::SERVER_BAR_WIDTH)
                            .clamp(theme::SIDEBAR_MIN_WIDTH, widest_sidebar(window));
                    } else {
                        app.resizing_sidebar = false;
                    }
                    cx.notify();
                }),
            )
            .on_mouse_up(
                gpui::MouseButton::Left,
                cx.listener(|app, _, _, _| app.resizing_sidebar = false),
            )
            .font_family(theme::font())
            .typeset(theme::BODY)
            .child(screen)
            .children(views::banner::render(self))
            .children(
                self.dialog
                    .clone()
                    .map(|dialog| views::dialogs::render(self, dialog, cx)),
            )
            .children(views::person_menu::render(self, window, cx))
    }
}

#[cfg(test)]
mod tests {
    use throcc_proto::messages::MAX_PASSWORD_CHARS;

    use super::*;

    #[test]
    fn names_are_checked_with_a_reason() {
        assert_eq!(checked_name(""), Ok(None));
        assert_eq!(checked_name("   "), Ok(None));
        assert_eq!(checked_name("  Lobby "), Ok(Some("Lobby".into())));
        let longest = "é".repeat(MAX_NAME_CHARS);
        assert_eq!(checked_name(&longest), Ok(Some(longest.clone())));
        assert_eq!(checked_name(&format!("{longest}x")), Err(NAME_TOO_LONG));
        assert_eq!(checked_name("a\tb"), Err(NAME_HIDES_TEXT));
        assert_eq!(checked_name("adm\u{202E}nimda"), Err(NAME_HIDES_TEXT));
        assert_eq!(checked_name("\u{200B}"), Err(NAME_HIDES_TEXT));
        for blank in [
            "\u{2800}",
            "\u{200D}",
            "\u{FE0F}\u{FE0F}",
            "\u{2800} \u{200C}",
        ] {
            assert_eq!(checked_name(blank), Err(NAME_SHOWS_NOTHING), "{blank:?}");
        }
        assert!(NAME_TOO_LONG.contains(&MAX_NAME_CHARS.to_string()));
    }

    #[test]
    fn passwords_are_checked_with_a_reason() {
        assert_eq!(checked_password(""), Ok(None));
        assert_eq!(checked_password(" "), Ok(Some(" ".into())));
        let longest = "p".repeat(MAX_PASSWORD_CHARS);
        assert_eq!(checked_password(&longest), Ok(Some(longest.clone())));
        assert_eq!(
            checked_password(&format!("{longest}p")),
            Err(PASSWORD_TOO_LONG)
        );
        assert!(PASSWORD_TOO_LONG.contains(&MAX_PASSWORD_CHARS.to_string()));
    }

    #[test]
    fn only_a_pick_nothing_overtook_is_applied() {
        let edit = || {
            Ok(PictureEdit {
                image: None,
                bytes: Some(vec![1]),
            })
        };
        let mut form = PictureForm::default();
        for overtake in [PictureForm::reset, PictureForm::outdate_pick] {
            form.outdate_pick();
            let pick = form.pick;
            overtake(&mut form);
            assert!(!form.picked(pick, edit()));
            assert!(form.edit.is_none());
        }
        form.outdate_pick();
        assert!(form.picked(form.pick, edit()));
        assert_eq!(form.change(), Some(Some(vec![1])));
    }
}
