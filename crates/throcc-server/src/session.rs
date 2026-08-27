use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use quinn::Connection;
use rand::RngExt as _;
use throcc_proto::{
    Auth, AuthResult, ErrorCode, Event, PROTOCOL_VERSION, Request, RequestEnvelope, Response,
    ResponseEnvelope, Role, RoomId, ServerHello, ServerMessage, User,
};
use tokio::sync::mpsc;

use crate::control::{ControlReader, ControlWriter};
use crate::rooms::Placement;
use crate::{State, auth, invite, perms};

const DRAIN_GRACE: Duration = Duration::from_secs(1);
const OUTBOUND_DEPTH: usize = 256;
const ROOM_NAME_MAX: usize = 64;

pub async fn serve(connection: Connection, state: Arc<State>) {
    tracing::info!(
        rtt = ?connection.rtt(),
        max_datagram = ?connection.max_datagram_size(),
        "connected"
    );

    if let Err(e) = control(&connection, &state).await {
        tracing::warn!(error = ?e, "control stream ended in error");
        connection.close(1u32.into(), b"protocol error");
    }

    let reason = connection.closed().await;
    tracing::info!(%reason, "disconnected");
}

async fn control(connection: &Connection, state: &Arc<State>) -> Result<()> {
    let (send, recv) = connection
        .open_bi()
        .await
        .context("opening the control stream")?;
    let mut writer = ControlWriter::new(send);
    let mut reader = ControlReader::new(recv);

    let Some(admitted) = greet(connection, state, &mut writer, &mut reader).await? else {
        let _ = tokio::time::timeout(DRAIN_GRACE, writer.drain()).await;
        return Ok(());
    };
    let actor = admitted.user.clone();

    let (outbound, queue) = mpsc::channel(OUTBOUND_DEPTH);
    if let Some(displaced) = state
        .rooms()
        .attach(actor.id, outbound.clone(), connection.clone())
    {
        tracing::info!(user = %actor.id, "a newer connection for this key displaces this one");
        displaced.close(3u32.into(), b"displaced by a newer connection");
    }

    let welcomed = welcome(state, &mut writer, admitted).await;
    let writing = tokio::spawn(write_outbound(writer, queue));
    let outcome = match welcomed {
        Ok(()) => serve_requests(&mut reader, &actor, state, &outbound).await,
        Err(e) => Err(e),
    };

    // The registry holds a clone of the sender, so the writer task cannot see the
    // queue close until this session is out of the registry.
    if let Err(e) = state.rooms().detach(&state.database, actor.id) {
        tracing::warn!(error = ?e, user = %actor.id, "could not record a departure");
    }
    drop(outbound);
    let _ = writing.await;
    outcome
}

async fn welcome(state: &State, writer: &mut ControlWriter, admitted: Admitted) -> Result<()> {
    let placed = state
        .rooms()
        .place(&state.database, admitted.user.id, admitted.want_room)?;
    tracing::info!(
        user = %admitted.user.id,
        room = ?placed.room,
        epoch = %placed.epoch,
        "placed"
    );

    writer
        .write(&AuthResult::Ok {
            me: admitted.user.id,
            role: admitted.user.role,
            users: admitted.users,
            rooms: state.database.list_rooms()?,
            placed,
        })
        .await
}

struct Admitted {
    user: User,
    users: Vec<User>,
    want_room: Option<RoomId>,
}

/// The admitted client, or `None` when it was refused and told why.
async fn greet(
    connection: &Connection,
    state: &State,
    writer: &mut ControlWriter,
    reader: &mut ControlReader,
) -> Result<Option<Admitted>> {
    let server_nonce: [u8; 32] = rand::rng().random();
    writer
        .write(&ServerHello {
            server_nonce,
            protocol: PROTOCOL_VERSION,
        })
        .await?;

    let auth: Auth = reader
        .read()
        .await?
        .context("the client closed before authenticating")?;
    match auth::decide(
        state,
        &auth,
        &server_nonce,
        &auth::keying_material(connection)?,
        connection.remote_address().ip(),
    )? {
        auth::Decision::Refused(error) => {
            writer.write(&AuthResult::Err(error)).await?;
            Ok(None)
        }
        auth::Decision::Admitted {
            user,
            users,
            enrolled,
        } => {
            if enrolled {
                tracing::info!(user = %user.id, role = ?user.role, "enrolled a new user");
            }
            tracing::info!(user = %user.id, role = ?user.role, "authenticated");
            Ok(Some(Admitted {
                user,
                users,
                want_room: auth.want_room,
            }))
        }
    }
}

async fn write_outbound(mut writer: ControlWriter, mut queue: mpsc::Receiver<ServerMessage>) {
    while let Some(message) = queue.recv().await {
        if let Err(e) = writer.write(&message).await {
            tracing::warn!(error = ?e, "could not write to the control stream");
            return;
        }
    }
    let _ = tokio::time::timeout(DRAIN_GRACE, writer.drain()).await;
}

async fn serve_requests(
    reader: &mut ControlReader,
    actor: &User,
    state: &State,
    outbound: &mpsc::Sender<ServerMessage>,
) -> Result<()> {
    while let Some(RequestEnvelope { id, request }) = reader.read().await? {
        tracing::debug!(id, ?request, "request");
        let response = handle(request, actor, state);
        if outbound
            .send(ServerMessage::Response(ResponseEnvelope { id, response }))
            .await
            .is_err()
        {
            return Ok(());
        }
    }
    Ok(())
}

fn handle(request: Request, actor: &User, state: &State) -> Response {
    match request {
        Request::SetRoom(room) => set_room(actor, state, room),
        Request::CreateInvite { role, ttl_secs } => create_invite(actor, state, role, ttl_secs),
        Request::CreateRoom { name } => create_room(actor, state, name),
        Request::RenameRoom { room, name } => rename_room(actor, state, room, name),
        Request::DeleteRoom(room) => delete_room(actor, state, room),
        other => Response::Err {
            code: ErrorCode::Unimplemented,
            message: format!("{other:?} is not implemented"),
        },
    }
}

fn set_room(actor: &User, state: &State, target: Option<RoomId>) -> Response {
    match state.rooms().set_room(&state.database, actor.id, target) {
        Ok(Placement::Placed(placed)) => {
            tracing::info!(user = %actor.id, room = ?placed.room, epoch = %placed.epoch, "placed");
            Response::Placed(placed)
        }
        Ok(Placement::NoSuchRoom(room)) => no_such_room(room),
        Err(e) => failed(e, "change rooms"),
    }
}

fn create_room(actor: &User, state: &State, name: String) -> Response {
    if let Err(denied) = perms::require_role(actor.role, Role::Manager) {
        return denied;
    }
    let name = match room_name(name) {
        Ok(name) => name,
        Err(invalid) => return invalid,
    };

    match state.database.create_room(&name) {
        Ok(room) => {
            tracing::info!(by = %actor.id, room = %room.id, %name, "created a room");
            state.rooms().broadcast(Event::RoomCreated(room));
            Response::Ok
        }
        Err(e) => failed(e, "create a room"),
    }
}

fn rename_room(actor: &User, state: &State, room: RoomId, name: String) -> Response {
    if let Err(denied) = perms::require_role(actor.role, Role::Manager) {
        return denied;
    }
    let name = match room_name(name) {
        Ok(name) => name,
        Err(invalid) => return invalid,
    };

    match state.database.rename_room(room, &name) {
        Ok(false) => no_such_room(room),
        Ok(true) => {
            tracing::info!(by = %actor.id, %room, %name, "renamed a room");
            state.rooms().broadcast(Event::RoomRenamed { room, name });
            Response::Ok
        }
        Err(e) => failed(e, "rename a room"),
    }
}

fn delete_room(actor: &User, state: &State, room: RoomId) -> Response {
    if let Err(denied) = perms::require_role(actor.role, Role::Manager) {
        return denied;
    }

    match state.database.delete_room(room) {
        Ok(false) => no_such_room(room),
        Ok(true) => {
            tracing::info!(by = %actor.id, %room, "deleted a room");
            let mut rooms = state.rooms();
            rooms.clear_room(room);
            rooms.broadcast(Event::RoomDeleted(room));
            Response::Ok
        }
        Err(e) => failed(e, "delete a room"),
    }
}

fn room_name(name: String) -> Result<String, Response> {
    let name = name.trim().to_string();
    if name.is_empty() || name.chars().count() > ROOM_NAME_MAX {
        return Err(Response::Err {
            code: ErrorCode::Invalid,
            message: format!("a room name is 1 to {ROOM_NAME_MAX} characters"),
        });
    }
    Ok(name)
}

fn no_such_room(room: RoomId) -> Response {
    Response::Err {
        code: ErrorCode::NotFound,
        message: format!("no room {room}"),
    }
}

fn failed(error: anyhow::Error, attempt: &str) -> Response {
    tracing::error!(error = ?error, "could not {attempt}");
    Response::Err {
        code: ErrorCode::Invalid,
        message: format!("the server could not {attempt}"),
    }
}

fn create_invite(actor: &User, state: &State, role: Role, ttl_secs: u32) -> Response {
    if let Err(denied) = perms::require_role(actor.role, Role::Admin) {
        return denied;
    }
    if let Err(denied) = perms::require_rank_above(actor.role, role) {
        return denied;
    }

    let ttl = match ttl_secs {
        0 => invite::DEFAULT_TTL,
        seconds => Duration::from_secs(seconds.into()),
    };

    match state.database.create_invite(role, ttl) {
        Ok(created) => {
            tracing::info!(by = %actor.id, ?role, expires_at = created.expires_at, "minted an invite");
            Response::InviteCode {
                code: created.code,
                expires: created.expires_at,
            }
        }
        Err(e) => failed(e, "mint an invite"),
    }
}
