# cap-server

[![CI](https://github.com/lawrence-millard/cap-rust/actions/workflows/ci.yml/badge.svg)](https://github.com/lawrence-millard/cap-rust/actions/workflows/ci.yml)
[![Docker](https://github.com/lawrence-millard/cap-rust/actions/workflows/docker.yml/badge.svg)](https://github.com/lawrence-millard/cap-rust/actions/workflows/docker.yml)
[![License: AGPL v3](https://img.shields.io/badge/License-AGPL_v3-blue.svg)](LICENSE)

Lightweight CAP-compatible server in Rust. Cap Desktop can sign in, record,
upload, share, and play recordings against one service backed by Postgres and
local disk, with beta direct `desktopMP4` S3 storage.

## Features

- Username/password account creation and login; Argon2 password hashes, JWTs,
  per-device API keys, API-key listing, and revocation.
- Cap Desktop session handshake, profile and plan responses, video creation,
  upload progress, status polling, deletion, feedback/log compatibility sinks,
  and storage/changelog compatibility responses.
- Video list/get/update/delete, public/private visibility, metadata, owner
  download, screenshots, MP4 playback, HLS playback for Instant Mode segments,
  share pages, embeds, and oEmbed.
- Signed single-part and batch PUT uploads. Local multipart initiate, part
  upload, complete, abort, and abandoned-staging cleanup.
- Background `ffmpeg` muxing of Instant Mode fMP4 segments into `result.mp4`,
  with status/error reporting and a 30-minute job timeout.
- Owner-managed WebVTT captions with language, label, enabled/default state,
  signed upload/read URLs, and public enabled-caption listing.
- Timestamped comments and one-level replies for authenticated users who can
  see a video. Authors can edit; authors or video owners can delete.
- Authenticated reaction toggles for `👍`, `❤️`, `😂`, `😮`, `😢`, and `🎉`,
  plus public aggregate counts.
- Cookie-deduplicated public views, counted once per visitor/video/day, with
  owner totals and up to 366 daily buckets.
- Per-video download preference: share pages use `controlsList="nodownload"` when
  disabled, and `GET /api/playlist?...&download=true` is forbidden for non-owners.
  Streaming playback URLs remain available so viewers can still watch.
- `GET /health` DB reachability checks.
- Automatic forward-only SQL migrations and graceful SIGINT/SIGTERM shutdown.

### Password access

Account passwords work for registration and login. Owners can set recording
access to public, private, or password through `PATCH /api/videos/{videoId}/access`.
Password share pages unlock through
`POST /api/public/videos/{videoId}/access/unlock`; a signed HttpOnly cookie grants
share, embed, playlist, and collaboration access for 15 minutes. Changing or
clearing the recording password invalidates existing unlock cookies. Generated
media URLs remain bearer URLs until they expire.

## Routes

Authentication is enforced per handler. Account/session, oEmbed, playlist,
changelog, and `/api/public/*` routes are unauthenticated; owner and
authenticated collaboration routes accept a JWT or API key.

| Method | Route | Purpose |
| --- | --- | --- |
| `GET` | `/health` | Liveness and Postgres reachability |
| `GET` | `/s/{videoId}`, `/embed/{videoId}` | Public share and embed pages |
| `GET` | `/media/{key}` | Signed local-media read with single-range support |
| `PUT`, `POST` | `/up/{key}` | Signed local-media upload |
| `GET`, `POST` | `/api/desktop/session/request` | Desktop browser sign-in/account creation and API-key redirect |
| `POST` | `/api/auth/register`, `/api/auth/login` | JWT registration and login |
| `GET` | `/api/oembed` | Public oEmbed response for local public share/embed URLs |
| `GET` | `/api/videos`, `/api/videos/{videoId}` | Owner video list/detail |
| `PATCH`, `DELETE` | `/api/videos/{videoId}` | Owner metadata/visibility update or deletion |
| `GET` | `/api/videos/{videoId}/status`, `/download` | Owner upload/mux status and signed download |
| `GET`, `POST` | `/api/videos/{videoId}/captions` | Owner caption list/create |
| `PUT`, `PATCH`, `DELETE` | `/api/videos/{videoId}/captions/{captionId}` | Owner caption update/delete |
| `GET`, `POST` | `/api/videos/{videoId}/comments` | Visible-video comment list/create |
| `GET`, `PATCH`, `DELETE` | `/api/videos/{videoId}/comments/{commentId}` | Comment detail/update/delete |
| `GET`, `PUT` | `/api/videos/{videoId}/reactions` | Authenticated reaction aggregates/toggle |
| `GET` | `/api/videos/{videoId}/views` | Owner view totals |
| `GET`, `PATCH` | `/api/videos/{videoId}/collaboration` | Owner download-preference metadata |
| `GET` | `/api/public/videos/{videoId}/captions`, `/reactions`, `/collaboration` | Public collaboration data |
| `POST` | `/api/public/videos/{videoId}/views` | Record public view |
| `PATCH` | `/api/videos/{videoId}/access` | Set public, private, or password recording access |
| `POST` | `/api/public/videos/{videoId}/access/unlock` | Unlock password recording for 15 minutes |
| `GET`, `DELETE` | `/api/api-keys`, `/api/api-keys/{keyId}` | List and revoke caller's API keys |
| `GET` | `/api/desktop/user/profile`, `/plan`, `/organizations`, `/s3/config/get`, `/storage/integrations` | Desktop compatibility data; organizations are empty and storage config points Desktop at this server |
| `GET`, `DELETE`, `POST` | `/api/desktop/video/create`, `/video/status`, `/video/delete`, `/video/progress` | Desktop video lifecycle |
| `POST` | `/api/desktop/feedback`, `/logs` | Authenticated compatibility sinks; payloads are not retained |
| `POST` | `/api/upload/signed`, `/signed/batch` | Signed local upload URLs |
| `POST` | `/api/upload/multipart/initiate`, `/presign-part`, `/complete`, `/abort` | Local multipart lifecycle |
| `POST` | `/api/upload/recording-complete` | Queue Instant Mode mux |
| `GET` | `/api/playlist` | Public MP4 redirect or generated segment playlist |
| `GET` | `/api/changelog`, `/changelog/status` | Empty/no-update Desktop compatibility responses |

## Limits

- Upload body or completed multipart object: 20 GiB maximum.
- Multipart: 10,000 parts maximum, 5 GiB per part, contiguous part numbers.
- Signed batch: 10,000 paths maximum. Upload URLs last 1 hour; generated media
  and caption read URLs last 24 hours.
- Instant Mode: 8 MiB manifest, 100,000 total segments, and 30-minute mux
  timeout. `ffmpeg` is required.
- Video and comment page size: 1-100, default 50. Comment offset: 0-10,000.
- Video name: 1-200 bytes. Metadata JSON: object up to 64 KiB.
- Comment: 1-2,000 bytes; timestamp from 0 to 86,400,000 ms. Replies may
  target top-level comments only.
- Caption language: 2-35 ASCII letters, digits, or hyphens. Label: 1-100
  bytes. Only one enabled default caption per video.
- Username: 3-32 bytes, case-sensitive. Account password: 8-1024 bytes.
  JWT default lifetime: 30 days.
- Media supports one HTTP byte range per request, not multipart ranges.

## Quick start

Download a binary from [Releases](https://github.com/lawrence-millard/cap-rust/releases),
pull `ghcr.io/lawrence-millard/cap-rust:latest`, or build:

```bash
cp .env.example .env
cargo build --release
set -a; . ./.env; set +a
./target/release/cap-server
```

Generate `SIGN_SECRET` with `openssl rand -hex 32`. Bare binaries need
`ffmpeg` on `PATH`; Docker image includes it. Docker Compose:

```bash
docker compose up -d
```

Put server behind HTTPS, then set Cap Desktop's Cap Server URL to `WEB_URL`.

## Configuration

| Variable | Required | Default | Description |
| --- | --- | --- | --- |
| `DATABASE_URL` | yes | none | Postgres connection string |
| `SIGN_SECRET` | yes | none | At least 16 characters; rejects known placeholders; signs JWTs and local upload/media URLs |
| `WEB_URL` | no | `http://localhost:8080` | Public server URL, without trailing slash |
| `CORS_ORIGINS` | no | none | Extra allowed CORS origins (comma-separated); `WEB_URL` is always allowed |
| `CAP_SIGNUPS` | no | `true` | Set `false` or `0` to disable new accounts (compose defaults to `false`) |
| `CAP_PLAN_UPGRADED` | no | `true` | Plan value returned to Cap Desktop |
| `JWT_TTL` | no | `2592000` | JWT lifetime in seconds |
| `DB_MAX_CONNECTIONS` | no | `5` | Postgres pool maximum; values below 1 become 1 |
| `STORAGE_BACKEND` | no | `local` | `local` or `s3`; see S3 limitations below |
| `STORAGE_DIR` | no | `./data` | Local recordings and multipart staging directory |
| `S3_ENDPOINT` | for `s3` | none | HTTP(S) S3-compatible endpoint |
| `S3_REGION` | for `s3` | none | SigV4 region |
| `S3_BUCKET` | for `s3` | none | Bucket name |
| `S3_ACCESS_KEY` | for `s3` | none | SigV4 access key |
| `S3_SECRET_KEY` | for `s3` | none | SigV4 secret key |
| `S3_PATH_STYLE` | no | `false` | `true`/`1` for path-style presigned URLs |
| `PORT` | no | `8080` | HTTP listen port |
| `FFMPEG_PATH` | no | `ffmpeg` | Instant Mode muxer executable |
| `RUST_LOG` | no | `info` | Tracing filter |

### S3 beta and limitations

S3 is a beta path for direct, single-part `desktopMP4` uploads and reads through
SigV4 presigned GET/PUT URLs. Screenshots, Instant Mode segments, multipart
uploads, captions, and object deletion are not supported S3 workflows. Caption
deletion is rejected for S3-backed videos so existing DB rows are not removed
while remote objects remain. Use local storage for full route support and
production deployments. Native S3 multipart APIs, object listing/deletion,
remote `ffmpeg` input/output, temporary credentials, session tokens, and
credential refresh are not implemented.

## Migrations and backup

Embedded migrations run automatically at startup before HTTP serving. They are
forward-only; back up before upgrading. For a consistent backup, stop writes
(normally stop server), then back up both Postgres and `STORAGE_DIR`:

```bash
pg_dump --format=custom "$DATABASE_URL" --file=cap-server.dump
tar -C "$(dirname "$STORAGE_DIR")" -czf cap-storage.tar.gz "$(basename "$STORAGE_DIR")"
```

Restore using standard `pg_restore` and filesystem extraction into an empty
target, with server stopped. This project does not provide automated rollback,
point-in-time recovery, or repair tooling.

## Contract tests

```bash
DATABASE_URL=... WEB_URL=... SIGN_SECRET=... CAP_SIGNUPS=true STORAGE_DIR=./data ./target/debug/cap-server &
bash scripts/contract-test.sh
```

## Security

- Use HTTPS. JWTs, API keys, and signed URLs are bearer credentials.
- Use strong `SIGN_SECRET` (not a placeholder); set `CAP_SIGNUPS=false` when open
  registration is unwanted.
- Desktop API keys are stored hashed; legacy plaintext key rows still authenticate
  until those devices re-login.
- Recordings use `{STORAGE_DIR}/{user}/{video}/...`; signed URL paths are
  traversal-checked and uploads publish through atomic rename.
- Share, embed, playlist, public collaboration, and view routes enforce current
  public/private/password access mode. Share and embed pages intentionally allow
  cross-origin framing so share links can be embedded on other sites.
- Non-public signed media responses use `Cache-Control: private, no-store`.
- CORS is limited to `WEB_URL` plus optional `CORS_ORIGINS`.

## Notes

- Neon `channel_binding` query parameters are stripped because sqlx does not
  support them.
- Existing pre-multi-user recordings belong to legacy user `u_single_user`.
- See [CHANGELOG.md](CHANGELOG.md) for release changes.
