use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use throcc_server::{Config, Congestion, Server};

#[derive(Parser)]
#[command(about = "throcc voice and screen-share server")]
struct Args {
    /// Directory holding server_key and the database.
    #[arg(long, default_value = "./throcc-data")]
    data: PathBuf,
    #[arg(long, default_value = "[::]:8476")]
    listen: SocketAddr,
    #[arg(long, value_enum, default_value_t)]
    congestion: Congestion,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let args = Args::parse();
    let server = Server::bind(&Config {
        data_dir: args.data,
        listen: args.listen,
        congestion: args.congestion,
    })?;
    tracing::info!("listening on {}", server.local_addr()?);
    tracing::info!("server fingerprint {}", server.fingerprint());
    server.run().await
}
