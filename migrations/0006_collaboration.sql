ALTER TABLE videos
    ADD COLUMN IF NOT EXISTS downloads_enabled BOOLEAN NOT NULL DEFAULT true;

CREATE TABLE IF NOT EXISTS video_captions (
    id TEXT PRIMARY KEY,
    video_id TEXT NOT NULL REFERENCES videos(id) ON DELETE CASCADE,
    language TEXT NOT NULL CHECK (char_length(language) BETWEEN 2 AND 35 AND language ~ '^[A-Za-z0-9-]+$'),
    label TEXT NOT NULL CHECK (char_length(label) BETWEEN 1 AND 100),
    storage_key TEXT NOT NULL UNIQUE,
    enabled BOOLEAN NOT NULL DEFAULT true,
    is_default BOOLEAN NOT NULL DEFAULT false,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (NOT is_default OR enabled)
);

CREATE INDEX IF NOT EXISTS idx_video_captions_video ON video_captions(video_id, created_at);
CREATE UNIQUE INDEX IF NOT EXISTS idx_video_captions_default ON video_captions(video_id) WHERE is_default;

CREATE TABLE IF NOT EXISTS video_comments (
    id TEXT PRIMARY KEY,
    video_id TEXT NOT NULL REFERENCES videos(id) ON DELETE CASCADE,
    author_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    parent_id TEXT,
    timestamp_ms BIGINT NOT NULL CHECK (timestamp_ms BETWEEN 0 AND 86400000),
    content TEXT NOT NULL CHECK (char_length(btrim(content)) BETWEEN 1 AND 2000),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (video_id, id),
    FOREIGN KEY (video_id, parent_id) REFERENCES video_comments(video_id, id) ON DELETE CASCADE,
    CHECK (parent_id IS NULL OR parent_id <> id)
);

CREATE INDEX IF NOT EXISTS idx_video_comments_page ON video_comments(video_id, created_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS idx_video_comments_parent ON video_comments(parent_id) WHERE parent_id IS NOT NULL;

CREATE TABLE IF NOT EXISTS video_reactions (
    video_id TEXT NOT NULL REFERENCES videos(id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    emoji TEXT NOT NULL CHECK (emoji IN ('👍', '❤️', '😂', '😮', '😢', '🎉')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (video_id, user_id, emoji)
);

CREATE INDEX IF NOT EXISTS idx_video_reactions_list ON video_reactions(video_id, emoji);

CREATE TABLE IF NOT EXISTS video_views (
    video_id TEXT NOT NULL REFERENCES videos(id) ON DELETE CASCADE,
    visitor_id TEXT NOT NULL CHECK (char_length(visitor_id) = 36),
    viewed_on DATE NOT NULL DEFAULT CURRENT_DATE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (video_id, visitor_id, viewed_on)
);

CREATE INDEX IF NOT EXISTS idx_video_views_aggregate ON video_views(video_id, viewed_on);
