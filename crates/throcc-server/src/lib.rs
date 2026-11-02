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
use throcc_proto::transport::{check_datagrams, close};
use tokio::sync::Semaphore;
use tracing::debug;

use crate::database::Database;
use crate::identity::ServerIdentity;
use crate::limits::{ArrivalLimiter, Caps, Gate, Pass, SourceKey, SourceLimiter};
use crate::refusals::{REFUSAL_REPORT, RefusalLog};
use crate::rooms::Hub;
use crate::sfu::Sfu;
pub use crate::sfu::{AUDIO_BURST_PACKETS, AUDIO_PACKETS_PER_SECOND};
pub use crate::tls::Congestion;

pub const DATABASE_FILE: &str = "throcc.sqlite";
/// Connections that have signed in.
pub const CONNECTIONS: Caps = Caps {
    per_source: 32,
    per_network: 128,
    total: 1024,
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

pub struct Config {
    pub data_dir: PathBuf,
    pub listen: SocketAddr,
    pub congestion: Congestion,
}

/// State shared by every connection.
pub struct Shared {
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

impl Shared {
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
    shared: Arc<Shared>,
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
        let server_config =
            tls::server_config(&identity, tls::transport_config(config.congestion)?)?;
        let endpoint = quinn::Endpoint::server(server_config, config.listen)
            .with_context(|| format!("binding {}", config.listen))?;
        let shared = Arc::new(Shared {
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

    pub fn shared(&self) -> Arc<Shared> {
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
        let reporter = Arc::downgrade(&self.shared);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(REFUSAL_REPORT);
            interval.tick().await;
            loop {
                interval.tick().await;
                let Some(shared) = reporter.upgrade() else {
                    return;
                };
                shared.refusals.report();
            }
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
        if !file.try_exists()? {
            bail!(
                "{} is missing; start the server with --data {} once before minting an invite",
                file.display(),
                data_dir.display()
            );
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

async fn handle(shared: Arc<Shared>, connection: quinn::Connection, unauthenticated: Pass) {
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
