# cap-server

[![CI](https://github.com/lawrence-millard/cap-rust/actions/workflows/ci.yml/badge.svg)](https://github.com/lawrence-millard/cap-rust/actions/workflows/ci.yml)
[![Docker](https://github.com/lawrence-millard/cap-rust/actions/workflows/docker.yml/badge.svg)](https://github.com/lawrence-millard/cap-rust/actions/workflows/docker.yml)
[![License: AGPL v3](https://img.shields.io/badge/License-AGPL_v3-blue.svg)](LICENSE)

Lightweight, wire-compatible CAP server in Rust. Point Cap Desktop at it and record, upload, share, and play back — all from a single ~3 MB binary on a 1 vCPU / 1 GB VPS.

No Next.js, no Node, no MySQL, no separate media server. Metadata lives in Neon Postgres; recordings land on local disk; signed URLs serve playback with range requests; ffmpeg muxes Instant Mode segments in the background.

## What works

- Desktop sign-in handshake (`/api/desktop/session/request`) with username/password login or account creation → API key → loopback redirect
- `POST /api/auth/register` and `POST /api/auth/login` return a JWT for programmatic access
- `user/profile`, `plan`, `organizations`, `s3/config/get`, `storage/integrations`, `changelog/status`
- `video/create`, `video/progress`, `video/delete`
- Uploads: single-part signed PUT, signed batch (Instant Mode segments), multipart initiate/presign-part/complete/abort, `recording-complete`
- Playback: `/api/playlist` (mp4 + HLS segments), signed `/media` with `Range` support, share pages at `/s/{videoId}`
- Instant Mode (desktopSegments) muxed to `result.mp4` via ffmpeg in the background; poll progress via `video/status`
- `GET /health` reports liveness + DB reachability (200 ok / 503 degraded)
- Graceful shutdown on SIGINT/SIGTERM; abandoned multipart upload staging dirs are swept hourly

Not implemented (out of scope for a simple multi-user server): Stripe, email, comments, orgs/teams, Google Drive, transcription, web dashboard.

## Quick start

Download a prebuilt static binary from [Releases](https://github.com/lawrence-millard/cap-rust/releases), or pull the Docker image:

```bash
docker pull ghcr.io/lawrence-millard/cap-rust:latest
```

Or build from source:

```bash
cp .env.example .env   # edit DATABASE_URL, WEB_URL, SIGN_SECRET
cargo build --release
./target/release/cap-server
```

Or with Docker Compose (pulls the published image):

```bash
cp .env.example .env   # edit the values
docker compose up -d
```

Generate a strong `SIGN_SECRET`:

```bash
openssl rand -hex 32
```

## Environment variables

| Variable | Required | Default | Description |
| --- | --- | --- | --- |
| `DATABASE_URL` | yes | — | Postgres connection string |
| `WEB_URL` | yes | — | Public URL of this server, e.g. `https://cap.example.com` |
| `SIGN_SECRET` | yes | — | Long random string (min 16 chars); signs upload/playback URLs and JWT tokens. Required — server refuses to start without it. |
| `CAP_SIGNUPS` | no | `true` | If `false`, registration is disabled — only existing accounts can log in from the desktop connect page. |
| `JWT_TTL` | no | `2592000` | JWT token lifetime in seconds (default 30 days). |
| `DB_MAX_CONNECTIONS` | no | `5` | Postgres connection pool size. |
| `STORAGE_DIR` | no | `./data` | Where recordings are stored |
| `PORT` | no | `8080` | HTTP listen port |
| `FFMPEG_PATH` | no | `ffmpeg` | ffmpeg binary for Instant Mode muxing |
| `RUST_LOG` | no | `info` | Log level |

## Deploy to a VPS

With Docker Compose behind Caddy (recommended):

```
docker-compose.yml   # runs the app
Caddyfile            # automatic HTTPS for your domain
```

1. Point your domain's DNS A record at the VPS.
2. Set the env vars in your shell or `.env`.
3. `docker compose up -d && caddy start`
4. In Cap Desktop → Settings → Cap Server URL → your `https://domain`.
5. The connect page opens in your browser — create an account or log in.

The prebuilt release binaries and Docker image both need `ffmpeg` on `PATH` for Instant Mode muxing (already included in the Docker image; install it yourself when using the bare binary).

Example `Caddyfile`:

```
cap.example.com {
    reverse_proxy localhost:8080
}
```

## Contract tests

Simulates the exact requests Cap Desktop makes (shapes from `packages/web-api-contract/src/desktop.ts` and `apps/desktop/src-tauri/src/api.rs`):

```bash
DATABASE_URL=... WEB_URL=... SIGN_SECRET=... CAP_SIGNUPS=true STORAGE_DIR=./data ./target/debug/cap-server &
bash scripts/contract-test.sh
```

## Security

- Always set a strong `SIGN_SECRET`. Set `CAP_SIGNUPS=false` on any internet-facing instance where you don't want open registration.
- Serve over HTTPS (Caddy example below) — signed URLs, API keys, and JWTs are bearer credentials.
- Passwords are hashed with Argon2; API keys and JWTs are per-user.
- The Docker image runs as a non-root user; recordings live in the `cap-data` volume.

## Notes

- The Neon `channel_binding=require` connection parameter is stripped automatically.
- Recordings are stored under `{STORAGE_DIR}/{user}/{video}/…` mirroring Cap's S3 keys (`result.mp4`, `raw-upload.mp4`, `segments/…`), so each user's recordings are isolated on disk.
- Share links are only served for videos where `public` is true (the default). Recordings uploaded before multi-user support are owned by the legacy built-in user `u_single_user`.
