# AGENTS.md

This file provides guidance to coding agents when working with code in this repository.

## Project Overview

Rust microservice that sends Firebase Cloud Messaging notifications for offline device features and generic alarms. It reads online/FCM data from Redis DB 0, notification history from DB 1, and alarm preferences/pending events from DB 3.

## Build & Development Commands

All commands use the Makefile:

- `make build` — format, lint (clippy), and build (default target)
- `make release` — production build with optimizations
- `make run` — hot-reload development server via cargo-watch
- `make test` — run all tests (single-threaded, with backtrace); Redis is expected to already be running on localhost
- `make test-coverage` — generate HTML/LCOV coverage reports via grcov
- `make check` — run `cargo audit` to find known vulnerabilities
- `make fmt` — format code with `cargo fmt`
- `make lint` — run `cargo clippy`
- `make clean` — clean build artifacts
- `make deps` — install dev dependencies (cargo-watch, grcov, llvm-tools)

Run a single test:
```bash
ENV=testing RUST_BACKTRACE=full cargo test <test_name> -- --nocapture --test-threads 1
```

**Tests**: Always run the full test suite with `make test` unless the user explicitly asks for a narrower test run. Tests must not be marked `#[ignore]`; Redis-backed tests are part of the normal suite. Assume Redis is already running on `localhost:6379`; do not skip tests on the assumption that external infrastructure is unavailable. `ENV=testing` switches Redis key patterns to `test_*`. Tests run single-threaded to ensure deterministic behavior.

## Architecture

**Main flow** (`src/main.rs`):
1. Initializes logging (rolling file appenders split by level) and loads env config
2. Connects to the online-status Redis database (`redis_client`)
3. Connects to notification-history DB 1 and alarms DB 3, deriving those database numbers from `ONLINE_REDIS_URI` when their dedicated URIs are omitted.
4. Initializes the FCM hub (`fcm_hub`) via `google-fcm1`
5. Creates a `DashMap` cache for notification deduplication (tracks recently-sent device notifications with configurable TTL)
6. Spawns a background tokio task (`notification_handle`) that:
   - Polls Redis every 10 seconds for all devices
   - Detects devices whose `modifiedAt` timestamp exceeds the offline threshold (`OFFLINE_TIMEOUT_SECONDS`) and whose `notificationSilenced` flag is not `true`
   - Groups due offline devices by `fcmToken`, so each recipient receives one notification for all currently due offline device features
   - Uses singular/plural notification bodies based on the grouped device count
   - Sends FCM notifications only for device features whose cache entry has expired
   - Updates the cache to prevent duplicate notifications within the timeout window
   - Persists sent-notification metadata to Redis after successful FCM sends
   - Reads pending DB 3 alarm events, groups them by FCM token and alarm type, persists history, and acknowledges them only after successful FCM delivery
   - Logs and continues on per-device errors (missing FCM tokens, transient Redis failures)

   The `JoinHandle` is stored and `abort()`ed when Rocket shuts down to ensure clean shutdown.
7. Starts the Rocket HTTP server

**Deduplication mechanism**: A `DashMap<String, u64>` cache stores `cache_key()` → `timestamp` entries for offline device features. On first offline detection, the device feature is cached but not notified. If it remains offline after `CACHE_TIMEOUT_SECONDS`, it is included in the next grouped notification for its `fcmToken`, and the cache timestamp is renewed after the send attempt. Device features that become online or are silenced are removed from the cache because `filter_online` treats all records excluded from the offline list as online for cache cleanup. Cache entries are not explicitly expired; stale entries stay in memory until the next poll encounters the same device feature and evaluates its timestamp. This is acceptable because: (1) devices usually come back online or are silenced, and (2) the DashMap is small (typically <1000 entries) relative to typical device counts.

**Notification history**: Successful FCM sends are persisted to Redis by `db::notification::save_sent_notification`. The main notification hash key is `notification:{id}` where `id` is `{sent_at_millis}-{sequence}`. Stored hash fields include `id`, `apiToken` (first affected token for compatibility), `apiTokens` (JSON array of all affected API tokens), `sentAt`, `title`, `body`, `deviceCount`, `devices` (JSON array of affected device/feature UUIDs and timestamps), `provider` (`fcm`), and `providerMessageId`. A Redis sorted-set index is also maintained per API token at `notifications:by_api_token:{api_token}` with `sentAt` as the score. Each save cleans notification hashes and index entries older than `NOTIFICATION_RETENTION_MILLIS` (90 days) for the affected API-token indexes.

**Module structure**:
- `config/` — logging setup and env loading, including `ONLINE_REDIS_URI`, `NOTIFICATIONS_REDIS_URI`, and `ALARMS_REDIS_URI`. Redis credentials are redacted in logs and debug output.
- `routes/` — single REST endpoint: `GET /keepalive` (health check for Kubernetes probes)
- `models/` — `Online` (device status with `device_uuid`, `feature_uuid`, `api_token`, `fcm_token`, `notification_silenced`; `created_at`/`modified_at` are `u64` millisecond epoch timestamps; `cache_key()` → `"{device_uuid}-{feature_uuid}"`), `Topic` (MQTT topic parser: `online/{device_uuid}/features/{feature_uuid}`). `Online` is constructed directly from Redis hash fields with no serde derives.
- `db/` — Redis operations:
  - `online` scans DB 0 `online_*` keys (or `test_*` when `ENV=testing`) and loads heartbeat/FCM fields. Notification preferences are never read from DB 0; `db::alarm` overlays them from DB 3 before offline filtering.
  - `notification` persists sent FCM notification history, maintains per-API-token sorted-set indexes, and performs 90-day retention cleanup during saves.
  - `alarm` overlays DB 3 silence preferences, reads/validates pending events, resolves FCM tokens from DB 0, and atomically removes delivered/silenced events from the pending index.
- `notifications.rs` — groups due offline device features by FCM token, applies cache-based notification deduplication, and builds singular/plural notification bodies.
- `errors/` — custom error types (`DbError`, `RedisError`, `ApiError`) implementing Rocket's `Responder`; error variants wrap their source errors via `#[source]`
- `catchers/` — HTTP error handlers (400, 404, 500, 503)

## Local Development Setup

Before running the service locally:

1. **Start Redis** (required):
   ```bash
   # Option 1: Via Docker
   docker run --name redis -p 6379:6379 -d redis redis-server \
     --save 60 1 --loglevel warning --user redisuser on '>Password1!' '~*' '+@all'

   # Option 2: Via Docker Compose (from home-anthill root)
   cd sharded-mongodb-compose && docker compose up -d redis && cd ..
   ```

2. **Generate Firebase service account key** (for FCM notifications):
   - Visit [Firebase Console](https://console.firebase.google.com/)
   - Project Settings → Service Accounts → Generate new private key
   - Save as `serviceAccountKey.json` in the project root (gitignored)

3. **Copy `.env_template` to `.env`** and verify values match your local Redis setup.

4. **Run the service**:
   ```bash
   make run   # Hot-reload development server via cargo-watch
   ```

## Configuration

- **Rocket.toml**: Debug port 8088 (localhost), Release port 80 (0.0.0.0). **Note:** Redis, environment, and secrets are not configured here — all come from `.env` and runtime env vars. `secret_key` is **not** stored in the file; set `ROCKET_SECRET_KEY` env var at runtime (`openssl rand -base64 32`).
- **Environment**: Copy `.env_template` to `.env` for local dev. `ONLINE_REDIS_URI` uses DB 0, `NOTIFICATIONS_REDIS_URI` DB 1, and `ALARMS_REDIS_URI` DB 3. If the latter two are omitted, the service derives DB 1/3 from `ONLINE_REDIS_URI`; DB 15 remains test-only.
- **Redis URI**: Supports both `redis://` and `rediss://` (TLS). When `REDIS_PASSWORD` is set, credentials are injected as `scheme://username:password@host:port` using percent-encoding via the `urlencoding` crate. Special characters in username/password are URL-encoded to prevent URI corruption. If `REDIS_USERNAME` is set but `REDIS_PASSWORD` is empty, a warning is logged and no authentication is attempted. The same credential-injection behavior is used for `NOTIFICATIONS_REDIS_URI` when present.
- **FCM credentials**: Path to `serviceAccountKey.json` is set via `FCM_SERVICE_ACCOUNT_KEY_PATH` (defaults to `./serviceAccountKey.json`). Must be injected at runtime — not baked into the Docker image.
- **Rust edition**: 2024; release profile uses LTO, opt-level 3, panic=abort
- **Formatting**: `rustfmt.toml` — 4 spaces, max width 120

## Error Handling Strategy

The notification loop prioritizes resilience over consistency. When processing all devices:

- **Per-device errors are logged and skipped**, not propagated to abort the entire operation. Examples:
  - Missing or empty FCM token for a device: log warning, skip notification for that device, continue to next device
  - `notificationSilenced=true` for a device feature: omit it from offline notification batches and clean its cache entry
  - HGETALL failure on a Redis key: log error, skip that device, continue scanning
  - Transient FCM API failures: log error, skip notification for that device (next poll will retry)
  - Notification-history persistence failures after successful FCM sends: log error, continue the loop

- **SCAN errors during Redis key iteration**: logged but don't prevent processing of keys that were successfully fetched

- **HTTP error responses** are handled via custom error types (`DbError`, `RedisError`, `ApiError`) that implement Rocket's `Responder` trait to return JSON-formatted errors.

This design ensures one misbehaving device or transient Redis issue doesn't disable notifications for all other devices. The tradeoff: a device might miss a notification due to a transient error, but it will be retried on the next poll cycle (10 seconds later).

## Runtime secrets (never hardcode)

| Secret | How to supply |
|--------|--------------|
| `ROCKET_SECRET_KEY` | `openssl rand -base64 32` → env var |
| `FCM_SERVICE_ACCOUNT_KEY_PATH` | Mount file via Docker volume / secret |
| `ONLINE_REDIS_URI` | Env var; credentials in URI are redacted in logs |
| `NOTIFICATIONS_REDIS_URI` | Optional env var for separate notification history storage; credentials are redacted in logs |
| `ALARMS_REDIS_URI` | Optional env var for Redis DB 3 alarm settings and pending events; credentials are redacted in logs |

## CI/CD

GitHub Actions workflow (`.github/workflows/docker-image.yml`):
- Test job: installs Rust, Redis, Mosquitto; runs `make test-coverage`
- Build job: multi-platform Docker image via buildx, published to `ks89/alarm-notifier` on Docker Hub
- The Docker runtime image does **not** bundle `.env` or `serviceAccountKey.json` — both must be injected at deploy time

## Recent Changes

See `CHANGELOG.md` for detailed release notes. Version 4.0.0 added:
- Grouped offline-device notifications by FCM token, including singular/plural notification bodies.
- `notificationSilenced=true` support for suppressing offline notifications per Redis online record.
- Redis-backed sent-notification history with affected devices, API tokens, FCM provider message ID, and sent timestamp.
- Per-API-token notification-history indexes and automatic 90-day retention cleanup.
- Optional `NOTIFICATIONS_REDIS_URI` for storing notification history separately from online-status data.
- Additional tests for malformed MQTT topics, JSON 404 catcher responses, and silenced offline devices.
