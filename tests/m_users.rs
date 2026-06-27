//! User management: admin list/create/role-toggle/reset/delete, self password
//! change, and the guard rails (non-admin forbidden, last-admin protected,
//! no self-delete).

use axum::body::Body;
use axum::http::{Request, StatusCode};
use homeconnect::config::Config;
use serde_json::{json, Value};
use tower::ServiceExt;

async fn jbody(r: axum::response::Response) -> Value {
    let b = axum::body::to_bytes(r.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&b).unwrap_or(Value::Null)
}

async fn login(app: &axum::Router, user: &str, pass: &str) -> Option<String> {
    let r = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(json!({"username":user,"password":pass}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    if r.status() != StatusCode::OK {
        return None;
    }
    jbody(r).await["access_token"].as_str().map(|s| s.to_string())
}

async fn call(app: &axum::Router, method: &str, uri: &str, jwt: &str, body: Option<Value>) -> axum::response::Response {
    let mut b = Request::builder().method(method).uri(uri).header("authorization", format!("JWT {jwt}"));
    let body = match body {
        Some(v) => {
            b = b.header("content-type", "application/json");
            Body::from(v.to_string())
        }
        None => Body::empty(),
    };
    app.clone().oneshot(b.body(body).unwrap()).await.unwrap()
}

#[tokio::test]
async fn user_management_flow() {
    let tmp = tempfile::tempdir().unwrap();
    let mut config = Config::from_env();
    config.data_dir = tmp.path().to_path_buf();
    let state = homeconnect::build_state(config).await.unwrap();
    let app = homeconnect::router(state.clone());

    // One admin to start.
    let _ = homeconnect::api::users::create_user_row(&state, "alice", "password1", None, true).await.unwrap();
    let alice = login(&app, "alice", "password1").await.unwrap();

    // Admin creates a regular user via the API.
    let r = call(&app, "POST", "/v1/admin/users", &alice,
        Some(json!({"username":"bob","password":"password1","email":"bob@x.io"}))).await;
    assert_eq!(r.status(), StatusCode::OK, "admin creates user");
    let bob = login(&app, "bob", "password1").await.unwrap();

    // List shows both; bob's id is what we address him by.
    let r = call(&app, "GET", "/v1/admin/users", &alice, None).await;
    let users = jbody(r).await["users"].as_array().cloned().unwrap();
    assert_eq!(users.len(), 2);
    let bob_id = users.iter().find(|u| u["username"] == "bob").unwrap()["id"].as_str().unwrap().to_string();
    assert_eq!(users.iter().find(|u| u["username"] == "alice").unwrap()["self"], json!(true));

    // Non-admin can't list or manage users.
    assert_eq!(call(&app, "GET", "/v1/admin/users", &bob, None).await.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        call(&app, "POST", "/v1/admin/users", &bob, Some(json!({"username":"x","password":"password1"}))).await.status(),
        StatusCode::FORBIDDEN
    );

    // Promote bob, then demote.
    assert_eq!(call(&app, "POST", &format!("/v1/admin/users/{bob_id}"), &alice, Some(json!({"is_admin":true}))).await.status(), StatusCode::OK);
    let r = call(&app, "GET", "/v1/admin/users", &alice, None).await;
    let admins = jbody(r).await["users"].as_array().cloned().unwrap().iter().filter(|u| u["is_admin"] == json!(true)).count();
    assert_eq!(admins, 2, "bob is admin now");
    assert_eq!(call(&app, "POST", &format!("/v1/admin/users/{bob_id}"), &alice, Some(json!({"is_admin":false}))).await.status(), StatusCode::OK);

    // Admin resets bob's password; old fails, new works.
    assert_eq!(call(&app, "POST", &format!("/v1/admin/users/{bob_id}/password"), &alice, Some(json!({"password":"newpass2"}))).await.status(), StatusCode::OK);
    assert!(login(&app, "bob", "password1").await.is_none(), "old password rejected");
    assert!(login(&app, "bob", "newpass2").await.is_some(), "new password works");

    // Bob changes his own password (needs the current one).
    let bob = login(&app, "bob", "newpass2").await.unwrap();
    assert_eq!(
        call(&app, "POST", "/v1/me/password", &bob, Some(json!({"current_password":"wrong","new_password":"another3"}))).await.status(),
        StatusCode::UNAUTHORIZED, "wrong current password rejected"
    );
    assert_eq!(
        call(&app, "POST", "/v1/me/password", &bob, Some(json!({"current_password":"newpass2","new_password":"another3"}))).await.status(),
        StatusCode::OK
    );
    assert!(login(&app, "bob", "another3").await.is_some());

    // Guard rails: alice is the only admin → can't demote or delete her.
    let alice_id = {
        let r = call(&app, "GET", "/v1/admin/users", &alice, None).await;
        jbody(r).await["users"].as_array().cloned().unwrap().iter()
            .find(|u| u["username"] == "alice").unwrap()["id"].as_str().unwrap().to_string()
    };
    assert_eq!(call(&app, "POST", &format!("/v1/admin/users/{alice_id}"), &alice, Some(json!({"is_admin":false}))).await.status(), StatusCode::BAD_REQUEST, "last admin protected");
    assert_eq!(call(&app, "DELETE", &format!("/v1/admin/users/{alice_id}"), &alice, None).await.status(), StatusCode::BAD_REQUEST, "no self-delete");

    // Delete bob.
    assert_eq!(call(&app, "DELETE", &format!("/v1/admin/users/{bob_id}"), &alice, None).await.status(), StatusCode::OK);
    let r = call(&app, "GET", "/v1/admin/users", &alice, None).await;
    assert_eq!(jbody(r).await["users"].as_array().unwrap().len(), 1, "only alice remains");
    assert!(login(&app, "bob", "another3").await.is_none(), "deleted user can't log in");
}
