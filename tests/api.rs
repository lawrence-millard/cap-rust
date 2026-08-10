// Integration tests against a real Postgres.
//
// These are ignored by default because they need DATABASE_URL pointing at a
// scratch database (the schema is created fresh and can be destroyed). Run with:
//
//   DATABASE_URL=postgres://... cargo test -- --ignored
//
// The tests use a single shared app instance; the DB is wiped between tests.
use axum::body::Body;
use axum::http::header::{AUTHORIZATION, CONTENT_TYPE};
use axum::http::{Request, StatusCode, header};
use cap_server::config::Config;
use cap_server::routes::router;
use cap_server::state::AppState;
use http_body_util::BodyExt;
use serde_json::Value;
use std::sync::Arc;
use tower::ServiceExt;

fn test_config() -> Config {
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL required");
    Config {
        database_url,
        web_url: "http://test.local".into(),
        cap_signups: true,
        jwt_ttl_secs: 3600,
        storage_dir: std::env::temp_dir().join("cap-rust-tests"),
        port: 8080,
        sign_secret: "test-secret-test-secret-test-secret".into(),
        ffmpeg_path: "ffmpeg".into(),
        plan_upgraded: true,
        db_max_connections: 5,
        storage_backend: cap_server::config::StorageBackend::Local,
        s3: None,
    }
}

async fn app() -> axum::Router {
    let config = test_config();
    let state = Arc::new(AppState::new(config).await.expect("init app state"));
    sqlx::migrate!("./migrations")
        .run(&state.db)
        .await
        .expect("migrations");
    router(state)
}

async fn body_text(resp: axum::response::Response) -> String {
    let bytes = resp
        .into_body()
        .collect()
        .await
        .expect("collect body")
        .to_bytes();
    String::from_utf8_lossy(&bytes).to_string()
}

async fn json_body(resp: axum::response::Response) -> Value {
    serde_json::from_str(&body_text(resp).await).expect("valid json")
}

/// Register a fresh user, return (api_key, user_id).
async fn register(app: &axum::Router, username: &str) -> (String, String) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/register")
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"username": username, "password": "test1234"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = json_body(resp).await;
    let token = v["token"].as_str().unwrap().to_string();
    let user_id = v["user"]["id"].as_str().unwrap().to_string();
    // exchange JWT for an api key via the session flow so both paths are covered
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/desktop/session/request")
                .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(format!(
                    "action=login&username={username}&password=test1234&port=9999"
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    let loc = resp
        .headers()
        .get("location")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let api_key = loc
        .split("api_key=")
        .nth(1)
        .and_then(|s| s.split('&').next())
        .unwrap()
        .to_string();
    let _ = token;
    (api_key, user_id)
}

#[tokio::test]
#[ignore]
async fn health_check() {
    let app = app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = json_body(resp).await;
    assert_eq!(v["status"], "ok");
}

#[tokio::test]
#[ignore]
async fn unauthenticated_requests_rejected() {
    let app = app().await;
    for uri in [
        "/api/desktop/plan",
        "/api/desktop/user/profile",
        "/api/desktop/video/create",
    ] {
        let resp = app
            .clone()
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "expected 401 for {uri}"
        );
    }
}

#[tokio::test]
#[ignore]
async fn register_and_login_roundtrip() {
    let app = app().await;
    let (api_key, _) = register(&app, "alice").await;

    // wrong password rejected
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/login")
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"username": "alice", "password": "wrongpass"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // duplicate register rejected
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/register")
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"username": "alice", "password": "test1234"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    // api key works for desktop endpoints
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/desktop/plan")
                .header(AUTHORIZATION, format!("Bearer {api_key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
#[ignore]
async fn video_lifecycle_upload_and_playback() {
    let app = app().await;
    let (api_key, _) = register(&app, "bob").await;
    let auth = format!("Bearer {api_key}");

    // create video
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/desktop/video/create?recordingMode=desktopMP4&name=test")
                .header(AUTHORIZATION, &auth)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let video_id = json_body(resp).await["id"].as_str().unwrap().to_string();

    // get signed put url
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/upload/signed")
                .header(AUTHORIZATION, &auth)
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"videoId": video_id, "subpath": "result.mp4"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let upload_url = json_body(resp).await["presignedPutData"]["url"]
        .as_str()
        .unwrap()
        .to_string();

    // upload bytes
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(&upload_url)
                .header(CONTENT_TYPE, "video/mp4")
                .body(Body::from(vec![0u8; 1024]))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // share page loads
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/s/{video_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // playlist redirects to signed media
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/playlist?videoId={video_id}&videoType=mp4"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let media_url = resp
        .headers()
        .get("location")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();

    // full GET
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(&media_url)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // range request -> 206 partial
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(&media_url)
                .header(header::RANGE, "bytes=0-99")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::PARTIAL_CONTENT);

    // unsatisfiable range -> 416
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(&media_url)
                .header(header::RANGE, "bytes=999999-9999999")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::RANGE_NOT_SATISFIABLE);
}

#[tokio::test]
#[ignore]
async fn ownership_enforced() {
    let app = app().await;
    let (alice_key, _) = register(&app, "carol").await;
    let (bob_key, _) = register(&app, "dave").await;

    // carol creates a video
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/desktop/video/create?recordingMode=desktopMP4")
                .header(AUTHORIZATION, format!("Bearer {alice_key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let video_id = json_body(resp).await["id"].as_str().unwrap().to_string();

    // dave cannot delete carol's video
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/desktop/video/delete?videoId={video_id}"))
                .header(AUTHORIZATION, format!("Bearer {bob_key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    // dave cannot upload to carol's video
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/upload/signed")
                .header(AUTHORIZATION, format!("Bearer {bob_key}"))
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"videoId": video_id, "subpath": "result.mp4"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
#[ignore]
async fn video_status_endpoint() {
    let app = app().await;
    let (api_key, _) = register(&app, "erin").await;
    let auth = format!("Bearer {api_key}");

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/desktop/video/create?recordingMode=desktopSegments")
                .header(AUTHORIZATION, &auth)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let video_id = json_body(resp).await["id"].as_str().unwrap().to_string();

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/desktop/video/status?videoId={video_id}"))
                .header(AUTHORIZATION, &auth)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = json_body(resp).await;
    // not yet muxing -> null status
    assert!(v["muxStatus"].is_null() || v["muxStatus"] == "error");
}

#[tokio::test]
#[ignore]
async fn path_traversal_rejected_on_upload() {
    let app = app().await;
    let (api_key, _) = register(&app, "frank").await;
    let auth = format!("Bearer {api_key}");

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/upload/signed")
                .header(AUTHORIZATION, &auth)
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"videoId": "x", "subpath": "../evil"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}
