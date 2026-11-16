//! `o5s-server` binary.

use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

#[derive(Parser)]
#[command(name = "o5s-server", version, about = "o5s HTTP server")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the server.
    Serve {
        /// Path to the TOML config file.
        #[arg(short, long, default_value = "/etc/o5s/server.toml")]
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
        Cmd::CheckConfig { config } => match o5s_server::config::Config::load(&config) {
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
            if !o5s_server::config::valid_principal(&principal) {
                eprintln!("error: invalid principal name");
                return ExitCode::from(2);
            }
            let mut raw = [0u8; 32];
            if let Err(e) = getrandom::fill(&mut raw) {
                eprintln!("error: no system randomness: {e}");
                return ExitCode::FAILURE;
            }
            let token = format!("o5s_{}", hex(&raw));
            let h = o5s_server::auth::sha256(token.as_bytes());
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
            println!("{}", hex(&o5s_server::auth::sha256(s.trim().as_bytes())));
            ExitCode::SUCCESS
        }
        Cmd::VerifyAudit { path } => match o5s_server::audit::verify_all(&path) {
            Ok((n, first)) => {
                if first > 1 {
                    println!("ok: {n} records (seq {first}..), chain intact; earlier records were archived");
                } else {
                    println!("ok: {n} records, chain intact");
                }
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
    let config = std::fs::canonicalize(&config).unwrap_or(config);
    let cfg = match o5s_server::config::Config::load(&config) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };
    o5s_server::init_logging(&cfg.log);
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("o5s-http")
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
        let state = tokio::task::spawn_blocking(move || o5s_server::build_state(&c2))
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?;
        let listener = tokio::net::TcpListener::bind(cfg.server.listen)
            .await
            .map_err(|e| format!("bind {}: {e}", cfg.server.listen))?;
        let cert = match &cfg.server.tls {
            Some(t) => Some(o5s_server::tls::ReloadableCert::load(t.cert.clone(), t.key.clone())?),
            None => None,
        };
        // SIGHUP: reload tokens, policies and the certificate.
        #[cfg(unix)]
        {
            let st = state.clone();
            let cert = cert.clone();
            let path = config.clone();
            tokio::spawn(async move {
                let Ok(mut hup) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup()) else {
                    return;
                };
                while hup.recv().await.is_some() {
                    if let Err(e) = o5s_server::reload(&st, &path, cert.as_deref()).await {
                        tracing::error!(error = %e, "configuration reload failed; keeping the old configuration");
                    }
                }
            });
        }
        // Shutdown fan-out: the API and the ops listener stop together.
        let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
        tokio::spawn(async move {
            o5s_server::shutdown_signal().await;
            let _ = stop_tx.send(true);
        });
        let stopped = |mut rx: tokio::sync::watch::Receiver<bool>| async move {
            let _ = rx.wait_for(|v| *v).await;
        };
        if let Some(addr) = cfg.server.metrics_listen {
            let l = tokio::net::TcpListener::bind(addr).await.map_err(|e| format!("bind metrics {addr}: {e}"))?;
            tracing::info!(listen = %addr, "metrics listening");
            let st = state.clone();
            let rx = stop_rx.clone();
            tokio::spawn(async move {
                if let Err(e) = o5s_server::serve_ops(l, st, stopped(rx)).await {
                    tracing::error!(error = %e, "metrics listener failed");
                }
            });
        }
        let timeout = Duration::from_secs(cfg.server.request_timeout_secs);
        let grace = Duration::from_secs(cfg.server.shutdown_grace_secs);
        let scheme = if cert.is_some() { "https" } else { "http" };
        tracing::info!(listen = %cfg.server.listen, scheme, repo = %cfg.repo.path.display(), "o5s-server listening");
        match cert {
            Some(c) => {
                let acceptor = o5s_server::tls::acceptor(c)?;
                let l = o5s_server::tls::TlsListener::new(listener, acceptor).map_err(|e| e.to_string())?;
                o5s_server::serve_on(l, state, timeout, grace, stopped(stop_rx)).await
            }
            None => o5s_server::serve(listener, state, timeout, grace, stopped(stop_rx)).await,
        }
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
