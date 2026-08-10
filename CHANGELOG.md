# Changelog

All notable changes are documented here. This project follows Semantic
Versioning.

## [1.0.1] - 2026-08-10

### Fixed

- Cap Desktop contract-test assertions: match current session page copy and
  check JWT presence instead of comparing token values to the literal `"token"`.

## [1.0.0] - 2026-08-10

First stable release for **local-storage** Cap Desktop self-hosting. The HTTP
and desktop-compatibility surface for local Postgres + disk is treated as
stable. Breaking changes after 1.0 require a major version bump.

### Supported in 1.0

- Cap Desktop sign-in, record, upload, share, and playback against one service
- Local disk storage with signed upload/playback URLs
- Multipart upload emulation, Instant Mode mux, screenshots
- Username/password accounts, JWTs, hashed desktop API keys
- Public / private / password recording access
- Captions, comments, reactions, views, download preference controls
- Embeds, oEmbed, health checks, forward-only migrations

### Experimental (not part of the 1.0 support commitment)

- `STORAGE_BACKEND=s3` remains a beta path for single-part `desktopMP4` only.
  Screenshots, Instant Mode, multipart, captions, and deletion are unsupported.

### Added

- `VIDEO_DEFAULT_PUBLIC` (default `true` for Cap Desktop share-link compatibility;
  set `false` for private-by-default self-hosts)
- `CORS_ORIGINS` for extra allowed browser origins; CORS is limited to `WEB_URL`
  plus this list
- Access-cookie epoch so password changes invalidate unlock cookies
- Hashed API key storage (`token_hash`) with legacy plaintext-key fallback
- Playlist `download=true` enforcement when downloads are disabled for non-owners
- Share-page `controlsList="nodownload"` when downloads are disabled
- Integration coverage for password access, download preference, cache headers,
  and desktop delete/create validation

### Security / hardening

- Reject known placeholder `SIGN_SECRET` values at boot
- Account passwords capped at 1024 bytes; login uses a dummy Argon2 verify when
  the username is missing (timing equalization)
- Non-public signed media uses `Cache-Control: private, no-store`
- Multipart staging cleanup skips uploads marked `finalizing`
- Signed local URLs percent-encode path segments; `/up/` requires `Content-Length`
- Compose defaults `CAP_SIGNUPS=false`

### Changed

- Desktop video create upsert refreshes `source` / `is_screenshot`
- Desktop delete of a missing video returns 404
- Video JSON includes `accessMode`
- Owner `PATCH /api/videos/{id}` no longer touches `public` unless visibility is
  explicitly set (avoids wiping password mode via the compat trigger)

### Known Cap Desktop compatibility constraints

- Desktop session still delivers the API key in a localhost / custom-scheme
  redirect query string (Cap protocol)
- New recordings default to public unless `VIDEO_DEFAULT_PUBLIC=false`
- JWTs are not server-revocable before expiry; API keys remain revocable

## [0.3.0] - 2026-08-10

### Added

- Owner video list/detail/update/delete, status, download, and API-key
  management endpoints.
- Captions, timestamped comments/replies, reactions, daily unique views, and
  per-video owner download-preference metadata.
- Public embeds, oEmbed, caption/reaction data, view recording, and
  collaboration settings.
- Upload ownership checks, atomic writes, bounded multipart uploads, stale
  staging cleanup, and graceful mux-job shutdown.
- Beta S3-compatible direct single-part GET/PUT integration for `desktopMP4`
  recordings.
- Recording public/private/password access routes and policy enforcement for
  share, embed, playlist, and collaboration handlers.

### Changed

- Release builds now use `opt-level = 3` for server throughput instead of
  size-focused `opt-level = "z"`.
- Instant Mode muxing reports status/errors, validates bounded input, and times
  out after 30 minutes.
- Media uploads enforce a 20 GiB maximum and local playback handles standard
  single byte ranges.

### Limitations

- S3 supports direct `desktopMP4` uploads and reads only. Screenshots, Instant
  Mode segments, multipart uploads, and remote deletion are unavailable; local
  storage remains recommended. S3 caption deletion is rejected so its DB row is
  retained rather than silently orphaning remote data.
- Share and embed pages allow cross-origin framing for third-party embeds.
- No automated rollback, point-in-time recovery, or repair workflow is
  provided.

## [0.2.0]

- Multi-user username/password authentication, Cap Desktop connection, signed
  local uploads/playback, multipart upload emulation, and Instant Mode muxing.

[1.0.1]: https://github.com/lawrence-millard/cap-rust/compare/v1.0.0...v1.0.1
[1.0.0]: https://github.com/lawrence-millard/cap-rust/compare/v0.3.0...v1.0.0
[0.3.0]: https://github.com/lawrence-millard/cap-rust/compare/v0.2.0...v0.3.0
