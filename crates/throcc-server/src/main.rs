use std::net::{Ipv6Addr, SocketAddr};
use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use throcc_server::{Config, Congestion, Server};

const DEFAULT_DATA: &str = "./throcc-data";

/// Without a command, runs the server.
#[derive(Parser)]
#[command(
    about = "throcc voice and screen-share server",
    args_conflicts_with_subcommands = true
)]
struct Args {
    #[command(subcommand)]
    command: Option<Command>,
    /// Directory holding server_key and the database.
    #[arg(long, default_value = DEFAULT_DATA)]
    data: PathBuf,
    #[arg(long, default_value_t = SocketAddr::from((Ipv6Addr::UNSPECIFIED, throcc_proto::DEFAULT_PORT)))]
    listen: SocketAddr,
    #[arg(long, value_enum, default_value_t)]
    congestion: Congestion,
}

#[derive(Subcommand)]
enum Command {
    /// Prints a single-use invite whose user becomes an admin. It lasts a day, and the
    /// server accepts it while running.
    AdminInvite {
        /// The data directory of a server that has started at least once.
        #[arg(long, default_value = DEFAULT_DATA)]
        data: PathBuf,
    },
}

fn main() -> Result<()> {
    let args = Args::parse();
    match args.command {
        Some(Command::AdminInvite { data }) => {
            // The invite goes to stdout, so logs go to stderr.
            init_logging(std::io::stderr);
            admin_invite(data)
        }
        None => {
            init_logging(std::io::stdout);
            serve(Config {
                data_dir: args.data,
                listen: args.listen,
                congestion: args.congestion,
            })
        }
    }
}

fn init_logging<W>(writer: W)
where
    W: for<'a> tracing_subscriber::fmt::MakeWriter<'a> + Send + Sync + 'static,
{
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(writer)
        .init();
}

#[tokio::main]
async fn serve(config: Config) -> Result<()> {
    let server = Server::bind(&config)?;
    tracing::info!("listening on {}", server.local_addr()?);
    tracing::info!("server fingerprint {}", server.fingerprint());
    server.run().await
}

fn admin_invite(data: PathBuf) -> Result<()> {
    let invite = throcc_server::admin_invite(&data)?;
    println!("invite       {}", invite.code);
    println!("expires      {}", utc(invite.expires_at));
    println!("fingerprint  {}", invite.fingerprint);
    Ok(())
}

/// Unix seconds as `YYYY-MM-DD HH:MM UTC`.
fn utc(unix: u64) -> String {
    let (days, seconds) = (unix / 86_400, unix % 86_400);
    // Howard Hinnant's civil_from_days, for days since 1970-01-01.
    let z = days + 719_468;
    let era = z / 146_097;
    let day_of_era = z % 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + u64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02} UTC",
        seconds / 3600,
        seconds % 3600 / 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_seconds_format_as_utc() {
        assert_eq!(utc(0), "1970-01-01 00:00 UTC");
        assert_eq!(utc(951_782_400 + 3_660), "2000-02-29 01:01 UTC");
        assert_eq!(utc(1_793_664_000), "2026-11-03 00:00 UTC");
    }

    #[test]
    fn the_server_runs_without_a_command_and_mints_with_one() {
        let serve = Args::parse_from(["throcc-server", "--data", "/data", "--listen", "[::]:8476"]);
        assert!(serve.command.is_none());
        assert_eq!(serve.data, PathBuf::from("/data"));
        let mint = Args::parse_from(["throcc-server", "admin-invite", "--data", "/data"]);
        let Some(Command::AdminInvite { data }) = mint.command else {
            panic!("expected admin-invite");
        };
        assert_eq!(data, PathBuf::from("/data"));
        for misplaced in [
            ["throcc-server", "--data", "/data", "admin-invite"],
            ["throcc-server", "admin-invite", "--listen", "[::]:1"],
        ] {
            assert!(Args::try_parse_from(misplaced).is_err());
        }
    }
}
