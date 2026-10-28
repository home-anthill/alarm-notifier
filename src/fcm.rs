use std::time::Duration;

use anyhow::{Context, Result};
use google_cloud_auth::credentials::{
    AccessTokenCredentials,
    service_account::{self, AccessSpecifier},
};
use serde::Deserialize;
use serde_json::json;

const FCM_SEND_ENDPOINT: &str = "https://fcm.googleapis.com/v1/projects/{project_id}/messages:send";
const FCM_OAUTH_SCOPE: &str = "https://www.googleapis.com/auth/firebase.messaging";

/// Sends notifications through the FCM HTTP v1 API.
pub struct FcmClient {
    http: reqwest::Client,
    credentials: AccessTokenCredentials,
    endpoint: String,
}

#[derive(Deserialize)]
struct SendResponse {
    name: String,
}

impl FcmClient {
    pub async fn new(service_account_key_path: &str) -> Result<Self> {
        let service_account_bytes =
            tokio::fs::read(service_account_key_path).await.context("failed to read FCM service account key")?;
        let service_account: serde_json::Value =
            serde_json::from_slice(&service_account_bytes).context("invalid FCM service account JSON")?;
        let project_id = service_account["project_id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .context("project_id missing in FCM service account key")?;
        let endpoint = FCM_SEND_ENDPOINT.replace("{project_id}", project_id);
        // The auth library caches access tokens and refreshes them when needed.
        let credentials = service_account::Builder::new(service_account)
            .with_access_specifier(AccessSpecifier::from_scopes([FCM_OAUTH_SCOPE]))
            .build_access_token_credentials()
            .context("failed to build FCM credentials")?;
        let http = reqwest::Client::builder().timeout(Duration::from_secs(30)).build()?;
        Ok(Self { http, credentials, endpoint })
    }

    /// Returns the provider message ID only after a successful send.
    pub async fn send(&self, token: &str, title: &str, body: &str) -> Result<String> {
        let access_token = self.credentials.access_token().await.context("failed to obtain FCM access token")?;
        let response = self
            .http
            .post(&self.endpoint)
            .bearer_auth(&access_token.token)
            .json(&json!({
                "message": {
                    "token": token,
                    "notification": { "title": title, "body": body },
                },
            }))
            .send()
            .await?;
        parse_send_response(response).await
    }
}

async fn parse_send_response(response: reqwest::Response) -> Result<String> {
    if let Err(error) = response.error_for_status_ref() {
        let status = response.status();
        let body =
            response.text().await.with_context(|| format!("FCM returned {status}; failed to read error body"))?;
        return Err(anyhow::Error::new(error).context(format!("FCM returned {status}: {body}")));
    }
    let message = response.json::<SendResponse>().await?;
    Ok(message.name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::StatusCode;

    fn build_response(status: StatusCode, body: impl Into<String>) -> reqwest::Response {
        http::Response::builder().status(status).body(body.into()).unwrap().into()
    }

    #[tokio::test]
    async fn preserves_fcm_error_details_and_status() {
        let body = json!({
            "error": {
                "code": 404,
                "status": "NOT_FOUND",
                "details": [{ "errorCode": "UNREGISTERED" }],
            },
        })
        .to_string();
        let response = build_response(StatusCode::NOT_FOUND, body.clone());

        let error = parse_send_response(response).await.unwrap_err();

        assert!(error.to_string().contains("404 Not Found"));
        assert!(error.to_string().contains(&body));
        assert_eq!(error.downcast_ref::<reqwest::Error>().unwrap().status(), Some(StatusCode::NOT_FOUND));
    }

    #[tokio::test]
    async fn preserves_non_json_error_body() {
        let response = build_response(StatusCode::SERVICE_UNAVAILABLE, "upstream unavailable");

        let error = parse_send_response(response).await.unwrap_err();

        assert!(error.to_string().contains("upstream unavailable"));
    }

    #[tokio::test]
    async fn returns_successful_message_id() {
        let message_id = "projects/example/messages/123";
        let body = json!({ "name": message_id }).to_string();
        let response = build_response(StatusCode::OK, body);

        let id = parse_send_response(response).await.unwrap();

        assert_eq!(id, message_id);
    }
}
