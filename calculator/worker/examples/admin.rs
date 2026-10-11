//! Native helpers for the worker:
//!
//! `cargo run -p calculator-worker --example admin -- ddl` prints the SQL that
//! creates the tables; run it on the database directly (Hyperdrive serves
//! only the engine's transactions).
//!
//! `H5I_APP_SECRET=... cargo run -p calculator-worker --example admin -- issue <org>:<user>`
//! prints a one-day token.

use calculator_worker::calc::Calc;
use calculator_worker::{SCHEMA, auth::HmacAuth, engine};
use std::time::{SystemTime, UNIX_EPOCH};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["ddl"] => {
            for stmt in engine::ddl::<Calc, Calc>(SCHEMA)? {
                println!("{stmt};");
            }
        }
        ["issue", spec] => {
            let (o, u) = spec.split_once(':').ok_or("issue <org>:<user>")?;
            let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
            let auth = HmacAuth::new(std::env::var("H5I_APP_SECRET")?);
            println!("{}", auth.issue(o.parse()?, u.parse()?, now + 86_400));
        }
        _ => return Err("usage: admin ddl | admin issue <org>:<user>".into()),
    }
    Ok(())
}
