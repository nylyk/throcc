use std::sync::{Arc, Mutex};

use throcc_client_core::fountain::Collector;
use throcc_client_core::transfer::{self, Export, ImportError};

use crate::{Core, CoreError, lock};

/// A QR code's modules with the quiet zone around them, row by row from the top left.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct QrFrame {
    pub width: u32,
    pub dark: Vec<bool>,
}

/// How far scanning an identity has come.
#[derive(Clone, Copy, Debug, PartialEq, uniffi::Record)]
pub struct ScanProgress {
    /// The share of the identity received, from 0 to 1.
    pub fraction: f32,
    /// How many seconds the rest should take, once there is a rate to tell.
    pub remaining_seconds: Option<f64>,
    /// Whether the whole identity is in, ready for its code.
    pub done: bool,
}

/// This install's identity sealed under a new code, to save as a file or show as QR codes.
#[derive(uniffi::Object)]
pub struct IdentityExport(Export);

#[uniffi::export]
impl IdentityExport {
    /// The code that opens the export, shown only once it has been sent.
    pub fn code(&self) -> String {
        self.0.code()
    }

    /// The export as a file holds it.
    pub fn file(&self) -> Vec<u8> {
        self.0.sealed().to_vec()
    }

    /// QR code `seq` of the endless run that carries the export, each shown for
    /// `qr_frame_millis`.
    pub fn frame(&self, seq: u32) -> QrFrame {
        let frame = self.0.frame(seq);
        QrFrame {
            width: frame.width,
            dark: frame.dark,
        }
    }
}

/// Collects the QR codes of an export, in any order, until the export is whole.
#[derive(uniffi::Object)]
pub struct IdentityScanner(Mutex<Collector>);

#[uniffi::export]
impl IdentityScanner {
    #[uniffi::constructor]
    pub fn new() -> Arc<Self> {
        Arc::new(Self(Mutex::new(Collector::new())))
    }

    /// Takes in the text of a scanned QR code, which may be one taken already or none of an
    /// export's.
    pub fn add(&self, text: String) -> ScanProgress {
        let mut collector = lock(&self.0);
        collector.add(&text);
        let progress = collector.progress();
        ScanProgress {
            fraction: progress.fraction,
            remaining_seconds: progress.remaining.map(|r| r.as_secs_f64()),
            done: collector.blob().is_some(),
        }
    }

    /// The whole export, as a file holds it, once every part is in.
    pub fn export(&self) -> Option<Vec<u8>> {
        lock(&self.0).blob().map(<[u8]>::to_vec)
    }
}

#[uniffi::export]
impl Core {
    /// Seals this install's identity, profile, servers, and verified and blocked people
    /// under a new code, refused while our screen is shared. A made-up identity in the mock.
    pub fn export_identity(&self) -> Result<Arc<IdentityExport>, CoreError> {
        let state = self.client_state();
        if let Some(refusal) = state.as_deref().and_then(transfer::sharing_refusal) {
            return Err(anyhow::anyhow!(refusal).into());
        }
        let export = if self.mock {
            transfer::sample_export()
        } else {
            let profile = lock(&self.settings).profile.clone();
            transfer::export(profile.as_ref(), &self.verified, &self.blocked)?
        };
        Ok(Arc::new(IdentityExport(export)))
    }

    /// Makes the identity in `file`, opened with the `code` the user typed, this device's,
    /// with its servers and people, and keeps its profile. Refused once servers know this
    /// device's own identity. The mock only opens it. It blocks for about a second.
    pub fn import_identity(&self, file: Vec<u8>, code: String) -> Result<(), CoreError> {
        let opened = transfer::open(&file, &code).map_err(failed)?;
        let profile = if self.mock {
            opened.profile()
        } else {
            opened
                .apply(&self.verified, &self.blocked)
                .map_err(failed)?
        };
        if let Some(profile) = profile {
            self.change_settings(|settings| settings.profile = Some(profile));
        }
        Ok(())
    }
}

/// Whether `file` looks like an exported identity, before anyone types its code.
#[uniffi::export]
pub fn is_identity_export(file: Vec<u8>) -> bool {
    transfer::is_export(&file)
}

/// The largest export file an import takes; a bigger one is no export.
#[uniffi::export]
pub fn max_export_bytes() -> u64 {
    transfer::MAX_EXPORT_BYTES as u64
}

/// How long each QR code of an export is shown, in milliseconds.
#[uniffi::export]
pub fn qr_frame_millis() -> u32 {
    transfer::FRAME_INTERVAL.as_millis() as u32
}

fn failed(error: ImportError) -> CoreError {
    CoreError::Failed {
        reason: error.to_string(),
    }
}
