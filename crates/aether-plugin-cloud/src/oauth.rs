//! Minimal OAuth2 refresh-token support shared by the cloud-drive plugins.
//!
//! The host passes per-task credentials through `TaskSpec::settings`. This
//! module only performs the `refresh_token` grant; tokens are never logged and
//! provider error bodies are redacted before they become error messages.

use std::time::Duration;

use reqwest::blocking::Client;
use reqwest::header::CONTENT_TYPE;
use serde_json::Value;

/// Upper bound for a token request. Kept short so a stuck provider cannot
/// stall the download worker indefinitely.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(20);

/// Longest provider detail kept in an error message.
const MAX_DETAIL_CHARS: usize = 200;

/// An access token returned by a refresh grant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessToken {
    pub token: String,
    pub expires_in: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OAuthError {
    /// Credentials are incomplete or contradictory.
    InvalidSettings(String),
    /// The HTTP request could not be completed (DNS, TLS, connection).
    Request(String),
    /// The token endpoint did not answer within the configured timeout.
    Timeout,
    /// The provider answered with a non-success status.
    Provider { status: u16, detail: String },
    /// The provider answered with a body we cannot understand.
    Malformed(String),
}

impl std::fmt::Display for OAuthError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidSettings(detail) => write!(formatter, "invalid OAuth settings: {detail}"),
            Self::Request(detail) => write!(formatter, "OAuth token request failed: {detail}"),
            Self::Timeout => write!(formatter, "OAuth token request timed out"),
            Self::Provider { status, detail } => {
                write!(
                    formatter,
                    "OAuth token endpoint returned HTTP {status}: {detail}"
                )
            }
            Self::Malformed(detail) => {
                write!(formatter, "OAuth token response is unusable: {detail}")
            }
        }
    }
}

impl std::error::Error for OAuthError {}

/// Resolve an access token for `settings`.
///
/// Precedence: a ready-made `access_token` wins; otherwise a refresh grant is
/// performed when `oauth_refresh_token` is present. `Ok(None)` means no
/// credentials were supplied, so the caller should use the public path.
pub fn access_token(
    client: &Client,
    settings: &Value,
    default_token_url: &str,
    timeout: Duration,
) -> Result<Option<AccessToken>, OAuthError> {
    if let Some(token) = setting(settings, "access_token") {
        return Ok(Some(AccessToken {
            token: token.to_string(),
            expires_in: None,
        }));
    }

    let Some(refresh_token) = setting(settings, "oauth_refresh_token") else {
        return Ok(None);
    };
    let client_id = setting(settings, "oauth_client_id").ok_or_else(|| {
        OAuthError::InvalidSettings(
            "oauth_client_id is required when oauth_refresh_token is set".into(),
        )
    })?;
    let client_secret = setting(settings, "oauth_client_secret").ok_or_else(|| {
        OAuthError::InvalidSettings(
            "oauth_client_secret is required when oauth_refresh_token is set".into(),
        )
    })?;
    let token_url = setting(settings, "oauth_token_url").unwrap_or(default_token_url);

    refresh(
        client,
        token_url,
        client_id,
        client_secret,
        refresh_token,
        timeout,
    )
    .map(Some)
}

fn refresh(
    client: &Client,
    token_url: &str,
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
    timeout: Duration,
) -> Result<AccessToken, OAuthError> {
    let response = client
        .post(token_url)
        .timeout(timeout)
        .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", client_id),
            ("client_secret", client_secret),
            ("refresh_token", refresh_token),
        ])
        .send()
        .map_err(|error| request_error(error, timeout))?;

    let status = response.status();
    let body = response
        .text()
        .map_err(|error| OAuthError::Request(format!("reading token response failed: {error}")))?;

    if !status.is_success() {
        let detail = sanitize_detail(&body, &[client_secret, refresh_token, client_id]);
        return Err(OAuthError::Provider {
            status: status.as_u16(),
            detail,
        });
    }

    parse_token(&body)
}

fn parse_token(body: &str) -> Result<AccessToken, OAuthError> {
    let value: Value = serde_json::from_str(body).map_err(|_| {
        OAuthError::Malformed(format!(
            "token endpoint returned invalid JSON ({} bytes)",
            body.len()
        ))
    })?;
    let token = value
        .get("access_token")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| OAuthError::Malformed("token response has no access_token".into()))?;
    let expires_in = value.get("expires_in").and_then(|value| {
        value
            .as_u64()
            .or_else(|| value.as_str()?.trim().parse::<u64>().ok())
    });
    Ok(AccessToken {
        token: token.to_string(),
        expires_in,
    })
}

fn request_error(error: reqwest::Error, timeout: Duration) -> OAuthError {
    if error.is_timeout() {
        OAuthError::Timeout
    } else {
        // `reqwest::Error` carries the request URL but never the form body.
        OAuthError::Request(format!(
            "request failed within {}s: {error}",
            timeout.as_secs()
        ))
    }
}

/// Render a provider response body into a short, secret-free detail string.
///
/// JSON bodies contribute their `error_description` / `error_summary` /
/// `error` field; anything else is truncated. Known secrets are replaced so a
/// provider that echoes the request cannot leak them into logs or the UI.
pub fn sanitize_detail(body: &str, secrets: &[&str]) -> String {
    let detail = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| {
            ["error_description", "error_summary", "error"]
                .iter()
                .find_map(|key| value.get(key).and_then(Value::as_str))
                .map(str::to_string)
        })
        .unwrap_or_else(|| body.trim().to_string());
    truncate(&redact(&detail, secrets), MAX_DETAIL_CHARS)
}

fn redact(input: &str, secrets: &[&str]) -> String {
    let mut output = input.to_string();
    for secret in secrets {
        if !secret.is_empty() {
            output = output.replace(secret, "[redacted]");
        }
    }
    output
}

fn truncate(input: &str, max_chars: usize) -> String {
    if input.chars().count() <= max_chars {
        return input.to_string();
    }
    let mut output = input.chars().take(max_chars).collect::<String>();
    output.push_str("...");
    output
}

fn setting<'a>(settings: &'a Value, key: &str) -> Option<&'a str> {
    settings
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testserver::{Response, Server};
    use serde_json::json;
    use std::time::Instant;

    fn client(timeout: Duration) -> Client {
        Client::builder()
            .timeout(timeout)
            .build()
            .expect("test client")
    }

    fn settings(server: &Server, secret: &str, refresh: &str) -> Value {
        json!({
            "oauth_client_id": "client-id",
            "oauth_client_secret": secret,
            "oauth_refresh_token": refresh,
            "oauth_token_url": server.url("/token"),
        })
    }

    #[test]
    fn refreshes_with_expected_parameters() {
        let server =
            Server::start(|_| Response::json(r#"{"access_token":"new-token","expires_in":3599}"#));
        let settings = settings(&server, "client-secret", "refresh-token-value");

        let token = access_token(
            &client(Duration::from_secs(5)),
            &settings,
            "https://example.invalid/token",
            Duration::from_secs(5),
        )
        .unwrap()
        .expect("token");

        assert_eq!(token.token, "new-token");
        assert_eq!(token.expires_in, Some(3599));

        let requests = server.requests();
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/token");
        assert_eq!(
            request.headers.get("content-type").map(String::as_str),
            Some("application/x-www-form-urlencoded")
        );
        assert!(request.body.contains("grant_type=refresh_token"));
        assert!(request.body.contains("client_id=client-id"));
        assert!(request.body.contains("client_secret=client-secret"));
        assert!(request.body.contains("refresh_token=refresh-token-value"));
    }

    #[test]
    fn provider_error_includes_status_and_redacts_secrets() {
        let server = Server::start(|_| {
            Response::json(
                r#"{"error":"invalid_grant","error_description":"client-secret and refresh-token-value were rejected"}"#,
            )
            .with_status(400)
        });
        let settings = settings(&server, "client-secret", "refresh-token-value");

        let error = access_token(
            &client(Duration::from_secs(5)),
            &settings,
            "https://example.invalid/token",
            Duration::from_secs(5),
        )
        .unwrap_err();
        let message = error.to_string();

        assert!(message.contains("400"), "{message}");
        assert!(message.contains("were rejected"), "{message}");
        assert!(!message.contains("client-secret"), "{message}");
        assert!(!message.contains("refresh-token-value"), "{message}");
    }

    #[test]
    fn malformed_json_is_reported_without_the_body() {
        let server = Server::start(|_| Response::json("<html>not a token</html>"));
        let settings = settings(&server, "client-secret", "refresh-token-value");

        let error = access_token(
            &client(Duration::from_secs(5)),
            &settings,
            "https://example.invalid/token",
            Duration::from_secs(5),
        )
        .unwrap_err();

        assert!(matches!(error, OAuthError::Malformed(_)), "{error:?}");
        assert!(!error.to_string().contains("not a token"));
    }

    #[test]
    fn missing_access_token_field_is_malformed() {
        let server = Server::start(|_| Response::json(r#"{"expires_in":60}"#));
        let settings = settings(&server, "client-secret", "refresh-token-value");

        let error = access_token(
            &client(Duration::from_secs(5)),
            &settings,
            "https://example.invalid/token",
            Duration::from_secs(5),
        )
        .unwrap_err();

        assert!(matches!(error, OAuthError::Malformed(_)), "{error:?}");
        assert!(error.to_string().contains("access_token"));
    }

    #[test]
    fn request_timeout_is_enforced() {
        let server = Server::start(|_| {
            Response::json(r#"{"access_token":"late"}"#).after(Duration::from_millis(500))
        });
        let settings = settings(&server, "client-secret", "refresh-token-value");

        let started = Instant::now();
        let error = access_token(
            &client(Duration::from_millis(150)),
            &settings,
            "https://example.invalid/token",
            Duration::from_millis(150),
        )
        .unwrap_err();

        assert_eq!(error, OAuthError::Timeout);
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(!error.to_string().contains("client-secret"));
    }

    #[test]
    fn ready_made_access_token_wins_over_refresh() {
        let settings = json!({
            "access_token": "direct-token",
            "oauth_client_id": "client-id",
            "oauth_client_secret": "client-secret",
            "oauth_refresh_token": "refresh-token-value",
            "oauth_token_url": "http://127.0.0.1:1/token",
        });

        let token = access_token(
            &client(Duration::from_millis(150)),
            &settings,
            "https://example.invalid/token",
            Duration::from_millis(150),
        )
        .unwrap()
        .expect("token");

        assert_eq!(token.token, "direct-token");
        assert_eq!(token.expires_in, None);
    }

    #[test]
    fn no_credentials_means_public_path() {
        let settings = json!({});
        let token = access_token(
            &client(Duration::from_secs(1)),
            &settings,
            "https://example.invalid/token",
            Duration::from_secs(1),
        )
        .unwrap();
        assert_eq!(token, None);
    }

    #[test]
    fn credentials_split_across_sources_is_rejected() {
        let settings = json!({
            "oauth_refresh_token": "refresh-token-value",
            "oauth_client_secret": "client-secret",
        });
        let error = access_token(
            &client(Duration::from_secs(1)),
            &settings,
            "https://example.invalid/token",
            Duration::from_secs(1),
        )
        .unwrap_err();

        assert!(matches!(error, OAuthError::InvalidSettings(_)), "{error:?}");
        assert!(error.to_string().contains("oauth_client_id"));
    }

    #[test]
    fn expires_in_accepts_numeric_strings() {
        let server = Server::start(|_| {
            Response::json(r#"{"access_token":"string-expiry","expires_in":"900"}"#)
        });
        let settings = settings(&server, "client-secret", "refresh-token-value");

        let token = access_token(
            &client(Duration::from_secs(5)),
            &settings,
            "https://example.invalid/token",
            Duration::from_secs(5),
        )
        .unwrap()
        .expect("token");

        assert_eq!(token.expires_in, Some(900));
    }
}
