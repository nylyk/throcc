use std::path::Path;

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use throcc_proto::ids::{Epoch, Hash, MediaId, Role, RoomId, UserId};
use throcc_proto::messages::{PublicKey, Room, ServerProfile, SignedEncKey, User};

const USERS_COLUMNS: &str = "(
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    pubkey BLOB NOT NULL UNIQUE,
    enc_key BLOB,
    enc_key_sig BLOB,
    enc_key_not_after INTEGER,
    name TEXT NOT NULL,
    avatar_hash BLOB,
    role INTEGER NOT NULL,
    invited_by INTEGER NULL,
    created_at INTEGER NOT NULL
)";
const ROOMS_COLUMNS: &str =
    "(id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, epoch INTEGER NOT NULL)";
const OTHER_TABLES_V1: &str = "
CREATE TABLE IF NOT EXISTS rooms (id INTEGER PRIMARY KEY, name TEXT NOT NULL, epoch INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS invites (
    code_id TEXT PRIMARY KEY,
    secret TEXT NOT NULL,
    minted_by INTEGER NULL,
    expires_at INTEGER NOT NULL,
    failures INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS blobs (hash BLOB PRIMARY KEY, bytes BLOB NOT NULL);
CREATE TABLE IF NOT EXISTS counters (name TEXT PRIMARY KEY, value INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS server_profile (id INTEGER PRIMARY KEY CHECK (id = 0), name TEXT, avatar_hash BLOB);
";

type Migration = fn(&Transaction) -> rusqlite::Result<()>;

/// Migration `i` moves the schema from version `i` to `i + 1`, kept in `PRAGMA user_version`.
const MIGRATIONS: [Migration; 2] = [to_v1, to_v2];

/// Creates the first versioned schema, or brings an unversioned database up to it.
fn to_v1(tx: &Transaction) -> rusqlite::Result<()> {
    tx.execute_batch(&format!(
        "CREATE TABLE IF NOT EXISTS users {USERS_COLUMNS}; {OTHER_TABLES_V1}"
    ))?;
    add_missing_column(tx, "users", "enc_key_not_after", "INTEGER")?;
    add_autoincrement(tx, "users", USERS_COLUMNS)
}

/// Makes room ids AUTOINCREMENT and records when each blob was uploaded.
fn to_v2(tx: &Transaction) -> rusqlite::Result<()> {
    add_autoincrement(tx, "rooms", ROOMS_COLUMNS)?;
    add_missing_column(tx, "blobs", "uploaded_at", "INTEGER NOT NULL DEFAULT 0")
}

fn add_missing_column(
    tx: &Transaction,
    table: &str,
    column: &str,
    definition: &str,
) -> rusqlite::Result<()> {
    if !column_names(tx, table)?.iter().any(|c| c == column) {
        tx.execute_batch(&format!(
            "ALTER TABLE {table} ADD COLUMN {column} {definition}"
        ))?;
    }
    Ok(())
}

/// Rebuilds `table` with `columns`, whose primary key is AUTOINCREMENT, keeping its rows.
fn add_autoincrement(tx: &Transaction, table: &str, columns: &str) -> rusqlite::Result<()> {
    let sql: String = tx.query_row(
        "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [table],
        |r| r.get(0),
    )?;
    if sql.contains("AUTOINCREMENT") {
        return Ok(());
    }
    let kept = column_names(tx, table)?.join(", ");
    tx.execute_batch(&format!(
        "CREATE TABLE {table}_new {columns};
         INSERT INTO {table}_new ({kept}) SELECT {kept} FROM {table};
         DROP TABLE {table};
         ALTER TABLE {table}_new RENAME TO {table};"
    ))
}

fn column_names(tx: &Transaction, table: &str) -> rusqlite::Result<Vec<String>> {
    let mut stmt = tx.prepare(&format!("PRAGMA table_info({table})"))?;
    let names = stmt.query_map([], |r| r.get::<_, String>(1))?;
    names.collect()
}

fn migrate(conn: &mut Connection) -> Result<()> {
    let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    let current = MIGRATIONS.len() as i64;
    if version > current {
        bail!("the database has schema version {version}, newer than this server's {current}");
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UserRow {
    pub id: UserId,
    pub pubkey: PublicKey,
    pub enc_key: Option<SignedEncKey>,
    pub name: String,
    pub avatar: Option<Hash>,
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

pub struct Database {
    conn: Connection,
}

impl Database {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(mut conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        migrate(&mut conn)?;
        Ok(Self { conn })
    }

    /// Reserves `count` consecutive media ids, or returns `None` once they are exhausted.
    /// Ids are never handed out twice, even across restarts.
    pub fn allocate_media_ids(&mut self, count: u32) -> Result<Option<MediaId>> {
        let tx = self.conn.transaction()?;
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

    pub fn user_count(&self) -> Result<u64> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM users", [], |r| r.get::<_, i64>(0))? as u64)
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

    /// Consumes the invite and adds its user, in one transaction. Returns `None`, changing
    /// nothing, when the invite is gone or expired. The bootstrap invite enrolls an admin;
    /// an existing key keeps its user and role.
    pub fn enroll(
        &mut self,
        pubkey: &PublicKey,
        code_id: &str,
        now: u64,
    ) -> Result<Option<UserRow>> {
        let tx = self.conn.transaction()?;
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
        let id = match existing {
            Some(id) => id,
            None => {
                tx.execute(
                    "INSERT INTO users (pubkey, name, role, invited_by, created_at) VALUES (?1, '', ?2, ?3, ?4)",
                    params![pubkey.as_slice(), role.rank(), minted_by, now as i64],
                )?;
                let id = tx.last_insert_rowid();
                tx.execute(
                    "UPDATE users SET name = ?1 WHERE id = ?2",
                    params![format!("user{id}"), id],
                )?;
                id
            }
        };
        tx.commit()?;
        self.user(UserId(id as u64))
    }

    pub fn set_enc_key(&self, user: UserId, enc_key: &SignedEncKey) -> Result<()> {
        self.conn.execute(
            "UPDATE users SET enc_key = ?1, enc_key_sig = ?2, enc_key_not_after = ?3 WHERE id = ?4",
            params![
                enc_key.key.as_slice(),
                enc_key.sig.as_slice(),
                enc_key.not_after as i64,
                user.0 as i64
            ],
        )?;
        Ok(())
    }

    pub fn set_profile(&self, user: UserId, name: &str, avatar: Option<Hash>) -> Result<()> {
        self.conn.execute(
            "UPDATE users SET name = ?1, avatar_hash = ?2 WHERE id = ?3",
            params![name, avatar.map(|h| h.0.to_vec()), user.0 as i64],
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

    /// Removes these users in one transaction, clearing `invited_by` on anyone they invited.
    pub fn remove_users(&mut self, users: &[UserId]) -> Result<()> {
        let tx = self.conn.transaction()?;
        for user in users {
            remove_one(&tx, user.0 as i64)?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn rooms(&self) -> Result<Vec<(Room, Epoch)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, name, epoch FROM rooms ORDER BY id")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                Room {
                    id: RoomId(r.get::<_, i64>(0)? as u64),
                    name: r.get(1)?,
                },
                Epoch(r.get::<_, i64>(2)? as u32),
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn create_room(&self, name: &str) -> Result<RoomId> {
        self.conn
            .execute("INSERT INTO rooms (name, epoch) VALUES (?1, 0)", [name])?;
        Ok(RoomId(self.conn.last_insert_rowid() as u64))
    }

    pub fn rename_room(&self, room: RoomId, name: &str) -> Result<bool> {
        Ok(self.conn.execute(
            "UPDATE rooms SET name = ?1 WHERE id = ?2",
            params![name, room.0 as i64],
        )? == 1)
    }

    pub fn delete_room(&self, room: RoomId) -> Result<bool> {
        Ok(self
            .conn
            .execute("DELETE FROM rooms WHERE id = ?1", [room.0 as i64])?
            == 1)
    }

    pub fn set_epoch(&self, room: RoomId, epoch: Epoch) -> Result<()> {
        self.conn.execute(
            "UPDATE rooms SET epoch = ?1 WHERE id = ?2",
            params![i64::from(epoch.0), room.0 as i64],
        )?;
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

    pub fn delete_invite(&self, code_id: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM invites WHERE code_id = ?1", [code_id])?;
        Ok(())
    }

    /// Deletes bootstrap invites, which have no minter.
    pub fn delete_bootstrap_invites(&self) -> Result<()> {
        self.conn
            .execute("DELETE FROM invites WHERE minted_by IS NULL", [])?;
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
                avatar: avatar.and_then(|h| h.try_into().ok()).map(Hash),
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
            params![avatar.map(|h| h.0.to_vec())],
        )?;
        Ok(())
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

const USER_SELECT: &str = "SELECT id, pubkey, enc_key, enc_key_sig, name, avatar_hash, role, invited_by, created_at, enc_key_not_after FROM users";

fn user_row(r: &rusqlite::Row) -> rusqlite::Result<UserRow> {
    let enc_key: Option<Vec<u8>> = r.get(2)?;
    let enc_key_sig: Option<Vec<u8>> = r.get(3)?;
    let not_after: Option<i64> = r.get(9)?;
    let enc_key = match (enc_key, enc_key_sig, not_after) {
        (Some(key), Some(sig), Some(not_after)) => match (key.try_into(), sig.try_into()) {
            (Ok(key), Ok(sig)) => Some(SignedEncKey {
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
        enc_key,
        name: r.get(4)?,
        avatar: r
            .get::<_, Option<Vec<u8>>>(5)?
            .and_then(|h| h.try_into().ok())
            .map(Hash),
        role: Role::from_rank(r.get(6)?).unwrap_or(Role::User),
        invited_by: r.get::<_, Option<i64>>(7)?.map(|id| UserId(id as u64)),
        created_at: r.get::<_, i64>(8)? as u64,
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

fn fixed<const N: usize>(bytes: Vec<u8>) -> rusqlite::Result<[u8; N]> {
    bytes.try_into().map_err(|_| rusqlite::Error::InvalidQuery)
}

fn remove_one(tx: &Transaction, id: i64) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE users SET invited_by = NULL WHERE invited_by = ?1",
        [id],
    )?;
    tx.execute("DELETE FROM invites WHERE minted_by = ?1", [id])?;
    tx.execute("DELETE FROM users WHERE id = ?1", [id])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enroll(db: &mut Database, key: u8, by: Option<UserId>) -> UserId {
        let code = format!("C{key:02}");
        db.insert_invite(&InviteRow {
            code_id: code.clone(),
            secret: "S".into(),
            minted_by: by,
            expires_at: 1000,
            failures: 0,
        })
        .unwrap();
        db.enroll(&[key; 32], &code, 100).unwrap().unwrap().id
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
        db.set_server_name(profile.name.as_deref()).unwrap();
        drop(db);
        assert_eq!(
            Database::open(&path).unwrap().server_profile().unwrap(),
            profile
        );
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
        db.create_room("a").unwrap();
        let last = db.create_room("b").unwrap();
        db.delete_room(last).unwrap();
        assert!(db.create_room("c").unwrap() > last);
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
        db.set_profile(user, "a", Some(used)).unwrap();
        db.set_server_avatar(Some(server)).unwrap();
        assert_eq!(db.delete_unused_blobs(150).unwrap(), 1);
        assert!(db.blob(&unused).unwrap().is_none());
        for kept in [used, server, fresh] {
            assert!(db.blob(&kept).unwrap().is_some());
        }
    }

    /// The schema before versioning: no `enc_key_not_after`, no server profile, and ids
    /// without AUTOINCREMENT.
    const UNVERSIONED_SCHEMA: &str = "
        CREATE TABLE users (
            id INTEGER PRIMARY KEY,
            pubkey BLOB NOT NULL UNIQUE,
            enc_key BLOB,
            enc_key_sig BLOB,
            name TEXT NOT NULL,
            avatar_hash BLOB,
            role INTEGER NOT NULL,
            invited_by INTEGER NULL,
            created_at INTEGER NOT NULL
        );
        CREATE TABLE rooms (id INTEGER PRIMARY KEY, name TEXT NOT NULL, epoch INTEGER NOT NULL);
        CREATE TABLE invites (
            code_id TEXT PRIMARY KEY,
            secret TEXT NOT NULL,
            minted_by INTEGER NULL,
            expires_at INTEGER NOT NULL,
            failures INTEGER NOT NULL
        );
        CREATE TABLE blobs (hash BLOB PRIMARY KEY, bytes BLOB NOT NULL);
        CREATE TABLE counters (name TEXT PRIMARY KEY, value INTEGER NOT NULL);
        INSERT INTO users (id, pubkey, name, role, created_at) VALUES (1, x'0101010101010101010101010101010101010101010101010101010101010101', 'Moss', 2, 5);
        INSERT INTO users (id, pubkey, name, role, invited_by, created_at) VALUES (2, x'0202020202020202020202020202020202020202020202020202020202020202', 'Fern', 0, 1, 6);
        INSERT INTO rooms (id, name, epoch) VALUES (1, 'General', 4), (2, 'Games', 9);
        INSERT INTO blobs (hash, bytes) VALUES (x'03', x'00');
        INSERT INTO counters (name, value) VALUES ('next_media_id', 40);
    ";

    #[test]
    fn an_unversioned_database_migrates_and_keeps_its_data() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db");
        Connection::open(&path)
            .unwrap()
            .execute_batch(UNVERSIONED_SCHEMA)
            .unwrap();

        let mut db = Database::open(&path).unwrap();
        let version: i64 = db
            .conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(version, MIGRATIONS.len() as i64);
        let fern = db.user_by_pubkey(&[2; 32]).unwrap().unwrap();
        assert_eq!((fern.id, fern.name.as_str()), (UserId(2), "Fern"));
        assert_eq!(fern.invited_by, Some(UserId(1)));
        assert_eq!(db.users().unwrap().len(), 2);
        let rooms = db.rooms().unwrap();
        assert_eq!(rooms[1].0.name, "Games");
        assert_eq!(rooms[1].1, Epoch(9));
        assert_eq!(db.allocate_media_ids(3).unwrap(), Some(MediaId(40)));
        assert_eq!(db.server_profile().unwrap(), ServerProfile::default());

        let key = SignedEncKey {
            key: [7; 32],
            not_after: 99,
            sig: [8; 64],
        };
        db.set_enc_key(fern.id, &key).unwrap();
        assert_eq!(db.user(fern.id).unwrap().unwrap().enc_key, Some(key));
        db.delete_room(RoomId(2)).unwrap();
        assert_eq!(db.create_room("New").unwrap(), RoomId(3));
        db.remove_users(&[fern.id]).unwrap();
        assert_eq!(enroll(&mut db, 9, None), UserId(3));
        assert_eq!(db.delete_unused_blobs(1).unwrap(), 1);

        drop(db);
        assert_eq!(Database::open(&path).unwrap().users().unwrap().len(), 2);
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
    fn re_enrolling_an_existing_key_keeps_the_user() {
        let mut db = Database::open_in_memory().unwrap();
        let root = enroll(&mut db, 1, None);
        assert_eq!(db.user(root).unwrap().unwrap().role, Role::Admin);
        let first = enroll(&mut db, 2, Some(root));
        db.set_role(first, Role::Manager).unwrap();
        let again = enroll(&mut db, 2, Some(root));
        assert_eq!(again, first);
        assert_eq!(db.user(first).unwrap().unwrap().role, Role::Manager);
    }

    #[test]
    fn an_invite_enrolls_once() {
        let mut db = Database::open_in_memory().unwrap();
        enroll(&mut db, 1, None);
        assert_eq!(db.enroll(&[9; 32], "C01", 100).unwrap(), None);
    }

    #[test]
    fn expired_invites_do_not_enroll() {
        let mut db = Database::open_in_memory().unwrap();
        let invite = InviteRow {
            code_id: "OLD".into(),
            secret: "S".into(),
            minted_by: None,
            expires_at: 50,
            failures: 0,
        };
        db.insert_invite(&invite).unwrap();
        assert_eq!(db.enroll(&[1; 32], "OLD", 100).unwrap(), None);
    }
}
