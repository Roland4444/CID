-- Remote metadata cache. A revision is downloaded only when size/mtime changed,
-- or when the most recent document is absent. SHA-256 provides final deduplication.
CREATE TABLE IF NOT EXISTS synology_file_index (
    remote_path TEXT PRIMARY KEY,
    size_bytes BIGINT NOT NULL,
    modified_epoch BIGINT NOT NULL,
    scanned_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
