//! `pico2peta-server` binary.

use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

#[derive(Parser)]
#[command(name = "pico2peta-server", version, about = "pico2peta HTTP server")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the server.
    Serve {
        /// Path to the TOML config file.
        #[arg(short, long, default_value = "/etc/pico2peta/server.toml")]
        config: PathBuf,
    },
    /// Check a config file and exit.
    CheckConfig {
        #[arg(short, long)]
        config: PathBuf,
    },
    /// Generate a random bearer token and print it with its config entry.
    GenToken {
        /// Principal the token authenticates as.
        principal: String,
    },
    /// Print the sha256 of a token read from stdin (for the config file).
    HashToken,
    /// Verify the hash chain of an audit log.
    VerifyAudit { path: PathBuf },
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Serve { config } => serve(config),
        Cmd::CheckConfig { config } => match pico2peta_server::config::Config::load(&config) {
            Ok(_) => {
                println!("ok");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::from(2)
            }
        },
        Cmd::GenToken { principal } => {
            if !pico2peta_server::config::valid_principal(&principal) {
                eprintln!("error: invalid principal name");
                return ExitCode::from(2);
            }
            let mut raw = [0u8; 32];
            if let Err(e) = getrandom::fill(&mut raw) {
                eprintln!("error: no system randomness: {e}");
                return ExitCode::FAILURE;
            }
            let token = format!("p2p_{}", hex(&raw));
            let h = pico2peta_server::auth::sha256(token.as_bytes());
            println!("token: {token}");
            println!();
            println!("[[auth.tokens]]\nprincipal = \"{principal}\"\nsha256 = \"{}\"", hex(&h));
            ExitCode::SUCCESS
        }
        Cmd::HashToken => {
            let mut s = String::new();
            if std::io::stdin().read_line(&mut s).is_err() {
                return ExitCode::FAILURE;
            }
            println!("{}", hex(&pico2peta_server::auth::sha256(s.trim().as_bytes())));
            ExitCode::SUCCESS
        }
        Cmd::VerifyAudit { path } => match pico2peta_server::audit::verify(&path) {
            Ok(n) => {
                println!("ok: {n} records, chain intact");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("audit log verification FAILED: {e}");
                ExitCode::from(3)
            }
        },
    }
}

fn serve(config: PathBuf) -> ExitCode {
    let cfg = match pico2peta_server::config::Config::load(&config) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };
    pico2peta_server::init_logging(&cfg.log);
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("p2p-http")
        .max_blocking_threads(cfg.server.max_concurrent_requests.saturating_mul(2).max(64))
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    let res = rt.block_on(async {
        // Opening the repo does blocking I/O; keep it off the async workers.
        let c2 = cfg.clone();
        let state = tokio::task::spawn_blocking(move || pico2peta_server::build_state(&c2))
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?;
        let listener = tokio::net::TcpListener::bind(cfg.server.listen)
            .await
            .map_err(|e| format!("bind {}: {e}", cfg.server.listen))?;
        tracing::info!(listen = %cfg.server.listen, repo = %cfg.repo.path.display(), "pico2peta-server listening");
        pico2peta_server::serve(
            listener,
            state,
            Duration::from_secs(cfg.server.request_timeout_secs),
            Duration::from_secs(cfg.server.shutdown_grace_secs),
            pico2peta_server::shutdown_signal(),
        )
        .await
        .map_err(|e| e.to_string())
    });
    match res {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!(error = %e, "server failed");
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
