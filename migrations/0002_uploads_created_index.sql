-- Time window counts for /metrics (uploads in the last hour etc.).
CREATE INDEX idx_uploads_created ON uploads (created_at);
