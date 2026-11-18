use std::time::Duration;

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use throcc_crypto::identity::IdentityKey;
use throcc_crypto::transfer::{self, TransferCode, TransferError};
use throcc_proto::Fingerprint;
use throcc_proto::ids::{Hash, UserId};
use throcc_proto::messages::{MAX_AVATAR_BYTES, valid_address, valid_name};
use zeroize::{Zeroize, Zeroizing};

use crate::avatars;
use crate::blocked::{BlockedRecord, BlockedRecords, BlockedStore};
use crate::fountain::{self, Fountain};
use crate::keystore::{self, IdentityInUse, Keystore};
use crate::picture;
use crate::profile::Profile;
use crate::server_look::ServerLook;
use crate::servers::{Hex, Servers};
use crate::state::State;
use crate::unix_now;
use crate::verified::{KeyChange, SeenAs, VerifiedEntry, VerifiedRecords, VerifiedStore};

/// The largest export, as a file or as QR codes, that an import takes.
pub const MAX_EXPORT_BYTES: usize = fountain::MAX_BLOB_BYTES;
/// How long the source shows each QR code.
pub const FRAME_INTERVAL: Duration = Duration::from_millis(50);
/// How old an export may be when it is imported.
const MAX_AGE: Duration = Duration::from_secs(60 * 60);
/// How far ahead of this device's clock an export's creation time may be.
const MAX_AHEAD: Duration = Duration::from_secs(5 * 60);
/// The light modules on every side of a QR code, which scanners need to find it.
const QUIET_ZONE: usize = 4;

/// Everything that moves an identity to another device, sealed as one blob.
#[derive(Default, Serialize, Deserialize)]
struct Payload {
    /// Seconds since the Unix epoch when the source sealed it.
    created: u64,
    identity: Seed,
    profile: Option<SentProfile>,
    servers: Vec<SentServer>,
    verified: Vec<SentVerified>,
    changed: Vec<SentChange>,
    blocked: Vec<SentBlocked>,
}

#[derive(Default, Serialize, Deserialize)]
struct Seed([u8; 32]);

impl Drop for Seed {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

#[derive(Serialize, Deserialize)]
struct SentProfile {
    name: String,
    picture: Option<Vec<u8>>,
}

#[derive(Serialize, Deserialize)]
struct SentServer {
    address: String,
    fingerprint: [u8; 32],
    name: Option<String>,
    picture: Option<Vec<u8>>,
}

#[derive(Serialize, Deserialize)]
struct SentVerified {
    key: [u8; 32],
    name: String,
    avatar: Option<[u8; 32]>,
    /// The server and user id pairs the key was seen under.
    seen: Vec<([u8; 32], u64)>,
}

#[derive(Serialize, Deserialize)]
struct SentChange {
    server: [u8; 32],
    user: u64,
    now: [u8; 32],
}

#[derive(Serialize, Deserialize)]
struct SentBlocked {
    key: [u8; 32],
    name: String,
    avatar: Option<[u8; 32]>,
    server: Option<[u8; 32]>,
}

/// A QR code's modules with the quiet zone around them, row by row from the top left.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QrCode {
    pub width: u32,
    pub dark: Vec<bool>,
}

/// This install's identity sealed under a new code, ready to save as a file or show as
/// QR codes.
pub struct Export {
    code: TransferCode,
    fountain: Fountain,
    /// The one QR version every frame is drawn at, so the code never changes size.
    version: qrcode::Version,
}

impl Export {
    /// The code that opens the export, to show once it has been sent.
    pub fn code(&self) -> String {
        self.code.to_string()
    }

    /// The sealed export, which a file holds as it is.
    pub fn sealed(&self) -> &[u8] {
        self.fountain.blob()
    }

    /// QR code `seq` of the endless run that carries [`Self::sealed`].
    pub fn frame(&self, seq: u32) -> QrCode {
        let text = self.fountain.frame(seq);
        let code = qrcode::QrCode::with_version(text, self.version, qrcode::EcLevel::L)
            .expect("a frame fits the version its length needs");
        modules(&code)
    }

    /// [`Self::code`] as a QR code, for a device that scanned the export.
    pub fn code_qr(&self) -> QrCode {
        let code = qrcode::QrCode::with_error_correction_level(self.code(), qrcode::EcLevel::M)
            .expect("a code fits a QR code");
        modules(&code)
    }
}

/// `code`'s modules with the quiet zone around them.
fn modules(code: &qrcode::QrCode) -> QrCode {
    let width = code.width() + 2 * QUIET_ZONE;
    let mut dark = vec![false; width * width];
    for (row, colors) in code.to_colors().chunks(code.width()).enumerate() {
        let start = (row + QUIET_ZONE) * width + QUIET_ZONE;
        for (module, color) in dark[start..].iter_mut().zip(colors) {
            *module = *color == qrcode::Color::Dark;
        }
    }
    QrCode {
        width: width as u32,
        dark,
    }
}

/// Whether scanned `text` is the code that opens an export, rather than one of its frames.
pub fn is_code(text: &str) -> bool {
    TransferCode::parse(text).is_some()
}

/// Seals this install's identity, `profile`, servers, and verified and blocked people for
/// another device.
pub fn export(
    profile: Option<&Profile>,
    verified: &VerifiedStore,
    blocked: &BlockedStore,
) -> Result<Export> {
    let keystore = Keystore::open_default()?;
    let servers = keystore.known_servers.iter().map(|(address, fingerprint)| {
        let look = ServerLook::load(keystore.dir(), &fingerprint).unwrap_or_default();
        let picture = look
            .avatar
            .and_then(|hash| avatars::load(keystore.cache_dir(), hash))
            .filter(|bytes| picture::showable(bytes));
        SentServer {
            address: address.to_owned(),
            fingerprint: fingerprint.0,
            name: look.name,
            picture: picture.map(|bytes| bytes.to_vec()),
        }
    });
    let mut payload = Payload {
        created: unix_now()?,
        identity: Seed(*keystore.identity().seed()),
        profile: profile.map(|profile| SentProfile {
            name: profile.name.clone(),
            picture: profile.picture.as_deref().map(<[u8]>::to_vec),
        }),
        servers: servers.collect(),
        ..people_payload(verified.records(), blocked.records())
    };
    seal_dropping_server_pictures(&mut payload)
}

/// `payload` sealed, after dropping server pictures, the largest first, until the export
/// fits in [`MAX_EXPORT_BYTES`].
fn seal_dropping_server_pictures(payload: &mut Payload) -> Result<Export> {
    loop {
        if let Some(export) = seal(payload) {
            return Ok(export);
        }
        let largest = payload
            .servers
            .iter_mut()
            .filter(|server| server.picture.is_some())
            .max_by_key(|server| server.picture.as_ref().map_or(0, Vec::len));
        match largest {
            Some(server) => server.picture = None,
            None => bail!(
                "its size would be greater than {} KiB",
                MAX_EXPORT_BYTES / 1024
            ),
        }
    }
}

/// Why an export is refused while `state` shows our screen shared, if it is.
pub fn sharing_refusal(state: &State) -> Option<&'static str> {
    let sharing = state.current.as_ref()?.sharing.is_some();
    sharing.then_some("Stop sharing your screen first, so nobody watching sees the export.")
}

/// An export of a made-up identity, for the sample screens.
pub fn sample_export() -> Export {
    let payload = Payload {
        created: unix_now().unwrap_or_default(),
        identity: Seed(*IdentityKey::generate().seed()),
        profile: Some(SentProfile {
            name: "Ilse".into(),
            picture: Some(include_bytes!("../assets/mock/sleepyfox.jpg").to_vec()),
        }),
        ..Payload::default()
    };
    seal(&payload).expect("the sample fits an export")
}

/// `payload` sealed under a new code, or `None` when the export would be larger than
/// [`MAX_EXPORT_BYTES`]. The plaintext is written once into a buffer wiped afterwards.
fn seal(payload: &Payload) -> Option<Export> {
    let mut buffer = Zeroizing::new(vec![0; MAX_EXPORT_BYTES - transfer::OVERHEAD_BYTES]);
    let plaintext = postcard::to_slice(payload, &mut buffer).ok()?;
    let code = TransferCode::generate();
    let fountain = Fountain::new(transfer::seal(&code, plaintext));
    Some(Export {
        version: frame_version(fountain.frame(0).len()),
        fountain,
        code,
    })
}

/// The smallest QR version that holds any frame of `chars` base 45 characters. Frames with
/// long runs of digits would fit a smaller one.
fn frame_version(chars: usize) -> qrcode::Version {
    qrcode::QrCode::with_error_correction_level("A".repeat(chars), qrcode::EcLevel::L)
        .expect("a frame fits a QR code")
        .version()
}

fn people_payload(verified: VerifiedRecords, blocked: BlockedRecords) -> Payload {
    let verified_people = verified
        .verified
        .into_iter()
        .map(|(key, entry)| SentVerified {
            key: key.0,
            name: entry.name,
            avatar: entry.avatar.map(|hash| hash.0),
            seen: entry
                .seen
                .into_iter()
                .map(|at| (at.server.0, at.user.0))
                .collect(),
        });
    let changed = verified.changed.into_iter().map(|change| SentChange {
        server: change.at.server.0,
        user: change.at.user.0,
        now: change.now.0,
    });
    let blocked = blocked.into_iter().map(|(key, record)| SentBlocked {
        key: key.0,
        name: record.name,
        avatar: record.avatar.map(|hash| hash.0),
        server: record.server.map(|server| server.0),
    });
    Payload {
        verified: verified_people.collect(),
        changed: changed.collect(),
        blocked: blocked.collect(),
        ..Payload::default()
    }
}

/// Why an identity couldn't be imported, in words for the user.
#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error("That isn’t an identity exported from throcc.")]
    NotAnExport,
    #[error("A code is 12 letters and digits, like 7K3M-Q9XZ-AB12.")]
    BadCode,
    #[error("That code doesn’t open this identity. Check it on the other device.")]
    WrongCode,
    #[error("This identity was exported more than an hour ago. Export it again.")]
    TooOld,
    #[error(
        "This identity was exported later than this device’s clock says it is now. Check the time on both devices."
    )]
    FromTheFuture,
    #[error("This device already has an identity that servers know.")]
    InUse,
    #[error("Couldn’t import the identity: {0:#}")]
    Failed(anyhow::Error),
}

/// Whether `bytes` look like an export, before anyone types its code.
pub fn is_export(bytes: &[u8]) -> bool {
    transfer::looks_sealed(bytes)
}

/// An export opened with its code and checked, not yet applied to this install.
pub struct Opened(Payload);

/// Opens `sealed` with the code the user typed, refusing an export older than an hour.
pub fn open(sealed: &[u8], typed_code: &str) -> Result<Opened, ImportError> {
    let code = TransferCode::parse(typed_code).ok_or(ImportError::BadCode)?;
    let plaintext = transfer::open(&code, sealed).map_err(|e| match e {
        TransferError::NotATransfer => ImportError::NotAnExport,
        TransferError::WrongCode => ImportError::WrongCode,
    })?;
    let payload: Payload =
        postcard::from_bytes(&plaintext).map_err(|_| ImportError::NotAnExport)?;
    let now = unix_now().map_err(|e| ImportError::Failed(e.into()))?;
    if payload.created > now + MAX_AHEAD.as_secs() {
        return Err(ImportError::FromTheFuture);
    }
    if now.saturating_sub(payload.created) > MAX_AGE.as_secs() {
        return Err(ImportError::TooOld);
    }
    Ok(Opened(payload))
}

impl Opened {
    /// The name and picture the other device keeps, where a server would take them.
    pub fn profile(&self) -> Option<Profile> {
        let sent = self.0.profile.as_ref()?;
        Some(Profile {
            name: valid_name(&sent.name)?.to_owned(),
            picture: sent
                .picture
                .as_deref()
                .filter(|bytes| fits_and_shows(bytes))
                .map(Into::into),
        })
    }

    /// Makes the identity this install's, with the servers it joined and the people it
    /// verified and blocked, and returns the profile to keep. Refused, before anything is
    /// written, while a server may know this install's own identity. The servers and their
    /// looks are kept last, in one write, so an import cut short can be tried again.
    pub fn apply(
        self,
        verified: &VerifiedStore,
        blocked: &BlockedStore,
    ) -> Result<Option<Profile>, ImportError> {
        let failed = ImportError::Failed;
        if keystore::identity_in_use().map_err(failed)? {
            return Err(ImportError::InUse);
        }
        let profile = self.profile();
        let payload = self.0;
        let dir = keystore::config_dir().map_err(failed)?;
        let cache = keystore::cache_dir().map_err(failed)?;
        let servers: Vec<(&SentServer, Option<&[u8]>)> = payload
            .servers
            .iter()
            .filter(|server| valid_address(&server.address))
            .map(|server| {
                let picture = server.picture.as_deref().filter(|b| fits_and_shows(b));
                (server, picture)
            })
            .collect();
        for picture in servers.iter().filter_map(|(_, picture)| *picture) {
            avatars::save(&cache, Hash::of(picture), picture).map_err(failed)?;
        }
        verified.merge(verified_records(&payload)).map_err(failed)?;
        blocked.merge(blocked_records(&payload)).map_err(failed)?;
        keystore::import_identity(&IdentityKey::from_seed(&payload.identity.0)).map_err(
            |e| match e.downcast_ref::<IdentityInUse>() {
                Some(_) => ImportError::InUse,
                None => ImportError::Failed(e),
            },
        )?;
        Servers::update(&dir, |kept| {
            for (server, picture) in &servers {
                let look = kept.server_mut(&Fingerprint(server.fingerprint));
                look.name = server.name.as_deref().and_then(valid_name).map(Into::into);
                look.avatar = picture.map(|bytes| Hex(Hash::of(bytes).0));
                kept.known
                    .insert(server.address.clone(), Hex(server.fingerprint));
            }
        })
        .map_err(failed)?;
        Ok(profile)
    }
}

fn verified_records(payload: &Payload) -> VerifiedRecords {
    let verified = payload.verified.iter().filter_map(|sent| {
        let entry = VerifiedEntry {
            name: valid_name(&sent.name)?.to_owned(),
            avatar: sent.avatar.map(Hex),
            seen: sent
                .seen
                .iter()
                .map(|(server, user)| seen_as(*server, *user))
                .collect(),
        };
        Some((Hex(sent.key), entry))
    });
    let changed = payload.changed.iter().map(|sent| KeyChange {
        at: seen_as(sent.server, sent.user),
        now: Hex(sent.now),
    });
    VerifiedRecords {
        verified: verified.collect(),
        changed: changed.collect(),
    }
}

fn blocked_records(payload: &Payload) -> BlockedRecords {
    let blocked = payload.blocked.iter().map(|sent| {
        let record = BlockedRecord {
            name: valid_name(&sent.name).unwrap_or_default().to_owned(),
            avatar: sent.avatar.map(Hex),
            server: sent.server.map(Hex),
        };
        (Hex(sent.key), record)
    });
    blocked.collect()
}

fn seen_as(server: [u8; 32], user: u64) -> SeenAs {
    SeenAs {
        server: Hex(server),
        user: UserId(user),
    }
}

fn fits_and_shows(picture: &[u8]) -> bool {
    picture.len() <= MAX_AVATAR_BYTES && picture::showable(picture)
}

#[cfg(test)]
mod tests {
    use super::*;

    const JPEG: &[u8] = include_bytes!("../assets/mock/nori.jpg");

    fn payload(created: u64) -> Payload {
        Payload {
            created,
            identity: Seed([7; 32]),
            profile: Some(SentProfile {
                name: "Ilse".into(),
                picture: Some(JPEG.to_vec()),
            }),
            servers: vec![SentServer {
                address: "voice.example.net".into(),
                fingerprint: [1; 32],
                name: Some("Moss".into()),
                picture: Some(JPEG.to_vec()),
            }],
            ..Payload::default()
        }
    }

    fn sealed_with_code(payload: &Payload) -> (String, Vec<u8>) {
        let export = seal(payload).unwrap();
        (export.code(), export.sealed().to_vec())
    }

    #[test]
    fn an_export_opens_with_its_code_and_keeps_its_profile() {
        let (code, sealed) = sealed_with_code(&payload(unix_now().unwrap()));
        assert!(is_export(&sealed));
        let opened = open(&sealed, &code.to_lowercase()).unwrap();
        assert_eq!(opened.0.identity.0, [7; 32]);
        assert_eq!(opened.0.servers[0].address, "voice.example.net");
        let profile = opened.profile().unwrap();
        assert_eq!(profile.name, "Ilse");
        assert_eq!(profile.picture.as_deref(), Some(JPEG));
    }

    #[test]
    fn a_wrong_or_malformed_code_and_other_files_are_refused() {
        let (code, sealed) = sealed_with_code(&payload(unix_now().unwrap()));
        let other = if code.starts_with('0') {
            "1111-1111-1111"
        } else {
            "0000-0000-0000"
        };
        assert!(matches!(open(&sealed, other), Err(ImportError::WrongCode)));
        assert!(matches!(open(&sealed, "1234"), Err(ImportError::BadCode)));
        assert!(matches!(
            open(b"\x89PNG", &code),
            Err(ImportError::NotAnExport)
        ));
        assert!(!is_export(b"\x89PNG"));
    }

    #[test]
    fn an_old_export_or_one_from_the_future_is_refused() {
        let now = unix_now().unwrap();
        let (code, old) = sealed_with_code(&payload(now - MAX_AGE.as_secs() - 1));
        assert!(matches!(open(&old, &code), Err(ImportError::TooOld)));
        let (code, ahead) = sealed_with_code(&payload(now + MAX_AHEAD.as_secs() + 60));
        assert!(matches!(
            open(&ahead, &code),
            Err(ImportError::FromTheFuture)
        ));
        let (code, recent) = sealed_with_code(&payload(now - MAX_AGE.as_secs() + 60));
        assert!(open(&recent, &code).is_ok());
    }

    #[test]
    fn a_picture_that_is_no_jpeg_or_a_bad_name_counts_as_none() {
        let mut sent = payload(unix_now().unwrap());
        sent.profile.as_mut().unwrap().picture = Some(b"\x89PNG".to_vec());
        let (code, sealed) = sealed_with_code(&sent);
        assert_eq!(
            open(&sealed, &code).unwrap().profile().unwrap().picture,
            None
        );

        sent.profile.as_mut().unwrap().name = "  ".into();
        let (code, sealed) = sealed_with_code(&sent);
        assert_eq!(open(&sealed, &code).unwrap().profile(), None);
    }

    #[test]
    fn people_survive_the_trip_and_merge_into_what_is_kept() {
        let here = Fingerprint([1; 32]);
        let there = VerifiedStore::in_memory();
        there
            .lock()
            .verify(&here, UserId(4), &[4; 32], "Mara", Some(Hash([6; 32])))
            .unwrap();
        there.lock().presented(&here, UserId(4), &[9; 32]).unwrap();
        let blocked_there = BlockedStore::in_memory();
        blocked_there
            .block(crate::state::BlockedPerson {
                key: Fingerprint([5; 32]),
                name: "Teo".into(),
                avatar: None,
                server: Some(here),
            })
            .unwrap();
        let mut sent = people_payload(there.records(), blocked_there.records());
        sent.created = unix_now().unwrap();
        let (code, sealed) = sealed_with_code(&sent);
        let opened = open(&sealed, &code).unwrap();

        let verified = VerifiedStore::in_memory();
        verified
            .lock()
            .verify(&here, UserId(4), &[8; 32], "Someone", None)
            .unwrap();
        verified.merge(verified_records(&opened.0)).unwrap();
        let list = verified.list();
        assert_eq!(list.len(), 2);
        let mara = list.iter().find(|p| p.key == Fingerprint([4; 32])).unwrap();
        assert_eq!(
            (mara.name.as_str(), mara.avatar),
            ("Mara", Some(Hash([6; 32])))
        );
        assert!(
            mara.servers.is_empty(),
            "the pair stays with the key seen here"
        );
        assert_eq!(verified.lock().verified_at(&here, UserId(4)), Some([8; 32]));
        assert_eq!(verified.lock().changed_at(&here, UserId(4)), None);

        let fresh = VerifiedStore::in_memory();
        fresh.merge(verified_records(&opened.0)).unwrap();
        assert_eq!(fresh.lock().verified_at(&here, UserId(4)), Some([4; 32]));
        assert_eq!(fresh.lock().changed_at(&here, UserId(4)), Some([9; 32]));

        let blocked = BlockedStore::in_memory();
        blocked.merge(blocked_records(&opened.0)).unwrap();
        assert!(blocked.is_blocked(&[5; 32]));
        assert_eq!(blocked.list()[0].server, Some(here));
    }

    #[test]
    fn server_pictures_go_largest_first_until_the_export_fits() {
        let server = |name: &str, picture: usize| SentServer {
            address: format!("{name}.example.net"),
            fingerprint: [1; 32],
            name: None,
            picture: Some(vec![7; picture]),
        };
        let mut payload = payload(unix_now().unwrap());
        payload.servers = vec![
            server("small", 1000),
            server("huge", MAX_EXPORT_BYTES - 10_000),
            server("big", 20_000),
        ];
        let export = seal_dropping_server_pictures(&mut payload).unwrap();
        assert!(export.sealed().len() <= MAX_EXPORT_BYTES);
        let kept: Vec<bool> = payload
            .servers
            .iter()
            .map(|s| s.picture.is_some())
            .collect();
        assert_eq!(kept, [true, false, true]);

        payload.verified = (0..30_000u32)
            .map(|i| SentVerified {
                key: [0; 32],
                name: i.to_string(),
                avatar: None,
                seen: Vec::new(),
            })
            .collect();
        assert!(seal_dropping_server_pictures(&mut payload).is_err());
    }

    #[test]
    fn every_qr_frame_has_the_same_size() {
        let export = sample_export();
        let first = export.frame(0);
        assert_eq!(first.dark.len(), (first.width * first.width) as usize);
        for seq in 1..2000 {
            assert_eq!(export.frame(seq).width, first.width, "frame {seq}");
        }
    }

    /// The text a scanner reads from `code`, drawn three pixels a module.
    fn scanned(code: &QrCode) -> String {
        const SCALE: usize = 3;
        let width = code.width as usize;
        let side = width * SCALE;
        let mut image = rqrr::PreparedImage::prepare_from_greyscale(side, side, |x, y| {
            if code.dark[y / SCALE * width + x / SCALE] {
                0
            } else {
                255
            }
        });
        let grids = image.detect_grids();
        assert_eq!(grids.len(), 1);
        grids[0].decode().unwrap().1
    }

    #[test]
    fn the_code_scans_back_as_a_code_and_frames_do_not() {
        let export = sample_export();
        let scanned_code = scanned(&export.code_qr());
        assert_eq!(scanned_code, export.code());
        assert!(is_code(&scanned_code));
        assert!(!is_code(&scanned(&export.frame(0))));
    }

    #[test]
    fn an_export_scanned_frame_by_frame_opens_with_its_code() {
        let export = sample_export();
        let mut collector = crate::fountain::Collector::new();
        for seq in (0..2000).step_by(2) {
            collector.add(&scanned(&export.frame(seq)));
            if collector.blob().is_some() {
                break;
            }
        }
        let opened = open(collector.blob().unwrap(), &export.code()).unwrap();
        assert_eq!(opened.profile().unwrap().name, "Ilse");
    }
}
