CREATE TABLE IF NOT EXISTS multipart_uploads (
    upload_id TEXT PRIMARY KEY,
    owner_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    video_id TEXT NOT NULL REFERENCES videos(id) ON DELETE CASCADE,
    destination TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'uploading'
        CHECK (status IN ('uploading', 'finalizing', 'completed', 'aborted')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_multipart_uploads_video
    ON multipart_uploads(video_id);
