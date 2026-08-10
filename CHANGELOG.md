# Changelog

All notable changes are documented here. This project follows Semantic
Versioning.

## [0.3.0] - 2026-08-10

### Added

- Owner video list/detail/update/delete, status, download, and API-key
  management endpoints.
- Captions, timestamped comments/replies, reactions, daily unique views, and
  per-video owner download-preference metadata. Media handlers do not enforce
  that preference.
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

[0.3.0]: https://github.com/lawrence-millard/cap-rust/compare/v0.2.0...v0.3.0
