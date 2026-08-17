use std::net::{Ipv6Addr, SocketAddr};
use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use throcc_server::Server;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(name = "throcc-server", version, about = "Voice + screenshare server")]
struct Args {
    /// Directory holding `server_key` and the database. The only persistent state.
    #[arg(long, env = "THROCC_DATA_DIR", default_value = "/data")]
    data_dir: PathBuf,

    /// UDP address to listen on. There is no TCP fallback.
    #[arg(
        long,
        env = "THROCC_LISTEN",
        default_value_t = SocketAddr::from((Ipv6Addr::UNSPECIFIED, throcc_proto::DEFAULT_PORT))
    )]
    listen: SocketAddr,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();

    let server = Server::bind(&args.data_dir, args.listen)?;
    let listening_on = server.local_addr()?;

    tracing::info!(
        listen = %listening_on,
        fingerprint = %server.fingerprint(),
        "listening on UDP"
    );

    server.run().await
}
