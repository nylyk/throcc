use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use rusqlite::types::Type;
use rusqlite::{Connection, OptionalExtension as _, Row, Transaction, params};
use throcc_proto::{Role, User, UserId};

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

    CREATE TABLE IF NOT EXISTS invites (
        secret_hash BLOB    PRIMARY KEY,
        expires_at  INTEGER NOT NULL
    ) STRICT;
";

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

    pub fn create_invite(&self) -> Result<Invite> {
        let connection = self.lock();
        insert_invite(&connection)
    }

    /// Every outstanding invite is invalidated and one is minted in its place. This
    /// is meaningful only while the allowlist is empty, where no invite can have come
    /// from a user.
    pub fn replace_invites(&self) -> Result<Invite> {
        let connection = self.lock();
        connection.execute("DELETE FROM invites", [])?;
        insert_invite(&connection)
    }

    pub fn prune_invites(&self) -> Result<usize> {
        let connection = self.lock();
        Ok(connection.execute(
            "DELETE FROM invites WHERE expires_at <= ?1",
            params![unix_seconds()],
        )?)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.connection.lock().expect("database mutex poisoned")
    }
}

fn insert_invite(connection: &Connection) -> Result<Invite> {
    let expires_at = unix_seconds() + invite::TTL.as_secs() as i64;

    // A hash that is already present belongs to a code nobody can redeem, so this
    // draws again rather than handing out a dead one.
    for _ in 0..8 {
        let code = invite::generate_code();
        let inserted = connection.execute(
            "INSERT OR IGNORE INTO invites (secret_hash, expires_at) VALUES (?1, ?2)",
            params![invite::hash(&code), expires_at],
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

    let spent = transaction.execute(
        "DELETE FROM invites WHERE secret_hash = ?1 AND expires_at > ?2",
        params![hash, now],
    )?;
    if spent == 0 {
        return Ok(None);
    }

    // The allowlist being empty means this is the deployment's first enrollment,
    // and an Admin has to exist for any other role to ever be granted.
    let enrolled_count: i64 =
        transaction.query_row("SELECT COUNT(*) FROM users", [], |row| row.get(0))?;
    let role = if enrolled_count == 0 {
        Role::Admin
    } else {
        Role::User
    };
    let rank = role.rank();

    transaction.execute(
        "INSERT INTO users (pubkey, name, avatar_hash, role, created_at)
         VALUES (?1, '', NULL, ?2, ?3)",
        params![pubkey.as_slice(), rank, now],
    )?;
    let id = transaction.last_insert_rowid();

    Ok(Some(User {
        id: UserId(id as u64),
        pubkey: *pubkey,
        name: String::new(),
        avatar: None,
        role,
    }))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn database() -> (Database, tempfile::TempDir) {
        let directory = tempfile::TempDir::new().unwrap();
        (Database::open(directory.path()).unwrap(), directory)
    }

    #[test]
    fn an_expired_code_does_not_enroll() {
        let (database, _directory) = database();
        let invite = database.create_invite().unwrap();
        database
            .lock()
            .execute(
                "UPDATE invites SET expires_at = ?1",
                params![unix_seconds()],
            )
            .unwrap();

        assert!(matches!(
            database.admit(&[9u8; 32], Some(&invite.code)).unwrap(),
            Admission::InviteRefused
        ));
        assert_eq!(database.user_count().unwrap(), 0);
    }
}
