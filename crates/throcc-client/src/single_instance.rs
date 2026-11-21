use std::io::Read;
use std::time::Duration;

use tokio::sync::mpsc::UnboundedSender;

use crate::APP_ID;

const MAX_LINK_BYTES: u64 = 4096;
const READ_TIMEOUT: Duration = Duration::from_secs(1);

/// Hands `link` to the instance already running and returns `false`, or makes this process the
/// one instance, which sends each later launch's link, if any, to `handed`, and returns `true`.
pub fn claim(link: Option<&str>, handed: UnboundedSender<Option<String>>) -> bool {
    if platform::hand_over(link.unwrap_or_default()).is_ok() {
        return false;
    }
    if let Err(e) = platform::listen(handed) {
        tracing::warn!("other launches can't reach this one: {e}");
    }
    true
}

fn read_link(from: impl Read) -> Option<String> {
    let mut link = String::new();
    from.take(MAX_LINK_BYTES).read_to_string(&mut link).ok()?;
    Some(link).filter(|l| !l.is_empty())
}

#[cfg(target_os = "linux")]
mod platform {
    use std::io::{self, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::PathBuf;

    use super::*;

    fn path() -> io::Result<PathBuf> {
        let dir = std::env::var_os("XDG_RUNTIME_DIR")
            .ok_or_else(|| io::Error::other("XDG_RUNTIME_DIR is not set"))?;
        let dir = PathBuf::from(dir);
        // Each flatpak instance has a runtime dir of its own; only this subdirectory is shared.
        Ok(match std::env::var_os("FLATPAK_ID") {
            Some(_) => dir.join("app").join(APP_ID).join("throcc.sock"),
            None => dir.join(format!("{APP_ID}.sock")),
        })
    }

    pub fn hand_over(link: &str) -> io::Result<()> {
        UnixStream::connect(path()?)?.write_all(link.as_bytes())
    }

    pub fn listen(handed: UnboundedSender<Option<String>>) -> io::Result<()> {
        let path = path()?;
        // A socket file that refused `hand_over` was left by an instance that has ended.
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path)?;
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
                if handed.send(read_link(stream)).is_err() {
                    return;
                }
            }
        });
        Ok(())
    }
}

#[cfg(windows)]
mod platform {
    use std::io::{self, Write};

    use tokio::io::AsyncReadExt;
    use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};

    use super::*;

    fn name() -> String {
        let user = std::env::var("USERNAME").unwrap_or_default();
        format!(r"\\.\pipe\{APP_ID}-{user}")
    }

    pub fn hand_over(link: &str) -> io::Result<()> {
        std::fs::OpenOptions::new()
            .write(true)
            .open(name())?
            .write_all(link.as_bytes())
    }

    fn serve(first: bool) -> io::Result<NamedPipeServer> {
        ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .create(name())
    }

    pub fn listen(handed: UnboundedSender<Option<String>>) -> io::Result<()> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let mut server = runtime.block_on(async { serve(true) })?;
        std::thread::spawn(move || {
            runtime.block_on(async move {
                loop {
                    let connected = server.connect().await;
                    let Ok(next) = serve(false) else { return };
                    let client = std::mem::replace(&mut server, next);
                    if connected.is_err() {
                        continue;
                    }
                    let mut bytes = Vec::new();
                    let mut limited = client.take(MAX_LINK_BYTES);
                    let read = limited.read_to_end(&mut bytes);
                    let link = match tokio::time::timeout(READ_TIMEOUT, read).await {
                        Ok(Ok(_)) => read_link(bytes.as_slice()),
                        _ => None,
                    };
                    if handed.send(link).is_err() {
                        return;
                    }
                }
            });
        });
        Ok(())
    }
}
