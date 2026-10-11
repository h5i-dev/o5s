use o5s_server::{AppState, Backend, Store, auth::HmacAuth, legacy, router};
use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("import-json") {
        let root = PathBuf::from(args.get(2).ok_or("import-json DIR")?);
        let count = legacy::import(&root)?;
        println!(
            "imported {count} registry tenants into {}",
            root.join(o5s_server::local::LOG).display()
        );
        return Ok(());
    }
    match args.get(1).map(String::as_str) {
        Some("verify") => {
            let root = PathBuf::from(args.get(2).ok_or("verify DIR")?);
            let summary = o5s_server::local::verify(&root.join(o5s_server::local::LOG))?;
            println!("{}", serde_json::to_string_pretty(&summary)?);
            return Ok(());
        }
        Some("ddl") => {
            // For databases whose service role cannot create tables (Hyperdrive).
            let schema = std::env::var("O5S_DATABASE_SCHEMA").unwrap_or("o5s".into());
            println!("BEGIN;");
            for statement in o5s_pgwire::ddl(&schema)? {
                println!("{statement};");
            }
            println!("COMMIT;");
            return Ok(());
        }
        Some("backup") => {
            let root = PathBuf::from(args.get(2).ok_or("backup DIR OUT")?);
            let out = PathBuf::from(args.get(3).ok_or("backup DIR OUT")?);
            let summary = o5s_server::local::backup(&root.join(o5s_server::local::LOG), &out)?;
            println!("{}", serde_json::to_string_pretty(&summary)?);
            return Ok(());
        }
        _ => {}
    }
    let secret = std::env::var("H5I_APP_SECRET")
        .map_err(|_| "Set H5I_APP_SECRET (scripts/dev.sh creates a local secret)")?;
    if args.get(1).map(String::as_str) == Some("token") {
        let tenant: u64 = args.get(2).ok_or("token TENANT USER")?.parse()?;
        let user: u64 = args.get(3).ok_or("token TENANT USER")?.parse()?;
        println!(
            "{}",
            HmacAuth::new(secret).issue(
                tenant,
                user,
                h5i_app_core::Timestamp::now().secs() + 86400
            )
        );
        return Ok(());
    }
    let root = PathBuf::from(std::env::var("O5S_DATA").unwrap_or(".o5s".into()));
    std::fs::create_dir_all(&root)?;
    let backend = match std::env::var("O5S_DATABASE_URL") {
        Ok(url) => {
            let pool = std::env::var("O5S_DATABASE_POOL")
                .ok()
                .map(|p| p.parse())
                .transpose()?
                .unwrap_or(16);
            let schema = std::env::var("O5S_DATABASE_SCHEMA").unwrap_or("o5s".into());
            println!("o5s storage: PostgreSQL (schema {schema}, pool {pool})");
            Backend::Postgres(std::sync::Arc::new(
                o5s_server::pg::connect(&url, &schema, pool).await?,
            ))
        }
        Err(_) => {
            println!("o5s storage: local log in {}", root.display());
            Backend::local(Store::open(&root)?)
        }
    };
    let previous = std::env::var("H5I_APP_PREVIOUS_SECRETS")
        .unwrap_or_default()
        .split(',')
        .map(|s| s.trim().as_bytes().to_vec())
        .collect();
    let mut state = AppState::new(backend, HmacAuth::new(secret).with_previous(previous), root);
    state.log_requests = std::env::var("O5S_LOG_REQUESTS").map_or(true, |v| v != "0");
    let bind = std::env::var("O5S_BIND").unwrap_or("127.0.0.1:3000".into());
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    println!("o5s listening at http://{bind}");
    axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown())
        .await?;
    Ok(())
}

async fn shutdown() {
    let _ = tokio::signal::ctrl_c().await;
}
