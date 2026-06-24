//! v1 device + browse endpoints. M1 subset: upload_url (GET/POST) and device
//! info/location/stats. Browse endpoints (routes_segments etc.) arrive in M3.

use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::auth::{self, Auth, AuthUser};
use crate::error::{AppError, AppResult};
use crate::models::{Device, User};
use crate::state::AppState;

const SECS_PER_DAY: i64 = 86_400;

/// Transform the device's `path` into the slash-separated URL tail used by the
/// connectincoming routes (mirrors the reference exactly):
///   `2024-03-02--19-02-46--0--rlog.bz2` -> `2024-03-02--19-02-46/0/rlog.bz2`
///   `boot/2024-03-02--19-02-46.bz2`     -> unchanged
///   `crash/<id>_<commit>_<name>`        -> `crash/<id>/<commit>/<name>`
fn transform_route_string(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("boot/") {
        return format!("boot/{rest}");
    }
    if let Some(rest) = path.strip_prefix("crash/") {
        return format!("crash/{}", rest.replacen('_', "/", 2));
    }
    // Driving segment: split "<ts>--<seg>--<file>" into "<ts>/<seg>/<file>".
    // Only the first two `--` separators are structural; the timestamp itself
    // contains `--`, so split from the right.
    if let Some((head, file)) = path.rsplit_once("--") {
        if let Some((ts, seg)) = head.rsplit_once("--") {
            return format!("{ts}/{seg}/{file}");
        }
    }
    path.to_string()
}

fn require_device(auth: &Auth, dongle: &str) -> AppResult<()> {
    match &auth.device {
        Some(d) if d.dongle_id == dongle => Ok(()),
        Some(_) => Err(AppError::Forbidden("device/dongle mismatch".into())),
        None => Err(AppError::Unauthorized("device token required".into())),
    }
}

fn upload_token(state: &AppState, dongle: &str, expiry_days: i64) -> AppResult<String> {
    let days = expiry_days.clamp(1, 30);
    auth::issue_hs512_ttl(&state.config.jwt_secret_b64, dongle, days * SECS_PER_DAY)
        .map_err(AppError::Other)
}

#[derive(Deserialize)]
pub struct UploadUrlQuery {
    pub path: String,
    pub expiry_days: Option<i64>,
}

/// GET /v1.4/:dongle_id/upload_url?path=&expiry_days= — single upload URL with
/// the auth token in a header.
pub async fn upload_url(
    State(state): State<AppState>,
    Path(dongle): Path<String>,
    auth: Auth,
    Query(q): Query<UploadUrlQuery>,
) -> AppResult<Json<Value>> {
    require_device(&auth, &dongle)?;
    let token = upload_token(&state, &dongle, q.expiry_days.unwrap_or(1))?;
    let tail = transform_route_string(&q.path);
    let url = format!("{}/connectincoming/{dongle}/{tail}", state.config.public_url);
    Ok(Json(json!({
        "url": url,
        "headers": {
            "Content-Type": "application/octet-stream",
            "Authorization": format!("JWT {token}"),
        }
    })))
}

#[derive(Deserialize)]
pub struct UploadUrlsBody {
    pub paths: Vec<String>,
    pub expiry_days: Option<i64>,
}

/// POST /v1/:dongle_id/upload_urls — batch; auth token carried in `?sig=`.
pub async fn upload_urls(
    State(state): State<AppState>,
    Path(dongle): Path<String>,
    auth: Auth,
    Json(body): Json<UploadUrlsBody>,
) -> AppResult<Json<Value>> {
    require_device(&auth, &dongle)?;
    let token = upload_token(&state, &dongle, body.expiry_days.unwrap_or(1))?;
    let base = &state.config.public_url;
    let urls: Vec<Value> = body
        .paths
        .iter()
        .map(|p| {
            let tail = transform_route_string(p);
            json!({ "url": format!("{base}/connectincoming/{dongle}/{tail}?sig={token}") })
        })
        .collect();
    Ok(Json(json!(urls)))
}

// ---- device info -------------------------------------------------------

async fn load_device(state: &AppState, dongle: &str) -> AppResult<Device> {
    sqlx::query_as::<_, Device>("SELECT * FROM devices WHERE dongle_id = ?")
        .bind(dongle)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("unknown device".into()))
}

/// Authorize: the caller is the device itself, the owner, or an authorized user.
async fn authorize_view(state: &AppState, auth: &Auth, device: &Device) -> AppResult<bool> {
    if let Some(d) = &auth.device {
        return Ok(d.dongle_id == device.dongle_id);
    }
    if let Some(u) = &auth.user {
        if device.owner_id == Some(u.id) {
            return Ok(true);
        }
        let shared: Option<(i64,)> = sqlx::query_as(
            "SELECT 1 FROM authorized_users WHERE user_id = ? AND device_dongle_id = ?",
        )
        .bind(u.id)
        .bind(&device.dongle_id)
        .fetch_optional(&state.pool)
        .await?;
        return Ok(shared.is_some());
    }
    Ok(false)
}

/// GET /v1.1/devices/:dongle_id — device info.
pub async fn device_info(
    State(state): State<AppState>,
    Path(dongle): Path<String>,
    auth: Auth,
) -> AppResult<Json<Value>> {
    let device = load_device(&state, &dongle).await?;
    if !authorize_view(&state, &auth, &device).await? {
        return Err(AppError::Forbidden("not authorized for device".into()));
    }
    let is_owner = auth.user.as_ref().map(|u| device.owner_id == Some(u.id)).unwrap_or(false);
    Ok(Json(json!({
        "dongle_id": device.dongle_id,
        "alias": device.alias,
        "serial": device.serial,
        "athena_host": state.config.public_url,
        "last_athena_ping": device.last_athena_ping,
        "ignore_uploads": device.uploads_allowed == 0,
        "is_paired": device.owner_id.is_some(),
        "is_owner": is_owner,
        "public_key": device.public_key,
        "prime": true,
        "prime_type": 0,
        "trial_claimed": true,
        "device_type": device.device_type,
        "last_gps_time": device.last_gps_time,
        "last_gps_lat": device.last_gps_lat,
        "last_gps_lng": device.last_gps_lng,
        "last_gps_accur": 0.0,
        "last_gps_speed": 0.0,
        "last_gps_bearing": 0.0,
        "openpilot_version": "",
        "sim_id": null,
        "online": device.online != 0,
    })))
}

/// GET /v1/devices/:dongle_id/location — latest known GPS.
pub async fn device_location(
    State(state): State<AppState>,
    Path(dongle): Path<String>,
    auth: Auth,
) -> AppResult<Json<Value>> {
    let device = load_device(&state, &dongle).await?;
    if !authorize_view(&state, &auth, &device).await? {
        return Err(AppError::Forbidden("not authorized for device".into()));
    }
    Ok(Json(json!({
        "dongle_id": device.dongle_id,
        "lat": device.last_gps_lat,
        "lng": device.last_gps_lng,
        "time": device.last_gps_time,
        "accuracy": 0.0,
        "speed": 0.0,
        "bearing": 0.0,
    })))
}

/// GET /v1.1/devices/:dongle_id/stats — aggregate drive stats.
pub async fn device_stats(
    State(state): State<AppState>,
    Path(dongle): Path<String>,
    auth: Auth,
) -> AppResult<Json<Value>> {
    let device = load_device(&state, &dongle).await?;
    if !authorize_view(&state, &auth, &device).await? {
        return Err(AppError::Forbidden("not authorized for device".into()));
    }
    let route = format!("{dongle}|");
    let (dist, routes, minutes): (f64, i64, i64) = sqlx::query_as(
        "SELECT COALESCE(SUM(length),0), COUNT(*), \
                COALESCE(SUM((end_time_utc_millis - start_time_utc_millis)/60000),0) \
         FROM routes WHERE device_dongle_id = ?",
    )
    .bind(&dongle)
    .fetch_one(&state.pool)
    .await
    .unwrap_or((0.0, 0, 0));
    let _ = route;
    let all = json!({ "distance": dist, "routes": routes, "minutes": minutes });
    // (week window omitted; home use rarely needs it — return same aggregate.)
    Ok(Json(json!({ "all": all, "week": all })))
}

// ---- user browse endpoints --------------------------------------------

fn device_json(d: &Device, user: &User, public_url: &str) -> Value {
    json!({
        "dongle_id": d.dongle_id,
        "alias": d.alias,
        "serial": d.serial,
        "athena_host": public_url,
        "device_type": d.device_type,
        "ignore_uploads": d.uploads_allowed == 0,
        "is_owner": d.owner_id == Some(user.id),
        "is_paired": d.owner_id.is_some(),
        "last_athena_ping": d.last_athena_ping,
        "last_gps_lat": d.last_gps_lat,
        "last_gps_lng": d.last_gps_lng,
        "last_gps_time": d.last_gps_time,
        "last_gps_accuracy": 0.0,
        "last_gps_speed": 0.0,
        "last_gps_bearing": 0.0,
        "online": d.online != 0,
        "openpilot_version": "",
        "prime": true,
        "prime_type": 0,
        "trial_claimed": true,
        "public_key": d.public_key,
        "sim_id": null,
        "eligible_features": { "nav": true, "prime": true, "prime_data": false },
    })
}

/// GET /v1/me/devices — the caller's owned + shared devices.
pub async fn my_devices(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
) -> AppResult<Json<Value>> {
    let devices: Vec<Device> = sqlx::query_as(
        "SELECT * FROM devices \
         WHERE owner_id = ? \
            OR dongle_id IN (SELECT device_dongle_id FROM authorized_users WHERE user_id = ?) \
         ORDER BY created_at DESC",
    )
    .bind(user.id)
    .bind(user.id)
    .fetch_all(&state.pool)
    .await?;
    let arr: Vec<Value> = devices
        .iter()
        .map(|d| device_json(d, &user, &state.config.public_url))
        .collect();
    Ok(Json(json!(arr)))
}

#[derive(Deserialize)]
pub struct RoutesQuery {
    pub start: Option<i64>,
    pub end: Option<i64>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
    pub route_str: Option<String>,
}

#[derive(sqlx::FromRow)]
struct RouteRow {
    fullname: String,
    device_dongle_id: String,
    start_time_utc_millis: i64,
    end_time_utc_millis: i64,
    start_lat: f64,
    start_lng: f64,
    end_lat: f64,
    end_lng: f64,
    length: f64,
    segment_numbers: String,
    segment_start_times: String,
    segment_end_times: String,
    maxcamera: i64,
    maxdcamera: i64,
    maxecamera: i64,
    maxqcamera: i64,
    maxlog: i64,
    maxqlog: i64,
    platform: String,
    git_remote: String,
    git_branch: String,
    git_commit: String,
    is_public: i64,
    created_at: i64,
}

fn json_arr(s: &str) -> Value {
    serde_json::from_str(s).unwrap_or_else(|_| json!([]))
}

fn route_json(r: &RouteRow, public_url: &str, sig: &str) -> Value {
    json!({
        "fullname": r.fullname,
        "dongle_id": r.device_dongle_id,
        "start_time_utc_millis": r.start_time_utc_millis,
        "end_time_utc_millis": r.end_time_utc_millis,
        "start_lat": r.start_lat,
        "start_lng": r.start_lng,
        "end_lat": r.end_lat,
        "end_lng": r.end_lng,
        "length": r.length,
        "segment_numbers": json_arr(&r.segment_numbers),
        "segment_start_times": json_arr(&r.segment_start_times),
        "segment_end_times": json_arr(&r.segment_end_times),
        "maxcamera": r.maxcamera,
        "maxdcamera": r.maxdcamera,
        "maxecamera": r.maxecamera,
        "maxqcamera": r.maxqcamera,
        "maxlog": r.maxlog,
        "maxqlog": r.maxqlog,
        "platform": r.platform,
        "git_remote": r.git_remote,
        "git_branch": r.git_branch,
        "git_commit": r.git_commit,
        "is_public": r.is_public != 0,
        "create_time": r.created_at,
        "url": format!("{public_url}/connectdata/{}", r.fullname.replace('|', "/")),
        "qcamera_m3u8": format!("{public_url}/v1/route/{}/qcamera.m3u8", r.fullname),
        "share_sig": sig,
        "share_exp": "86400",
    })
}

/// GET /v1/devices/:dongle_id/routes_segments — the device's drives (each route
/// carries its segment arrays). User-only.
pub async fn routes_segments(
    State(state): State<AppState>,
    Path(dongle): Path<String>,
    AuthUser(user): AuthUser,
    Query(q): Query<RoutesQuery>,
) -> AppResult<Json<Value>> {
    let device = load_device(&state, &dongle).await?;
    if !user_owns(&state, &user, &device).await? {
        return Err(AppError::Forbidden("not authorized for device".into()));
    }

    let limit = q.limit.unwrap_or(50).clamp(1, 1000);
    let offset = q.offset.unwrap_or(0).max(0);
    let start = q.start.unwrap_or(0);
    let end = q.end.unwrap_or(i64::MAX);

    let rows: Vec<RouteRow> = if let Some(rs) = q.route_str {
        sqlx::query_as(
            "SELECT * FROM routes WHERE fullname = ? AND device_dongle_id = ? AND maxqlog != -1",
        )
        .bind(&rs)
        .bind(&dongle)
        .fetch_all(&state.pool)
        .await?
    } else {
        sqlx::query_as(
            // start_time_utc_millis > 0 hides GPS-less stubs (engine-on-but-never-
            // moved / no fix) that have no date, distance, or useful track.
            "SELECT * FROM routes \
             WHERE device_dongle_id = ? AND maxqlog != -1 AND start_time_utc_millis > 0 \
               AND start_time_utc_millis >= ? AND start_time_utc_millis <= ? \
             ORDER BY start_time_utc_millis DESC LIMIT ? OFFSET ?",
        )
        .bind(&dongle)
        .bind(start)
        .bind(end)
        .bind(limit)
        .bind(offset)
        .fetch_all(&state.pool)
        .await?
    };

    // One short-lived share token for the caller, appended to media URLs.
    let sig = auth::issue_hs512_ttl(&state.config.jwt_secret_b64, &user.identity, 86_400)
        .map_err(AppError::Other)?;
    let arr: Vec<Value> = rows
        .iter()
        .map(|r| route_json(r, &state.config.public_url, &sig))
        .collect();
    Ok(Json(json!(arr)))
}

/// GET /v1/route/:fullname/:cam — HLS playlist for one camera of a route. `cam`
/// is e.g. `qcamera.m3u8`, `fcamera.m3u8`, `dcamera.m3u8`, `ecamera.m3u8`.
/// qcamera points at the stored `.ts` segments directly; the HEVC cameras point
/// at the on-demand transcode endpoint. Each URL is signed for media auth.
pub async fn camera_m3u8(
    State(state): State<AppState>,
    Path((fullname, cam)): Path<(String, String)>,
    AuthUser(user): AuthUser,
) -> AppResult<Response> {
    let stem = cam
        .strip_suffix(".m3u8")
        .ok_or_else(|| AppError::BadRequest("expected <camera>.m3u8".into()))?;
    let dongle = fullname
        .split_once('|')
        .map(|(d, _)| d.to_string())
        .ok_or_else(|| AppError::BadRequest("bad route name".into()))?;
    let ts = fullname.split_once('|').map(|(_, t)| t.to_string()).unwrap_or_default();
    let device = load_device(&state, &dongle).await?;
    if !user_owns(&state, &user, &device).await? {
        return Err(AppError::Forbidden("not authorized for device".into()));
    }

    #[derive(sqlx::FromRow)]
    struct SegLite {
        number: i64,
        qcam_url: String,
        fcam_url: String,
        dcam_url: String,
        ecam_url: String,
        qcam_duration: f64,
    }
    let segs: Vec<SegLite> = sqlx::query_as(
        "SELECT number, qcam_url, fcam_url, dcam_url, ecam_url, qcam_duration \
         FROM segments WHERE canonical_route_name = ? ORDER BY number ASC",
    )
    .bind(&fullname)
    .fetch_all(&state.pool)
    .await?;

    // For a given segment, does this camera exist, and what URL plays it?
    let public = &state.config.public_url;
    let media = |s: &SegLite| -> Option<String> {
        match stem {
            "qcamera" => (!s.qcam_url.is_empty()).then(|| s.qcam_url.clone()),
            "fcamera" => (!s.fcam_url.is_empty())
                .then(|| format!("{public}/v1/transcode/{dongle}/{ts}/{}/fcamera.ts", s.number)),
            "dcamera" => (!s.dcam_url.is_empty())
                .then(|| format!("{public}/v1/transcode/{dongle}/{ts}/{}/dcamera.ts", s.number)),
            "ecamera" => (!s.ecam_url.is_empty())
                .then(|| format!("{public}/v1/transcode/{dongle}/{ts}/{}/ecamera.ts", s.number)),
            _ => None,
        }
    };

    let sig = auth::issue_hs512_ttl(&state.config.jwt_secret_b64, &user.identity, 86_400)
        .map_err(AppError::Other)?;

    let mut m = String::from("#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:61\n#EXT-X-MEDIA-SEQUENCE:0\n#EXT-X-PLAYLIST-TYPE:VOD\n");
    let playable: Vec<(&SegLite, String)> =
        segs.iter().filter_map(|s| media(s).map(|u| (s, u))).collect();
    let mut expected = playable.first().map(|(s, _)| s.number).unwrap_or(0);
    for (s, url) in &playable {
        if s.number != expected {
            m.push_str("#EXT-X-DISCONTINUITY\n");
        }
        let dur = if s.qcam_duration > 0.0 { s.qcam_duration } else { 60.0 };
        m.push_str(&format!("#EXTINF:{dur:.3},{}\n", s.number));
        let sep = if url.contains('?') { '&' } else { '?' };
        m.push_str(&format!("{url}{sep}sig={sig}\n"));
        expected = s.number + 1;
    }
    m.push_str("#EXT-X-ENDLIST\n");

    Ok((
        [(axum::http::header::CONTENT_TYPE, "application/vnd.apple.mpegurl")],
        m,
    )
        .into_response())
}

/// GET /v1/me/unpaired_devices — devices that have registered (so we know their
/// key) but aren't yet claimed by anyone. Home onboarding: any logged-in user
/// can see and claim these.
pub async fn unpaired_devices(
    State(state): State<AppState>,
    AuthUser(_user): AuthUser,
) -> AppResult<Json<Value>> {
    let devices: Vec<Device> = sqlx::query_as(
        "SELECT * FROM devices WHERE owner_id IS NULL ORDER BY created_at DESC",
    )
    .fetch_all(&state.pool)
    .await?;
    let arr: Vec<Value> = devices
        .iter()
        .map(|d| {
            json!({
                "dongle_id": d.dongle_id,
                "serial": d.serial,
                "device_type": d.device_type,
                "online": d.online != 0,
                "last_athena_ping": d.last_athena_ping,
            })
        })
        .collect();
    Ok(Json(json!(arr)))
}

/// POST /v1/devices/:dongle_id/claim — claim an unpaired device for the caller.
/// Home-friendly alternative to the device-signed pair-token flow (pilotpair):
/// trusted users on a home server can claim a registered-but-unowned device.
pub async fn claim_device(
    State(state): State<AppState>,
    Path(dongle): Path<String>,
    AuthUser(user): AuthUser,
) -> AppResult<Json<Value>> {
    let device = load_device(&state, &dongle).await?;
    match device.owner_id {
        None => {}
        Some(owner) if owner == user.id => {
            return Ok(Json(json!({ "dongle_id": dongle, "already_owned": true })));
        }
        Some(_) => return Err(AppError::Forbidden("device already paired".into())),
    }
    sqlx::query("UPDATE devices SET owner_id = ? WHERE dongle_id = ? AND owner_id IS NULL")
        .bind(user.id)
        .bind(&dongle)
        .execute(&state.pool)
        .await?;
    sqlx::query("INSERT OR IGNORE INTO authorized_users (user_id, device_dongle_id, created_at) VALUES (?, ?, ?)")
        .bind(user.id)
        .bind(&dongle)
        .bind(crate::db::now_millis())
        .execute(&state.pool)
        .await?;
    tracing::info!(%dongle, user = %user.username, "device claimed");
    Ok(Json(json!({ "dongle_id": dongle, "first_pair": true })))
}

/// Does this user own (or share) the device?
async fn user_owns(state: &AppState, user: &User, device: &Device) -> AppResult<bool> {
    if device.owner_id == Some(user.id) {
        return Ok(true);
    }
    let shared: Option<(i64,)> = sqlx::query_as(
        "SELECT 1 FROM authorized_users WHERE user_id = ? AND device_dongle_id = ?",
    )
    .bind(user.id)
    .bind(&device.dongle_id)
    .fetch_optional(&state.pool)
    .await?;
    Ok(shared.is_some())
}
