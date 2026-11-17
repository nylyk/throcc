use std::sync::Arc;

use gpui::{Context, PathPromptOptions, RenderImage, Task, Window};
use throcc_client_core::profile::Profile;
use throcc_client_core::transfer::{self, Export, FRAME_INTERVAL, ImportError, QrCode};

use super::{Dialog, Screen, ThroccApp, set_field};

/// Light modules around a QR code, which scanners need to find it.
const QUIET_ZONE: u32 = 4;
/// The pixels each module of a QR code is drawn with.
pub const QR_MODULE_PIXELS: u32 = 8;
const EXPORT_FILE_NAME: &str = "throcc-identity.throcc";

/// An export made on the Profile page, kept while its dialog is open.
pub struct Exporting {
    pub export: Arc<Export>,
    /// Whether the code shows, which hides the QR code for good.
    pub code_shown: bool,
    pub qr: Option<Arc<RenderImage>>,
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
        if self
            .state
            .current
            .as_ref()
            .is_some_and(|c| c.sharing.is_some())
        {
            self.transfer_problem =
                Some("Stop sharing your screen first, so nobody watching sees the export.".into());
            cx.notify();
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
            let export = sealing.await.map(Arc::new);
            let export = this.update(cx, |app, cx| {
                app.preparing_export = false;
                cx.notify();
                export.map_err(|e| {
                    tracing::warn!("exporting the identity: {e:#}");
                    app.transfer_problem = Some(format!("Couldn’t export your identity: {e:#}"));
                })
            });
            let Ok(Ok(export)) = export else {
                return;
            };
            if !to_file {
                this.update(cx, |app, cx| app.show_export(export, false, cx))
                    .ok();
                return;
            }
            let Ok(chosen) = this.update(cx, |_, cx| {
                let home = std::env::home_dir().unwrap_or_default();
                cx.prompt_for_new_path(&home, Some(EXPORT_FILE_NAME))
            }) else {
                return;
            };
            let Ok(Ok(Some(path))) = chosen.await else {
                return;
            };
            let sealed = export.sealed().to_vec();
            let written = cx
                .background_executor()
                .spawn(async move { std::fs::write(&path, sealed) })
                .await;
            this.update(cx, |app, cx| match written {
                Ok(()) => app.show_export(export, true, cx),
                Err(e) => {
                    app.transfer_problem = Some(format!("Couldn’t save the export: {e}"));
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Opens the export dialog on `export`'s QR codes, cycling through them, or on its code.
    pub fn show_export(&mut self, export: Arc<Export>, code_shown: bool, cx: &mut Context<Self>) {
        let qr = (!code_shown).then(|| qr_image(&export.frame(0)));
        let frames = (!code_shown).then(|| self.cycle_frames(cx));
        self.exporting = Some(Exporting {
            export,
            code_shown,
            qr,
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
                    let Some(exporting) = app.exporting.as_mut().filter(|e| !e.code_shown) else {
                        return false;
                    };
                    exporting.frame = exporting.frame.wrapping_add(1);
                    exporting.qr = Some(qr_image(&exporting.export.frame(exporting.frame)));
                    cx.notify();
                    true
                });
                if !matches!(cycling, Ok(true)) {
                    return;
                }
            }
        })
    }

    /// Hides the QR codes for good and shows the code that opens them.
    pub fn show_export_code(&mut self, cx: &mut Context<Self>) {
        if let Some(exporting) = &mut self.exporting {
            exporting.code_shown = true;
            exporting.qr = None;
            exporting._frames = None;
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
                .spawn(async move { std::fs::read(&path) })
                .await;
            this.update_in(cx, |app, window, cx| {
                match read {
                    Ok(file) if transfer::is_export(&file) => {
                        app.importing = Some(Importing {
                            file: file.into(),
                            running: false,
                            error: None,
                        });
                        set_field(&app.transfer_code, "", true, window, cx);
                        app.set_dialog(Some(Dialog::ImportIdentity), cx);
                    }
                    Ok(_) => app.transfer_problem = Some(ImportError::NotAnExport.to_string()),
                    Err(e) => app.transfer_problem = Some(format!("Couldn’t read that file: {e}")),
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

/// `code` with its quiet zone, dark on light whatever the theme, for gpui's BGRA pixels.
fn qr_image(code: &QrCode) -> Arc<RenderImage> {
    let modules = code.width + 2 * QUIET_ZONE;
    let side = modules * QR_MODULE_PIXELS;
    let picture = image::RgbaImage::from_fn(side, side, |x, y| {
        let (column, row) = (x / QR_MODULE_PIXELS, y / QR_MODULE_PIXELS);
        let inside = |m: u32| m.checked_sub(QUIET_ZONE).filter(|m| *m < code.width);
        let dark = matches!(
            (inside(column), inside(row)),
            (Some(c), Some(r)) if code.dark[(r * code.width + c) as usize]
        );
        image::Rgba(if dark { [0, 0, 0, 255] } else { [255; 4] })
    });
    Arc::new(RenderImage::new(smallvec::smallvec![image::Frame::new(
        picture
    )]))
}
