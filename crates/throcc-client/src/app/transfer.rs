use std::sync::Arc;

use gpui::{Context, PathPromptOptions, Task, Window};
use throcc_client_core::profile::Profile;
use throcc_client_core::transfer::{self, Export, FRAME_INTERVAL, ImportError, QrCode};

use super::{Dialog, Screen, ThroccApp, set_field};

const EXPORT_FILE_NAME: &str = "throcc-identity.throcc";

/// An export made on the Profile page, kept while its dialog is open.
pub struct Exporting {
    pub export: Arc<Export>,
    /// The QR code on show, none once the code that opens the export shows in its place.
    pub qr: Option<QrCode>,
    /// The code as a QR code, for a device that scanned the export.
    pub code_qr: Option<QrCode>,
    frame: u32,
    _frames: Option<Task<()>>,
}

/// A file picked to import, kept while its code is asked for.
pub struct Importing {
    pub file: Arc<[u8]>,
    pub running: bool,
    pub error: Option<String>,
}

impl ThroccApp {
    /// Seals our identity under a new code, then shows it as QR codes or saves it to a file
    /// the user picks, refused while our screen is shared.
    pub fn export_identity(&mut self, to_file: bool, cx: &mut Context<Self>) {
        self.transfer_problem = None;
        if self.refuse_export_while_sharing(cx) {
            return;
        }
        self.preparing_export = true;
        cx.notify();
        let (mock, profile) = (self.mock, self.settings.profile.clone());
        let (verified, blocked) = (self.verified.clone(), self.blocked.clone());
        let sealing = cx.background_executor().spawn(async move {
            if mock {
                return Ok(transfer::sample_export());
            }
            transfer::export(profile.as_ref(), &verified, &blocked)
        });
        cx.spawn(async move |this, cx| {
            let export = sealing.await.map(Arc::new).map_err(|e| {
                tracing::warn!("exporting the identity: {e:#}");
                format!("Couldn’t export your identity: {e:#}")
            });
            let shown = match export {
                Ok(export) if to_file => save_export(&this, cx, export).await,
                Ok(export) => Ok(Some((export, false))),
                Err(problem) => Err(problem),
            };
            this.update(cx, |app, cx| {
                app.preparing_export = false;
                match shown {
                    Ok(Some((export, code_shown))) => app.show_export(export, code_shown, cx),
                    Ok(None) => {}
                    Err(problem) => app.transfer_problem = Some(problem),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Refuses an export while our screen is shared, closing one on screen and saying why.
    pub(super) fn refuse_export_while_sharing(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(refusal) = transfer::sharing_refusal(&self.state) else {
            return false;
        };
        self.transfer_problem = Some(refusal.into());
        if self.exporting.is_some() {
            self.set_dialog(None, cx);
        }
        cx.notify();
        true
    }

    /// Opens the export dialog on `export`'s QR codes, cycling through them, or on its code.
    /// Refused once our screen is shared.
    pub fn show_export(&mut self, export: Arc<Export>, code_shown: bool, cx: &mut Context<Self>) {
        if self.refuse_export_while_sharing(cx) {
            return;
        }
        let qr = (!code_shown).then(|| export.frame(0));
        let frames = (!code_shown).then(|| self.cycle_frames(cx));
        self.exporting = Some(Exporting {
            export,
            qr,
            code_qr: None,
            frame: 0,
            _frames: frames,
        });
        self.set_dialog(Some(Dialog::ExportIdentity), cx);
    }

    fn cycle_frames(&self, cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(FRAME_INTERVAL).await;
                let cycling = this.update(cx, |app, cx| {
                    let Some(exporting) = app.exporting.as_mut().filter(|e| e.qr.is_some()) else {
                        return false;
                    };
                    exporting.frame = exporting.frame.wrapping_add(1);
                    exporting.qr = Some(exporting.export.frame(exporting.frame));
                    cx.notify();
                    true
                });
                if !matches!(cycling, Ok(true)) {
                    return;
                }
            }
        })
    }

    /// Hides the QR codes for good and shows the code that opens them, as text and as a QR
    /// code, refused once our screen is shared.
    pub fn show_export_code(&mut self, cx: &mut Context<Self>) {
        if self.refuse_export_while_sharing(cx) {
            return;
        }
        if let Some(exporting) = &mut self.exporting {
            exporting.qr = None;
            exporting._frames = None;
            exporting.code_qr = Some(exporting.export.code_qr());
        }
        cx.notify();
    }

    /// Asks for an exported identity's file, then for its code.
    pub fn choose_import_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.transfer_problem = None;
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Import".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = chosen.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let read = cx
                .background_executor()
                .spawn(async move { read_capped(&path) })
                .await;
            this.update_in(cx, |app, window, cx| {
                match read {
                    Ok(Some(file)) if transfer::is_export(&file) => {
                        app.importing = Some(Importing {
                            file: file.into(),
                            running: false,
                            error: None,
                        });
                        set_field(&app.transfer_code, "", true, window, cx);
                        app.set_dialog(Some(Dialog::ImportIdentity), cx);
                    }
                    Ok(_) => app.transfer_problem = Some(ImportError::NotAnExport.to_string()),
                    Err(e) => {
                        tracing::warn!("reading an identity export: {e}");
                        app.transfer_problem = Some("Couldn’t read that file.".into());
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Opens the picked export with the typed code and makes its identity ours.
    pub fn import_identity(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let code = self.transfer_code.read(cx).value().to_string();
        let Some(importing) = self.importing.as_mut().filter(|i| !i.running) else {
            return;
        };
        importing.running = true;
        importing.error = None;
        cx.notify();
        let file = importing.file.clone();
        let (mock, verified, blocked) = (self.mock, self.verified.clone(), self.blocked.clone());
        let opening = cx.background_executor().spawn(async move {
            let opened = transfer::open(&file, &code)?;
            if mock {
                return Ok(opened.profile());
            }
            opened.apply(&verified, &blocked)
        });
        cx.spawn_in(window, async move |this, cx| {
            let imported = opening.await;
            this.update_in(cx, |app, window, cx| match imported {
                Ok(profile) => app.imported(profile, window, cx),
                Err(e) => {
                    if let Some(importing) = &mut app.importing {
                        importing.running = false;
                        importing.error = Some(e.to_string());
                    }
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn imported(&mut self, profile: Option<Profile>, window: &mut Window, cx: &mut Context<Self>) {
        self.importing = None;
        self.set_dialog(None, cx);
        self.reload_known_servers();
        self.list_verified();
        self.list_blocked();
        if let Some(profile) = profile {
            self.keep_profile(profile, window, cx);
            self.show_screen(Screen::Join, window, cx);
        }
    }
}

/// Asks where to save `export`, then writes it there. `None` when the user cancelled.
async fn save_export(
    this: &gpui::WeakEntity<ThroccApp>,
    cx: &mut gpui::AsyncApp,
    export: Arc<Export>,
) -> Result<Option<(Arc<Export>, bool)>, String> {
    let chosen = this
        .update(cx, |_, cx| {
            let home = std::env::home_dir().unwrap_or_default();
            cx.prompt_for_new_path(&home, Some(EXPORT_FILE_NAME))
        })
        .map_err(|e| e.to_string())?;
    let Ok(Ok(Some(path))) = chosen.await else {
        return Ok(None);
    };
    let sealed = export.sealed().to_vec();
    cx.background_executor()
        .spawn(async move { std::fs::write(&path, sealed) })
        .await
        .map_err(|e| format!("Couldn’t save the export: {e}"))?;
    Ok(Some((export, true)))
}

/// The file at `path`, or `None` when it is larger than any export.
fn read_capped(path: &std::path::Path) -> std::io::Result<Option<Vec<u8>>> {
    if std::fs::metadata(path)?.len() > transfer::MAX_EXPORT_BYTES as u64 {
        return Ok(None);
    }
    std::fs::read(path).map(Some)
}
