//! End-to-end tests of the HTTP API.
//!
//! Runs against a temporary SQLite database by default. Set
//! `RUSTSHOT_TEST_DATABASE_URL=postgres://...` to run the same suite on PostgreSQL.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use rustshot::api::{self, ApiConfig};
use rustshot::db::{self, NewUser, User};
use rustshot::service::Ctx;
use rustshot::storage::Storage;
use serde_json::{Value, json};
use tower::ServiceExt;

/// A valid 1x1 PNG.
const PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52, 0x00,
    0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4, 0x89, 0x00,
    0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0xf0, 0x1f, 0x00, 0x05,
    0x00, 0x01, 0xff, 0x89, 0x99, 0x3d, 0x1d, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42,
    0x60, 0x82,
];
const PUBLIC_URL: &str = "https://shots.example.com";

struct TestApp {
    app: Router,
    ctx: Ctx,
    _dir: tempfile::TempDir,
}

async fn setup() -> TestApp {
    setup_with_limit(1 << 20).await
}

async fn setup_with_limit(max_upload_bytes: usize) -> TestApp {
    let dir = tempfile::tempdir().unwrap();
    let url = std::env::var("RUSTSHOT_TEST_DATABASE_URL")
        .unwrap_or_else(|_| format!("sqlite://{}?mode=rwc", dir.path().join("test.db").display()));
    let db = db::connect(&url, 5).await.unwrap();
    db::migrate(&db).await.unwrap();
    let ctx = Ctx {
        db,
        storage: Storage::open(dir.path().join("data")).await.unwrap(),
        public_url: PUBLIC_URL.into(),
    };
    let config = ApiConfig {
        max_upload_bytes,
        cors_origins: vec![],
    };
    TestApp {
        app: api::router(ctx.clone(), config),
        ctx,
        _dir: dir,
    }
}

impl TestApp {
    async fn user(&self, role: &str, quota_bytes: Option<i64>, quota_uploads: Option<i64>) -> (User, String) {
        // Unique names so the suite can share one PostgreSQL database.
        let username = format!("u{}", &uuid_simple()[..12]);
        let user = db::create_user(
            &self.ctx.db,
            NewUser {
                username: &username,
                role,
                quota_bytes,
                quota_uploads,
            },
        )
        .await
        .unwrap();
        let (_, token) = db::create_api_key(&self.ctx.db, &user.id, "test").await.unwrap();
        (user, token)
    }

    async fn send(&self, req: Request<Body>) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
        let res = self.app.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let headers = res.headers().clone();
        let body = res.into_body().collect().await.unwrap().to_bytes().to_vec();
        (status, headers, body)
    }

    async fn json(&self, req: Request<Body>) -> (StatusCode, Value) {
        let (status, _, body) = self.send(req).await;
        let value = if body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&body).unwrap()
        };
        (status, value)
    }

    async fn upload(&self, token: &str, data: &[u8]) -> (StatusCode, Value) {
        self.json(multipart_upload(token, "shot.png", data)).await
    }
}

fn uuid_simple() -> String {
    rustshot::ids::new_id().replace('-', "")
}

/// PNG with trailing bytes, to get distinct sizes and hashes.
fn png_padded(extra: usize) -> Vec<u8> {
    let mut data = PNG.to_vec();
    data.extend(std::iter::repeat_n(0u8, extra));
    data
}

fn multipart_upload(token: &str, filename: &str, data: &[u8]) -> Request<Body> {
    let boundary = "rustshotboundary";
    let mut body = Vec::new();
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\n\
             Content-Type: application/octet-stream\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(data);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    Request::post("/api/v1/uploads")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(Body::from(body))
        .unwrap()
}

fn get(uri: &str, token: &str) -> Request<Body> {
    Request::get(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap()
}

fn delete(uri: &str, token: &str) -> Request<Body> {
    Request::delete(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap()
}

fn send_json(method: &str, uri: &str, token: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn share_path(url: &str) -> &str {
    url.strip_prefix(PUBLIC_URL).unwrap()
}

#[tokio::test]
async fn upload_and_serve() {
    let t = setup().await;
    let (user, token) = t.user("user", None, None).await;

    let (status, body) = t.upload(&token, PNG).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let url = body["url"].as_str().unwrap();
    let file = url.strip_prefix(&format!("{PUBLIC_URL}/share/")).unwrap();
    let (slug, ext) = file.split_once('.').unwrap();
    assert_eq!(ext, "png");
    assert!(rustshot::ids::is_valid_slug(slug), "{slug}");
    assert_eq!(body["original_name"], "shot.png");
    assert_eq!(
        (body["width"].as_i64(), body["height"].as_i64()),
        (Some(1), Some(1))
    );

    // Stored in <user_id>/<YYYY>/<MM>/<slug>.<ext>.
    let now = time::OffsetDateTime::now_utc();
    let rel = format!("{}/{:04}/{:02}/{file}", user.id, now.year(), now.month() as u8);
    assert!(t.ctx.storage.absolute(&rel).unwrap().is_file(), "missing {rel}");

    // Public, no auth needed.
    let (status, headers, data) = t
        .send(Request::get(share_path(url)).body(Body::empty()).unwrap())
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(data, PNG);
    assert_eq!(headers[header::CONTENT_TYPE], "image/png");
    assert_eq!(headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
    assert!(
        headers[header::CACHE_CONTROL]
            .to_str()
            .unwrap()
            .contains("immutable")
    );

    // Wrong extension or unknown slug.
    let (status, _, _) = t
        .send(
            Request::get(format!("/share/{slug}.jpg"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = t
        .send(
            Request::get(format!("/share/{}.png", "a".repeat(26)))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = t
        .send(
            Request::get("/share/..%2F..%2Fetc%2Fpasswd")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Usage is tracked.
    let (status, me) = t.json(get("/api/v1/me", &token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(me["used_bytes"], PNG.len());
    assert_eq!(me["upload_count"], 1);
}

#[tokio::test]
async fn raw_body_upload() {
    let t = setup().await;
    let (_, token) = t.user("user", None, None).await;
    let req = Request::post("/api/v1/uploads")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "image/png")
        .header("x-filename", "../../evil\t.png")
        .body(Body::from(PNG))
        .unwrap();
    let (status, body) = t.json(req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["original_name"], "evil.png");
}

#[tokio::test]
async fn slugs_are_unique() {
    let t = setup().await;
    let (_, token) = t.user("user", None, None).await;
    let mut urls = std::collections::HashSet::new();
    for _ in 0..20 {
        let (status, body) = t.upload(&token, PNG).await;
        assert_eq!(status, StatusCode::CREATED);
        assert!(urls.insert(body["url"].as_str().unwrap().to_string()));
    }
}

#[tokio::test]
async fn authentication() {
    let t = setup().await;
    let (user, token) = t.user("user", None, None).await;

    let (status, headers, _) = t
        .send(Request::get("/api/v1/me").body(Body::empty()).unwrap())
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(headers[header::WWW_AUTHENTICATE], "Bearer");

    let (status, _) = t.json(get("/api/v1/me", "rsk_wrong")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, _) = t.upload("rsk_wrong", PNG).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Keys can be created and revoked through the API.
    let (status, key) = t
        .json(send_json(
            "POST",
            "/api/v1/keys",
            &token,
            json!({"name": "laptop"}),
        ))
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let new_token = key["token"].as_str().unwrap();
    let (status, me) = t.json(get("/api/v1/me", new_token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(me["id"], user.id);

    let (status, keys) = t.json(get("/api/v1/keys", &token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(keys.as_array().unwrap().len(), 2);
    assert!(
        !keys.to_string().contains(new_token),
        "secrets must never be listed"
    );

    let (status, _) = t
        .json(delete(
            &format!("/api/v1/keys/{}", key["id"].as_str().unwrap()),
            &token,
        ))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = t.json(get("/api/v1/me", new_token)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn rejects_non_images() {
    let t = setup().await;
    let (_, token) = t.user("user", None, None).await;
    for data in [
        &b"just some text"[..],
        b"<svg xmlns=\"http://www.w3.org/2000/svg\"><script/></svg>",
        b"",
    ] {
        let (status, body) = t.upload(&token, data).await;
        assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE, "{body}");
    }
    let (_, me) = t.json(get("/api/v1/me", &token)).await;
    assert_eq!(me["upload_count"], 0);
}

#[tokio::test]
async fn enforces_upload_size_limit() {
    let t = setup_with_limit(1000).await;
    let (_, token) = t.user("user", None, None).await;
    let (status, _) = t.upload(&token, &png_padded(900)).await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, body) = t.upload(&token, &png_padded(1000)).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{body}");
    let (status, _) = t.upload(&token, &png_padded(200_000)).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn enforces_quotas_and_refunds_on_delete() {
    let t = setup().await;
    let size = PNG.len() as i64;
    let (_, token) = t.user("user", Some(size * 2), None).await;

    let (_, first) = t.upload(&token, PNG).await;
    assert_eq!(t.upload(&token, PNG).await.0, StatusCode::CREATED);
    let (status, body) = t.upload(&token, PNG).await;
    assert_eq!(status, StatusCode::INSUFFICIENT_STORAGE);
    assert_eq!(body["error"], "upload quota exceeded");

    // Deleting gives the quota back and removes the file.
    let id = first["id"].as_str().unwrap();
    let (status, _) = t.json(delete(&format!("/api/v1/uploads/{id}"), &token)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _, _) = t
        .send(
            Request::get(share_path(first["url"].as_str().unwrap()))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(t.upload(&token, PNG).await.0, StatusCode::CREATED);

    // Upload count quota.
    let (_, token) = t.user("user", None, Some(1)).await;
    assert_eq!(t.upload(&token, PNG).await.0, StatusCode::CREATED);
    assert_eq!(t.upload(&token, PNG).await.0, StatusCode::INSUFFICIENT_STORAGE);
}

#[tokio::test]
async fn concurrent_uploads_respect_quota() {
    let t = setup().await;
    let (user, token) = t.user("user", None, Some(3)).await;
    let tasks: Vec<_> = (0..12)
        .map(|_| {
            let app = t.app.clone();
            let req = multipart_upload(&token, "shot.png", PNG);
            tokio::spawn(async move { app.oneshot(req).await.unwrap().status() })
        })
        .collect();
    let mut created = 0;
    for task in tasks {
        let status = task.await.unwrap();
        assert!(
            matches!(status, StatusCode::CREATED | StatusCode::INSUFFICIENT_STORAGE),
            "{status}"
        );
        created += usize::from(status == StatusCode::CREATED);
    }
    assert_eq!(created, 3);
    let user = db::get_user(&t.ctx.db, &user.id).await.unwrap().unwrap();
    assert_eq!(user.upload_count, 3);
    assert_eq!(user.used_bytes, 3 * PNG.len() as i64);
}

#[tokio::test]
async fn users_are_isolated() {
    let t = setup().await;
    let (_, alice) = t.user("user", None, None).await;
    let (_, bob) = t.user("user", None, None).await;
    let (_, upload) = t.upload(&alice, PNG).await;
    let uri = format!("/api/v1/uploads/{}", upload["id"].as_str().unwrap());

    assert_eq!(t.json(get(&uri, &bob)).await.0, StatusCode::NOT_FOUND);
    assert_eq!(t.json(delete(&uri, &bob)).await.0, StatusCode::NOT_FOUND);
    assert_eq!(t.json(get(&uri, &alice)).await.0, StatusCode::OK);

    let (_, list) = t.json(get("/api/v1/uploads", &bob)).await;
    assert_eq!(list["total"], 0);
    assert!(list["items"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn list_pagination() {
    let t = setup().await;
    let (_, token) = t.user("user", None, None).await;
    for _ in 0..5 {
        t.upload(&token, PNG).await;
    }
    let (_, page1) = t.json(get("/api/v1/uploads?limit=3", &token)).await;
    let (_, page2) = t.json(get("/api/v1/uploads?limit=3&offset=3", &token)).await;
    assert_eq!(page1["total"], 5);
    assert_eq!(page1["items"].as_array().unwrap().len(), 3);
    assert_eq!(page2["items"].as_array().unwrap().len(), 2);
    let ids: std::collections::HashSet<_> = page1["items"]
        .as_array()
        .unwrap()
        .iter()
        .chain(page2["items"].as_array().unwrap())
        .map(|i| i["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids.len(), 5);
}

#[tokio::test]
async fn admin_user_management() {
    let t = setup().await;
    let (admin, admin_token) = t.user("admin", None, None).await;
    let (_, user_token) = t.user("user", None, None).await;

    let (status, _) = t.json(get("/api/v1/admin/users", &user_token)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let name = format!("new{}", &uuid_simple()[..8]);
    let body = json!({"username": name, "quota_bytes": 10});
    let (status, created) = t
        .json(send_json(
            "POST",
            "/api/v1/admin/users",
            &admin_token,
            body.clone(),
        ))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["role"], "user");
    assert_eq!(created["quota_bytes"], 10);
    let (status, _) = t
        .json(send_json("POST", "/api/v1/admin/users", &admin_token, body))
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _) = t
        .json(send_json(
            "POST",
            "/api/v1/admin/users",
            &admin_token,
            json!({"username": "Bad Name"}),
        ))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let id = created["id"].as_str().unwrap();
    let (status, key) = t
        .json(send_json(
            "POST",
            &format!("/api/v1/admin/users/{id}/keys"),
            &admin_token,
            json!({"name": "sharex"}),
        ))
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let new_token = key["token"].as_str().unwrap();
    assert_eq!(t.upload(new_token, PNG).await.0, StatusCode::INSUFFICIENT_STORAGE);

    // null = unlimited, absent fields stay unchanged.
    let (status, patched) = t
        .json(send_json(
            "PATCH",
            &format!("/api/v1/admin/users/{id}"),
            &admin_token,
            json!({"quota_bytes": null}),
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(patched["quota_bytes"], Value::Null);
    let (_, upload) = t.upload(new_token, PNG).await;
    let file = t.ctx.storage.root().join(id);
    assert!(file.is_dir());

    // Deleting a user removes keys, uploads and files.
    let (status, _) = t
        .json(delete(&format!("/api/v1/admin/users/{id}"), &admin_token))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(!file.exists());
    assert_eq!(
        t.json(get("/api/v1/me", new_token)).await.0,
        StatusCode::UNAUTHORIZED
    );
    let (status, _, _) = t
        .send(
            Request::get(share_path(upload["url"].as_str().unwrap()))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = t
        .json(delete(&format!("/api/v1/admin/users/{}", admin.id), &admin_token))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn sharex_config() {
    let t = setup().await;
    let (_, token) = t.user("user", None, None).await;
    let (status, headers, body) = t.send(get("/api/v1/sharex", &token)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        headers[header::CONTENT_DISPOSITION]
            .to_str()
            .unwrap()
            .contains(".sxcu")
    );
    let config: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(config["RequestURL"], format!("{PUBLIC_URL}/api/v1/uploads"));
    assert_eq!(config["Headers"]["Authorization"], format!("Bearer {token}"));
    assert_eq!(config["URL"], "{json:url}");
}
