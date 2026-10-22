use std::collections::BTreeMap;

use dashmap::DashMap;
use tracing::{debug, warn};

use crate::models::alarm::AlarmEvent;
use crate::models::online::Online;

#[derive(Debug, Clone)]
pub struct OfflineNotificationBatch {
    pub fcm_token: String,
    pub devices: Vec<Online>,
}

pub fn build_offline_by_fcm_token_map(
    cache: &DashMap<String, u64>,
    offline_devices_features: Vec<Online>,
    curr_date: u64,
    cache_timeout_seconds: u64,
) -> BTreeMap<String, Vec<Online>> {
    let cache_expiry = curr_date.saturating_sub(cache_timeout_seconds.saturating_mul(1000));
    let mut batches_by_fcm_token: BTreeMap<String, Vec<Online>> = BTreeMap::new();

    // iterate over offline devices features (NOT silenced)
    // and add them to the cache if they are not already cached
    for offline_device_feature in offline_devices_features {
        let key = offline_device_feature.cache_key();
        match cache.get(&key) {
            None => {
                warn!(target: "app", "[CACHE] adding offline device feature key={} to cache", &key);
                cache.insert(key, curr_date);
            }
            Some(entry) => {
                let cached_date = *entry.value();
                drop(entry);

                debug!(target: "app", "[CACHE] offline device feature key={} is already in cache", &key);
                // if the device feature in cache is expired,
                // we need to add it to the batches_by_fcm_token to send the notification
                if cached_date < cache_expiry {
                    batches_by_fcm_token
                        .entry(offline_device_feature.fcm_token.clone())
                        .or_default()
                        .push(offline_device_feature);
                }
            }
        }
    }

    batches_by_fcm_token // .into_iter().map(|(fcm_token, devices)| OfflineNotificationBatch { fcm_token, devices }).collect()
}

pub fn offline_notification_body(device_count: usize) -> String {
    if device_count == 1 {
        "Device feature is offline".to_string()
    } else {
        format!("{device_count} devices features are offline")
    }
}

pub fn build_alarm_batches(events: Vec<AlarmEvent>) -> BTreeMap<(String, String), Vec<AlarmEvent>> {
    let mut batches = BTreeMap::new();
    for event in events {
        batches.entry((event.fcm_token.clone(), event.alarm_type.clone())).or_insert_with(Vec::new).push(event);
    }
    batches
}

pub fn alarm_notification_body(alarm_type: &str, event_count: usize) -> String {
    match (alarm_type, event_count) {
        ("level", 1) => "Level threshold reached".to_string(),
        ("level", count) => format!("{count} level threshold events detected"),
        ("motion", 1) => "Motion detected".to_string(),
        ("motion", count) => format!("{count} motion events detected"),
        ("thermostat-mode-error", 1) => "Thermostat mode error".to_string(),
        ("thermostat-mode-error", count) => format!("{count} thermostat mode errors"),
        (alarm_type, 1) => format!("Alarm: {}", alarm_type.replace('-', " ")),
        (alarm_type, count) => format!("{count} alarms: {}", alarm_type.replace('-', " ")),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::time::{SystemTime, UNIX_EPOCH};

    use dashmap::DashMap;
    use pretty_assertions::assert_eq;

    use super::{
        alarm_notification_body, build_alarm_batches, build_offline_by_fcm_token_map, offline_notification_body,
    };
    use crate::db::online::filter_offline;
    use crate::models::alarm::AlarmEvent;
    use crate::models::online::Online;

    const IMPORTANT_API_TOKEN: &str = "api-token-family-rossi";
    const IMPORTANT_FCM_TOKEN: &str = "fcm-token-family-rossi-phone";

    fn online(device_uuid: &str, feature_uuid: &str, fcm_token: &str) -> Online {
        Online {
            api_token: "api-token".to_string(),
            device_uuid: device_uuid.to_string(),
            feature_uuid: feature_uuid.to_string(),
            fcm_token: fcm_token.to_string(),
            notification_silenced: false,
            created_at: 1,
            modified_at: 2,
        }
    }

    fn online_for_user(
        api_token: &str,
        device_uuid: &str,
        feature_uuid: &str,
        fcm_token: &str,
        created_at: u64,
        modified_at: u64,
    ) -> Online {
        Online {
            api_token: api_token.to_string(),
            device_uuid: device_uuid.to_string(),
            feature_uuid: feature_uuid.to_string(),
            fcm_token: fcm_token.to_string(),
            notification_silenced: false,
            created_at,
            modified_at,
        }
    }

    fn now_millis() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is before UNIX epoch")
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX)
    }

    fn power_outage_redis_snapshot(now: u64) -> Vec<Online> {
        let important_devices = [
            ("home-hub", "power"),
            ("garage-door", "power"),
            ("kitchen-fridge", "power"),
            ("living-room-tv", "power"),
            ("bedroom-light", "power"),
            ("heat-pump", "power"),
            ("router-ups", "power"),
            ("water-heater", "power"),
            ("solar-inverter", "power"),
            ("basement-sensor", "power"),
        ];
        let modified_offsets = [72_140, 72_277, 72_411, 72_689, 73_002, 73_119, 73_431, 73_690, 74_003, 74_287];

        let mut devices = important_devices
            .iter()
            .zip(modified_offsets)
            .enumerate()
            .map(|(index, ((device_uuid, feature_uuid), modified_offset))| {
                let modified_at = now.saturating_sub(modified_offset);
                let created_at =
                    if index % 3 == 0 { modified_at } else { now.saturating_sub(3_600_000 + (index as u64 * 41_000)) };
                online_for_user(
                    IMPORTANT_API_TOKEN,
                    device_uuid,
                    feature_uuid,
                    IMPORTANT_FCM_TOKEN,
                    created_at,
                    modified_at,
                )
            })
            .collect::<Vec<_>>();

        devices.extend([
            online_for_user(
                "api-token-neighbor",
                "neighbor-gateway",
                "power",
                "fcm-token-neighbor-phone",
                now.saturating_sub(7_200_000),
                now.saturating_sub(72_990),
            ),
            online_for_user(
                "api-token-neighbor",
                "neighbor-freezer",
                "power",
                "fcm-token-neighbor-phone",
                now.saturating_sub(7_100_000),
                now.saturating_sub(73_250),
            ),
            online_for_user(
                "api-token-shop",
                "shop-router",
                "power",
                "fcm-token-shop-phone",
                now.saturating_sub(86_400_000),
                now.saturating_sub(75_500),
            ),
            online_for_user(
                "api-token-online-user",
                "office-router",
                "power",
                "fcm-token-online-user-phone",
                now.saturating_sub(86_400_000),
                now.saturating_sub(8_000),
            ),
        ]);

        devices
    }

    #[test]
    fn collect_due_offline_notifications_groups_devices_by_fcm_token() {
        let curr_date = 10_000;
        let cache_timeout_seconds = 5;
        let cache = DashMap::new();
        cache.insert("device-a-feature-a".to_string(), 1);
        cache.insert("device-b-feature-b".to_string(), 1);
        cache.insert("device-c-feature-c".to_string(), 1);

        let batches = build_offline_by_fcm_token_map(
            &cache,
            vec![
                online("device-a", "feature-a", "token-1"),
                online("device-b", "feature-b", "token-1"),
                online("device-c", "feature-c", "token-2"),
            ],
            curr_date,
            cache_timeout_seconds,
        );

        assert_eq!(2, batches.len());
        assert_eq!(
            vec!["device-a-feature-a", "device-b-feature-b"],
            batches["token-1"].iter().map(Online::cache_key).collect::<Vec<_>>()
        );
        assert_eq!(vec!["device-c-feature-c"], batches["token-2"].iter().map(Online::cache_key).collect::<Vec<_>>());
    }

    #[test]
    fn collect_due_offline_notifications_caches_new_offline_devices_without_notifying() {
        let cache = DashMap::new();

        let batches =
            build_offline_by_fcm_token_map(&cache, vec![online("device-a", "feature-a", "token-1")], 10_000, 5);

        assert!(batches.is_empty());
        assert_eq!(Some(10_000), cache.get("device-a-feature-a").map(|entry| *entry.value()));
    }

    #[test]
    fn collect_due_offline_notifications_skips_recently_cached_devices() {
        let cache = DashMap::new();
        cache.insert("device-a-feature-a".to_string(), 8_000);

        let batches =
            build_offline_by_fcm_token_map(&cache, vec![online("device-a", "feature-a", "token-1")], 10_000, 5);

        assert!(batches.is_empty());
    }

    #[test]
    fn collect_due_offline_notifications_skips_silenced_devices() {
        let cache = DashMap::new();
        cache.insert("device-a-feature-a".to_string(), 1);
        let mut device = online("device-a", "feature-a", "token-1");
        device.notification_silenced = true;

        let offline_devices = filter_offline(&[device], 60);
        let batches = build_offline_by_fcm_token_map(&cache, offline_devices, 10_000, 5);

        assert!(batches.is_empty());
        assert!(cache.contains_key("device-a-feature-a"));
    }

    #[test]
    fn power_outage_first_detection_caches_all_offline_devices_without_sending_notifications() {
        let now = now_millis();
        let devices = power_outage_redis_snapshot(now);
        let offline_devices = filter_offline(&devices, 60);
        let cache = DashMap::new();

        let batches = build_offline_by_fcm_token_map(&cache, offline_devices.clone(), now, 300);

        assert!(batches.is_empty());
        assert_eq!(13, cache.len());
        assert!(offline_devices.iter().all(|device| cache.contains_key(&device.cache_key())));
        assert!(!cache.contains_key("office-router-power"));
    }

    #[test]
    fn power_outage_groups_ten_due_offline_devices_for_same_user_into_one_notification() {
        let now = now_millis();
        let devices = power_outage_redis_snapshot(now);
        let offline_devices = filter_offline(&devices, 60);
        let cache = DashMap::new();

        for device in &offline_devices {
            cache.insert(device.cache_key(), now.saturating_sub(900_000));
        }

        let batches = build_offline_by_fcm_token_map(&cache, offline_devices.clone(), now, 300);
        let important_devices = batches.get(IMPORTANT_FCM_TOKEN).expect("important user batch should exist");
        let important_modified_dates =
            important_devices.iter().map(|device| device.modified_at).collect::<HashSet<_>>();
        let important_device_keys = important_devices.iter().map(Online::cache_key).collect::<Vec<_>>();

        assert_eq!(3, batches.len());
        assert_eq!(10, important_devices.len());
        assert_eq!(10, important_modified_dates.len());
        assert!(important_devices.iter().all(|device| device.api_token == IMPORTANT_API_TOKEN));
        assert!(important_devices.iter().any(|device| device.created_at == device.modified_at));
        assert!(important_devices.iter().any(|device| device.created_at != device.modified_at));
        assert_eq!(
            vec![
                "home-hub-power",
                "garage-door-power",
                "kitchen-fridge-power",
                "living-room-tv-power",
                "bedroom-light-power",
                "heat-pump-power",
                "router-ups-power",
                "water-heater-power",
                "solar-inverter-power",
                "basement-sensor-power",
            ],
            important_device_keys
        );
        assert_eq!("10 devices features are offline", offline_notification_body(important_devices.len()));
    }

    #[test]
    fn offline_notification_body_reports_single_or_multiple_devices() {
        assert_eq!("Device feature is offline", offline_notification_body(1));
        assert_eq!("2 devices features are offline", offline_notification_body(2));
    }

    #[test]
    fn alarm_batches_group_by_recipient_and_type() {
        let event = |id: &str, token: &str, alarm_type: &str| AlarmEvent {
            id: id.to_string(),
            api_token: "api-token".to_string(),
            device_uuid: "device".to_string(),
            feature_uuid: "feature".to_string(),
            alarm_type: alarm_type.to_string(),
            payload: "{}".to_string(),
            created_at: 1,
            received_at: 2,
            fcm_token: token.to_string(),
        };

        let batches = build_alarm_batches(vec![
            event("1", "fcm-a", "motion"),
            event("2", "fcm-a", "motion"),
            event("3", "fcm-a", "thermostat-mode-error"),
        ]);

        assert_eq!(2, batches.len());
        assert_eq!(2, batches[&("fcm-a".to_string(), "motion".to_string())].len());
    }

    #[test]
    fn alarm_bodies_cover_known_and_generic_types() {
        assert_eq!("Level threshold reached", alarm_notification_body("level", 1));
        assert_eq!("2 level threshold events detected", alarm_notification_body("level", 2));
        assert_eq!("Motion detected", alarm_notification_body("motion", 1));
        assert_eq!("2 motion events detected", alarm_notification_body("motion", 2));
        assert_eq!("Thermostat mode error", alarm_notification_body("thermostat-mode-error", 1));
        assert_eq!("Alarm: smoke detected", alarm_notification_body("smoke-detected", 1));
    }
}
