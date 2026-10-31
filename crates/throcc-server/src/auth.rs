use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::time::{Duration, Instant};

use throcc_crypto::identity::{self, random};
use throcc_crypto::pake::{ServerPake, Transcript};
use throcc_proto::ids::RoomId;
use throcc_proto::invite::{self, ID_CHARS, SECRET_CHARS};
use throcc_proto::messages::{AuthError, Event, Hello, PakeFinish, PakeReply, PakeStep, PublicKey};
use tracing::{info, warn};

use crate::Shared;
use crate::database::{InviteRow, UserRow};
use crate::rooms::unix_now;
use crate::session::{FrameStream, read_handshake, write_frame};

pub const INVITE_TRIES: u32 = 3;
const PAKE_TIMEOUT: Duration = Duration::from_secs(30);
const SOURCE_RUNS: usize = 10;
const SOURCE_WINDOW: Duration = Duration::from_secs(10 * 60);

/// A refusal, tagged with the message type the client is waiting for.
#[derive(Debug)]
pub enum Refused {
    Hello(AuthError),
    Enroll(AuthError),
}

pub struct Authenticated {
    pub row: UserRow,
    pub want_room: Option<RoomId>,
}

pub async fn authenticate(
    shared: &Shared,
    connection: &quinn::Connection,
    stream: &mut FrameStream,
    server_nonce: &[u8; 32],
    hello: Hello,
) -> Result<Authenticated, Refused> {
    let exporter = exporter(connection).map_err(Refused::Hello)?;
    match hello {
        Hello::Auth {
            pubkey,
            client_nonce,
            want_room,
            signature,
        } => {
            let message = identity::auth_message(server_nonce, &client_nonce, &exporter);
            identity::verify(&pubkey, &message, &signature)
                .map_err(|_| Refused::Hello(AuthError::BadSignature))?;
            let row = shared
                .hub()
                .db
                .user_by_pubkey(&pubkey)
                .ok()
                .flatten()
                .ok_or(Refused::Hello(AuthError::UnknownKey))?;
            Ok(Authenticated { row, want_room })
        }
        Hello::Enroll {
            code_id,
            pubkey,
            client_nonce,
            pake_a,
        } => {
            let run = Enrollment {
                shared,
                source: SourceKey::of(connection.remote_address().ip()),
                code_id: invite::normalize(&code_id),
                pubkey,
                server_nonce,
                client_nonce,
                exporter,
            };
            let row = run.run(stream, &pake_a).await?;
            Ok(Authenticated {
                row,
                want_room: None,
            })
        }
    }
}

struct Enrollment<'a> {
    shared: &'a Shared,
    source: SourceKey,
    code_id: String,
    pubkey: PublicKey,
    server_nonce: &'a [u8; 32],
    client_nonce: [u8; 32],
    exporter: [u8; 32],
}

impl Enrollment<'_> {
    async fn run(&self, stream: &mut FrameStream, pake_a: &[u8]) -> Result<UserRow, Refused> {
        // A run counts against its source from the start, so parallel runs can't outpace the limit.
        if !self.shared.limiter().begin(self.source) {
            warn!(source = ?self.source, "enrollment rate-limited");
            return Err(Refused::Enroll(AuthError::RateLimited));
        }
        let now = unix_now();
        let invite = self.claim_try(now)?;
        let secret = match &invite {
            Some(invite) => invite.secret.clone(),
            None => decoy_secret(),
        };

        let exchanged = self.exchange(stream, &secret, pake_a).await.is_ok();
        let mut hub = self.shared.hub();
        if exchanged && invite.is_some() {
            match hub.db.enroll(&self.pubkey, &self.code_id, now) {
                Ok(Some(row)) => {
                    hub.emit_admins(Event::UserAdded(row.to_user()));
                    drop(hub);
                    self.shared.limiter().forgive(self.source);
                    info!(user = row.id.0, role = ?row.role, "enrolled");
                    return Ok(row);
                }
                Ok(None) => info!(code_id = %self.code_id, "invite was used up during the run"),
                Err(e) => warn!("enrollment failed: {e:#}"),
            }
        }
        if let Some(invite) = &invite
            && hub
                .db
                .invite(&self.code_id)
                .ok()
                .flatten()
                .is_some_and(|i| i.failures >= INVITE_TRIES)
        {
            hub.db.delete_invite(&self.code_id).ok();
            warn!(code_id = %self.code_id, "invite burned after {INVITE_TRIES} failed runs");
            if let Some(minter) = invite.minted_by {
                hub.emit_to(
                    minter,
                    Event::InviteBurned {
                        code_id: self.code_id.clone(),
                    },
                );
            }
        }
        Err(Refused::Hello(AuthError::BadInvite))
    }

    /// Looks up the invite and uses up one of its tries before any PAKE message is sent,
    /// so a run that is dropped halfway still costs a try.
    fn claim_try(&self, now: u64) -> Result<Option<InviteRow>, Refused> {
        let hub = self.shared.hub();
        let invite = if self.code_id.len() == ID_CHARS {
            hub.db
                .invite(&self.code_id)
                .map_err(|_| Refused::Enroll(AuthError::BadInvite))?
                .filter(|i| i.expires_at > now && i.failures < INVITE_TRIES)
        } else {
            None
        };
        if invite.is_some() {
            hub.db
                .consume_invite_try(&self.code_id)
                .map_err(|_| Refused::Enroll(AuthError::BadInvite))?;
        } else {
            let shown: String = self.code_id.chars().take(8).collect();
            warn!(source = ?self.source, code_id = %shown, "enrollment with an unknown invite id");
        }
        Ok(invite)
    }

    async fn exchange(
        &self,
        stream: &mut FrameStream,
        secret: &str,
        pake_a: &[u8],
    ) -> Result<(), AuthError> {
        let pake =
            ServerPake::respond(&self.code_id, secret, pake_a).map_err(|_| AuthError::BadInvite)?;
        let spki_hash = self.shared.fingerprint.0;
        let transcript = Transcript {
            server_nonce: self.server_nonce,
            client_nonce: &self.client_nonce,
            code_id: &self.code_id,
            pubkey: &self.pubkey,
            spki_hash: &spki_hash,
            exporter: &self.exporter,
            pake_a,
            pake_b: &pake.pake_b,
        };
        let reply = PakeReply {
            pake_b: pake.pake_b.clone(),
            confirm_b: pake.confirm_b(&transcript),
        };
        let step: PakeStep = Ok(reply);
        write_frame(&mut stream.send, &step)
            .await
            .map_err(|_| AuthError::BadInvite)?;
        let finish: PakeFinish =
            tokio::time::timeout(PAKE_TIMEOUT, read_handshake(&mut stream.recv))
                .await
                .map_err(|_| AuthError::BadInvite)?
                .map_err(|_| AuthError::BadInvite)?;
        pake.check_confirm_a(&transcript, &finish.confirm_a)
            .map_err(|_| AuthError::BadInvite)?;
        identity::verify(&self.pubkey, &transcript.to_bytes(), &finish.signature)
            .map_err(|_| AuthError::BadSignature)
    }
}

fn exporter(connection: &quinn::Connection) -> Result<[u8; 32], AuthError> {
    let mut out = [0u8; 32];
    connection
        .export_keying_material(&mut out, b"throcc/exporter/1", b"")
        .map_err(|_| AuthError::ProtocolMismatch)?;
    Ok(out)
}

/// A random secret for unknown invite ids, so they fail exactly like a wrong secret.
fn decoy_secret() -> String {
    let bytes: [u8; ID_CHARS + SECRET_CHARS] = random();
    invite::InviteCode::from_random(bytes).secret
}

/// A source address for rate limiting: the full IPv4 address, or the IPv6 /64.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SourceKey {
    V4(u32),
    V6(u64),
}

impl SourceKey {
    pub fn of(ip: IpAddr) -> Self {
        match ip {
            IpAddr::V4(v4) => Self::V4(v4.into()),
            IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
                Some(v4) => Self::V4(v4.into()),
                None => Self::V6((u128::from(v6) >> 64) as u64),
            },
        }
    }
}

/// Counts enrollment runs per source in a sliding window. Successful runs are forgiven.
#[derive(Default)]
pub struct SourceLimiter {
    runs: HashMap<SourceKey, VecDeque<Instant>>,
}

impl SourceLimiter {
    /// Records the start of a run, or returns false if the source is over its limit.
    pub fn begin(&mut self, source: SourceKey) -> bool {
        self.runs.retain(|_, times| {
            while times.front().is_some_and(|t| t.elapsed() > SOURCE_WINDOW) {
                times.pop_front();
            }
            !times.is_empty()
        });
        let times = self.runs.entry(source).or_default();
        if times.len() >= SOURCE_RUNS {
            return false;
        }
        times.push_back(Instant::now());
        true
    }

    pub fn forgive(&mut self, source: SourceKey) {
        if let Some(times) = self.runs.get_mut(&source) {
            times.pop_back();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipv6_sources_group_by_64() {
        let a: IpAddr = "2001:db8:1:2::1".parse().unwrap();
        let b: IpAddr = "2001:db8:1:2:ffff::9".parse().unwrap();
        let c: IpAddr = "2001:db8:1:3::1".parse().unwrap();
        assert_eq!(SourceKey::of(a), SourceKey::of(b));
        assert_ne!(SourceKey::of(a), SourceKey::of(c));
        let mapped: IpAddr = "::ffff:192.0.2.1".parse().unwrap();
        let v4: IpAddr = "192.0.2.1".parse().unwrap();
        assert_eq!(SourceKey::of(mapped), SourceKey::of(v4));
    }

    #[test]
    fn limiter_blocks_after_repeated_failures() {
        let mut limiter = SourceLimiter::default();
        let source = SourceKey::of("192.0.2.1".parse().unwrap());
        for _ in 0..SOURCE_RUNS {
            assert!(limiter.begin(source));
        }
        assert!(!limiter.begin(source));
        limiter.forgive(source);
        assert!(limiter.begin(source));
        assert!(limiter.begin(SourceKey::of("192.0.2.2".parse().unwrap())));
    }
}
