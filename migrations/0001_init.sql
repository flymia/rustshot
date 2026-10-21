-- Portable schema: must run unchanged on SQLite and PostgreSQL.
-- IDs are UUID strings, timestamps are unix seconds (BIGINT), no booleans.

CREATE TABLE users (
    id            TEXT PRIMARY KEY,
    username      TEXT NOT NULL UNIQUE,
    role          TEXT NOT NULL DEFAULT 'user',
    quota_bytes   BIGINT,
    quota_uploads BIGINT,
    used_bytes    BIGINT NOT NULL DEFAULT 0,
    upload_count  BIGINT NOT NULL DEFAULT 0,
    created_at    BIGINT NOT NULL
);

CREATE TABLE api_keys (
    id           TEXT PRIMARY KEY,
    user_id      TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    name         TEXT NOT NULL,
    prefix       TEXT NOT NULL,
    key_hash     TEXT NOT NULL UNIQUE,
    created_at   BIGINT NOT NULL,
    last_used_at BIGINT,
    revoked_at   BIGINT
);

CREATE INDEX idx_api_keys_user ON api_keys (user_id);

CREATE TABLE uploads (
    id            TEXT PRIMARY KEY,
    user_id       TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    slug          TEXT NOT NULL UNIQUE,
    ext           TEXT NOT NULL,
    content_type  TEXT NOT NULL,
    size_bytes    BIGINT NOT NULL,
    width         BIGINT NOT NULL,
    height        BIGINT NOT NULL,
    sha256        TEXT NOT NULL,
    storage_path  TEXT NOT NULL,
    original_name TEXT,
    created_at    BIGINT NOT NULL
);

CREATE INDEX idx_uploads_user_created ON uploads (user_id, created_at);
CREATE INDEX idx_uploads_user_sha256 ON uploads (user_id, sha256);
