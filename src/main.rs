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
    homeconnect::movie::spawn(state.clone()); // build per-drive watchable movies
    homeconnect::transcode::warm(); // probe GPUs once now, not on first Settings load
    {
        // Sweep orphaned partial transcode outputs left by an interrupted encode.
        let s = state.clone();
        tokio::spawn(async move { homeconnect::transcode::clean_cache_tmp(&s).await });
    }

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
        "reparse-model" => {
            // Build model.json (top-down modelV2) from every stored rlog.
            let segs: Vec<(String, i64)> = sqlx::query_as(
                "SELECT canonical_route_name, number FROM segments WHERE rlog_url != '' ORDER BY canonical_route_name, number",
            )
            .fetch_all(&state.pool)
            .await?;
            let mut n = 0;
            for (route, seg) in &segs {
                let Some((dongle, ts)) = route.split_once('|') else { continue };
                for f in ["rlog.zst", "rlog.bz2"] {
                    let key = homeconnect::storage::blob_key(dongle, ts, *seg, f);
                    if let Ok(bytes) = state.blobs.get(&key).await {
                        if homeconnect::parse::parse_model_and_store(&state, dongle, ts, *seg, f, &bytes).await.is_ok() {
                            n += 1;
                        }
                        break;
                    }
                }
            }
            println!("built model.json for {n} segments");
            Ok(())
        }
        "qlog-inspect" => {
            // usage: qlog-inspect <dongle> <ts> <seg> [file]
            let (dongle, ts, seg) = (&args[1], &args[2], args[3].parse::<i64>().unwrap_or(0));
            let files: Vec<String> = match args.get(4) {
                Some(f) => vec![f.clone()],
                None => vec!["qlog.zst".into(), "qlog.bz2".into()],
            };
            for f in &files {
                let key = homeconnect::storage::blob_key(dongle, ts, seg, f);
                if let Ok(bytes) = state.blobs.get(&key).await {
                    println!("# {f} ({} bytes)", bytes.len());
                    for (k, c) in homeconnect::parse::inspect_qlog(f, &bytes) {
                        println!("{k}: {c}");
                    }
                    return Ok(());
                }
            }
            eprintln!("no log found for {dongle}|{ts}--{seg}");
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
