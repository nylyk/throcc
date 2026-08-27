use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use rusqlite::types::Type;
use rusqlite::{Connection, OptionalExtension as _, Row, Transaction, params};
use throcc_proto::{Epoch, MediaId, Role, Room, RoomId, Share, Tracks, User, UserId};

use crate::invite;

pub const DATABASE_FILE: &str = "throcc.sqlite";

const SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS users (
        id          INTEGER PRIMARY KEY,
        pubkey      BLOB    NOT NULL UNIQUE,
        name        TEXT    NOT NULL,
        avatar_hash BLOB,
        role        INTEGER NOT NULL,
        created_at  INTEGER NOT NULL
    ) STRICT;

    CREATE TABLE IF NOT EXISTS rooms (
        id    INTEGER PRIMARY KEY,
        name  TEXT    NOT NULL,
        epoch INTEGER NOT NULL
    ) STRICT;

    CREATE TABLE IF NOT EXISTS counters (
        name  TEXT    PRIMARY KEY,
        value INTEGER NOT NULL
    ) STRICT;

    CREATE TABLE IF NOT EXISTS invites (
        secret_hash BLOB    PRIMARY KEY,
        role        INTEGER NOT NULL,
        expires_at  INTEGER NOT NULL,
        redeemed_by INTEGER REFERENCES users(id),
        redeemed_at INTEGER
    ) STRICT;
";

/// A redeemed invite is kept as an enrollment record for this long before it is
/// pruned.
const REDEEMED_RETENTION: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// A generated invite. The code goes to whoever will redeem it, and the server
/// keeps only its hash.
pub struct Invite {
    pub code: String,
    pub expires_at: u64,
}

/// SQLite has no unsigned integers, so times are `i64` seconds internally.
fn unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the clock is before the unix epoch")
        .as_secs() as i64
}

/// The epoch each room a membership change touched now carries, and the media
/// ids allocated for the room entered.
pub struct Transition {
    pub leaving: Option<Epoch>,
    pub entering: Option<Epoch>,
    pub tracks: Option<Tracks>,
}

const MEDIA_ID_COUNTER: &str = "media_id";

pub enum Admission {
    Admitted {
        user: User,
        users: Vec<User>,
        enrolled: bool,
    },
    NotAllowlisted,
    InviteRefused,
}

pub struct Database {
    connection: Mutex<Connection>,
}

impl Database {
    pub fn open(data_dir: &Path) -> Result<Self> {
        let path = data_dir.join(DATABASE_FILE);
        let connection = Connection::open(&path)
            .with_context(|| format!("opening the database at {}", path.display()))?;
        connection
            .execute_batch(
                "PRAGMA journal_mode = WAL;
                 PRAGMA foreign_keys = ON;
                 PRAGMA busy_timeout = 5000;",
            )
            .context("configuring the database")?;
        connection
            .execute_batch(SCHEMA)
            .context("applying the schema")?;

        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    pub fn user_count(&self) -> Result<i64> {
        let connection = self.lock();
        Ok(connection.query_row("SELECT count(*) FROM users", [], |row| row.get(0))?)
    }

    /// A public key is matched against the allowlist, redeeming the invite presented
    /// with it if the key is new. The roster comes from that same transaction, so it
    /// cannot shift between admitting and answering.
    pub fn admit(&self, pubkey: &[u8; 32], invite_code: Option<&str>) -> Result<Admission> {
        let mut connection = self.lock();
        let transaction = connection.transaction()?;

        let existing = transaction
            .query_row(
                "SELECT id, pubkey, name, avatar_hash, role FROM users WHERE pubkey = ?1",
                params![pubkey.as_slice()],
                user_from_row,
            )
            .optional()?;

        let (user, enrolled) = match existing {
            Some(user) => (user, false),
            None => {
                let Some(code) = invite_code else {
                    return Ok(Admission::NotAllowlisted);
                };
                match redeem(&transaction, code, pubkey)? {
                    Some(user) => (user, true),
                    None => return Ok(Admission::InviteRefused),
                }
            }
        };

        let users = transaction
            .prepare("SELECT id, pubkey, name, avatar_hash, role FROM users ORDER BY id")?
            .query_map([], user_from_row)?
            .collect::<rusqlite::Result<Vec<User>>>()?;
        transaction.commit()?;

        Ok(Admission::Admitted {
            user,
            users,
            enrolled,
        })
    }

    pub fn list_rooms(&self) -> Result<Vec<Room>> {
        let connection = self.lock();
        let rooms = connection
            .prepare("SELECT id, name, epoch FROM rooms ORDER BY id")?
            .query_map([], room_from_row)?
            .collect::<rusqlite::Result<Vec<Room>>>()?;
        Ok(rooms)
    }

    pub fn create_room(&self, name: &str) -> Result<Room> {
        let connection = self.lock();
        Ok(connection.query_row(
            "INSERT INTO rooms (name, epoch) VALUES (?1, 0) RETURNING id, name, epoch",
            params![name],
            room_from_row,
        )?)
    }

    /// False when no room carries that id.
    pub fn rename_room(&self, room: RoomId, name: &str) -> Result<bool> {
        let connection = self.lock();
        let renamed = connection.execute(
            "UPDATE rooms SET name = ?1 WHERE id = ?2",
            params![name, room.0 as i64],
        )?;
        Ok(renamed == 1)
    }

    /// False when no room carries that id.
    pub fn delete_room(&self, room: RoomId) -> Result<bool> {
        let connection = self.lock();
        let deleted =
            connection.execute("DELETE FROM rooms WHERE id = ?1", params![room.0 as i64])?;
        Ok(deleted == 1)
    }

    /// The epoch each room the change touched now carries. `None` when the room
    /// being entered no longer exists, in which case nothing was written.
    pub fn transition(
        &self,
        leaving: Option<RoomId>,
        entering: Option<RoomId>,
    ) -> Result<Option<Transition>> {
        let mut connection = self.lock();
        let transaction = connection.transaction()?;

        let entering_epoch = match entering {
            None => None,
            Some(room) => match bump_epoch(&transaction, room)? {
                None => return Ok(None),
                epoch => epoch,
            },
        };
        let leaving_epoch = match leaving {
            None => None,
            Some(_) if leaving == entering => entering_epoch,
            // A room deleted while it was occupied leaves nothing to bump.
            Some(room) => bump_epoch(&transaction, room)?,
        };
        let tracks = entering
            .map(|_| allocate_tracks(&transaction))
            .transpose()?;
        transaction.commit()?;

        Ok(Some(Transition {
            leaving: leaving_epoch,
            entering: entering_epoch,
            tracks,
        }))
    }

    pub fn create_invite(&self, role: Role, ttl: Duration) -> Result<Invite> {
        let connection = self.lock();
        insert_invite(&connection, role, ttl)
    }

    /// Every unredeemed invite is invalidated and one is minted in their place. This
    /// is meaningful only while the allowlist is empty, where no invite can have come
    /// from a user.
    pub fn replace_unredeemed_invites(&self, role: Role, ttl: Duration) -> Result<Invite> {
        let connection = self.lock();
        connection.execute("DELETE FROM invites WHERE redeemed_by IS NULL", [])?;
        insert_invite(&connection, role, ttl)
    }

    pub fn prune_invites(&self) -> Result<usize> {
        let connection = self.lock();
        let now = unix_seconds();
        Ok(connection.execute(
            "DELETE FROM invites
             WHERE (redeemed_by IS NULL AND expires_at <= ?1)
                OR (redeemed_at IS NOT NULL AND redeemed_at <= ?2)",
            params![now, now - REDEEMED_RETENTION.as_secs() as i64],
        )?)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.connection.lock().expect("database mutex poisoned")
    }
}

fn insert_invite(connection: &Connection, role: Role, ttl: Duration) -> Result<Invite> {
    let expires_at = unix_seconds() + ttl.as_secs() as i64;

    // A hash that is already present belongs to a code nobody can redeem, so this
    // draws again rather than handing out a dead one.
    for _ in 0..8 {
        let code = invite::generate_code();
        let inserted = connection.execute(
            "INSERT OR IGNORE INTO invites (secret_hash, role, expires_at) VALUES (?1, ?2, ?3)",
            params![invite::hash(&code), role.rank(), expires_at],
        )?;
        if inserted == 1 {
            return Ok(Invite {
                code,
                expires_at: expires_at as u64,
            });
        }
    }
    anyhow::bail!("could not draw an unused invite code")
}

fn redeem(transaction: &Transaction<'_>, code: &str, pubkey: &[u8; 32]) -> Result<Option<User>> {
    let hash = invite::hash(code);
    let now = unix_seconds();

    let rank: Option<u8> = transaction
        .query_row(
            "SELECT role FROM invites
             WHERE secret_hash = ?1 AND redeemed_by IS NULL AND expires_at > ?2",
            params![hash, now],
            |row| row.get(0),
        )
        .optional()?;
    let Some(rank) = rank else {
        return Ok(None);
    };
    let role = Role::from_rank(rank).with_context(|| format!("invite carries rank {rank}"))?;

    transaction.execute(
        "INSERT INTO users (pubkey, name, avatar_hash, role, created_at)
         VALUES (?1, '', NULL, ?2, ?3)",
        params![pubkey.as_slice(), rank, now],
    )?;
    let id = transaction.last_insert_rowid();
    transaction.execute(
        "UPDATE invites SET redeemed_by = ?1, redeemed_at = ?2 WHERE secret_hash = ?3",
        params![id, now, hash],
    )?;

    Ok(Some(User {
        id: UserId(id as u64),
        pubkey: *pubkey,
        name: String::new(),
        avatar: None,
        role,
    }))
}

/// One mic id and one share, all fresh. A media id is never reused, so
/// exhaustion is fatal rather than a wrap.
fn allocate_tracks(transaction: &Transaction<'_>) -> Result<Tracks> {
    let allocated: i64 = transaction.query_row(
        "INSERT INTO counters (name, value) VALUES (?1, 3)
         ON CONFLICT(name) DO UPDATE SET value = value + 3
         RETURNING value",
        params![MEDIA_ID_COUNTER],
        |row| row.get(0),
    )?;
    if allocated > u32::MAX as i64 {
        anyhow::bail!("this deployment has run out of media ids");
    }

    let first = (allocated - 3) as u32;
    Ok(Tracks {
        mic: MediaId(first),
        shares: vec![Share {
            video: MediaId(first + 1),
            audio: Some(MediaId(first + 2)),
        }],
    })
}

/// `None` when no room carries that id. Overflow is fatal: the epoch is what a
/// future rekey selects a key by, so it must never repeat.
fn bump_epoch(transaction: &Transaction<'_>, room: RoomId) -> Result<Option<Epoch>> {
    let bumped: Option<i64> = transaction
        .query_row(
            "UPDATE rooms SET epoch = epoch + 1 WHERE id = ?1 RETURNING epoch",
            params![room.0 as i64],
            |row| row.get(0),
        )
        .optional()?;
    match bumped {
        None => Ok(None),
        Some(epoch) if epoch <= u32::MAX as i64 => Ok(Some(Epoch(epoch as u32))),
        Some(epoch) => anyhow::bail!("room {room} has run out of epochs at {epoch}"),
    }
}

fn room_from_row(row: &Row<'_>) -> rusqlite::Result<Room> {
    Ok(Room {
        id: RoomId(row.get::<_, i64>("id")? as u64),
        name: row.get("name")?,
        epoch: Epoch(row.get::<_, i64>("epoch")? as u32),
    })
}

fn user_from_row(row: &Row<'_>) -> rusqlite::Result<User> {
    let rank: u8 = row.get("role")?;
    Ok(User {
        id: UserId(row.get::<_, i64>("id")? as u64),
        pubkey: fixed_blob(row.get("pubkey")?, "pubkey")?,
        name: row.get("name")?,
        avatar: row
            .get::<_, Option<Vec<u8>>>("avatar_hash")?
            .map(|bytes| fixed_blob(bytes, "avatar_hash"))
            .transpose()?,
        role: Role::from_rank(rank)
            .ok_or(rusqlite::Error::IntegralValueOutOfRange(0, rank as i64))?,
    })
}

fn fixed_blob<const N: usize>(bytes: Vec<u8>, column: &str) -> rusqlite::Result<[u8; N]> {
    let found = bytes.len();
    bytes.try_into().map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            Type::Blob,
            format!("{column} is {found} bytes, expected {N}").into(),
        )
    })
}
