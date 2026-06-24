//! M1 athena: a device dials the outbound websocket; the server marks it online
//! (and the reaper marks it offline once the socket drops). Uses a real bound
//! TCP listener + a tungstenite client speaking the device's ES256 auth.

use std::process::Command;

use axum::body::Body;
use axum::http::Request;
use homeconnect::config::Config;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde_json::json;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tower::ServiceExt;

fn gen_ec_keypair(dir: &std::path::Path) -> (String, String) {
    let priv_path = dir.join("k.pem");
    let pub_path = dir.join("k.pub");
    assert!(Command::new("openssl")
        .args(["genpkey", "-algorithm", "EC", "-pkeyopt", "ec_paramgen_curve:P-256", "-out"])
        .arg(&priv_path)
        .status()
        .unwrap()
        .success());
    assert!(Command::new("openssl")
        .args(["pkey", "-pubout", "-in"])
        .arg(&priv_path)
        .arg("-out")
        .arg(&pub_path)
        .status()
        .unwrap()
        .success());
    (
        std::fs::read_to_string(&priv_path).unwrap(),
        std::fs::read_to_string(&pub_path).unwrap(),
    )
}

fn sign(priv_pem: &str, claims: &serde_json::Value) -> String {
    let key = EncodingKey::from_ec_pem(priv_pem.as_bytes()).unwrap();
    jsonwebtoken::encode(&Header::new(Algorithm::ES256), claims, &key).unwrap()
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

fn pct(s: &str) -> String {
    let mut o = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => o.push(b as char),
            _ => o.push_str(&format!("%{b:02X}")),
        }
    }
    o
}

#[tokio::test]
async fn athena_marks_device_online_then_offline() {
    let tmp = tempfile::tempdir().unwrap();
    let (priv_pem, pub_pem) = gen_ec_keypair(tmp.path());
    let mut config = Config::from_env();
    config.data_dir = tmp.path().to_path_buf();
    let state = homeconnect::build_state(config).await.unwrap();

    // Register the device so its key is on file.
    let app = homeconnect::router(state.clone());
    let register_token = sign(&priv_pem, &json!({ "register": true, "exp": now() + 3600 }));
    let qs = format!(
        "imei=a&imei2=b&serial=c&public_key={}&register_token={}",
        pct(&pub_pem),
        pct(&register_token)
    );
    let resp = app
        .oneshot(Request::builder().method("POST").uri(format!("/v2/pilotauth?{qs}")).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let dongle = serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["dongle_id"]
        .as_str()
        .unwrap()
        .to_string();

    // Serve on an ephemeral port.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let serve_state = state.clone();
    let server = tokio::spawn(async move {
        axum::serve(listener, homeconnect::router(serve_state)).await.unwrap();
    });

    // Device dials the athena ws with its device JWT.
    let device_jwt = sign(&priv_pem, &json!({ "identity": dongle, "exp": now() + 3600 }));
    let url = format!("ws://{addr}/ws/v2/{dongle}");
    let mut req = url.into_client_request().unwrap();
    req.headers_mut()
        .insert("Authorization", format!("JWT {device_jwt}").parse().unwrap());
    let (mut ws, _resp) = tokio_tungstenite::connect_async(req).await.expect("ws connect");

    // Give the handler a moment to mark online.
    wait_for_online(&state, &dongle, 1, 3000).await;
    assert_eq!(online_flag(&state, &dongle).await, 1, "device should be online while connected");

    // Close the socket; the handler's disconnect path marks it offline.
    ws.close(None).await.unwrap();
    drop(ws);
    wait_for_online(&state, &dongle, 0, 3000).await;
    assert_eq!(online_flag(&state, &dongle).await, 0, "device should be offline after close");

    server.abort();
}

async fn online_flag(state: &homeconnect::state::AppState, dongle: &str) -> i64 {
    sqlx::query_scalar::<_, i64>("SELECT online FROM devices WHERE dongle_id = ?")
        .bind(dongle)
        .fetch_one(&state.pool)
        .await
        .unwrap()
}

async fn wait_for_online(state: &homeconnect::state::AppState, dongle: &str, want: i64, max_ms: u64) {
    let step = 50;
    let mut waited = 0;
    while waited < max_ms {
        if online_flag(state, dongle).await == want {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(step)).await;
        waited += step;
    }
}
