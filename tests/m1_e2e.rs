//! M1 end-to-end: a synthetic ES256 "comma" device registers, gets an upload
//! URL, uploads a file, and is browsable; a user pairs it. Drives the real
//! router in-process (no network), signing device tokens exactly as the device
//! would (ES256 against an EC P-256 key).

use std::process::Command;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use homeconnect::config::Config;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde_json::{json, Value};
use tower::ServiceExt; // oneshot

/// Generate an EC P-256 keypair via openssl; return (private_pem_pkcs8, public_pem).
fn gen_ec_keypair(dir: &std::path::Path) -> (String, String) {
    let priv_path = dir.join("ec_priv.pem");
    let pub_path = dir.join("ec_pub.pem");
    let ok = Command::new("openssl")
        .args(["genpkey", "-algorithm", "EC", "-pkeyopt", "ec_paramgen_curve:P-256", "-out"])
        .arg(&priv_path)
        .status()
        .expect("run openssl genpkey")
        .success();
    assert!(ok, "openssl genpkey failed");
    let ok = Command::new("openssl")
        .args(["pkey", "-pubout", "-in"])
        .arg(&priv_path)
        .arg("-out")
        .arg(&pub_path)
        .status()
        .expect("run openssl pkey")
        .success();
    assert!(ok, "openssl pkey -pubout failed");
    (
        std::fs::read_to_string(&priv_path).unwrap(),
        std::fs::read_to_string(&pub_path).unwrap(),
    )
}

fn sign_es256(priv_pem: &str, claims: &Value) -> String {
    let key = EncodingKey::from_ec_pem(priv_pem.as_bytes()).expect("load EC priv");
    jsonwebtoken::encode(&Header::new(Algorithm::ES256), claims, &key).expect("sign")
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

#[tokio::test]
async fn device_register_upload_and_pair() {
    let tmp = tempfile::tempdir().unwrap();
    let (priv_pem, pub_pem) = gen_ec_keypair(tmp.path());

    let mut config = Config::from_env();
    config.data_dir = tmp.path().to_path_buf();
    config.public_url = "http://test.local".to_string();
    let state = homeconnect::build_state(config).await.unwrap();
    let app = homeconnect::router(state.clone());

    // 1. Register: device-signed register_token {register:true}.
    let register_token = sign_es256(&priv_pem, &json!({ "register": true, "exp": now() + 3600 }));
    let qs = format!(
        "imei=imeiA&imei2=imeiB&serial=ser123&public_key={}&register_token={}",
        urlencoding(&pub_pem),
        urlencoding(&register_token),
    );
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/v2/pilotauth?{qs}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "pilotauth should succeed");
    let v = body_json(resp).await;
    let dongle = v["dongle_id"].as_str().expect("dongle_id").to_string();
    assert_eq!(dongle.len(), 16, "dongle_id is 16 hex chars");
    assert_eq!(v["access_token"], "");

    // The device's own JWT (identity == dongle), used for upload/info/ws auth.
    let device_jwt = sign_es256(&priv_pem, &json!({ "identity": dongle, "exp": now() + 3600 }));

    // 2. Ask for an upload URL.
    let path = "2024-01-02--03-04-05--0--rlog.bz2";
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/v1.4/{dongle}/upload_url?path={}", urlencoding(path)))
                .header("Authorization", format!("JWT {device_jwt}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let v = body_json(resp).await;
    assert_eq!(status, StatusCode::OK, "upload_url should succeed: {v:?}");
    let url = v["url"].as_str().expect("url").to_string();
    assert_eq!(
        url,
        "http://test.local/connectincoming/{dongle}/2024-01-02--03-04-05/0/rlog.bz2"
            .replace("{dongle}", &dongle),
        "url path transform"
    );
    let upload_auth = v["headers"]["Authorization"].as_str().expect("auth header").to_string();
    assert!(upload_auth.starts_with("JWT "));

    // 3. Upload to that URL with the returned auth header.
    let upload_path = url.strip_prefix("http://test.local").unwrap();
    let payload = b"hello-rlog-bytes";
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(upload_path)
                .header("Authorization", upload_auth.clone())
                .body(Body::from(payload.as_slice()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED, "upload should be created");

    // It actually landed in the blob store under the canonical key.
    let key = homeconnect::storage::blob_key(&dongle, "2024-01-02--03-04-05", 0, "rlog.bz2");
    let stored = state.blobs.get(&key).await.expect("blob present");
    assert_eq!(stored, payload);

    // Re-upload the same file is rejected (no silent overwrite).
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(upload_path)
                .header("Authorization", upload_auth)
                .body(Body::from(payload.as_slice()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN, "duplicate upload rejected");

    // 4. Create a user and pair the device to it.
    let _ = homeconnect::api::users::create_user_row(&state, "alice", "password1", None, true)
        .await
        .unwrap();
    let login = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(json!({"username":"alice","password":"password1"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(login.status(), StatusCode::OK);
    let user_jwt = body_json(login).await["access_token"].as_str().unwrap().to_string();

    let pair_token = sign_es256(&priv_pem, &json!({ "identity": dongle, "pair": true, "exp": now() + 3600 }));
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v2/pilotpair")
                .header("Authorization", format!("JWT {user_jwt}"))
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(format!("pair_token={}", urlencoding(&pair_token))))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "pilotpair should succeed");
    let v = body_json(resp).await;
    assert_eq!(v["first_pair"], true);
    assert_eq!(v["dongle_id"], dongle);

    // 5. Owner can read device info; it shows paired + owner.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/v1.1/devices/{dongle}"))
                .header("Authorization", format!("JWT {user_jwt}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    assert_eq!(v["is_paired"], true);
    assert_eq!(v["is_owner"], true);
    assert_eq!(v["dongle_id"], dongle);
}

#[tokio::test]
async fn upload_url_rejects_wrong_device() {
    let tmp = tempfile::tempdir().unwrap();
    let (priv_pem, pub_pem) = gen_ec_keypair(tmp.path());
    let mut config = Config::from_env();
    config.data_dir = tmp.path().to_path_buf();
    let state = homeconnect::build_state(config).await.unwrap();
    let app = homeconnect::router(state);

    // Register a device.
    let register_token = sign_es256(&priv_pem, &json!({ "register": true, "exp": now() + 3600 }));
    let qs = format!(
        "imei=i1&imei2=i2&serial=s1&public_key={}&register_token={}",
        urlencoding(&pub_pem),
        urlencoding(&register_token)
    );
    let resp = app
        .clone()
        .oneshot(Request::builder().method("POST").uri(format!("/v2/pilotauth?{qs}")).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let dongle = body_json(resp).await["dongle_id"].as_str().unwrap().to_string();

    // A token for a *different* identity must not get an upload URL for `dongle`.
    let wrong_jwt = sign_es256(&priv_pem, &json!({ "identity": "ffffffffffffffff", "exp": now() + 3600 }));
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/v1.4/{dongle}/upload_url?path=x--0--rlog.bz2"))
                .header("Authorization", format!("JWT {wrong_jwt}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    // The wrong identity isn't a known device → 401 (unknown device).
    assert!(
        resp.status() == StatusCode::UNAUTHORIZED || resp.status() == StatusCode::FORBIDDEN,
        "wrong device must be rejected, got {}",
        resp.status()
    );
}

/// Minimal percent-encoding for query/form values (PEMs have +/=/newlines).
fn urlencoding(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
