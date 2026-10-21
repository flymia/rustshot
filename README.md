# rustshot

Self-hosted screenshot upload server with a Bearer-token API, built for ShareX and similar tools.

- Accepts only raster screenshots (PNG, JPEG, GIF, WebP). The type is detected from the file contents, never from the file name
- Separate user accounts with API keys, upload tracking and per-user quotas (bytes and/or number of uploads)
- SQLite for a single node, PostgreSQL to run several stateless instances. The schema is the same for both
- Unguessable, unique share links: `https://example.com/share/k3v9q0x1m2b7n4c8z5a6s1d0f2.png`
- A CLI to manage users and keys and to bulk-import screenshots from disk
- A single static binary. Migrations run automatically on start

## Quick start

```sh
cargo build --release

# Creates the database on first use and prints an API key
./target/release/rustshot user create marc --admin --quota-bytes 10GiB

RUSTSHOT_PUBLIC_URL=https://shots.example.com ./target/release/rustshot serve
```

Upload a screenshot:

```sh
curl -H "Authorization: Bearer rsk_..." -F file=@screenshot.png https://shots.example.com/api/v1/uploads
# or as raw body
curl -H "Authorization: Bearer rsk_..." -H "X-Filename: screenshot.png" \
     --data-binary @screenshot.png https://shots.example.com/api/v1/uploads
```

```json
{
  "id": "e467aa3b-090c-4f47-86b3-8e6f56373adf",
  "url": "https://shots.example.com/share/r8iuh2u0osn8a0am7ca62w1apq.png",
  "file_name": "r8iuh2u0osn8a0am7ca62w1apq.png",
  "content_type": "image/png",
  "size_bytes": 48211,
  "width": 1920,
  "height": 1080,
  "sha256": "b8f4cf...",
  "original_name": "screenshot.png",
  "created_at": "2026-10-21T19:42:57Z"
}
```

### ShareX

Download a ready-made custom uploader that already contains your key, then import it in ShareX (double-click the file):

```sh
curl -H "Authorization: Bearer rsk_..." -o rustshot.sxcu https://shots.example.com/api/v1/sharex
```

To set it up by hand, use `POST <public-url>/api/v1/uploads`, body `Form data (multipart/form-data)`, file form name `file`, header `Authorization: Bearer rsk_...`, URL `{json:url}` and error message `{json:error}`.

## Configuration

Every option can be set as a flag or an environment variable.

| Variable | Default | Description |
|---|---|---|
| `RUSTSHOT_DATABASE_URL` | `sqlite://rustshot.db?mode=rwc` | `sqlite://...` or `postgres://user:pass@host/db` |
| `RUSTSHOT_DB_MAX_CONNECTIONS` | `10` | Connection pool size |
| `RUSTSHOT_STORAGE_DIR` | `./data` | Where screenshots are stored |
| `RUSTSHOT_PUBLIC_URL` | `http://localhost:8080` | Base URL for share links |
| `RUSTSHOT_SKIP_MIGRATIONS` | `false` | Don't migrate on start (then run `rustshot migrate` yourself) |
| `RUSTSHOT_BIND` | `127.0.0.1:8080` | Listen address (`serve` only) |
| `RUSTSHOT_MAX_UPLOAD_SIZE` | `20MiB` | Max size per screenshot (`serve` only) |
| `RUSTSHOT_CORS_ORIGINS` | none | Comma-separated origins allowed to use the API from a browser, e.g. a web UI |
| `RUSTSHOT_METRICS_TOKEN` | none | Bearer token required to scrape `/metrics`. Without it `/metrics` is public |
| `RUSTSHOT_LOG_LEVEL` | `info` | `error`, `warn`, `info`, `debug` or `trace` (also `--log-level`). `info` logs user actions (uploads, deletions, key and user changes) to stdout. Takes precedence over `RUST_LOG` |
| `RUST_LOG` | `info,sqlx=warn` | Fine-grained log filter, used when no log level is set |

Run rustshot behind a TLS-terminating reverse proxy (Caddy, nginx, Traefik). API keys are sent in clear text otherwise.

## Storage layout

```
<storage-dir>/<user-id>/<YYYY>/<MM>/<slug>.<ext>
<storage-dir>/.tmp/                  in-flight uploads, atomically renamed into place
```

The database stores paths relative to the storage directory, so the directory can be moved freely. The date is the upload time. For imports it's the file's modification time.

## Scaling

- **Single node:** SQLite in WAL mode. Nothing else to run
- **Multiple nodes:** Point every instance at the same PostgreSQL database and the same storage directory (NFS, CephFS, a shared volume). Instances are stateless. Migrations take a database lock, so instances can start in parallel. See `docker-compose.yml`
- Quota accounting is a single conditional `UPDATE`, so concurrent uploads can't exceed a quota on either backend
- Share files are immutable and are served with `Cache-Control: immutable`, `ETag`/`Last-Modified` and range support, so a CDN or caching proxy can be put in front of `/share/`

## CLI

```text
rustshot serve                                   run the HTTP server
rustshot migrate                                 apply migrations and exit
rustshot user create <name> [--admin] [--quota-bytes 5GiB] [--quota-uploads 1000] [--no-key]
rustshot user list
rustshot user update <name> [--role admin|user] [--quota-bytes 10GiB|unlimited] [--quota-uploads N|unlimited]
rustshot user delete <name> --yes                deletes keys, uploads and files
rustshot key create <name> [--name laptop]
rustshot key list <name>
rustshot key revoke <key-id>
rustshot import <name> <paths>... [-r] [--use-current-time] [--dry-run]
```

`import` walks the given files and directories (with `-r` recursively, ignoring dot-files), skips anything that isn't a supported image, charges the user's quota and prints the share URL of each file. Files the user already uploaded are skipped, so it is safe to re-run over the same folder.

## API

All `/api/v1` endpoints require `Authorization: Bearer <api key>`. Errors are `{"error": "message"}`.

| Method | Path | Description |
|---|---|---|
| `GET` | `/share/{slug}.{ext}` | Public: the screenshot |
| `GET` | `/healthz` | Public: health check, see [Monitoring](#monitoring) |
| `GET` | `/metrics` | Prometheus metrics, see [Monitoring](#monitoring) |
| `GET` | `/api/v1/me` | Current user incl. quota and usage |
| `POST` | `/api/v1/uploads` | Upload (multipart field `file`, or raw body + optional `X-Filename`) → `201`, or `200` with the existing upload if the user already uploaded the same file (SHA-256) |
| `GET` | `/api/v1/uploads?limit=50&offset=0` | Own uploads, newest first |
| `GET` | `/api/v1/uploads/{id}` | One upload |
| `DELETE` | `/api/v1/uploads/{id}` | Delete upload and file, refunds quota → `204` |
| `GET` | `/api/v1/keys` | Own API keys (never includes secrets) |
| `POST` | `/api/v1/keys` | `{"name": "laptop"}` → new key, `token` is shown only once |
| `DELETE` | `/api/v1/keys/{id}` | Revoke a key |
| `GET` | `/api/v1/sharex` | ShareX `.sxcu` config for the calling key |

Admin only (`role = admin`):

| Method | Path | Description |
|---|---|---|
| `GET` | `/api/v1/admin/users` | List users |
| `POST` | `/api/v1/admin/users` | `{"username", "role"?, "quota_bytes"?, "quota_uploads"?}` |
| `GET` | `/api/v1/admin/users/{id}` | Get user |
| `PATCH` | `/api/v1/admin/users/{id}` | Change `role`, `quota_bytes`, `quota_uploads` (`null` = unlimited) |
| `DELETE` | `/api/v1/admin/users/{id}` | Delete user with all data |
| `GET`/`POST` | `/api/v1/admin/users/{id}/keys` | List / create keys for a user |
| `DELETE` | `/api/v1/admin/users/{id}/keys/{key_id}` | Revoke a user's key |

Status codes: `401` missing/invalid key, `403` not an admin, `413` file too large, `415` not a supported image, `507` quota exceeded.

## Monitoring

`GET /healthz` checks the database (`SELECT 1`) and the storage directory (writes and removes a temp file), each with a 5 s timeout. It returns `200`, or `503` if a check fails (details go to the log):

```json
{"status": "ok", "version": "0.1.0", "uptime_seconds": 3600, "checks": {"database": "ok", "storage": "ok"}}
```

`GET /metrics` serves Prometheus metrics. Set `RUSTSHOT_METRICS_TOKEN` to require `Authorization: Bearer <token>` (API keys are not accepted), or restrict the path in your reverse proxy.

| Metric | Type | Description |
|---|---|---|
| `rustshot_build_info{version}` | gauge | Always `1` |
| `rustshot_start_time_seconds` | gauge | Process start, unix seconds |
| `rustshot_users{role}` | gauge | Users by role (`admin`, `user`) |
| `rustshot_uploads` | gauge | Stored screenshots |
| `rustshot_storage_used_bytes` | gauge | Total size of stored screenshots |
| `rustshot_recent_uploads{window}` | gauge | Screenshots created in the last `1h`, `24h`, `7d` |
| `rustshot_last_upload_timestamp_seconds` | gauge | Creation time of the newest screenshot (absent while there are none) |
| `rustshot_api_keys{state}` | gauge | API keys by state (`active`, `revoked`) |
| `rustshot_api_keys_recently_used{window}` | gauge | Active keys used in the last `24h` |
| `rustshot_http_requests_total{method,status}` | counter | HTTP requests handled by this process |

The `rustshot_http_*` counters are per instance. All other metrics come from the database and are the same on every instance sharing it. Imported screenshots count by their file modification time.

```yaml
scrape_configs:
  - job_name: rustshot
    authorization:
      credentials: <RUSTSHOT_METRICS_TOKEN>
    static_configs:
      - targets: ["rustshot:8080"]
```

## Security notes

- API keys (`rsk_` + 40 random chars) are stored as SHA-256 hashes only
- Slugs are 26 random base36 chars (about 134 bits) and enforced unique by the database
- SVG and other active content is rejected. Files are served with their detected content type, `X-Content-Type-Options: nosniff` and a `default-src 'none'` CSP
- Metadata (e.g. EXIF) is stored unchanged

## Development

```sh
cargo test                                              # SQLite
docker run -d --rm -e POSTGRES_PASSWORD=test -p 55432:5432 postgres:17-alpine
RUSTSHOT_TEST_DATABASE_URL=postgres://postgres:test@127.0.0.1:55432/postgres cargo test   # PostgreSQL
```
