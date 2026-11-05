//! Discord webhook boundary. Webhook URLs are credentials and must never be logged.

use std::{fmt, time::Duration};

use reqwest::{Client, StatusCode, Url, redirect::Policy};
use rmcp::serde_json::json;

#[derive(Clone)]
pub struct WebhookUrl(Url);

impl WebhookUrl {
    pub fn parse(value: &str) -> Result<Self, WebhookError> {
        let url = Url::parse(value).map_err(|_| WebhookError::InvalidWebhook)?;
        let segments: Vec<_> = url
            .path_segments()
            .ok_or(WebhookError::InvalidWebhook)?
            .collect();
        if url.scheme() != "https"
            || url.host_str() != Some("discord.com")
            || url.port().is_some()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || segments.len() != 4
            || segments[0] != "api"
            || segments[1] != "webhooks"
            || segments[2].is_empty()
            || !segments[2].bytes().all(|c| c.is_ascii_digit())
            || segments[3].is_empty()
            || !segments[3]
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
        {
            return Err(WebhookError::InvalidWebhook);
        }
        Ok(Self(url))
    }
}

impl fmt::Debug for WebhookUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("WebhookUrl([REDACTED])")
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum WebhookError {
    InvalidWebhook,
    InvalidMessage,
    RateLimited,
    Rejected,
    Unavailable,
    /// The remote may have received the message; do not retry automatically.
    DeliveryUnknown,
}

impl fmt::Display for WebhookError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidWebhook => "invalid Discord webhook",
            Self::InvalidMessage => "message must contain 1–2000 characters",
            Self::RateLimited => "Discord rate limit reached",
            Self::Rejected => "Discord rejected the message",
            Self::Unavailable => "Discord service unavailable",
            Self::DeliveryUnknown => "delivery outcome unknown",
        })
    }
}

impl std::error::Error for WebhookError {}

#[derive(Clone)]
pub struct WebhookClient {
    client: Client,
}

impl WebhookClient {
    pub fn new() -> Result<Self, WebhookError> {
        let client = Client::builder()
            .redirect(Policy::none())
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|_| WebhookError::Unavailable)?;
        Ok(Self { client })
    }

    pub async fn send(&self, webhook: &WebhookUrl, content: &str) -> Result<(), WebhookError> {
        self.send_to(&webhook.0, content).await
    }

    async fn send_to(&self, url: &Url, content: &str) -> Result<(), WebhookError> {
        if content.trim().is_empty() || content.encode_utf16().count() > 2000 {
            return Err(WebhookError::InvalidMessage);
        }
        let response = self
            .client
            .post(url.clone())
            .json(&json!({"content": content, "allowed_mentions": {"parse": []}}))
            .send()
            .await
            .map_err(|_| WebhookError::DeliveryUnknown)?;
        match response.status() {
            status if status.is_success() => Ok(()),
            StatusCode::TOO_MANY_REQUESTS => Err(WebhookError::RateLimited),
            status if status.is_server_error() => Err(WebhookError::DeliveryUnknown),
            _ => Err(WebhookError::Rejected),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    #[test]
    fn webhook_validation_and_debug_do_not_leak_secret() {
        let webhook = WebhookUrl::parse("https://discord.com/api/webhooks/123/secret-token")
            .expect("valid webhook");
        assert!(!format!("{webhook:?}").contains("secret-token"));
        for value in [
            "http://discord.com/api/webhooks/123/token",
            "https://discord.com.evil.test/api/webhooks/123/token",
            "https://discord.com:8443/api/webhooks/123/token",
            "https://discord.com/api/webhooks/abc/token",
            "https://discord.com/api/webhooks/123/token?x=1",
            "https://discord.com/api/webhooks/123/token/extra",
        ] {
            assert!(WebhookUrl::parse(value).is_err());
        }
    }

    #[tokio::test]
    async fn sends_message_without_mentions_to_protocol_fake() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake");
        let addr = listener.local_addr().expect("address");
        let fake = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            let mut chunk = [0; 4096];
            loop {
                let count = stream.read(&mut chunk).expect("read");
                request.extend_from_slice(&chunk[..count]);
                if let Some(header_end) = request.windows(4).position(|v| v == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&request[..header_end]);
                    let length: usize = headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            if name.eq_ignore_ascii_case("content-length") {
                                value.trim().parse().ok()
                            } else {
                                None
                            }
                        })
                        .expect("content length");
                    if request.len() >= header_end + 4 + length {
                        break;
                    }
                }
            }
            stream
                .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n")
                .unwrap();
            String::from_utf8(request).unwrap()
        });
        let client = WebhookClient::new().unwrap();
        client
            .send_to(
                &Url::parse(&format!("http://{addr}/hook")).unwrap(),
                "hello",
            )
            .await
            .unwrap();
        let request = fake.join().unwrap();
        assert!(request.starts_with("POST /hook HTTP/1.1"));
        let body = request.split_once("\r\n\r\n").unwrap().1;
        let body: rmcp::serde_json::Value = rmcp::serde_json::from_str(body).unwrap();
        assert_eq!(body["content"], "hello");
        assert_eq!(body["allowed_mentions"]["parse"], json!([]));
    }

    #[tokio::test]
    async fn rejects_empty_and_oversized_messages_before_request() {
        let client = WebhookClient::new().unwrap();
        let url = Url::parse("http://127.0.0.1:1/hook").unwrap();
        assert_eq!(
            client.send_to(&url, " ").await,
            Err(WebhookError::InvalidMessage)
        );
        assert_eq!(
            client.send_to(&url, &"x".repeat(2001)).await,
            Err(WebhookError::InvalidMessage)
        );
    }
}
