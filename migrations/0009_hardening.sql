-- Cookie invalidation epoch for password-protected recordings.
ALTER TABLE videos
    ADD COLUMN IF NOT EXISTS access_cookie_epoch INTEGER NOT NULL DEFAULT 0;

-- Hashed desktop API keys (legacy rows keep plaintext id until re-auth).
ALTER TABLE auth_api_keys
    ADD COLUMN IF NOT EXISTS token_hash TEXT;

CREATE UNIQUE INDEX IF NOT EXISTS idx_auth_api_keys_token_hash
    ON auth_api_keys(token_hash)
    WHERE token_hash IS NOT NULL;

-- Only clear password protection when visibility actually flips via `public`.
CREATE OR REPLACE FUNCTION sync_video_access_compat()
RETURNS trigger AS $$
BEGIN
    IF TG_OP = 'INSERT' AND NEW.access_mode IS NULL THEN
        NEW.access_mode := CASE WHEN NEW.public THEN 'public' ELSE 'private' END;
    ELSIF TG_OP = 'UPDATE' AND NEW.access_mode IS DISTINCT FROM OLD.access_mode THEN
        NEW.public := NEW.access_mode = 'public';
        IF NEW.access_mode <> 'password' THEN
            NEW.access_password_hash := NULL;
        END IF;
        NEW.access_cookie_epoch := COALESCE(OLD.access_cookie_epoch, 0) + 1;
    ELSIF TG_OP = 'UPDATE' AND NEW.public IS DISTINCT FROM OLD.public THEN
        NEW.access_mode := CASE WHEN NEW.public THEN 'public' ELSE 'private' END;
        NEW.access_password_hash := NULL;
        NEW.access_cookie_epoch := COALESCE(OLD.access_cookie_epoch, 0) + 1;
    ELSE
        NEW.public := NEW.access_mode = 'public';
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;
