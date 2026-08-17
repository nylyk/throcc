use std::net::Ipv6Addr;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::Parser;
use throcc_client_core::{Connector, Keystore};
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "throcc-cli",
    version,
    about = "Headless voice + screenshare client"
)]
struct Args {
    /// The server's domain or IP address.
    server: String,

    #[arg(long, default_value_t = throcc_proto::DEFAULT_PORT)]
    port: u16,

    /// Keystore location.
    #[arg(long)]
    keystore: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();

    let keystore = Keystore::open(args.keystore)?;
    tracing::info!(keystore = %keystore.path().display(), "loaded keystore");

    if args.server.contains(':') && args.server.parse::<Ipv6Addr>().is_err() {
        bail!(
            "{} is not a host name or IP address; the port goes in --port",
            args.server
        );
    }

    let authority = format_authority(&args.server, args.port);
    let address = tokio::net::lookup_host(&authority)
        .await
        .with_context(|| format!("resolving {authority}"))?
        .next()
        .ok_or_else(|| anyhow::anyhow!("{authority} resolved to no addresses"))?;

    let mut connector = Connector::new(keystore)?;
    let connection = match connector.connect(address, &authority).await {
        Ok(connection) => connection,
        Err(throcc_client_core::Error::PinMismatch {
            server,
            pinned,
            presented,
        }) => {
            bail!(
                "the key for {server} has changed.\n  pinned:    {pinned}\n  presented: {presented}\n\
                 \nIf the server was rebuilt this is expected; re-accept by removing that entry \
                 from the keystore. If it was not, stop."
            );
        }
        Err(e) => return Err(e).context("connecting"),
    };

    tracing::info!(
        remote = %connection.remote_address(),
        rtt = ?connection.rtt(),
        "connected"
    );

    let reason = connection.closed().await;
    tracing::info!(%reason, "disconnected");

    Ok(())
}

fn format_authority(host: &str, port: u16) -> String {
    if host.parse::<Ipv6Addr>().is_ok() {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brackets_only_ipv6_addresses() {
        assert_eq!(format_authority("::1", 8476), "[::1]:8476");
        assert_eq!(format_authority("1.2.3.4", 8476), "1.2.3.4:8476");
        assert_eq!(format_authority("example.com", 8476), "example.com:8476");
    }
}
