use std::path::Path;

use anyhow::{Context, Result, bail};
use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior, params,
};
use throcc_proto::ids::{Epoch, Hash, MediaId, Role, RoomId, UserId};
use throcc_proto::messages::{PublicKey, ServerProfile, Signature, SignedEncryptionKey, User};

use crate::files;

const SCHEMA_V1: &str = "
CREATE TABLE users (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    pubkey BLOB NOT NULL UNIQUE,
    encryption_key BLOB,
    encryption_key_sig BLOB,
    encryption_key_not_after INTEGER,
    name TEXT NOT NULL,
    avatar_hash BLOB,
    role INTEGER NOT NULL,
    invited_by INTEGER NULL,
    created_at INTEGER NOT NULL,
    last_seen INTEGER NOT NULL
);
CREATE TABLE rooms (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    epoch INTEGER NOT NULL,
    password_hash TEXT
);
CREATE TABLE invites (
    code_id TEXT PRIMARY KEY,
    secret TEXT NOT NULL,
    minted_by INTEGER NULL,
    expires_at INTEGER NOT NULL,
    failures INTEGER NOT NULL
);
CREATE TABLE blobs (hash BLOB PRIMARY KEY, bytes BLOB NOT NULL, uploaded_at INTEGER NOT NULL);
CREATE TABLE counters (name TEXT PRIMARY KEY, value INTEGER NOT NULL);
CREATE TABLE server_profile (id INTEGER PRIMARY KEY CHECK (id = 0), name TEXT, avatar_hash BLOB);
";

type Migration = fn(&Transaction) -> rusqlite::Result<()>;

/// Migration `i` moves the schema from version `i` to `i + 1`, kept in `PRAGMA user_version`.
/// Version 1 is the schema throcc 0.1 shipped; changing the schema means adding a migration.
const MIGRATIONS: [Migration; 2] = [to_v1, to_v2];

/// Creates the first schema in an empty database.
fn to_v1(tx: &Transaction) -> rusqlite::Result<()> {
    tx.execute_batch(SCHEMA_V1)
}

/// Keeps each user's signature over their name and picture.
fn to_v2(tx: &Transaction) -> rusqlite::Result<()> {
    tx.execute_batch("ALTER TABLE users ADD COLUMN profile_sig BLOB")
}

const SCHEMA_VERSION: i64 = MIGRATIONS.len() as i64;

fn schema_version(conn: &Connection) -> rusqlite::Result<i64> {
    conn.pragma_query_value(None, "user_version", |r| r.get(0))
}

fn migrate(conn: &mut Connection) -> Result<()> {
    let version = schema_version(conn)?;
    if version > SCHEMA_VERSION {
        bail!(
            "the database has schema version {version}, newer than this server's {SCHEMA_VERSION}"
        );
    }
    for (from, migration) in MIGRATIONS.iter().enumerate().skip(version as usize) {
        let to = from as i64 + 1;
        let tx = conn.transaction()?;
        migration(&tx).with_context(|| format!("migrating the database to schema version {to}"))?;
        tx.pragma_update(None, "user_version", to)?;
        tx.commit()?;
    }
    Ok(())
}

const NEXT_MEDIA_ID: &str = "next_media_id";

pub struct RoomRow {
    pub id: RoomId,
    pub name: String,
    pub epoch: Epoch,
    pub password_hash: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UserRow {
    pub id: UserId,
    pub pubkey: PublicKey,
    pub encryption_key: Option<SignedEncryptionKey>,
    pub name: String,
    pub avatar: Option<Hash>,
    /// The identity key's signature over `name` and `avatar`, once the user has given one.
    pub profile_sig: Option<Signature>,
    pub role: Role,
    pub invited_by: Option<UserId>,
    pub created_at: u64,
}

impl UserRow {
    pub fn to_user(&self) -> User {
        User {
            id: self.id,
            name: self.name.clone(),
            role: self.role,
            invited_by: self.invited_by,
            identity_key: self.pubkey,
            created_at: self.created_at,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InviteRow {
    pub code_id: String,
    pub secret: String,
    pub minted_by: Option<UserId>,
    pub expires_at: u64,
    pub failures: u32,
}

/// The user an enrollment signed in, and what it did to them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Enrolled {
    Added(UserRow),
    /// An existing user whom an admin invite made an admin.
    Promoted(UserRow),
    /// An existing user, left as they were.
    Returning(UserRow),
}

impl Enrolled {
    pub fn into_row(self) -> UserRow {
        match self {
            Self::Added(row) | Self::Promoted(row) | Self::Returning(row) => row,
        }
    }
}

pub struct Database {
    conn: Connection,
}

impl Database {
    /// Opens the database at `path`, creating it at mode 0600. SQLite gives its journal
    /// files the database's mode. Files another user owns are refused.
    pub fn open(path: &Path) -> Result<Self> {
        check_private_files(path)?;
        files::create_private_file(path)?;
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        let mut db = Self::init(conn)?;
        migrate(&mut db.conn)?;
        Ok(db)
    }

    /// Opens the database of a server that may be running, without creating or migrating
    /// it, so a running server never sees its schema change underneath it. The schema
    /// must be the one this binary uses.
    pub fn open_existing(path: &Path) -> Result<Self> {
        check_private_files(path)?;
        let flags = OpenFlags::default() - OpenFlags::SQLITE_OPEN_CREATE;
        let conn = Connection::open_with_flags(path, flags)
            .with_context(|| format!("opening {}", path.display()))?;
        let db = Self::init(conn)?;
        let version = schema_version(&db.conn)?;
        if version != SCHEMA_VERSION {
            bail!(
                "{} has schema version {version}, but this throcc-server uses {SCHEMA_VERSION}; \
                 start this version of the server on it first",
                path.display()
            );
        }
        Ok(db)
    }

    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self> {
        let mut db = Self::init(Connection::open_in_memory()?)?;
        migrate(&mut db.conn)?;
        Ok(db)
    }

    /// rusqlite already makes a write wait up to 5 s for another connection's, such as
    /// `throcc-server admin-invite` minting next to a running server.
    fn init(conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        // A commit is on disk before it returns, so a power loss never rolls back a room's
        // epoch or hands out a media id again.
        conn.pragma_update(None, "synchronous", "FULL")?;
        Ok(Self { conn })
    }

    /// Starts a write transaction holding the write lock from its first statement, so
    /// another connection's commit can't invalidate what it read.
    fn write(&mut self) -> rusqlite::Result<Transaction<'_>> {
        self.conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
    }

    /// Reserves `count` consecutive media ids, or returns `None` once they are exhausted.
    /// Ids are never handed out twice, across restarts and power loss alike.
    pub fn allocate_media_ids(&mut self, count: u32) -> Result<Option<MediaId>> {
        let tx = self.write()?;
        let first = next_media_id(&tx)?;
        let next = first + i64::from(count);
        if next - 1 > i64::from(u32::MAX) {
            return Ok(None);
        }
        tx.execute(
            "INSERT INTO counters (name, value) VALUES (?1, ?2)
             ON CONFLICT(name) DO UPDATE SET value = excluded.value",
            params![NEXT_MEDIA_ID, next],
        )?;
        tx.commit()?;
        Ok(Some(MediaId(first as u32)))
    }

    pub fn admin_count(&self) -> Result<u64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM users WHERE role = ?1",
            [Role::Admin.rank()],
            |r| r.get::<_, i64>(0),
        )? as u64)
    }

    pub fn user_count(&self) -> Result<usize> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))?;
        Ok(count as usize)
    }

    pub fn user_by_pubkey(&self, pubkey: &PublicKey) -> Result<Option<UserRow>> {
        Ok(self
            .conn
            .query_row(
                &format!("{USER_SELECT} WHERE pubkey = ?1"),
                [pubkey.as_slice()],
                user_row,
            )
            .optional()?)
    }

    pub fn user(&self, id: UserId) -> Result<Option<UserRow>> {
        Ok(self
            .conn
            .query_row(
                &format!("{USER_SELECT} WHERE id = ?1"),
                [id.0 as i64],
                user_row,
            )
            .optional()?)
    }

    pub fn users(&self) -> Result<Vec<UserRow>> {
        let mut stmt = self.conn.prepare(&format!("{USER_SELECT} ORDER BY id"))?;
        let rows = stmt.query_map([], user_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Consumes the invite and adds its user as `name`; `None` if the invite is gone or expired.
    /// A minterless invite makes or promotes an admin; others leave a known key's user as is.
    pub fn enroll(
        &mut self,
        pubkey: &PublicKey,
        code_id: &str,
        name: &str,
        now: u64,
    ) -> Result<Option<Enrolled>> {
        let tx = self.write()?;
        let minted_by: Option<Option<i64>> = tx
            .query_row(
                "SELECT minted_by FROM invites WHERE code_id = ?1 AND expires_at > ?2",
                params![code_id, now as i64],
                |r| r.get(0),
            )
            .optional()?;
        let Some(minted_by) = minted_by else {
            return Ok(None);
        };
        if tx.execute("DELETE FROM invites WHERE code_id = ?1", [code_id])? != 1 {
            return Ok(None);
        }
        let role = if minted_by.is_none() {
            Role::Admin
        } else {
            Role::User
        };
        let existing: Option<i64> = tx
            .query_row(
                "SELECT id FROM users WHERE pubkey = ?1",
                [pubkey.as_slice()],
                |r| r.get(0),
            )
            .optional()?;
        let (id, enrolled): (i64, fn(UserRow) -> Enrolled) = match existing {
            Some(id) => {
                let promoted = tx.execute(
                    "UPDATE users SET role = ?1 WHERE id = ?2 AND role < ?1",
                    params![role.rank(), id],
                )? == 1;
                let enrolled = if promoted {
                    Enrolled::Promoted
                } else {
                    Enrolled::Returning
                };
                (id, enrolled)
            }
            None => {
                tx.execute(
                    "INSERT INTO users (pubkey, name, role, invited_by, created_at, last_seen) VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
                    params![pubkey.as_slice(), name, role.rank(), minted_by, now as i64],
                )?;
                (tx.last_insert_rowid(), Enrolled::Added)
            }
        };
        tx.commit()?;
        Ok(self.user(UserId(id as u64))?.map(enrolled))
    }

    pub fn set_encryption_key(
        &self,
        user: UserId,
        encryption_key: &SignedEncryptionKey,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE users SET encryption_key = ?1, encryption_key_sig = ?2, encryption_key_not_after = ?3 WHERE id = ?4",
            params![
                encryption_key.key.as_slice(),
                encryption_key.sig.as_slice(),
                encryption_key.not_after as i64,
                user.0 as i64
            ],
        )?;
        Ok(())
    }

    pub fn set_profile(
        &self,
        user: UserId,
        name: &str,
        avatar: Option<Hash>,
        sig: &Signature,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE users SET name = ?1, avatar_hash = ?2, profile_sig = ?3 WHERE id = ?4",
            params![name, hash_param(&avatar), sig.as_slice(), user.0 as i64],
        )?;
        Ok(())
    }

    pub fn set_role(&self, user: UserId, role: Role) -> Result<()> {
        self.conn.execute(
            "UPDATE users SET role = ?1 WHERE id = ?2",
            params![role.rank(), user.0 as i64],
        )?;
        Ok(())
    }

    /// Notes that `user` signed in at `now`.
    pub fn mark_seen(&self, user: UserId, now: u64) -> Result<()> {
        self.conn.execute(
            "UPDATE users SET last_seen = ?1 WHERE id = ?2",
            params![now as i64, user.0 as i64],
        )?;
        Ok(())
    }

    /// Everyone but admins last seen before `before`.
    pub fn unseen_since(&self, before: u64) -> Result<Vec<UserId>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id FROM users WHERE last_seen < ?1 AND role != ?2 ORDER BY id")?;
        let ids = stmt.query_map(params![before as i64, Role::Admin.rank()], |r| {
            Ok(UserId(r.get::<_, i64>(0)? as u64))
        })?;
        Ok(ids.collect::<rusqlite::Result<_>>()?)
    }

    /// `user` and everyone they invited, transitively.
    pub fn invite_tree(&self, user: UserId) -> Result<Vec<UserId>> {
        let mut stmt = self.conn.prepare(
            "WITH RECURSIVE tree(id) AS (
                 SELECT ?1 UNION SELECT users.id FROM users JOIN tree ON users.invited_by = tree.id
             ) SELECT id FROM tree",
        )?;
        let ids = stmt.query_map([user.0 as i64], |r| r.get::<_, i64>(0))?;
        Ok(ids
            .map(|id| id.map(|id| UserId(id as u64)))
            .collect::<rusqlite::Result<_>>()?)
    }

    /// Removes these users in one transaction. Whoever one of them invited passes to that
    /// one's own inviter, so they stay in an invite tree a cascade reaches and still count
    /// against an invite allowance: leaving frees neither.
    pub fn remove_users(&mut self, users: &[UserId]) -> Result<()> {
        let tx = self.write()?;
        for user in users {
            remove_one(&tx, user.0 as i64)?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn rooms(&self) -> Result<Vec<RoomRow>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, name, epoch, password_hash FROM rooms ORDER BY id")?;
        let rows = stmt.query_map([], |r| {
            Ok(RoomRow {
                id: RoomId(r.get::<_, i64>(0)? as u64),
                name: r.get(1)?,
                epoch: Epoch(r.get::<_, i64>(2)? as u32),
                password_hash: r.get(3)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn create_room(&self, name: &str, password_hash: Option<&str>) -> Result<RoomId> {
        self.conn.execute(
            "INSERT INTO rooms (name, epoch, password_hash) VALUES (?1, 0, ?2)",
            params![name, password_hash],
        )?;
        Ok(RoomId(self.conn.last_insert_rowid() as u64))
    }

    pub fn rename_room(&self, room: RoomId, name: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE rooms SET name = ?1 WHERE id = ?2",
            params![name, room.0 as i64],
        )?;
        Ok(())
    }

    pub fn set_room_password_hash(&self, room: RoomId, hash: Option<&str>) -> Result<()> {
        self.conn.execute(
            "UPDATE rooms SET password_hash = ?1 WHERE id = ?2",
            params![hash, room.0 as i64],
        )?;
        Ok(())
    }

    pub fn delete_room(&self, room: RoomId) -> Result<()> {
        self.conn
            .execute("DELETE FROM rooms WHERE id = ?1", [room.0 as i64])?;
        Ok(())
    }

    /// Sets the epochs of several rooms, all or none.
    pub fn set_epochs(&mut self, epochs: &[(RoomId, Epoch)]) -> Result<()> {
        let tx = self.write()?;
        for (room, epoch) in epochs {
            tx.execute(
                "UPDATE rooms SET epoch = ?1 WHERE id = ?2",
                params![i64::from(epoch.0), room.0 as i64],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn insert_invite(&self, invite: &InviteRow) -> Result<bool> {
        Ok(self.conn.execute(
            "INSERT OR IGNORE INTO invites (code_id, secret, minted_by, expires_at, failures) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                invite.code_id,
                invite.secret,
                invite.minted_by.map(|u| u.0 as i64),
                invite.expires_at as i64,
                invite.failures
            ],
        )? == 1)
    }

    pub fn invite(&self, code_id: &str) -> Result<Option<InviteRow>> {
        Ok(self
            .conn
            .query_row(
                "SELECT code_id, secret, minted_by, expires_at, failures FROM invites WHERE code_id = ?1",
                [code_id],
                |r| {
                    Ok(InviteRow {
                        code_id: r.get(0)?,
                        secret: r.get(1)?,
                        minted_by: r.get::<_, Option<i64>>(2)?.map(|id| UserId(id as u64)),
                        expires_at: r.get::<_, i64>(3)? as u64,
                        failures: r.get(4)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn consume_invite_try(&self, code_id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE invites SET failures = failures + 1 WHERE code_id = ?1",
            [code_id],
        )?;
        Ok(())
    }

    pub fn return_invite_try(&self, code_id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE invites SET failures = failures - 1 WHERE code_id = ?1 AND failures > 0",
            [code_id],
        )?;
        Ok(())
    }

    pub fn delete_invite(&self, code_id: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM invites WHERE code_id = ?1", [code_id])?;
        Ok(())
    }

    /// Deletes invites that have used up their `tries`. Only safe while no run is in flight,
    /// since the last run of an invite is what burns it.
    pub fn delete_spent_invites(&self, tries: u32) -> Result<()> {
        self.conn
            .execute("DELETE FROM invites WHERE failures >= ?1", [tries])?;
        Ok(())
    }

    pub fn purge_expired_invites(&self, now: u64) -> Result<()> {
        self.conn
            .execute("DELETE FROM invites WHERE expires_at <= ?1", [now as i64])?;
        Ok(())
    }

    /// Live invites minted by `user` plus users they enrolled since `since`.
    pub fn invites_used(&self, user: UserId, now: u64, since: u64) -> Result<u32> {
        let live: u32 = self.conn.query_row(
            "SELECT COUNT(*) FROM invites WHERE minted_by = ?1 AND expires_at > ?2",
            params![user.0 as i64, now as i64],
            |r| r.get(0),
        )?;
        let enrolled: u32 = self.conn.query_row(
            "SELECT COUNT(*) FROM users WHERE invited_by = ?1 AND created_at > ?2",
            params![user.0 as i64, since as i64],
            |r| r.get(0),
        )?;
        Ok(live + enrolled)
    }

    pub fn put_blob(&self, hash: &Hash, bytes: &[u8], now: u64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO blobs (hash, bytes, uploaded_at) VALUES (?1, ?2, ?3)
             ON CONFLICT (hash) DO UPDATE SET uploaded_at = excluded.uploaded_at",
            params![hash.0.as_slice(), bytes, now as i64],
        )?;
        Ok(())
    }

    /// Deletes blobs uploaded before `uploaded_before` that no user or server profile uses.
    pub fn delete_unused_blobs(&self, uploaded_before: u64) -> Result<usize> {
        Ok(self.conn.execute(
            "DELETE FROM blobs WHERE uploaded_at < ?1
             AND hash NOT IN (SELECT avatar_hash FROM users WHERE avatar_hash IS NOT NULL)
             AND hash NOT IN (SELECT avatar_hash FROM server_profile WHERE avatar_hash IS NOT NULL)",
            [uploaded_before as i64],
        )?)
    }

    pub fn server_profile(&self) -> Result<ServerProfile> {
        let row = self
            .conn
            .query_row(
                "SELECT name, avatar_hash FROM server_profile WHERE id = 0",
                [],
                |r| {
                    Ok((
                        r.get::<_, Option<String>>(0)?,
                        r.get::<_, Option<Vec<u8>>>(1)?,
                    ))
                },
            )
            .optional()?;
        Ok(
            row.map_or_else(ServerProfile::default, |(name, avatar)| ServerProfile {
                name,
                avatar: hash_column(avatar),
            }),
        )
    }

    pub fn set_server_name(&self, name: Option<&str>) -> Result<()> {
        self.conn.execute(
            "INSERT INTO server_profile (id, name) VALUES (0, ?1)
             ON CONFLICT (id) DO UPDATE SET name = excluded.name",
            params![name],
        )?;
        Ok(())
    }

    pub fn set_server_avatar(&self, avatar: Option<Hash>) -> Result<()> {
        self.conn.execute(
            "INSERT INTO server_profile (id, avatar_hash) VALUES (0, ?1)
             ON CONFLICT (id) DO UPDATE SET avatar_hash = excluded.avatar_hash",
            params![hash_param(&avatar)],
        )?;
        Ok(())
    }

    pub fn has_blob(&self, hash: &Hash) -> Result<bool> {
        Ok(self
            .conn
            .query_row(
                "SELECT 1 FROM blobs WHERE hash = ?1",
                [hash.0.as_slice()],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    pub fn blob(&self, hash: &Hash) -> Result<Option<Vec<u8>>> {
        Ok(self
            .conn
            .query_row(
                "SELECT bytes FROM blobs WHERE hash = ?1",
                [hash.0.as_slice()],
                |r| r.get(0),
            )
            .optional()?)
    }
}

fn check_private_files(database: &Path) -> Result<()> {
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let mut file = database.as_os_str().to_owned();
        file.push(suffix);
        files::check_private_file(Path::new(&file))?;
    }
    Ok(())
}

const USER_SELECT: &str = "SELECT id, pubkey, encryption_key, encryption_key_sig, encryption_key_not_after, name, avatar_hash, role, invited_by, created_at, profile_sig FROM users";

fn user_row(r: &rusqlite::Row) -> rusqlite::Result<UserRow> {
    let encryption_key: Option<Vec<u8>> = r.get(2)?;
    let encryption_key_sig: Option<Vec<u8>> = r.get(3)?;
    let not_after: Option<i64> = r.get(4)?;
    let encryption_key = match (encryption_key, encryption_key_sig, not_after) {
        (Some(key), Some(sig), Some(not_after)) => match (key.try_into(), sig.try_into()) {
            (Ok(key), Ok(sig)) => Some(SignedEncryptionKey {
                key,
                not_after: not_after as u64,
                sig,
            }),
            _ => None,
        },
        _ => None,
    };
    Ok(UserRow {
        id: UserId(r.get::<_, i64>(0)? as u64),
        pubkey: fixed(r.get::<_, Vec<u8>>(1)?)?,
        encryption_key,
        name: r.get(5)?,
        avatar: hash_column(r.get(6)?),
        profile_sig: r
            .get::<_, Option<Vec<u8>>>(10)?
            .and_then(|sig| sig.try_into().ok()),
        role: Role::from_rank(r.get(7)?).unwrap_or(Role::User),
        invited_by: r.get::<_, Option<i64>>(8)?.map(|id| UserId(id as u64)),
        created_at: r.get::<_, i64>(9)? as u64,
    })
}

fn next_media_id(conn: &Connection) -> rusqlite::Result<i64> {
    Ok(conn
        .query_row(
            "SELECT value FROM counters WHERE name = ?1",
            [NEXT_MEDIA_ID],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(1))
}

fn hash_param(hash: &Option<Hash>) -> Option<&[u8]> {
    hash.as_ref().map(|h| h.0.as_slice())
}

fn hash_column(bytes: Option<Vec<u8>>) -> Option<Hash> {
    bytes.and_then(|b| b.try_into().ok()).map(Hash)
}

fn fixed<const N: usize>(bytes: Vec<u8>) -> rusqlite::Result<[u8; N]> {
    bytes.try_into().map_err(|_| rusqlite::Error::InvalidQuery)
}

fn remove_one(tx: &Transaction, id: i64) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE users SET invited_by = (SELECT invited_by FROM users WHERE id = ?1)
         WHERE invited_by = ?1",
        [id],
    )?;
    tx.execute("DELETE FROM invites WHERE minted_by = ?1", [id])?;
    tx.execute("DELETE FROM users WHERE id = ?1", [id])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invite(code: &str, minted_by: Option<UserId>, expires_at: u64, failures: u32) -> InviteRow {
        InviteRow {
            code_id: code.into(),
            secret: "S".into(),
            minted_by,
            expires_at,
            failures,
        }
    }

    fn enroll(db: &mut Database, key: u8, by: Option<UserId>) -> UserId {
        let code = format!("C{key:02}");
        db.insert_invite(&invite(&code, by, 1000, 0)).unwrap();
        db.enroll(&[key; 32], &code, "Ilse", 100)
            .unwrap()
            .unwrap()
            .into_row()
            .id
    }

    #[test]
    fn enrolling_counts_as_seen_signing_in_moves_it_and_admins_are_never_unseen() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = Database::open(&dir.path().join("db")).unwrap();
        let admin = enroll(&mut db, 1, None);
        let mara = enroll(&mut db, 2, Some(admin));
        let teo = enroll(&mut db, 3, Some(admin));
        assert_eq!(db.unseen_since(100).unwrap(), []);
        assert_eq!(db.unseen_since(101).unwrap(), [mara, teo]);
        db.mark_seen(teo, 500).unwrap();
        assert_eq!(db.unseen_since(501).unwrap(), [mara, teo]);
        assert_eq!(db.unseen_since(500).unwrap(), [mara]);
        db.set_role(mara, Role::Admin).unwrap();
        assert_eq!(db.unseen_since(1_000).unwrap(), [teo]);
    }

    #[test]
    fn the_server_profile_survives_a_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db");
        let db = Database::open(&path).unwrap();
        assert_eq!(db.server_profile().unwrap(), ServerProfile::default());
        let profile = ServerProfile {
            name: Some("Moss".into()),
            avatar: Some(Hash([7; 32])),
        };
        db.set_server_avatar(profile.avatar).unwrap();
        db.set_server_name(profile.name.as_deref()).unwrap();
        drop(db);
        assert_eq!(
            Database::open(&path).unwrap().server_profile().unwrap(),
            profile
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_database_and_its_journal_are_private_even_in_an_open_directory() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = dir.path().join("db");
        let db = Database::open(&path).unwrap();
        db.set_server_name(Some("Moss")).unwrap();
        let mode = |suffix: &str| {
            let file = dir.path().join(format!("db{suffix}"));
            std::fs::metadata(file).unwrap().permissions().mode() & 0o777
        };
        assert_eq!(
            (mode(""), mode("-wal"), mode("-shm")),
            (0o600, 0o600, 0o600)
        );
        drop(db);

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        Database::open(&path).unwrap();
        assert_eq!(mode(""), 0o600);
    }

    #[test]
    fn media_ids_never_repeat_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db");
        let first = Database::open(&path)
            .unwrap()
            .allocate_media_ids(3)
            .unwrap()
            .unwrap();
        let second = Database::open(&path)
            .unwrap()
            .allocate_media_ids(3)
            .unwrap()
            .unwrap();
        assert_eq!(second.0, first.0 + 3);
    }

    #[test]
    fn exhausted_media_ids_refuse_allocation_but_not_startup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db");
        let mut db = Database::open(&path).unwrap();
        db.conn
            .execute(
                "INSERT INTO counters (name, value) VALUES (?1, ?2)",
                params![NEXT_MEDIA_ID, i64::from(u32::MAX) - 1],
            )
            .unwrap();
        assert_eq!(db.allocate_media_ids(3).unwrap(), None);
        assert_eq!(
            db.allocate_media_ids(2).unwrap(),
            Some(MediaId(u32::MAX - 1))
        );
        drop(db);
        let mut db = Database::open(&path).unwrap();
        assert_eq!(db.allocate_media_ids(1).unwrap(), None);
    }

    #[test]
    fn room_ids_are_not_reused_after_deleting_the_last_room() {
        let db = Database::open_in_memory().unwrap();
        db.create_room("a", None).unwrap();
        let last = db.create_room("b", None).unwrap();
        db.delete_room(last).unwrap();
        assert!(db.create_room("c", None).unwrap() > last);
    }

    #[test]
    fn unused_blobs_are_deleted_after_their_grace() {
        let mut db = Database::open_in_memory().unwrap();
        let user = enroll(&mut db, 1, None);
        let (used, server, unused, fresh) =
            (Hash([1; 32]), Hash([2; 32]), Hash([3; 32]), Hash([4; 32]));
        for hash in [used, server, unused] {
            db.put_blob(&hash, b"png", 100).unwrap();
        }
        db.put_blob(&fresh, b"png", 200).unwrap();
        db.set_profile(user, "a", Some(used), &[0; 64]).unwrap();
        db.set_server_avatar(Some(server)).unwrap();
        assert_eq!(db.delete_unused_blobs(150).unwrap(), 1);
        assert!(db.blob(&unused).unwrap().is_none());
        for kept in [used, server, fresh] {
            assert!(db.blob(&kept).unwrap().is_some());
        }
    }

    #[test]
    fn opening_an_existing_database_neither_creates_nor_migrates_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db");
        assert!(Database::open_existing(&path).is_err());
        assert!(!path.exists());
        drop(Database::open(&path).unwrap());
        Connection::open(&path)
            .unwrap()
            .pragma_update(None, "user_version", 0)
            .unwrap();
        assert!(Database::open_existing(&path).is_err());
    }

    #[test]
    fn a_second_connection_writes_next_to_the_first() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db");
        let mut server = Database::open(&path).unwrap();
        let side = Database::open_existing(&path).unwrap();
        assert!(side.insert_invite(&invite("ADM", None, 1000, 0)).unwrap());
        let user = server
            .enroll(&[1; 32], "ADM", "Ilse", 100)
            .unwrap()
            .unwrap();
        assert!(matches!(user, Enrolled::Added(row) if row.role == Role::Admin));
        assert!(server.allocate_media_ids(1).unwrap().is_some());
    }

    #[test]
    fn a_version_1_database_keeps_its_users_and_gains_profile_signatures() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db");
        drop(Database::open(&path).unwrap());
        let v1 = Connection::open(&path).unwrap();
        v1.execute_batch("ALTER TABLE users DROP COLUMN profile_sig; PRAGMA user_version = 1")
            .unwrap();
        v1.execute(
            "INSERT INTO users (pubkey, name, role, created_at, last_seen) VALUES (?1, 'Ilse', 0, 1, 1)",
            [[1u8; 32].as_slice()],
        )
        .unwrap();
        drop(v1);
        let db = Database::open(&path).unwrap();
        let user = db.user_by_pubkey(&[1; 32]).unwrap().unwrap();
        assert_eq!((user.name.as_str(), user.profile_sig), ("Ilse", None));
        db.set_profile(user.id, "Ilse", None, &[7; 64]).unwrap();
        assert_eq!(
            db.user(user.id).unwrap().unwrap().profile_sig,
            Some([7; 64])
        );
    }

    #[test]
    fn a_database_from_a_newer_server_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db");
        drop(Database::open(&path).unwrap());
        Connection::open(&path)
            .unwrap()
            .pragma_update(None, "user_version", 99)
            .unwrap();
        assert!(Database::open(&path).is_err());
    }

    #[test]
    fn cascade_removes_the_invite_tree() {
        let mut db = Database::open_in_memory().unwrap();
        let root = enroll(&mut db, 1, None);
        let child = enroll(&mut db, 2, Some(root));
        let grandchild = enroll(&mut db, 3, Some(child));
        let other = enroll(&mut db, 4, Some(root));

        let mut tree = db.invite_tree(child).unwrap();
        tree.sort();
        assert_eq!(tree, vec![child, grandchild]);
        db.remove_users(&tree).unwrap();
        assert!(db.user(grandchild).unwrap().is_none());
        assert!(db.user(other).unwrap().is_some());

        db.remove_users(&[root]).unwrap();
        assert_eq!(db.user(other).unwrap().unwrap().invited_by, None);
    }

    #[test]
    fn whoever_a_removed_user_invited_passes_to_that_users_inviter() {
        let mut db = Database::open_in_memory().unwrap();
        let root = enroll(&mut db, 1, None);
        let between = enroll(&mut db, 2, Some(root));
        let invited: Vec<UserId> = (3..6)
            .map(|key| enroll(&mut db, key, Some(between)))
            .collect();
        assert_eq!(db.invites_used(root, 100, 0).unwrap(), 1);

        db.remove_users(&[between]).unwrap();
        for user in &invited {
            assert_eq!(db.user(*user).unwrap().unwrap().invited_by, Some(root));
        }
        assert_eq!(db.invites_used(root, 100, 0).unwrap(), 3);
        let mut tree = db.invite_tree(root).unwrap();
        tree.sort();
        assert_eq!(tree, [vec![root], invited].concat());
    }

    #[test]
    fn re_enrolling_an_existing_key_keeps_the_user() {
        let mut db = Database::open_in_memory().unwrap();
        let root = enroll(&mut db, 1, None);
        assert_eq!(db.user(root).unwrap().unwrap().role, Role::Admin);
        let first = enroll(&mut db, 2, Some(root));
        assert_eq!(db.user(first).unwrap().unwrap().name, "Ilse");
        db.set_role(first, Role::Manager).unwrap();
        db.set_profile(first, "Moss", None, &[0; 64]).unwrap();
        let again = enroll(&mut db, 2, Some(root));
        assert_eq!(again, first);
        let row = db.user(first).unwrap().unwrap();
        assert_eq!((row.role, row.name.as_str()), (Role::Manager, "Moss"));
    }

    #[test]
    fn an_admin_invite_promotes_an_existing_key() {
        let mut db = Database::open_in_memory().unwrap();
        let root = enroll(&mut db, 1, None);
        let member = enroll(&mut db, 2, Some(root));
        let mut redeem = |code: &str, by: Option<UserId>| {
            db.insert_invite(&invite(code, by, 1000, 0)).unwrap();
            db.enroll(&[2; 32], code, "Ilse", 100).unwrap().unwrap()
        };

        let returning = redeem("USR", Some(root));
        assert!(matches!(&returning, Enrolled::Returning(row) if row.role == Role::User));

        let promoted = redeem("ADM", None);
        assert!(
            matches!(&promoted, Enrolled::Promoted(row) if row.id == member && row.role == Role::Admin)
        );

        assert!(matches!(redeem("AD2", None), Enrolled::Returning(_)));
    }

    #[test]
    fn used_or_expired_invites_do_not_enroll() {
        let mut db = Database::open_in_memory().unwrap();
        enroll(&mut db, 1, None);
        assert_eq!(db.enroll(&[9; 32], "C01", "Ilse", 100).unwrap(), None);
        db.insert_invite(&invite("OLD", None, 50, 0)).unwrap();
        assert_eq!(db.enroll(&[2; 32], "OLD", "Ilse", 100).unwrap(), None);
    }

    #[test]
    fn spent_invites_are_deleted() {
        let db = Database::open_in_memory().unwrap();
        for (code_id, failures) in [("ONE", 2), ("TWO", 3)] {
            db.insert_invite(&invite(code_id, None, 1000, failures))
                .unwrap();
        }
        db.delete_spent_invites(3).unwrap();
        assert!(db.invite("ONE").unwrap().is_some());
        assert!(db.invite("TWO").unwrap().is_none());
    }

    #[test]
    fn epochs_are_set_together() {
        let mut db = Database::open_in_memory().unwrap();
        let (a, b) = (
            db.create_room("a", None).unwrap(),
            db.create_room("b", None).unwrap(),
        );
        db.set_epochs(&[(a, Epoch(4)), (b, Epoch(9))]).unwrap();
        let epochs: Vec<_> = db.rooms().unwrap().iter().map(|r| r.epoch).collect();
        assert_eq!(epochs, vec![Epoch(4), Epoch(9)]);
    }
}
