# rustshot

Self-hosted screenshot upload server (Rust). One binary: HTTP API + admin CLI. User-facing docs live in `README.md`.

## Commands

```sh
export PATH="$(echo ~/.rustup/toolchains/stable-*/bin):$PATH"

cargo build
cargo test                       # unit + integration tests on a temp SQLite DB
cargo clippy --all-targets       # must stay warning-free
cargo fmt                        # rustfmt.toml: max_width = 110

# Same integration suite on PostgreSQL
docker run -d --rm --name rustshot-pg -e POSTGRES_PASSWORD=test -p 55432:5432 postgres:17-alpine
RUSTSHOT_TEST_DATABASE_URL=postgres://postgres:test@127.0.0.1:55432/postgres cargo test --test api
```

Every change touching SQL must pass the test suite on **both** SQLite and PostgreSQL.

## Architecture

| File | Responsibility |
|---|---|
| `src/main.rs` | Logging setup, calls `cli::run` |
| `src/cli.rs` | clap commands (`serve`, `migrate`, `user`, `key`, `import`), global config via flags/env (`RUSTSHOT_*`) |
| `src/service.rs` | `Ctx` (db + storage + public URL) and operations shared by API and CLI: `store_screenshot`, `delete_upload`, `delete_user`, input validation |
| `src/db.rs` | All SQL. Models `User`, `ApiKey`, `Upload` |
| `src/storage.rs` | Filesystem layout `<root>/<user_id>/<YYYY>/<MM>/<slug>.<ext>`, temp files in `<root>/.tmp` |
| `src/image.rs` | Content-based format detection (PNG/JPEG/GIF/WebP only) |
| `src/ids.rs` | Share slugs (26 chars base36), API keys (`rsk_` + 40 base62, stored as SHA-256), UUIDs |
| `src/size.rs` | Human-readable byte sizes / `unlimited` limits for CLI args |
| `src/import.rs` | `rustshot import` filesystem walker |
| `src/api/` | axum router (`mod.rs`), Bearer auth extractors `Auth`/`Admin` (`auth.rs`), `ApiError` -> JSON `{"error"}` (`error.rs`), handlers per resource |
| `migrations/` | sqlx migrations, embedded via `sqlx::migrate!` and run on startup |
| `tests/api.rs` | End-to-end API tests via `tower::ServiceExt::oneshot` |
| `.github/workflows/` | `ci.yml` (fmt, clippy, tests on SQLite + PostgreSQL), `release.yml` (on `v*` tags: musl binaries, GitHub release, GHCR image via the `prebuilt` Dockerfile target) |

Business logic that both API and CLI need belongs in `service.rs`, not in handlers or CLI code.

## Rules and invariants

- **Portable SQL** (sqlx `Any` driver, one schema for SQLite + PostgreSQL):
  - placeholders `$1, $2, ...` only
  - only `TEXT` and `BIGINT` columns: IDs are UUID strings, timestamps are unix seconds, no booleans (roles are text)
  - no aggregates whose type differs per backend (e.g. `SUM`); usage is kept in counter columns on `users`
  - never edit an existing migration, add a new numbered file
- **Upload ordering** in `Ctx::store_screenshot`: write temp file -> begin tx -> reserve quota -> insert row -> move file into place -> commit. A committed row must always have its file.
- **Quotas** are reserved with one conditional `UPDATE` (`db::reserve_quota`) so concurrent uploads can't exceed them. Deleting an upload releases quota in the same transaction.
- Share URLs are `/share/{slug}.{ext}`; uniqueness is enforced by the `UNIQUE` index on `uploads.slug`.
- Never accept SVG or other active content. Format comes from file contents, never from client-supplied names or content types.
- API key secrets are only returned once at creation (`NewKeyView`); never log or list them.
- Storage paths in the DB are relative; always resolve via `Storage::absolute` (rejects traversal).

## Library versions with recent breaking changes

- **sqlx 0.9**: `query*()` take `&'static str`; dynamic SQL must be wrapped in `sqlx::AssertSqlSafe(String)`. Call `sqlx::any::install_default_drivers()` before connecting (done in `db::connect`).
- **rand 0.10**: use `rand::random_range` / `rand::RngExt`.
- **axum 0.8**: path params use `{id}` syntax; `FromRequestParts` uses native async fn (no `async_trait`).

## Conventions

- Errors: `anyhow` in CLI, `thiserror` enums in library code, `ApiError` at the HTTP boundary
- Keep comments sparse and explanatory (why, not what)
