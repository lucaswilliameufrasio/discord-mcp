//! Narrow Discord bot API client; bot tokens never appear in errors or Debug.
use std::{fmt, time::Duration};

use reqwest::{Client, StatusCode, Url, redirect::Policy};
use rmcp::serde_json::json;
use serde::{Deserialize, Serialize};

#[derive(Clone)]
pub struct BotChannel {
    channel_id: String,
    token: String,
}

impl BotChannel {
    pub fn new(channel_id: &str, token: &str) -> Option<Self> {
        if channel_id.is_empty()
            || channel_id.len() > 20
            || !channel_id.bytes().all(|b| b.is_ascii_digit())
            || token.is_empty()
            || token.len() > 512
            || !token
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        {
            return None;
        }
        Some(Self {
            channel_id: channel_id.into(),
            token: token.into(),
        })
    }

    pub(crate) fn encode(&self) -> String {
        format!("bot:{}:{}", self.channel_id, self.token)
    }
    pub(crate) fn decode(value: &str) -> Option<Self> {
        let (channel, token) = value.strip_prefix("bot:")?.split_once(':')?;
        Self::new(channel, token)
    }
}

#[derive(Debug)]
pub enum BotError {
    InvalidMessage,
    InvalidLimit,
    RateLimited,
    Forbidden,
    Rejected,
    DeliveryUnknown,
    Unavailable,
    InvalidResponse,
}

impl fmt::Display for BotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidMessage => "message must contain 1–2000 characters",
            Self::InvalidLimit => "limit must be between 1 and 20",
            Self::RateLimited => "Discord rate limit reached",
            Self::Forbidden => "bot cannot access this channel",
            Self::Rejected => "Discord rejected the request",
            Self::DeliveryUnknown => "delivery outcome unknown",
            Self::Unavailable => "Discord service unavailable",
            Self::InvalidResponse => "invalid Discord response",
        })
    }
}

impl std::error::Error for BotError {}

#[derive(Deserialize, Serialize)]
pub struct Message {
    pub id: String,
    pub content: String,
    pub author: Author,
    pub timestamp: String,
}

#[derive(Deserialize, Serialize)]
pub struct Author {
    pub id: String,
    pub username: String,
}

#[derive(Clone)]
pub struct BotClient {
    client: Client,
}

impl BotClient {
    pub fn new() -> Result<Self, BotError> {
        let client = Client::builder()
            .redirect(Policy::none())
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|_| BotError::Unavailable)?;
        Ok(Self { client })
    }

    pub async fn send(&self, bot: &BotChannel, content: &str) -> Result<(), BotError> {
        self.send_at(
            bot,
            content,
            &Url::parse("https://discord.com/api/v10/").map_err(|_| BotError::Unavailable)?,
        )
        .await
    }

    async fn send_at(&self, bot: &BotChannel, content: &str, base: &Url) -> Result<(), BotError> {
        if content.trim().is_empty() || content.encode_utf16().count() > 2000 {
            return Err(BotError::InvalidMessage);
        }
        let url = base
            .join(&format!("channels/{}/messages", bot.channel_id))
            .map_err(|_| BotError::Unavailable)?;
        let response = self
            .client
            .post(url)
            .header(reqwest::header::AUTHORIZATION, format!("Bot {}", bot.token))
            .json(&json!({"content":content,"allowed_mentions":{"parse":[]}}))
            .send()
            .await
            .map_err(|_| BotError::DeliveryUnknown)?;
        classify(response.status(), true)
    }

    pub async fn read(&self, bot: &BotChannel, limit: u8) -> Result<Vec<Message>, BotError> {
        self.read_at(
            bot,
            limit,
            &Url::parse("https://discord.com/api/v10/").map_err(|_| BotError::Unavailable)?,
        )
        .await
    }

    async fn read_at(
        &self,
        bot: &BotChannel,
        limit: u8,
        base: &Url,
    ) -> Result<Vec<Message>, BotError> {
        if !(1..=20).contains(&limit) {
            return Err(BotError::InvalidLimit);
        }
        let mut url = base
            .join(&format!("channels/{}/messages", bot.channel_id))
            .map_err(|_| BotError::Unavailable)?;
        url.query_pairs_mut()
            .append_pair("limit", &limit.to_string());
        let mut response = self
            .client
            .get(url)
            .header(reqwest::header::AUTHORIZATION, format!("Bot {}", bot.token))
            .send()
            .await
            .map_err(|_| BotError::Unavailable)?;
        classify(response.status(), false)?;
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| BotError::Unavailable)? {
            if bytes.len() + chunk.len() > 256 * 1024 {
                return Err(BotError::InvalidResponse);
            }
            bytes.extend_from_slice(&chunk);
        }
        rmcp::serde_json::from_slice(&bytes).map_err(|_| BotError::InvalidResponse)
    }
}

fn classify(status: StatusCode, sending: bool) -> Result<(), BotError> {
    match status {
        s if s.is_success() => Ok(()),
        StatusCode::TOO_MANY_REQUESTS => Err(BotError::RateLimited),
        StatusCode::FORBIDDEN => Err(BotError::Forbidden),
        s if s.is_server_error() && sending => Err(BotError::DeliveryUnknown),
        s if s.is_server_error() => Err(BotError::Unavailable),
        _ => Err(BotError::Rejected),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
    };

    fn fake(response: String) -> (Url, thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut data = Vec::new();
            let mut chunk = [0; 4096];
            loop {
                let count = stream.read(&mut chunk).unwrap();
                data.extend_from_slice(&chunk[..count]);
                if let Some(end) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&data[..end]);
                    let length = headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            if name.eq_ignore_ascii_case("content-length") {
                                value.trim().parse::<usize>().ok()
                            } else {
                                None
                            }
                        })
                        .unwrap_or(0);
                    if data.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            stream.write_all(response.as_bytes()).unwrap();
            String::from_utf8(data).unwrap()
        });
        (url, handle)
    }

    #[tokio::test]
    async fn bot_sends_without_mentions_and_reads_messages_using_expected_api() {
        let bot = BotChannel::new("123", "test-token").unwrap();
        let client = BotClient::new().unwrap();
        let (base, server) = fake("HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n".into());
        client.send_at(&bot, "hello", &base).await.unwrap();
        let request = server.join().unwrap();
        assert!(request.starts_with("POST /channels/123/messages HTTP/1.1"));
        assert!(
            request
                .to_ascii_lowercase()
                .contains("authorization: bot test-token")
        );
        let body: rmcp::serde_json::Value =
            rmcp::serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(body["allowed_mentions"]["parse"], json!([]));
        let body = r#"[{"id":"5","content":"hello","author":{"id":"6","username":"test"},"timestamp":"2026-01-01T00:00:00Z"}]"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let (base, server) = fake(response);
        let messages = client.read_at(&bot, 5, &base).await.unwrap();
        assert_eq!(messages[0].content, "hello");
        let request = server.join().unwrap();
        assert!(request.starts_with("GET /channels/123/messages?limit=5 HTTP/1.1"));
    }

    #[tokio::test]
    async fn bot_permission_and_validation_errors_do_not_leak_token() {
        let bot = BotChannel::new("123", "test-token").unwrap();
        let client = BotClient::new().unwrap();
        let (base, server) = fake("HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n".into());
        let error = client.read_at(&bot, 10, &base).await.err().unwrap();
        server.join().unwrap();
        assert!(matches!(error, BotError::Forbidden));
        assert!(!error.to_string().contains("test-token"));
        assert!(matches!(
            client.read_at(&bot, 0, &base).await,
            Err(BotError::InvalidLimit)
        ));
        assert!(BotChannel::new("../123", "token").is_none());
    }
}
