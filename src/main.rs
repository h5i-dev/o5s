use o5s_server::{AppState, Store, auth::HmacAuth, router};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let secret = std::env::var("H5I_APP_SECRET")
        .map_err(|_| "Set H5I_APP_SECRET (scripts/dev.sh creates a local secret)")?;
    let args: Vec<String> = std::env::args().collect();
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
    let state = AppState {
        store: Arc::new(Mutex::new(Store::open(&root)?)),
        auth: Arc::new(HmacAuth::new(secret)),
        root,
    };
    let bind = std::env::var("O5S_BIND").unwrap_or("127.0.0.1:3000".into());
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    println!("o5s listening at http://{bind}");
    axum::serve(listener, router(state)).await?;
    Ok(())
}
