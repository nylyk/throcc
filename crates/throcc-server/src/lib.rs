mod auth;
pub mod database;
mod egress;
mod files;
pub mod identity;
mod limits;
mod refusals;
mod room_password;
pub mod rooms;
mod session;
mod sfu;
pub mod tls;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use throcc_proto::Fingerprint;
use throcc_proto::invite::InviteCode;
use throcc_proto::limits::MAX_SIGNED_IN;
use throcc_proto::transport::{self, check_datagrams, close};
use tokio::sync::Semaphore;
use tracing::{debug, warn};

use crate::database::Database;
use crate::identity::ServerIdentity;
use crate::limits::{ArrivalLimiter, Caps, Gate, Pass, SourceKey, SourceLimiter};
use crate::refusals::{REFUSAL_REPORT, RefusalLog};
use crate::rooms::Hub;
use crate::sfu::Sfu;
pub use crate::sfu::{AUDIO_BURST_PACKETS, AUDIO_PACKETS_PER_SECOND};
pub use crate::tls::Congestion;

pub const DATABASE_FILE: &str = "throcc.sqlite";
/// Connections past their hello, signing in or signed in: everyone who may be signed in,
/// admins past the limit included, and every handshake that may be signing in.
pub const CONNECTIONS: Caps = Caps {
    per_source: 32,
    per_network: 128,
    total: MAX_SIGNED_IN + HANDSHAKES.total,
};
/// Connections from their first packet until they have signed in. A network gets a 16th
/// of them, so filling them takes hosts in 16 networks; a source gets as many as its
/// network, so an office behind one address gets back in at once after a restart.
pub const UNAUTHENTICATED: Caps = Caps {
    per_source: 32,
    per_network: 32,
    total: 512,
};
pub const HANDSHAKES: Caps = Caps {
    per_source: 4,
    per_network: 16,
    total: 128,
};
/// A real handshake takes a few round trips; this leaves room for several lost packets.
pub const TLS_TIMEOUT: Duration = Duration::from_secs(5);
/// Room-password Argon2 runs allowed at once; each takes 19 MiB.
const PASSWORD_WORKERS: usize = 2;
const KEYFRAME_TICK: Duration = Duration::from_millis(100);
/// How often accounts are checked for inactivity.
const INACTIVE_SWEEP: Duration = Duration::from_secs(60 * 60);

pub struct Config {
    pub data_dir: PathBuf,
    pub listen: SocketAddr,
    pub congestion: Congestion,
    /// How long a connection may go without a packet before it is closed; QUIC uses the
    /// shorter of this and the client's. At least a second. Tests shorten it.
    pub idle_timeout: Duration,
}

impl Config {
    pub fn new(data_dir: PathBuf, listen: SocketAddr) -> Self {
        Self {
            data_dir,
            listen,
            congestion: Congestion::default(),
            idle_timeout: transport::IDLE_TIMEOUT,
        }
    }
}

/// State shared by every connection.
pub struct ServerState {
    pub fingerprint: Fingerprint,
    pub sfu: Arc<Sfu>,
    hub: Mutex<Hub>,
    limiter: Mutex<SourceLimiter>,
    arrivals: Mutex<ArrivalLimiter>,
    refusals: RefusalLog,
    unauthenticated: Arc<Gate>,
    connections: Arc<Gate>,
    handshakes: Arc<Gate>,
    password_work: Semaphore,
}

impl ServerState {
    /// Locks the hub. Never hold the guard across an `.await`.
    pub fn hub(&self) -> MutexGuard<'_, Hub> {
        lock(&self.hub)
    }

    pub fn limiter(&self) -> MutexGuard<'_, SourceLimiter> {
        lock(&self.limiter)
    }
}

pub struct Server {
    endpoint: quinn::Endpoint,
    shared: Arc<ServerState>,
}

impl Server {
    /// Binds the UDP listener. Must be called inside a tokio runtime.
    pub fn bind(config: &Config) -> Result<Self> {
        files::prepare_data_dir(&config.data_dir)?;
        let identity = ServerIdentity::load_or_create(&config.data_dir)?;
        let sfu = Arc::new(Sfu::default());
        let hub = Hub::new(
            Database::open(&config.data_dir.join(DATABASE_FILE))?,
            sfu.clone(),
            identity.fingerprint,
        )?;
        // No enrollment run is in flight yet, so invites a stop left spent can go now.
        hub.db.delete_spent_invites(auth::INVITE_TRIES)?;
        let server_config = tls::server_config(
            &identity,
            tls::transport_config(config.congestion, config.idle_timeout)?,
        )?;
        let endpoint = quinn::Endpoint::server(server_config, config.listen)
            .with_context(|| format!("binding {}", config.listen))?;
        let shared = Arc::new(ServerState {
            fingerprint: identity.fingerprint,
            sfu,
            hub: Mutex::new(hub),
            limiter: Mutex::default(),
            arrivals: Mutex::default(),
            refusals: RefusalLog::default(),
            unauthenticated: Gate::new(UNAUTHENTICATED),
            connections: Gate::new(CONNECTIONS),
            handshakes: Gate::new(HANDSHAKES),
            password_work: Semaphore::new(PASSWORD_WORKERS),
        });
        Ok(Self { endpoint, shared })
    }

    pub fn local_addr(&self) -> Result<SocketAddr> {
        Ok(self.endpoint.local_addr()?)
    }

    pub fn fingerprint(&self) -> Fingerprint {
        self.shared.fingerprint
    }

    pub fn shared(&self) -> Arc<ServerState> {
        self.shared.clone()
    }

    pub async fn run(self) -> Result<()> {
        let ticker = Arc::downgrade(&self.shared);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(KEYFRAME_TICK);
            while let Some(shared) = ticker.upgrade() {
                shared.hub().flush_keyframes();
                drop(shared);
                interval.tick().await;
            }
        });
        spawn_every(&self.shared, INACTIVE_SWEEP, |shared| {
            if let Err(e) = shared.hub().remove_inactive(unix_now()) {
                warn!("removing inactive accounts: {e:#}");
            }
        });
        spawn_every(&self.shared, REFUSAL_REPORT, |shared| {
            shared.refusals.report()
        });
        while let Some(incoming) = self.endpoint.accept().await {
            let peer = incoming.remote_address();
            // The per-source caps count validated addresses only, so spoofed packets can't
            // fill another source's share.
            if !incoming.remote_address_validated() {
                if let Err(e) = incoming.retry() {
                    e.into_incoming().refuse();
                }
                continue;
            }
            let source = SourceKey::of(peer.ip());
            let Some(pass) = self.shared.unauthenticated.try_enter(source) else {
                debug!(%peer, "refusing connection: too many sign-ins from its network");
                self.shared
                    .refusals
                    .count("refused: too many from its network are signing in");
                incoming.refuse();
                continue;
            };
            if !lock(&self.shared.arrivals).admit(source, Instant::now()) {
                debug!(%peer, "ignoring connection: its network connects too often");
                self.shared
                    .refusals
                    .count("ignored: its source or network connects too often");
                incoming.ignore();
                continue;
            }
            let shared = self.shared.clone();
            tokio::spawn(async move {
                match tokio::time::timeout(TLS_TIMEOUT, incoming).await {
                    Ok(Ok(connection)) => handle(shared, connection, pass).await,
                    Ok(Err(e)) => {
                        debug!(%peer, "handshake failed: {e}");
                        shared.refusals.count("TLS handshake failed");
                    }
                    Err(_) => {
                        debug!(%peer, "handshake timed out");
                        shared.refusals.count("TLS handshake timed out");
                    }
                }
            });
        }
        Ok(())
    }
}

/// An invite whose user becomes an admin.
pub struct AdminInvite {
    pub code: InviteCode,
    /// Unix seconds.
    pub expires_at: u64,
    pub fingerprint: Fingerprint,
}

/// Mints a single-use admin invite that lasts a day into `data_dir`, for
/// `throcc-server admin-invite`. It works whether the server is running or not, and while
/// users exist, so an admin who lost their identity can get back in. The data directory
/// must hold a server that has started once: minting into a fresh directory, say after a
/// mistyped `--data`, would print an invite and a fingerprint no running server knows.
pub fn admin_invite(data_dir: &Path) -> Result<AdminInvite> {
    let database = data_dir.join(DATABASE_FILE);
    for file in [data_dir.join(identity::KEY_FILE), database.clone()] {
        let exists = file.try_exists().with_context(|| {
            format!(
                "checking {}; run this as the user that owns {}",
                file.display(),
                data_dir.display()
            )
        })?;
        if !exists {
            bail!(
                "{} is missing; start the server with --data {} once before minting an invite",
                file.display(),
                data_dir.display()
            );
        }
        // Checked here because the server's own refusal of a foreign file would suggest
        // handing the data directory to this user, which locks the real server out.
        if let Some(owner) = files::other_owner(&file)? {
            return Err(not_the_servers_user(&file, owner, data_dir));
        }
    }
    let fingerprint = ServerIdentity::load_or_create(data_dir)?.fingerprint;
    let db = Database::open_existing(&database)?;
    let (code, expires_at) = rooms::mint_invite(&db, None, unix_now())?;
    Ok(AdminInvite {
        code,
        expires_at,
        fingerprint,
    })
}

fn not_the_servers_user(file: &Path, owner: u32, data_dir: &Path) -> anyhow::Error {
    anyhow::anyhow!(
        "{} belongs to uid {owner}, the user the server runs as, and admin-invite has to run \
         as that user too. Run it as uid {owner}, for instance \
         `sudo -u '#{owner}' throcc-server admin-invite --data {}`; in Docker, plain \
         `docker exec` already does, so leave out `-u`",
        file.display(),
        data_dir.display()
    )
}

/// Runs `job` every `period`, starting one period from now, until the server is dropped.
fn spawn_every(
    shared: &Arc<ServerState>,
    period: Duration,
    job: impl Fn(&ServerState) + Send + 'static,
) {
    let shared = Arc::downgrade(shared);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(period);
        interval.tick().await;
        loop {
            interval.tick().await;
            let Some(shared) = shared.upgrade() else {
                return;
            };
            job(&shared);
        }
    });
}

async fn handle(shared: Arc<ServerState>, connection: quinn::Connection, unauthenticated: Pass) {
    let peer = connection.remote_address();
    if let Err(e) = check_datagrams(connection.max_datagram_size()) {
        debug!(%peer, "refusing connection: {e}");
        shared.refusals.count("refused: no QUIC datagrams");
        connection.close(close::DATAGRAMS_REQUIRED.into(), e.to_string().as_bytes());
        return;
    }
    debug!(%peer, "connected");
    session::run(shared, connection, unauthenticated).await;
}

/// Locks `mutex`, carrying on past a panic of an earlier holder.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_server_times_out_idle_connections_as_clients_do() {
        let config = Config::new(PathBuf::from("data"), "[::]:8476".parse().unwrap());
        assert_eq!(config.idle_timeout, transport::IDLE_TIMEOUT);
    }

    #[test]
    fn an_admin_invite_needs_a_server_that_has_started_in_its_data_directory() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("typo");
        assert!(admin_invite(&missing).is_err());
        assert!(!missing.exists());
        assert!(admin_invite(dir.path()).is_err());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn running_admin_invite_as_another_user_says_which_user_to_run_it_as() {
        let refusal = format!(
            "{:#}",
            not_the_servers_user(Path::new("/data/server_key"), 65532, Path::new("/data"))
        );
        assert!(refusal.contains("Run it as uid 65532"), "{refusal}");
        assert!(refusal.contains("sudo -u '#65532'"), "{refusal}");
        assert!(!refusal.contains("chown"), "{refusal}");
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "needs root to hand the data files to another user"]
    fn admin_invite_as_another_user_says_to_run_it_as_the_servers_user() {
        let dir = tempfile::tempdir().unwrap();
        ServerIdentity::load_or_create(dir.path()).unwrap();
        drop(Database::open(&dir.path().join(DATABASE_FILE)).unwrap());
        let server_uid = 65532;
        for file in [identity::KEY_FILE, DATABASE_FILE] {
            std::os::unix::fs::chown(dir.path().join(file), Some(server_uid), None)
                .expect("only root can hand a file to another user");
        }

        let refusal = format!("{:#}", admin_invite(dir.path()).err().unwrap());
        assert!(refusal.contains("Run it as uid 65532"), "{refusal}");
    }
}
