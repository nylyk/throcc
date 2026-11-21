use std::io::Read;
use std::time::Duration;

use tokio::sync::mpsc::UnboundedSender;

use crate::APP_ID;

const MAX_LINK_BYTES: u64 = 4096;
const READ_TIMEOUT: Duration = Duration::from_secs(1);
const HAND_OVER_RETRIES: u32 = 5;
const HAND_OVER_RETRY_DELAY: Duration = Duration::from_millis(100);

/// Hands `link` to the instance already running and returns `false`, or makes this process the
/// one instance, which sends each later launch's link, if any, to `handed`, and returns `true`.
/// A launch that loses the race to become the instance hands its link over after all.
pub fn claim(link: Option<&str>, handed: UnboundedSender<Option<String>>) -> bool {
    let link = link.unwrap_or_default();
    if platform::hand_over(link).is_ok() {
        return false;
    }
    let Err(e) = platform::listen(handed) else {
        return true;
    };
    for _ in 0..HAND_OVER_RETRIES {
        std::thread::sleep(HAND_OVER_RETRY_DELAY);
        if platform::hand_over(link).is_ok() {
            return false;
        }
    }
    tracing::warn!("other launches can't reach this one: {e}");
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
        let listener = match UnixListener::bind(&path) {
            Err(e)
                if e.kind() == io::ErrorKind::AddrInUse && UnixStream::connect(&path).is_err() =>
            {
                std::fs::remove_file(&path)?;
                UnixListener::bind(&path)?
            }
            bound => bound?,
        };
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
    use std::fs::File;
    use std::io::{self, Write};
    use std::os::windows::io::AsRawHandle;

    use tokio::io::AsyncReadExt;
    use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Security::{
        EqualSid, GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser,
    };
    use windows::Win32::System::Pipes::GetNamedPipeServerProcessId;
    use windows::Win32::System::Threading::{
        GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    use super::*;

    fn name() -> String {
        let user = std::env::var("USERNAME").unwrap_or_default();
        format!(r"\\.\pipe\{APP_ID}-{user}")
    }

    /// Writes `link` to the pipe, if a process of our own user serves it.
    pub fn hand_over(link: &str) -> io::Result<()> {
        let mut pipe = std::fs::OpenOptions::new().write(true).open(name())?;
        // Any user on the machine can create a pipe of this name.
        if !served_by_us(&pipe)? {
            return Err(io::Error::other("another user serves the pipe"));
        }
        pipe.write_all(link.as_bytes())
    }

    fn served_by_us(pipe: &File) -> io::Result<bool> {
        let mut server = 0;
        // SAFETY: `pipe` is an open pipe handle and `server` outlives the call.
        unsafe { GetNamedPipeServerProcessId(HANDLE(pipe.as_raw_handle()), &mut server) }?;
        // SAFETY: the returned handle is closed below and used for nothing else.
        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, server) }?;
        let theirs = user_of(process);
        // SAFETY: `process` is a handle this function opened.
        unsafe { CloseHandle(process) }?;
        let theirs = theirs?;
        // SAFETY: the current-process pseudo handle needs no closing.
        let ours = user_of(unsafe { GetCurrentProcess() })?;
        // SAFETY: both buffers hold a TOKEN_USER that GetTokenInformation filled in.
        Ok(unsafe { EqualSid(sid_of(&theirs), sid_of(&ours)) }.is_ok())
    }

    /// The `TOKEN_USER` of `process`, in a buffer aligned for it.
    fn user_of(process: HANDLE) -> io::Result<Vec<u64>> {
        let mut token = HANDLE::default();
        // SAFETY: `token` is closed below once read.
        unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) }?;
        let mut size = 0;
        // SAFETY: a call with no buffer only reports the size the token information needs.
        let _ = unsafe { GetTokenInformation(token, TokenUser, None, 0, &mut size) };
        let mut buffer = vec![0u64; (size as usize).div_ceil(8)];
        // SAFETY: `buffer` holds at least `size` bytes, aligned for TOKEN_USER.
        let read = unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                Some(buffer.as_mut_ptr().cast()),
                size,
                &mut size,
            )
        };
        // SAFETY: `token` is a handle this function opened.
        unsafe { CloseHandle(token) }?;
        read?;
        Ok(buffer)
    }

    /// # Safety
    /// `user` holds a `TOKEN_USER` filled in by `GetTokenInformation`.
    unsafe fn sid_of(user: &[u64]) -> windows::Win32::Security::PSID {
        // SAFETY: the caller guarantees the buffer holds a TOKEN_USER.
        unsafe { (*user.as_ptr().cast::<TOKEN_USER>()).User.Sid }
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
