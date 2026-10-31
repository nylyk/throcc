use std::sync::Arc;

use gpui::{
    App, AppContext, Context, Entity, InteractiveElement, IntoElement, KeyBinding, ParentElement,
    PathPromptOptions, Render, RenderImage, Styled, Subscription, Task, Window, WindowHandle,
    actions, div,
};
use gpui_component::input::{InputEvent, InputState};
use gpui_component::notification::Notification;
use gpui_component::slider::{SliderEvent, SliderState};
use gpui_component::{Root, WindowExt};
use throcc_client_core::account::SignInError;
use throcc_client_core::client::{Client, ClientApi, ClientOptions, Command, Event, MockClient};
use throcc_client_core::keystore::Keystore;
use throcc_client_core::media::audio::VoiceSettings;
use throcc_client_core::media::devices;
use throcc_client_core::state::{ShareChoice, State};
use throcc_proto::Fingerprint;
use throcc_proto::ids::{Hash, Role, RoomId, UserId};
use throcc_proto::invite::{self, InviteCode};
use throcc_proto::messages::User;

use crate::theme;
use crate::views;

actions!(throcc, [ToggleMic, ToggleDeafen, CloseDialog]);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-shift-m", ToggleMic, None),
        KeyBinding::new("ctrl-shift-m", ToggleMic, None),
        KeyBinding::new("cmd-shift-d", ToggleDeafen, None),
        KeyBinding::new("ctrl-shift-d", ToggleDeafen, None),
        KeyBinding::new("escape", CloseDialog, None),
    ]);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsPage {
    Profile,
    Audio,
    Appearance,
    Servers,
}

/// Pages about the server we are connected to, apart from this machine's settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServerPage {
    Overview,
    Keys,
    Members,
}

/// Whose picture a chosen file becomes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PictureFor {
    Profile,
    Server,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    Join,
    Main,
    Settings(SettingsPage),
    Server(ServerPage),
}

/// How the last change on the profile page went.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProfileStatus {
    Saving,
    Saved,
    /// The picture is kept and goes on our profile when we next join a room.
    UntilJoined,
    Failed(String),
}

/// How the last change on the server page went.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServerStatus {
    Saving,
    Saved,
    Failed(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Dialog {
    Share,
    Connection,
    Verify(UserId),
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
    EditRoom {
        room: RoomId,
        confirm_delete: bool,
    },
}

fn known_servers() -> Vec<(String, Fingerprint)> {
    Keystore::default_known_servers()
        .map(|k| k.iter().map(|(a, f)| (a.to_owned(), f)).collect())
        .unwrap_or_default()
}

pub struct ThroccApp {
    pub client: Option<Arc<dyn ClientApi>>,
    pub state: Arc<State>,
    pub screen: Screen,
    pub dialog: Option<Dialog>,
    pub address: Entity<InputState>,
    pub code: Entity<InputState>,
    pub name: Entity<InputState>,
    pub room_name: Entity<InputState>,
    pub server_name: Entity<InputState>,
    /// Why the server's new picture couldn't be used.
    pub server_status: Option<ServerStatus>,
    /// One hex field per palette colour, in [`theme::Palette::LABELS`] order.
    pub palette_fields: Vec<Entity<InputState>>,
    pub pictures: std::collections::HashMap<Hash, Arc<RenderImage>>,
    pub joining: bool,
    /// Whether the join screen shows the form for a new server over the list of known ones.
    pub adding_server: bool,
    pub join_error: Option<String>,
    pub profile_status: Option<ProfileStatus>,
    /// The picture just chosen on the profile page, or `Some(None)` once removed, shown
    /// until the server has it.
    pub profile_preview: Option<Option<Arc<RenderImage>>>,
    pub known_servers: Vec<(String, Fingerprint)>,
    pub users: Vec<User>,
    pub invite: Option<(String, u64)>,
    pub voice: VoiceSettings,
    pub input_device: Option<String>,
    pub output_device: Option<String>,
    pub mic_test: bool,
    pub audio: views::settings::AudioControls,
    push_to_talk: Option<(crate::hotkey::PushToTalkKey, Task<()>)>,
    pub feeds: crate::video::Feeds,
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
    pub popouts: std::collections::HashMap<UserId, WindowHandle<Root>>,
    /// The share filling the whole window, with the rest of the UI hidden.
    pub fullscreen: Option<UserId>,
    /// Until when the controls over the fullscreen share show.
    pub controls_until: std::time::Instant,
    pub fullscreen_focus: gpui::FocusHandle,
    pub sidebar_width: gpui::Pixels,
    pub resizing_sidebar: bool,
    events: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl ThroccApp {
    pub fn new(mock: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let address = cx.new(|cx| InputState::new(window, cx).placeholder("voice.example.net"));
        let code = cx.new(|cx| InputState::new(window, cx).placeholder("K7Q-M2XPD"));
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Your name"));
        let room_name = cx.new(|cx| InputState::new(window, cx).placeholder("Room name"));
        let server_name = cx.new(|cx| InputState::new(window, cx).placeholder("Server name"));
        let palette_fields: Vec<Entity<InputState>> = theme::palette()
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
            cx.subscribe_in(&room_name, window, |app, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    match app.dialog {
                        Some(Dialog::NewRoom) => app.create_room(cx),
                        Some(Dialog::EditRoom { .. }) => app.rename_room(cx),
                        _ => {}
                    }
                }
            }),
        ]);
        let known_servers = if mock { Vec::new() } else { known_servers() };
        let share_choice = ShareChoice::default();
        let (bitrate, bitrate_changes) = Self::bitrate_slider(share_choice, cx);
        let (inputs, outputs) = devices::list();
        let voice = VoiceSettings::default();
        let audio = views::settings::AudioControls::new(&voice, &inputs, &outputs, window, cx);
        let mut app = Self {
            client: None,
            state: Arc::new(State::default()),
            screen: Screen::Join,
            dialog: None,
            address,
            code,
            name,
            room_name,
            server_name,
            server_status: None,
            palette_fields,
            pictures: std::collections::HashMap::new(),
            joining: false,
            adding_server: false,
            join_error: None,
            profile_status: None,
            profile_preview: None,
            known_servers,
            users: Vec::new(),
            invite: None,
            voice,
            input_device: None,
            output_device: None,
            mic_test: false,
            audio,
            push_to_talk: None,
            feeds: crate::video::Feeds::default(),
            share_choice,
            share_audio: true,
            bitrate,
            _bitrate_changes: bitrate_changes,
            focus: None,
            preview_self: false,
            popouts: std::collections::HashMap::new(),
            fullscreen: None,
            controls_until: std::time::Instant::now(),
            fullscreen_focus: cx.focus_handle(),
            sidebar_width: theme::SIDEBAR_WIDTH,
            resizing_sidebar: false,
            events: None,
            _subscriptions: subscriptions,
        };
        if mock {
            app.attach(Arc::new(MockClient::new()), window, cx);
        }
        app
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

    pub fn command(&self, command: Command) {
        if let Some(client) = &self.client {
            client.command(command);
        }
    }

    fn attach(&mut self, client: Arc<dyn ClientApi>, window: &mut Window, cx: &mut Context<Self>) {
        self.state = client.state();
        let mut events = client.events();
        self.client = Some(client);
        self.screen = Screen::Main;
        self.joining = false;
        self.adding_server = false;
        self.join_error = None;
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

    fn on_event(&mut self, event: Event, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            Event::State(state) => {
                self.state = state;
                self.close_unwatched_popouts(cx);
                if self.fullscreen.is_some_and(|user| !self.shows_stream(user)) {
                    self.exit_fullscreen(window, cx);
                }
            }
            Event::Invite { code, expires } => self.invite = Some((code, expires)),
            Event::Users(users) => self.users = users,
            Event::Notice(text) => window.push_notification(Notification::new().message(text), cx),
            Event::Kicked(reason) => {
                for user in self.popouts.keys().copied().collect::<Vec<_>>() {
                    self.close_popout(user, cx);
                }
                self.client = None;
                self.screen = Screen::Join;
                self.join_error = Some(reason);
            }
            Event::AvatarReady(_) => {}
            Event::ProfileSaved => self.profile_status = Some(ProfileStatus::Saved),
            Event::ProfileRefused(reason) => {
                self.profile_preview = None;
                self.profile_status = Some(ProfileStatus::Failed(reason));
            }
            Event::ServerProfileSaved => self.server_status = Some(ServerStatus::Saved),
            Event::ServerProfileRefused(reason) => {
                self.server_status = Some(ServerStatus::Failed(reason))
            }
        }
        cx.notify();
    }

    /// The decoded pictures of everyone in the state who has one and has arrived.
    pub fn pictures_of_people(&mut self) -> std::collections::HashMap<UserId, Arc<RenderImage>> {
        let wanted: Vec<(UserId, Option<Hash>)> = self
            .state
            .people
            .iter()
            .map(|(user, p)| (*user, p.avatar))
            .collect();
        wanted
            .into_iter()
            .filter_map(|(user, avatar)| Some((user, self.picture(avatar)?)))
            .collect()
    }

    /// The decoded profile picture behind `avatar`, once the client has it.
    pub fn picture(&mut self, avatar: Option<Hash>) -> Option<Arc<RenderImage>> {
        let hash = avatar?;
        if let Some(picture) = self.pictures.get(&hash) {
            return Some(picture.clone());
        }
        let picture = crate::avatar::decode(&self.client.as_ref()?.avatar(hash)?)?;
        self.pictures.insert(hash, picture.clone());
        Some(picture)
    }

    /// Switches every window to `palette`, keeps it for next time and shows it in the fields.
    pub fn use_palette(
        &mut self,
        palette: theme::Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        theme::set(palette, cx);
        theme::save(palette);
        for (field, color) in self.palette_fields.iter().zip(palette.colors()) {
            field.update(cx, |field, cx| {
                field.set_value(theme::format_hex(color), window, cx)
            });
        }
    }

    /// Takes a typed hex colour into the palette once it parses.
    fn set_palette_color(&mut self, index: usize, typed: &str, cx: &mut Context<Self>) {
        let Some(color) = theme::parse_hex(typed) else {
            return;
        };
        let mut colors = theme::palette().colors();
        if colors[index] == color {
            return;
        }
        colors[index] = color;
        let palette = theme::Palette::from_colors(colors);
        theme::set(palette, cx);
        theme::save(palette);
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
        self.show_controls(cx);
        window.focus(&self.fullscreen_focus, cx);
        if !window.is_fullscreen() {
            window.toggle_fullscreen();
        }
    }

    pub fn exit_fullscreen(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.fullscreen = None;
        if window.is_fullscreen() {
            window.toggle_fullscreen();
        }
        cx.notify();
    }

    pub fn show_controls(&mut self, cx: &mut Context<Self>) {
        self.controls_until = std::time::Instant::now() + crate::views::common::CONTROLS_LINGER;
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
            let (SliderEvent::Change(value) | SliderEvent::Release(value)) = event;
            let at = value.start().round().max(0.) as usize;
            if let Some(kbps) = offered.get(at) {
                app.share_choice.kbps = *kbps;
                cx.notify();
            }
        });
        (slider, changes)
    }

    /// Moves a watched share into a window of its own.
    pub fn pop_out(&mut self, user: UserId, cx: &mut Context<Self>) {
        if self.popouts.contains_key(&user) {
            return;
        }
        let name = self
            .state
            .person(user)
            .map_or("Someone".into(), |p| p.name.clone());
        let title = format!("{name}'s screen");
        let app = cx.entity().downgrade();
        cx.defer(move |cx| {
            let Some(window) = crate::popout::PopOut::open(app.clone(), user, title, cx) else {
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
            window
                .update(cx, |_, window, _| window.remove_window())
                .ok();
        }
        cx.notify();
    }

    fn close_unwatched_popouts(&mut self, cx: &mut Context<Self>) {
        let watching = self
            .state
            .current
            .as_ref()
            .map(|c| c.watching.clone())
            .unwrap_or_default();
        let gone: Vec<UserId> = self
            .popouts
            .keys()
            .filter(|user| {
                !watching.contains(user)
                    || self
                        .state
                        .person(**user)
                        .is_none_or(|p| p.sharing.is_none())
            })
            .copied()
            .collect();
        for user in gone {
            self.close_popout(user, cx);
        }
    }

    /// Whether we may create, rename and delete rooms.
    pub fn manages_rooms(&self) -> bool {
        matches!(self.state.role, Some(Role::Admin | Role::Manager))
    }

    pub fn open_new_room(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.room_name.update(cx, |s, cx| {
            s.set_value("", window, cx);
            s.focus(window, cx);
        });
        self.dialog = Some(Dialog::NewRoom);
        cx.notify();
    }

    pub fn create_room(&mut self, cx: &mut Context<Self>) {
        let name = self.room_name.read(cx).value().trim().to_owned();
        if name.is_empty() {
            return;
        }
        self.command(Command::CreateRoom(name));
        self.dialog = None;
        cx.notify();
    }

    pub fn open_edit_room(&mut self, room: RoomId, window: &mut Window, cx: &mut Context<Self>) {
        let name = self
            .state
            .room(room)
            .map(|r| r.name.clone())
            .unwrap_or_default();
        self.room_name.update(cx, |s, cx| {
            s.set_value(name, window, cx);
            s.focus(window, cx);
        });
        self.dialog = Some(Dialog::EditRoom {
            room,
            confirm_delete: false,
        });
        cx.notify();
    }

    pub fn rename_room(&mut self, cx: &mut Context<Self>) {
        let Some(Dialog::EditRoom { room, .. }) = self.dialog else {
            return;
        };
        let name = self.room_name.read(cx).value().trim().to_owned();
        let unchanged = self.state.room(room).is_some_and(|r| r.name == name);
        if !name.is_empty() && !unchanged {
            self.command(Command::RenameRoom(room, name));
        }
        self.dialog = None;
        cx.notify();
    }

    /// What a picture change waits on: the server, or, without a name kept here, the name
    /// the server tells us when we next join a room.
    pub fn saving_status(&self) -> ProfileStatus {
        if self.state.profile_name.is_some() || self.state.current.is_some() {
            ProfileStatus::Saving
        } else {
            ProfileStatus::UntilJoined
        }
    }

    /// Puts our kept name in the profile page's name field.
    pub fn fill_profile(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.profile_status = None;
        self.profile_preview = None;
        let name = self.state.profile_name.clone().unwrap_or_default();
        self.name.update(cx, |s, cx| s.set_value(name, window, cx));
    }

    /// Puts the server's name in the server page's name field.
    pub fn fill_server(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.server_status = None;
        let name = self
            .state
            .server
            .as_ref()
            .and_then(|s| s.name.clone())
            .unwrap_or_default();
        self.server_name
            .update(cx, |s, cx| s.set_value(name, window, cx));
    }

    /// Asks for a picture file, shrinks it and sets it as our or the server's picture.
    pub fn choose_picture(&mut self, target: PictureFor, cx: &mut Context<Self>) {
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Use as picture".into()),
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
                    crate::avatar::prepare(&file)
                })
                .await;
            this.update(cx, |app, cx| {
                match (target, prepared) {
                    (PictureFor::Profile, Ok(bytes)) => {
                        app.profile_preview = Some(crate::avatar::decode(&bytes));
                        app.profile_status = Some(app.saving_status());
                        app.command(Command::SetAvatar(Some(bytes)));
                    }
                    (PictureFor::Profile, Err(message)) => {
                        app.profile_status = Some(ProfileStatus::Failed(message))
                    }
                    (PictureFor::Server, Ok(bytes)) => {
                        app.server_status = Some(ServerStatus::Saving);
                        app.command(Command::SetServerPicture(Some(bytes)));
                    }
                    (PictureFor::Server, Err(message)) => {
                        app.server_status = Some(ServerStatus::Failed(message))
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub fn join(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let address = self.address.read(cx).value().trim().to_owned();
        let code_text = self.code.read(cx).value().to_string();
        if address.is_empty() {
            self.join_error = Some("Enter the server's address.".into());
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
        self.joining = true;
        self.join_error = None;
        let options = ClientOptions {
            voice: self.voice,
            input_device: self.input_device.clone(),
            output_device: self.output_device.clone(),
            audio: true,
        };
        let window_handle = window.window_handle();
        let connecting = cx.background_executor().spawn({
            let address = address.clone();
            async move {
                let keystore = Keystore::open_default().map_err(SignInError::Store)?;
                Client::connect(&address, keystore, invite.as_ref(), options)
            }
        });
        cx.spawn(async move |this, cx| {
            let result = connecting.await;
            cx.update_window(window_handle, |_, window, cx| {
                this.update(cx, |app, cx| {
                    match result {
                        Ok(client) => app.attach(Arc::new(client), window, cx),
                        Err(e) => {
                            app.joining = false;
                            match e.pin_mismatch() {
                                Some((expected, actual)) => {
                                    app.dialog = Some(Dialog::PinMismatch {
                                        address: address.clone(),
                                        expected,
                                        actual,
                                    });
                                }
                                None => app.join_error = Some(views::join::explain(&e)),
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
        if let Ok(mut known) = Keystore::default_known_servers()
            && known.trust(address, actual).is_ok()
        {
            self.dialog = None;
            self.join(window, cx);
        }
    }

    pub fn leave_server(&mut self, cx: &mut Context<Self>) {
        for user in self.popouts.keys().copied().collect::<Vec<_>>() {
            self.close_popout(user, cx);
        }
        self.command(Command::Disconnect);
        self.client = None;
        self.events = None;
        self.screen = Screen::Join;
        self.known_servers = known_servers();
        cx.notify();
    }

    /// Opens a screen of the mock client by name, for screenshots: join, lobby, room, focus,
    /// fullscreen, popout, share, settings, appearance, profile, connection, verify, members
    /// or keychange.
    pub fn show_sample(&mut self, screen: &str, window: &mut Window, cx: &mut Context<Self>) {
        let room = throcc_proto::ids::RoomId(1);
        let joined = |app: &mut Self| {
            app.command(Command::Join(Some(room)));
            app.command(Command::SetMic(true));
            app.command(Command::Watch(UserId(4)));
        };
        match screen {
            "join" => {
                self.client = None;
                self.screen = Screen::Join;
                self.known_servers = vec![("voice.example.net".into(), Fingerprint([0x2c; 32]))];
            }
            "room" => joined(self),
            "focus" => {
                joined(self);
                self.focus = Some(UserId(4));
            }
            "fullscreen" => {
                joined(self);
                cx.spawn(async move |this, cx| {
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(300))
                        .await;
                    this.update(cx, |app, cx| {
                        app.fullscreen = Some(UserId(4));
                        app.controls_until =
                            std::time::Instant::now() + std::time::Duration::from_secs(60);
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
            }
            "popout" => {
                joined(self);
                cx.spawn(async move |this, cx| {
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(300))
                        .await;
                    this.update(cx, |app, cx| app.pop_out(UserId(4), cx)).ok();
                })
                .detach();
            }
            "share" => {
                joined(self);
                self.dialog = Some(Dialog::Share);
            }
            "connection" => {
                joined(self);
                self.dialog = Some(Dialog::Connection);
            }
            "verify" => self.dialog = Some(Dialog::Verify(UserId(2))),
            "keychange" => self.dialog = Some(Dialog::Verify(UserId(3))),
            "settings" => self.screen = Screen::Settings(SettingsPage::Audio),
            "appearance" => self.screen = Screen::Settings(SettingsPage::Appearance),
            "newroom" => self.dialog = Some(Dialog::NewRoom),
            "editroom" => {
                self.dialog = Some(Dialog::EditRoom {
                    room: throcc_proto::ids::RoomId(1),
                    confirm_delete: false,
                })
            }
            "deleteroom" => {
                self.dialog = Some(Dialog::EditRoom {
                    room: throcc_proto::ids::RoomId(1),
                    confirm_delete: true,
                })
            }
            "profile" => {
                self.screen = Screen::Settings(SettingsPage::Profile);
                joined(self);
                self.command(Command::SetAvatar(Some(crate::avatar::sample())));
            }
            "server" => {
                self.screen = Screen::Server(ServerPage::Overview);
                self.fill_server(window, cx);
            }
            "members" => {
                self.screen = Screen::Server(ServerPage::Members);
                self.command(Command::ListUsers);
                self.command(Command::CreateInvite);
            }
            _ => {}
        }
        if let Some(client) = &self.client {
            self.state = client.state();
        }
        if self.screen == Screen::Settings(SettingsPage::Profile) {
            self.fill_profile(window, cx);
        }
    }

    /// Registers or drops the global push-to-talk key to match the transmit mode.
    pub fn sync_push_to_talk(&mut self, cx: &mut Context<Self>) {
        let wanted = matches!(
            self.voice.transmit,
            throcc_client_core::media::audio::Transmit::PushToTalk
        );
        if !wanted {
            self.push_to_talk = None;
            return;
        }
        if self.push_to_talk.is_some() {
            return;
        }
        match crate::hotkey::PushToTalkKey::register(crate::hotkey::DEFAULT_KEY) {
            Ok(key) => {
                let poller = cx.spawn(async move |this, cx| {
                    loop {
                        cx.background_executor()
                            .timer(std::time::Duration::from_millis(10))
                            .await;
                        let alive = this.update(cx, |app, _| {
                            if let Some(down) =
                                app.push_to_talk.as_ref().and_then(|(key, _)| key.poll())
                            {
                                app.command(Command::PushToTalk(down));
                            }
                        });
                        if alive.is_err() {
                            return;
                        }
                    }
                });
                self.push_to_talk = Some((key, poller));
            }
            Err(e) => tracing::warn!(
                "no global push-to-talk key ({e:#}); hold Space in the window instead"
            ),
        }
    }

    fn push_to_talk_ready(&self) -> bool {
        self.screen == Screen::Main
            && self.dialog.is_none()
            && matches!(
                self.voice.transmit,
                throcc_client_core::media::audio::Transmit::PushToTalk
            )
    }

    pub fn safety_number(&self, user: UserId) -> Option<String> {
        self.client.as_ref()?.safety_number(user)
    }

    fn toggle_mic(&mut self, _: &ToggleMic, _: &mut Window, _: &mut Context<Self>) {
        if let Some(current) = &self.state.current {
            self.command(Command::SetMic(!current.mic));
        }
    }

    fn toggle_deafen(&mut self, _: &ToggleDeafen, _: &mut Window, _: &mut Context<Self>) {
        if let Some(current) = &self.state.current {
            self.command(Command::SetDeafened(!current.deafened));
        }
    }

    fn close_dialog(&mut self, _: &CloseDialog, window: &mut Window, cx: &mut Context<Self>) {
        if self.dialog.is_none() && self.fullscreen.is_some() {
            self.exit_fullscreen(window, cx);
        }
        self.dialog = None;
        cx.notify();
    }
}

impl Render for ThroccApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let screen = match self.screen {
            Screen::Join => views::join::render(self, window, cx).into_any_element(),
            Screen::Main => views::main::render(self, window, cx).into_any_element(),
            Screen::Settings(page) => {
                views::settings::render(self, page, window, cx).into_any_element()
            }
            Screen::Server(page) => {
                views::server::render(self, page, window, cx).into_any_element()
            }
        };
        div()
            .id("throcc")
            .key_context("Throcc")
            .on_action(cx.listener(Self::toggle_mic))
            .on_action(cx.listener(Self::toggle_deafen))
            .on_action(cx.listener(Self::close_dialog))
            .on_key_down(cx.listener(|app, event: &gpui::KeyDownEvent, _, _| {
                if event.keystroke.key == "space" && !event.is_held && app.push_to_talk_ready() {
                    app.command(Command::PushToTalk(true));
                }
            }))
            .on_key_up(cx.listener(|app, event: &gpui::KeyUpEvent, _, _| {
                if event.keystroke.key == "space" && app.push_to_talk_ready() {
                    app.command(Command::PushToTalk(false));
                }
            }))
            .size_full()
            .relative()
            .bg(theme::window_bg())
            .text_color(theme::text())
            .on_mouse_move(cx.listener(|app, event: &gpui::MouseMoveEvent, _, cx| {
                if !app.resizing_sidebar {
                    return;
                }
                if event.pressed_button == Some(gpui::MouseButton::Left) {
                    app.sidebar_width = event
                        .position
                        .x
                        .clamp(theme::SIDEBAR_MIN_WIDTH, theme::SIDEBAR_MAX_WIDTH);
                } else {
                    app.resizing_sidebar = false;
                }
                cx.notify();
            }))
            .on_mouse_up(
                gpui::MouseButton::Left,
                cx.listener(|app, _, _, _| app.resizing_sidebar = false),
            )
            .font_family(theme::font())
            .text_size(gpui::px(14.))
            .child(screen)
            .children(views::banner::render(self))
            .children(
                self.dialog
                    .clone()
                    .map(|dialog| views::dialogs::render(self, dialog, window, cx)),
            )
    }
}
