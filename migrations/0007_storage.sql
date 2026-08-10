ALTER TABLE videos
    ADD COLUMN IF NOT EXISTS storage_backend TEXT NOT NULL DEFAULT 'local';

ALTER TABLE videos
    DROP CONSTRAINT IF EXISTS videos_storage_backend_check;

ALTER TABLE videos
    ADD CONSTRAINT videos_storage_backend_check
    CHECK (storage_backend IN ('local', 's3'));
