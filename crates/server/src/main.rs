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
    let root = PathBuf::from(std::env::var("O5S_DATA").unwrap_or(".o5s".into()));
    o5s_platform::limits::set(o5s_platform::limits::Limits::from_lookup(|k| {
        std::env::var(k).ok()
    })?);
    if matches!(args.get(1).map(String::as_str), Some("token" | "revoke")) {
        let usage = "token TENANT USER [SECONDS] [--read-only] | revoke TENANT USER [NOT_AFTER]";
        let tenant: u64 = args.get(2).ok_or(usage)?.parse()?;
        let user: u64 = args.get(3).ok_or(usage)?.parse()?;
        let now = h5i_app_core::Timestamp::now().secs();
        let pg = match std::env::var("O5S_DATABASE_URL") {
            Ok(url) => {
                let schema = std::env::var("O5S_DATABASE_SCHEMA").unwrap_or("o5s".into());
                Some(o5s_server::pg::connect(&url, &schema, 1).await?)
            }
            Err(_) => None,
        };
        let file = root.join(o5s_server::revocation::FILE);
        if args[1] == "revoke" {
            // By default every token `token` issued so far (they last a
            // day); give NOT_AFTER for longer-lived ones.
            let not_after = match args.get(4) {
                Some(n) => n.parse()?,
                None => now + 86400,
            };
            let r = o5s_pgwire::revoke::Revocation::User {
                tenant,
                user,
                not_after,
            };
            match &pg {
                Some(pg) => pg.engine.revoke(&r).await?,
                None => {
                    o5s_server::revocation::write(&file, &r, now).map_err(|e| format!("{e:?}"))?
                }
            }
            println!("revoked tokens of {tenant}/{user} expiring at or before {not_after}");
            return Ok(());
        }
        // A new token must expire after any revocation of this user.
        let floor = match &pg {
            Some(pg) => pg
                .engine
                .not_after(tenant, user)
                .await?
                .map_or(0, |n| n + 1),
            None => o5s_server::revocation::read(&file)
                .map_err(|e| format!("{e:?}"))?
                .first_valid_expiry(tenant, user),
        };
        // A day unless SECONDS says otherwise.
        let read_only = args[4..].iter().any(|a| a == "--read-only");
        let lifetime: u64 = match args[4..].iter().find(|a| *a != "--read-only") {
            Some(s) => s.parse()?,
            None => 86400,
        };
        let exp = (now + lifetime).max(floor);
        let auth = HmacAuth::new(secret);
        println!(
            "{}",
            if read_only {
                auth.issue_read_only(tenant, user, exp)
            } else {
                auth.issue(tenant, user, exp)
            }
        );
        return Ok(());
    }
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
