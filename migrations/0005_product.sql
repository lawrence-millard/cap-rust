ALTER TABLE videos
    ADD COLUMN IF NOT EXISTS updated_at TIMESTAMPTZ NOT NULL DEFAULT now();

ALTER TABLE auth_api_keys
    ADD COLUMN IF NOT EXISTS key_id BIGSERIAL;

CREATE UNIQUE INDEX IF NOT EXISTS idx_auth_api_keys_key_id
    ON auth_api_keys(key_id);

CREATE INDEX IF NOT EXISTS idx_videos_owner_created
    ON videos(owner_id, created_at DESC);
