CREATE TABLE IF NOT EXISTS users (
    id TEXT PRIMARY KEY,
    name TEXT,
    email TEXT,
    image_url TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS auth_api_keys (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    source TEXT NOT NULL DEFAULT 'desktop',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_auth_api_keys_user ON auth_api_keys(user_id);

CREATE TABLE IF NOT EXISTS videos (
    id TEXT PRIMARY KEY,
    owner_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name TEXT,
    source JSONB,
    public BOOLEAN NOT NULL DEFAULT true,
    metadata JSONB,
    duration DOUBLE PRECISION,
    width INTEGER,
    height INTEGER,
    fps DOUBLE PRECISION,
    is_screenshot BOOLEAN NOT NULL DEFAULT false,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_videos_owner ON videos(owner_id);

CREATE TABLE IF NOT EXISTS video_uploads (
    video_id TEXT PRIMARY KEY REFERENCES videos(id) ON DELETE CASCADE,
    uploaded BIGINT NOT NULL DEFAULT 0,
    total BIGINT NOT NULL DEFAULT 0,
    mode TEXT NOT NULL DEFAULT 'singlepart',
    phase TEXT,
    raw_file_key TEXT,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
