use std::collections::HashMap;
use std::time::{Duration, Instant};

use throcc_crypto::identity::{self, random};
use throcc_crypto::pake::{ServerPake, Transcript};
use throcc_proto::ids::{RoomId, UserId};
use throcc_proto::invite::{self, ID_CHARS, SECRET_CHARS};
use throcc_proto::messages::{
    AuthError, Event, PakeFinish, PakeReply, PakeStep, PublicKey, SignIn, valid_name,
};
use tracing::{debug, info, warn};

use crate::database::{Enrolled, InviteRow, UserRow};
use crate::limits::SourceKey;
use crate::rooms::Hub;
use crate::session::{FrameStream, read_handshake, write_frame};
use crate::{ServerState, unix_now};

pub const INVITE_TRIES: u32 = 3;
const PAKE_TIMEOUT: Duration = Duration::from_secs(30);
const PAKE_REPLY_TIMEOUT: Duration = Duration::from_secs(5);
/// Client and server must agree on this byte for byte.
const EXPORTER_LABEL: &[u8] = b"throcc/exporter/1";

/// A refusal, tagged with the message type the client is waiting for.
#[derive(Debug)]
pub enum AuthRefusal {
    Auth(AuthError),
    Enroll(AuthError),
    /// A failure on the server's side. The connection is closed without an answer.
    Internal,
}

pub struct Authenticated {
    pub row: UserRow,
    pub want_room: Option<RoomId>,
}

pub async fn authenticate(
    shared: &ServerState,
    connection: &quinn::Connection,
    stream: &mut FrameStream,
    server_nonce: &[u8; 32],
    sign_in: SignIn,
    source: SourceKey,
) -> Result<Authenticated, AuthRefusal> {
    let exporter = exporter(connection).map_err(AuthRefusal::Auth)?;
    match sign_in {
        SignIn::Auth {
            pubkey,
            client_nonce,
            want_room,
            signature,
        } => {
            let message = identity::auth_message(server_nonce, &client_nonce, &exporter);
            identity::verify(&pubkey, &message, &signature)
                .map_err(|_| AuthRefusal::Auth(AuthError::BadSignature))?;
            let row = shared
                .hub()
                .db
                .user_by_pubkey(&pubkey)
                .map_err(internal)?
                .ok_or(AuthRefusal::Auth(AuthError::UnknownKey))?;
            Ok(Authenticated { row, want_room })
        }
        SignIn::Enroll {
            code_id,
            pubkey,
            client_nonce,
            pake_a,
            name,
        } => {
            let name = valid_name(&name)
                .ok_or(AuthRefusal::Enroll(AuthError::BadName))?
                .to_owned();
            let run = Enrollment {
                shared,
                source,
                code_id: invite::normalize(&code_id),
                pubkey,
                name,
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
    shared: &'a ServerState,
    source: SourceKey,
    code_id: String,
    pubkey: PublicKey,
    name: String,
    server_nonce: &'a [u8; 32],
    client_nonce: [u8; 32],
    exporter: [u8; 32],
}

impl Enrollment<'_> {
    async fn run(&self, stream: &mut FrameStream, pake_a: &[u8]) -> Result<UserRow, AuthRefusal> {
        // Checked before the run counts or the invite is touched, so a full server spends
        // neither, for admin invites too. Someone may still sign in during the run; then the
        // account is made and, unless it is an admin's, the sign-in refused.
        if self.shared.hub().is_full() {
            return Err(AuthRefusal::Enroll(AuthError::ServerFull));
        }
        // A run counts against its source from the start, so parallel runs can't outpace the limit.
        if !self.shared.limiter().begin(self.source, Instant::now()) {
            debug!(source = ?self.source, "enrollment rate-limited");
            self.shared
                .refusals
                .count("enrollment refused: too many from its source or network");
            return Err(AuthRefusal::Enroll(AuthError::RateLimited));
        }
        let now = unix_now();
        let invite = self.claim_try(now)?;
        let secret = match &invite {
            Some(invite) => invite.secret.clone(),
            None => decoy_secret(),
        };
        let _run = invite.as_ref().map(|invite| InviteRun {
            shared: self.shared,
            code_id: &self.code_id,
            minted_by: invite.minted_by,
        });

        let exchanged = self.exchange(stream, &secret, pake_a).await.is_some();
        let Some(invite) = invite.filter(|_| exchanged) else {
            return Err(AuthRefusal::Auth(AuthError::BadInvite));
        };
        let enrolled = {
            let mut hub = self.shared.hub();
            // Said only to someone who knew the invite's secret and holds the key, so it
            // tells a stranger nothing, and under the lock every account is made under, so
            // the count is exact. The try the run used is given back: the code was right.
            if self.no_account_is_left(&hub, &invite).map_err(internal)? {
                hub.db.return_invite_try(&self.code_id).map_err(internal)?;
                info!(code_id = %self.code_id, "enrollment refused: the server keeps no more accounts");
                return Err(AuthRefusal::Auth(AuthError::ServerFull));
            }
            let enrolled = hub.db.enroll(&self.pubkey, &self.code_id, &self.name, now);
            match &enrolled {
                Ok(Some(Enrolled::Added(row))) => {
                    hub.emit_user_list_change(Event::UserAdded(row.to_user()));
                }
                Ok(Some(Enrolled::Promoted(row))) => hub.role_changed(row.id, row.role),
                _ => {}
            }
            enrolled
        };
        match enrolled {
            Ok(Some(enrolled)) => {
                let row = enrolled.into_row();
                self.shared.limiter().forgive(self.source);
                info!(user = row.id.0, role = ?row.role, "enrolled");
                Ok(row)
            }
            Ok(None) => {
                info!(code_id = %self.code_id, "invite was used up during the run");
                Err(AuthRefusal::Auth(AuthError::BadInvite))
            }
            Err(e) => Err(internal(e)),
        }
    }

    /// Whether enrolling would make one account more than the server keeps. An admin invite
    /// always enrolls, so a server holding every account it keeps can still be managed, and
    /// a key that has an account gets no new one.
    fn no_account_is_left(&self, hub: &Hub, invite: &InviteRow) -> anyhow::Result<bool> {
        Ok(invite.minted_by.is_some()
            && hub.db.user_count()? >= self.shared.max_accounts
            && hub.db.user_by_pubkey(&self.pubkey)?.is_none())
    }

    /// Looks up the invite and uses up one of its tries before any PAKE message is sent,
    /// so a run that is dropped halfway still costs a try.
    fn claim_try(&self, now: u64) -> Result<Option<InviteRow>, AuthRefusal> {
        let mut hub = self.shared.hub();
        let invite = if self.code_id.len() == ID_CHARS {
            hub.db
                .invite(&self.code_id)
                .map_err(internal)?
                .filter(|i| i.expires_at > now && i.failures < INVITE_TRIES)
        } else {
            None
        };
        if invite.is_some() {
            hub.db.consume_invite_try(&self.code_id).map_err(internal)?;
            hub.invite_runs.start(&self.code_id);
        } else {
            let shown: String = self.code_id.chars().take(8).collect();
            debug!(source = ?self.source, code_id = %shown, "enrollment with an unknown invite id");
            self.shared
                .refusals
                .count("enrollment with an unknown invite id");
        }
        Ok(invite)
    }

    /// Runs the PAKE. Every failure looks the same, so a decoy run fails like a wrong secret.
    async fn exchange(&self, stream: &mut FrameStream, secret: &str, pake_a: &[u8]) -> Option<()> {
        let pake = ServerPake::respond(&self.code_id, secret, pake_a).ok()?;
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
        let confirm = pake.confirm(&transcript);
        let reply = PakeReply {
            pake_b: pake.pake_b.clone(),
            confirm_b: confirm.confirm_b,
        };
        let step: PakeStep = Ok(reply);
        tokio::time::timeout(PAKE_REPLY_TIMEOUT, write_frame(&mut stream.send, &step))
            .await
            .ok()?
            .ok()?;
        let finish: PakeFinish =
            tokio::time::timeout(PAKE_TIMEOUT, read_handshake(&mut stream.recv))
                .await
                .ok()?
                .ok()?;
        confirm.check_confirm_a(&finish.confirm_a).ok()?;
        identity::verify(&self.pubkey, &transcript.to_bytes(), &finish.signature).ok()
    }
}

/// One PAKE run of a live invite, from its claimed try until it ends or is dropped. The last
/// run of an invite that has used up its tries burns it.
struct InviteRun<'a> {
    shared: &'a ServerState,
    code_id: &'a str,
    minted_by: Option<UserId>,
}

impl Drop for InviteRun<'_> {
    fn drop(&mut self) {
        let mut hub = self.shared.hub();
        if !hub.invite_runs.finish(self.code_id) {
            return;
        }
        let spent = hub
            .db
            .invite(self.code_id)
            .ok()
            .flatten()
            .is_some_and(|i| i.failures >= INVITE_TRIES);
        if !spent {
            return;
        }
        hub.db.delete_invite(self.code_id).ok();
        warn!(code_id = %self.code_id, "invite burned after {INVITE_TRIES} failed runs");
        if let Some(minter) = self.minted_by {
            hub.emit_to(
                minter,
                Event::InviteSpent {
                    code_id: self.code_id.to_owned(),
                },
            );
        }
    }
}

fn internal(e: anyhow::Error) -> AuthRefusal {
    warn!("authentication failed on the server: {e:#}");
    AuthRefusal::Internal
}

fn exporter(connection: &quinn::Connection) -> Result<[u8; 32], AuthError> {
    let mut out = [0u8; 32];
    connection
        .export_keying_material(&mut out, EXPORTER_LABEL, b"")
        .map_err(|_| AuthError::ProtocolMismatch)?;
    Ok(out)
}

/// A random secret for unknown invite ids, so they fail exactly like a wrong secret.
fn decoy_secret() -> String {
    let bytes: [u8; ID_CHARS + SECRET_CHARS] = random();
    invite::InviteCode::from_random(bytes).secret
}

/// PAKE runs in flight per invite id.
#[derive(Default)]
pub struct InviteRuns(HashMap<String, u32>);

impl InviteRuns {
    pub fn start(&mut self, code_id: &str) {
        *self.0.entry(code_id.to_owned()).or_default() += 1;
    }

    /// Ends one run. Returns true if no other run of this invite is in flight.
    pub fn finish(&mut self, code_id: &str) -> bool {
        let Some(runs) = self.0.get_mut(code_id) else {
            return true;
        };
        *runs -= 1;
        if *runs > 0 {
            return false;
        }
        self.0.remove(code_id);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_last_run_of_an_invite_may_burn_it() {
        let mut runs = InviteRuns::default();
        runs.start("K7Q");
        runs.start("K7Q");
        assert!(!runs.finish("K7Q"));
        assert!(runs.finish("K7Q"));
        assert!(runs.0.is_empty());
    }
}
