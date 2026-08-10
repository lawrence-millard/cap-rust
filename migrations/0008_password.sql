ALTER TABLE videos ADD COLUMN IF NOT EXISTS access_mode TEXT;
ALTER TABLE videos ADD COLUMN IF NOT EXISTS access_password_hash TEXT;

UPDATE videos
SET access_mode = CASE WHEN public THEN 'public' ELSE 'private' END
WHERE access_mode IS NULL;

ALTER TABLE videos ALTER COLUMN access_mode SET NOT NULL;

ALTER TABLE videos DROP CONSTRAINT IF EXISTS videos_access_mode_check;
ALTER TABLE videos ADD CONSTRAINT videos_access_mode_check
    CHECK (access_mode IN ('public', 'private', 'password'));

ALTER TABLE videos DROP CONSTRAINT IF EXISTS videos_password_access_hash_check;
ALTER TABLE videos ADD CONSTRAINT videos_password_access_hash_check
    CHECK (access_mode <> 'password' OR access_password_hash IS NOT NULL);

CREATE OR REPLACE FUNCTION sync_video_access_compat()
RETURNS trigger AS $$
BEGIN
    IF TG_OP = 'INSERT' AND NEW.access_mode IS NULL THEN
        NEW.access_mode := CASE WHEN NEW.public THEN 'public' ELSE 'private' END;
    ELSIF TG_OP = 'UPDATE' AND NEW.access_mode IS DISTINCT FROM OLD.access_mode THEN
        NEW.public := NEW.access_mode = 'public';
    ELSIF TG_OP = 'UPDATE' AND NEW.public IS DISTINCT FROM OLD.public THEN
        NEW.access_mode := CASE WHEN NEW.public THEN 'public' ELSE 'private' END;
        NEW.access_password_hash := NULL;
    ELSE
        NEW.public := NEW.access_mode = 'public';
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS videos_access_compat ON videos;
CREATE TRIGGER videos_access_compat
BEFORE INSERT OR UPDATE OF public, access_mode ON videos
FOR EACH ROW EXECUTE FUNCTION sync_video_access_compat();
