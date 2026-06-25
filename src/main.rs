//! homeconnect binary: bootstrap config, state, background workers, and serve.

use homeconnect::config::Config;
use homeconnect::state::AppState;
use homeconnect::{api, athena, build_state, retention, router};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "homeconnect=info,tower_http=info,sqlx=warn".into()),
        )
        .init();

    let config = Config::from_env();
    let state = build_state(config).await?;

    // CLI subcommands (e.g. `homeconnect create-user <name> <pass> [email]`).
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 {
        return run_cli(&state, &args[1..]).await;
    }

    // Background: mark stale devices offline + enforce retention + pull drives
    // off the device over SSH (the uploader can't be repointed at us).
    athena::spawn_reaper(state.clone());
    retention::spawn(state.clone());
    homeconnect::devsync::spawn_workers(state.clone());
    homeconnect::devsync::spawn(state.clone());

    let app = router(state.clone());
    let listener = tokio::net::TcpListener::bind(&state.config.bind).await?;
    tracing::info!("homeconnect listening on {}", state.config.bind);
    axum::serve(listener, app).await?;
    Ok(())
}

/// Minimal CLI for out-of-band admin tasks (no HTTP server started).
async fn run_cli(state: &AppState, args: &[String]) -> anyhow::Result<()> {
    match args[0].as_str() {
        "create-user" => {
            if args.len() < 3 {
                eprintln!("usage: homeconnect create-user <username> <password> [email]");
                std::process::exit(2);
            }
            let username = &args[1];
            let password = &args[2];
            let email = args.get(3).map(|s| s.as_str());
            match api::users::create_user_row(state, username, password, email, true).await {
                Ok(_) => {
                    println!("created admin user '{username}'");
                    Ok(())
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
            }
        }
        "reparse" => {
            let n = homeconnect::parse::reparse_all(state).await?;
            println!("reparsed {n} segments");
            Ok(())
        }
        "device-pubkey" => {
            println!("{}", homeconnect::device_ssh::public_key(state).await?);
            Ok(())
        }
        other => {
            eprintln!("unknown command: {other}");
            std::process::exit(2);
        }
    }
}
