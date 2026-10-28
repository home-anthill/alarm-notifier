use std::time::{Duration, SystemTime, UNIX_EPOCH};

use dashmap::DashMap;
use redis::aio::ConnectionManager;
use rocket::{self, catchers, routes};
use tracing::{debug, error, info, warn};

use alarm_notifier::catchers as app_catchers;
use alarm_notifier::config::{AppEnv, Env, init};
use alarm_notifier::db::alarm::{acknowledge_alarm_events, apply_notification_preferences, find_pending_alarms};
use alarm_notifier::db::notification::{
    SentNotification, api_tokens_for_devices_features, get_next_notification_id, save_sent_notification,
};
use alarm_notifier::db::online::{filter_offline, filter_online, find_all};
use alarm_notifier::fcm::FcmClient;
use alarm_notifier::models::notification::NotificationDevice;
use alarm_notifier::notifications::{
    alarm_notification_body, build_alarm_batches, build_offline_by_fcm_token_map, offline_notification_body,
};
use alarm_notifier::routes as app_routes;

#[rocket::main]
#[allow(clippy::result_large_err)]
async fn main() -> Result<(), rocket::Error> {
    // 1. Init logger and env
    let (env, app_env): (Env, AppEnv) = init();
    let cache_timeout_seconds = env.cache_timeout_seconds;
    let offline_timeout_seconds = env.offline_timeout_seconds;
    let is_testing = app_env.is_testing();

    // 2. Init and connect to Redis
    // If credentials are configured, inject them into the URI:
    //   redis://host:port -> redis://username:password@host:port
    if !env.redis_username.is_empty() && env.redis_password.is_empty() {
        warn!(target: "app", "REDIS_USERNAME is set but REDIS_PASSWORD is empty — no authentication will be attempted");
    }
    let redis_url = redis_url_with_credentials(&env.online_redis_uri, &env.redis_username, &env.redis_password);
    let redis_client = redis::Client::open(redis_url).expect("invalid Redis URI");
    let con: ConnectionManager = redis_client.get_connection_manager().await.expect("failed to connect to Redis");
    let notifications_redis_uri =
        env.notifications_redis_uri.clone().unwrap_or_else(|| redis_uri_for_database(&env.online_redis_uri, 1));
    let notifications_redis_url =
        redis_url_with_credentials(&notifications_redis_uri, &env.redis_username, &env.redis_password);
    let notifications_redis_client =
        redis::Client::open(notifications_redis_url).expect("invalid notifications Redis URI");
    let notifications_con: ConnectionManager =
        notifications_redis_client.get_connection_manager().await.expect("failed to connect to notifications Redis");
    let alarms_redis_uri =
        env.alarms_redis_uri.clone().unwrap_or_else(|| redis_uri_for_database(&env.online_redis_uri, 3));
    let alarms_redis_url = redis_url_with_credentials(&alarms_redis_uri, &env.redis_username, &env.redis_password);
    let alarms_redis_client = redis::Client::open(alarms_redis_url).expect("invalid alarms Redis URI");
    let alarms_con: ConnectionManager =
        alarms_redis_client.get_connection_manager().await.expect("failed to connect to alarms Redis");

    // 3. Init Firebase client
    // To download the service account file, follow this procedure:
    // a. Go to https://console.firebase.google.com/
    // b. Open your project
    // c. Click on the settings button and choose the Project settings option
    // d. Click on the Service Account tab. You should be in a page like this:
    //    https://console.firebase.google.com/project/<YOUR_PROJECT_ID>/settings/serviceaccounts/adminsdk
    // e. Click on the "Generate new private key" button to download the service account .json file
    // f. Place `serviceAccountKey.json` at the root of this project
    let fcm_client = FcmClient::new(&env.fcm_service_account_key_path).await.expect("failed to initialize FCM client");

    // 4. Init cache
    // It's used to store UUIDs as keys and insertion date as value to prevent too many notifications
    let cache: DashMap<String, u64> = DashMap::new();

    // 5. spawn a tokio task to loop infinitely to
    // read/write db and cache to send notifications
    let notification_loop_task = tokio::task::spawn(async move {
        // infinite loop
        loop {
            // read all online device features hash tables from Redis with key format 'online_<deviceUuid>_feature_<featureUuid>'
            // TODO this is bad, because I have to improve logic to clean old devices from redis and so on
            let mut all_devices_features = match find_all(&con, is_testing).await {
                Ok(device_feature) => device_feature,
                Err(err) => {
                    error!(target: "app", "cannot find all elements in db, err = {:?}", err);
                    tokio::time::sleep(Duration::from_secs(10)).await;
                    continue;
                }
            };
            if let Err(err) = apply_notification_preferences(&alarms_con, &mut all_devices_features, is_testing).await {
                error!(target: "app", "cannot read alarm notification preferences, err = {:?}", err);
                tokio::time::sleep(Duration::from_secs(10)).await;
                continue;
            }
            // offline_devices_features are all devices features that are offline and NOT silenced
            let offline_devices_features = filter_offline(&all_devices_features, offline_timeout_seconds);
            // online_devices_features are all devices features that are not in the offline list above
            let online_devices_features = filter_online(&all_devices_features, &offline_devices_features);

            // clean from cache all devices features that become online or silenced
            for online_device_feature in online_devices_features {
                let key = online_device_feature.cache_key();
                if cache.remove(&key).is_some() {
                    debug!(target: "info", "[CACHE] cleaned online device feature key={} from cache", &key);
                }
            }

            // print all offline devices features that are offline and NOT silenced
            for offline_device_feature in &offline_devices_features {
                let key = offline_device_feature.cache_key();
                debug!(target: "app", "[CACHE] offline device feature key={} (created_at={}, modified_at={})",
                    &key,
                    &offline_device_feature.created_at,
                    &offline_device_feature.modified_at
                );
            }

            let curr_date: u64 = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock is before UNIX epoch")
                .as_millis()
                .try_into()
                .unwrap_or(u64::MAX);
            let offline_map =
                build_offline_by_fcm_token_map(&cache, offline_devices_features, curr_date, cache_timeout_seconds);

            for (key, value) in &offline_map {
                let device_count = value.len();
                let title = "home anthill".to_string();
                let body = offline_notification_body(device_count);
                warn!(
                    target: "app",
                    "sending grouped message to FCM for {} offline device(s)",
                    device_count
                );
                match fcm_client.send(key, &title, &body).await {
                    Ok(message_id) => {
                        debug!(target: "app", "FCM response = {:?}", &message_id);
                        let notification_id = get_next_notification_id(curr_date);
                        let notification_devices = value.iter().map(NotificationDevice::from).collect::<Vec<_>>();
                        let api_tokens = api_tokens_for_devices_features(&notification_devices);

                        // save notifications sent via FCM to Redis to create a history
                        let sent_notification = SentNotification {
                            id: &notification_id,
                            api_tokens: &api_tokens,
                            sent_at: curr_date,
                            title: &title,
                            body: &body,
                            devices: &notification_devices,
                            provider_message_id: Some(message_id.as_str()),
                        };

                        if let Err(err) = save_sent_notification(&notifications_con, &sent_notification).await {
                            error!(target: "app", "cannot save sent notification to Redis, err = {:?}", err);
                        }
                    }
                    Err(err) => {
                        error!(target: "app", "cannot send message to FCM, err = {:?}", err);
                    }
                }

                // renew cache with a new date (insert overwrites existing entry)
                for offline in value {
                    let key = offline.cache_key();
                    warn!(target: "app", "[CACHE] re-adding offline device key={} to cache", &key);
                    cache.insert(key, curr_date);
                }
            }

            let pending_alarms = match find_pending_alarms(&alarms_con, &con, is_testing).await {
                Ok(events) => events,
                Err(err) => {
                    error!(target: "app", "cannot read pending alarms, err = {:?}", err);
                    tokio::time::sleep(Duration::from_secs(10)).await;
                    continue;
                }
            };
            for ((fcm_token, alarm_type), events) in build_alarm_batches(pending_alarms) {
                let event_count = events.len();
                let title = "home anthill".to_string();
                let body = if matches!(alarm_type.as_str(), "temperature-status") && event_count == 1 {
                    // in case of a single temperature error event, use the specific body
                    // to show a detailed message about the temperature error itself
                    alarm_notifier::notifications::temperature_error_body(&events[0].payload)
                } else {
                    // otherwise, use the generic body to show a summary of the alarm event
                    alarm_notification_body(&alarm_type, event_count)
                };
                warn!(
                    target: "app",
                    "sending grouped message to FCM for {} alarm event(s) of type {}",
                    event_count,
                    alarm_type
                );
                match fcm_client.send(&fcm_token, &title, &body).await {
                    Ok(message_id) => {
                        debug!(target: "app", "FCM alarm response = {:?}", &message_id);
                        let notification_id = get_next_notification_id(curr_date);
                        let notification_devices = events.iter().map(NotificationDevice::from).collect::<Vec<_>>();
                        let api_tokens = api_tokens_for_devices_features(&notification_devices);
                        let sent_notification = SentNotification {
                            id: &notification_id,
                            api_tokens: &api_tokens,
                            sent_at: curr_date,
                            title: &title,
                            body: &body,
                            devices: &notification_devices,
                            provider_message_id: Some(message_id.as_str()),
                        };
                        if let Err(err) = save_sent_notification(&notifications_con, &sent_notification).await {
                            error!(target: "app", "cannot save sent alarm notification to Redis, err = {:?}", err);
                        }
                        if let Err(err) = acknowledge_alarm_events(&alarms_con, &events, is_testing).await {
                            error!(target: "app", "cannot acknowledge sent alarm events, err = {:?}", err);
                        }
                    }
                    Err(err) => {
                        error!(target: "app", "cannot send alarm message to FCM, err = {:?}", err);
                    }
                }
            }
            tokio::time::sleep(Duration::from_secs(10)).await;
        }
    });

    // 6. Launch Rocket for the health endpoint
    info!(target: "app", "Starting Rocket...");
    let _rocket = rocket::build()
        .mount("/", routes![app_routes::api::keep_alive])
        .register(
            "/",
            catchers![
                app_catchers::bad_request,
                app_catchers::not_found,
                app_catchers::internal_server_error,
                app_catchers::service_unavailable,
            ],
        )
        .launch()
        .await?;

    notification_loop_task.abort();
    Ok(())
}

fn redis_url_with_credentials(redis_uri: &str, redis_username: &str, redis_password: &str) -> String {
    if redis_password.is_empty() {
        return redis_uri.to_string();
    }

    match redis_uri.find("://") {
        Some(scheme_end) => format!(
            "{scheme}{username}:{password}@{rest}",
            scheme = &redis_uri[..scheme_end + 3],
            username = urlencoding::encode(redis_username),
            password = urlencoding::encode(redis_password),
            rest = &redis_uri[scheme_end + 3..],
        ),
        None => {
            warn!(target: "app", "Redis URI has no recognizable scheme (missing '://'), skipping credential injection");
            redis_uri.to_string()
        }
    }
}

fn redis_uri_for_database(redis_uri: &str, database: u8) -> String {
    let Some(scheme_end) = redis_uri.find("://") else {
        return redis_uri.to_string();
    };
    let authority_start = scheme_end + 3;
    let query_start = redis_uri[authority_start..].find('?').map(|index| authority_start + index);
    let path_start = redis_uri[authority_start..].find('/').map(|index| authority_start + index);
    let end_before_query = query_start.unwrap_or(redis_uri.len());
    let authority_end = match path_start {
        Some(index) if index < end_before_query => index,
        _ => end_before_query,
    };
    let query = query_start.map(|index| &redis_uri[index..]).unwrap_or("");
    format!("{}{}/{}{}", &redis_uri[..authority_start], &redis_uri[authority_start..authority_end], database, query)
}

#[cfg(test)]
mod redis_uri_tests {
    use super::redis_uri_for_database;

    #[test]
    fn selects_dedicated_redis_database() {
        assert_eq!(redis_uri_for_database("redis://localhost:6379/0", 3), "redis://localhost:6379/3");
        assert_eq!(
            redis_uri_for_database("redis://redis.example:6379?protocol=3", 1),
            "redis://redis.example:6379/1?protocol=3"
        );
    }
}

// testing
#[cfg(test)]
mod tests_integration;
