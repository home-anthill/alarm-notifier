# Changelog

## 4.0.0

### Features

- Add singular/plural level threshold notification messages through the existing grouped alarm delivery path.
- Renamed the service, Cargo package/binary, Docker image, and repository references from `online-alarm` to `alarm-notifier` without changing offline detection or FCM behavior.
- Added Redis DB 3 alarm preference and pending-event consumption without mixing alarm data into the online state or notification history.
- Added grouped FCM notifications for motion, thermostat mode errors, and generic alarm types, acknowledging events only after successful delivery.
- Added alarm type metadata to notification history while preserving the existing offline-history format.
- Grouped due offline devices by FCM token, so each recipient gets a single notification for multiple offline devices.
- Skipped offline push notifications for Redis online records with `notificationSilenced=true`.
- Added singular/plural notification bodies so grouped alerts report the number of offline devices.
- Persisted sent notification metadata in Redis, including affected devices, API tokens, provider message ID, and sent timestamp.
- Added per-API-token Redis indexes for notification history lookup.
- Added automatic 90-day retention cleanup for stored notification history and API-token indexes.
- Added optional `NOTIFICATIONS_REDIS_URI` support for storing notification history separately from online-status data.
- Added optional `ALARMS_REDIS_URI` support with a Redis DB 3 fallback.

### Tests

- Added error-case coverage for malformed MQTT topics:
  - missing `features/{feature_uuid}` segment
  - invalid static path segments
  - empty device or feature identifiers
- Added coverage for the JSON 404 catcher response.
- Added unit coverage for silenced offline devices being omitted from notification batches.


# 3.0.0

### Features

- Made the FCM service-account key path configurable through `FCM_SERVICE_ACCOUNT_KEY_PATH`.
- Registered the missing 503 catcher so service-unavailable responses use the application's JSON format.

### Bug fixes

- URL-encoded Redis usernames and passwords before injecting them into Redis URIs.
- Fixed Redis credential injection for `rediss://` TLS URIs.
- Added warnings for incomplete Redis authentication settings and invalid `LOG_LEVEL` values.
- Logged missing or empty `fcmToken` values instead of silently sending invalid FCM requests.
- Logged Redis SCAN iteration errors instead of silently discarding them.
- Removed `.unwrap()` from `ApiError::respond_to`.
- Handled per-key `HGETALL` failures with `error!` and `continue` instead of aborting the full lookup.
- Returned an error for missing Redis date fields instead of treating them as epoch timestamps.
- Replaced unchecked timeout multiplication with `saturating_mul(1000)`.
- Made `Duration::as_millis()` narrowing explicit with `try_into().unwrap_or(u64::MAX)`.
- Stored and aborted the notification task `JoinHandle` after Rocket shutdown.

### Security issues

- Redacted Redis credentials from `print_env()` and the `Debug` implementation for `Env`.
- Removed the full `Env` debug dump that exposed secrets.
- Removed the hardcoded Rocket release secret from `Rocket.toml`.
- Removed `.env_template` and `serviceAccountKey.json_template` from Docker runtime image layers.

### Idiomatic Rust issues

- Preserved source errors with `#[source]` and variant constructor mapping.
- Changed fallible constructors and serializers to return `Option` or `Result` instead of panicking.
- Removed unused MQTT messaging code copied from `online-receiver`.
- Removed unused helpers, derives, imports, and dead error variants.
- Changed `filter_offline` and `filter_online` to accept slices instead of consuming vectors.
- Used zero-allocation string-slice pairs for online cache lookup.
- Extracted repeated cache-key formatting into `Online::cache_key()`.
- Moved `Notification` fields by destructuring instead of cloning them.
- Stored `created_at` and `modified_at` as `u64` instead of `String`.
- Renamed shadowed or non-snake-case variables.
- Applied Clippy simplifications for nested `if let`, formatting, unit structs, iterator syntax, and `write!`.
- Removed redundant `Sized` bounds and avoidable `String` allocation.

### Chores

- Removed the unused `hide_rocket_log_stdout` configuration field from `Env`, `Debug`, `print_env`, and `.env_template`.

### Tests

- Removed direct `std::env` reads from Redis helpers and passed `is_testing` explicitly.
- Write new unit and integration tests increasing code coverage
- Removed a duplicate `#[test]` attribute above `#[test_log::test]`.
