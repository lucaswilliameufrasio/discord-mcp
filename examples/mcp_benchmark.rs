//! Isolated, closed-loop benchmark with a synthetic loopback Discord endpoint.
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use axum::{
    Json, Router,
    extract::{Query, State},
    http::{HeaderMap, StatusCode, header},
    routing::get,
};
use discord_mcp::{
    channels::ChannelStore,
    discord_login::DiscordLoginClient,
    http_auth::{AuditState, AuthState, MetricsState},
    http_mcp,
    oauth_web::OAuthWebConfig,
};
use rmcp::serde_json::{Value, json};

#[derive(Clone, Copy)]
enum Scenario {
    Read,
    Send,
    Denied,
    ReadSuccess,
    SendSuccess,
}

impl Scenario {
    fn name(self) -> &'static str {
        match self {
            Self::Read => "read_invalid_limit",
            Self::Send => "send_empty",
            Self::Denied => "read_denied",
            Self::ReadSuccess => "read_success_fake",
            Self::SendSuccess => "send_success_fake",
        }
    }
    fn request(self) -> Value {
        let (name, arguments) = match self {
            Self::Read => ("read_messages", json!({"channel":"allowed","limit":0})),
            Self::Send => ("send_message", json!({"channel":"allowed","content":" "})),
            Self::Denied => ("read_messages", json!({"channel":"denied","limit":0})),
            Self::ReadSuccess => ("read_messages", json!({"channel":"allowed","limit":1})),
            Self::SendSuccess => (
                "send_message",
                json!({"channel":"allowed","content":"synthetic benchmark"}),
            ),
        };
        json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":arguments}})
    }
    fn expected(self) -> &'static str {
        match self {
            Self::Read => "limit must be",
            Self::Send => "message must contain",
            Self::Denied => "access denied",
            Self::ReadSuccess => "synthetic response",
            Self::SendSuccess => "Message sent.",
        }
    }

    fn success(self) -> bool {
        matches!(self, Self::ReadSuccess | Self::SendSuccess)
    }
}

#[derive(Default)]
struct FixtureCounters {
    reads: AtomicUsize,
    sends: AtomicUsize,
}

async fn fake_read(
    State(counters): State<Arc<FixtureCounters>>,
    headers: HeaderMap,
    Query(query): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<Value>, StatusCode> {
    if headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        != Some("Bot synthetic-token")
        || query.get("limit").map(String::as_str) != Some("1")
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    counters.reads.fetch_add(1, Ordering::Relaxed);
    Ok(Json(
        json!([{"id":"1","content":"synthetic response","author":{"id":"2","username":"fixture"},"timestamp":"2026-01-01T00:00:00Z"}]),
    ))
}

async fn fake_send(
    State(counters): State<Arc<FixtureCounters>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> StatusCode {
    if headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        != Some("Bot synthetic-token")
        || body["content"] != "synthetic benchmark"
        || body["allowed_mentions"]["parse"] != json!([])
    {
        return StatusCode::BAD_REQUEST;
    }
    counters.sends.fetch_add(1, Ordering::Relaxed);
    StatusCode::NO_CONTENT
}

async fn one(
    client: &reqwest::Client,
    resource: &str,
    token: &str,
    scenario: Scenario,
) -> Result<f64, String> {
    let start = Instant::now();
    let response = client
        .post(resource)
        .bearer_auth(token)
        .header("MCP-Protocol-Version", "2025-11-25")
        .header(
            reqwest::header::ACCEPT,
            "application/json, text/event-stream",
        )
        .json(&scenario.request())
        .send()
        .await
        .map_err(|_| "transport")?;
    if !response.status().is_success() {
        return Err("http status".into());
    }
    let value: Value = response.json().await.map_err(|_| "response json")?;
    let text = value["result"]["content"][0]["text"].as_str().unwrap_or("");
    if value["result"]["isError"].as_bool().unwrap_or(false) == scenario.success()
        || !text.contains(scenario.expected())
    {
        return Err("unexpected result".into());
    }
    if matches!(scenario, Scenario::ReadSuccess) {
        let messages: Value = rmcp::serde_json::from_str(text).map_err(|_| "invalid messages")?;
        if messages.as_array().map(Vec::len) != Some(1)
            || messages[0]["id"] != "1"
            || messages[0]["content"] != "synthetic response"
        {
            return Err("unexpected messages".into());
        }
    }
    Ok(start.elapsed().as_secs_f64() * 1000.0)
}

fn percentile(sorted: &[f64], percent: usize) -> f64 {
    sorted[(sorted.len() * percent).div_ceil(100).saturating_sub(1)]
}

#[cfg(test)]
mod tests {
    use super::percentile;

    #[test]
    fn nearest_rank_percentiles_use_sorted_samples() {
        let values: Vec<f64> = (1..=100).map(f64::from).collect();
        assert_eq!(percentile(&values, 50), 50.0);
        assert_eq!(percentile(&values, 95), 95.0);
        assert_eq!(percentile(&values, 99), 99.0);
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("benchmark.db");
    let key = [42; 32];
    let store = ChannelStore::open(&path, &key)?;
    store.put_bot("allowed", "123", "synthetic-token")?;
    store.put_bot("denied", "456", "synthetic-token")?;
    store.set_permissions("1", "allowed", true, true)?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let fake_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let fake_address = fake_listener.local_addr()?;
    let counters = Arc::new(FixtureCounters::default());
    let fake_router = Router::new()
        .route(
            "/api/v10/channels/123/messages",
            get(fake_read).post(fake_send),
        )
        .with_state(counters.clone());
    let fake = tokio::spawn(async move { axum::serve(fake_listener, fake_router).await.unwrap() });
    let origin = format!("http://{}", listener.local_addr()?);
    let resource = format!("{origin}/mcp");
    let now = i64::try_from(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())?;
    let token = store.issue_access_token("1", &resource, now, 3600)?;
    drop(store);
    let config = OAuthWebConfig {
        auth: AuthState {
            database_path: path,
            encryption_key: key,
            resource: resource.clone(),
            metrics: MetricsState::default(),
            audit: AuditState::default(),
        },
        public_origin: origin.clone(),
        client_id: "benchmark-client".into(),
        client_name: "Benchmark".into(),
        redirect_uri: "http://127.0.0.1:8765/callback".into(),
        discord: DiscordLoginClient::new(
            "123",
            "synthetic-secret",
            &format!("{origin}/oauth/discord/callback"),
        )?,
    };
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            http_mcp::router_with_loopback_fixture(config, fake_address).unwrap(),
        )
        .await
        .unwrap()
    });
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()?;
    let mut results = Vec::new();
    for scenario in [
        Scenario::Read,
        Scenario::Send,
        Scenario::Denied,
        Scenario::ReadSuccess,
        Scenario::SendSuccess,
    ] {
        for concurrency in [1usize, 4] {
            for run in 1..=3 {
                for _ in 0..10 {
                    one(&client, &resource, &token, scenario)
                        .await
                        .map_err(|e| format!("warmup {e}"))?;
                }
                let start = Instant::now();
                let mut tasks = Vec::new();
                let resource = Arc::new(resource.clone());
                let token = Arc::new(token.clone());
                for _ in 0..concurrency {
                    let client = client.clone();
                    let resource = resource.clone();
                    let token = token.clone();
                    tasks.push(tokio::spawn(async move {
                        let mut samples = Vec::new();
                        let mut errors = 0;
                        for _ in 0..100 {
                            match one(&client, &resource, &token, scenario).await {
                                Ok(latency) => samples.push(latency),
                                Err(_) => errors += 1,
                            }
                        }
                        (samples, errors)
                    }));
                }
                let mut samples = Vec::new();
                let mut errors = 0;
                for task in tasks {
                    let (values, failed) = task.await?;
                    samples.extend(values);
                    errors += failed;
                }
                let seconds = start.elapsed().as_secs_f64();
                samples.sort_by(f64::total_cmp);
                if samples.is_empty() {
                    return Err("no successful measurements".into());
                }
                results.push(json!({"scenario":scenario.name(),"concurrency":concurrency,"run":run,"attempts":concurrency*100,"completed":samples.len(),"errors":errors,"seconds":seconds,"completed_per_second":samples.len() as f64 / seconds,"p50_ms":percentile(&samples,50),"p95_ms":percentile(&samples,95),"p99_ms":percentile(&samples,99)}));
            }
        }
    }
    let reads = counters.reads.load(Ordering::Relaxed);
    let sends = counters.sends.load(Ordering::Relaxed);
    // Per successful scenario: 3 runs × (100 + 400 measured + 20 warmup).
    if reads != 1560 || sends != 1560 {
        return Err("fixture request count mismatch".into());
    }
    server.abort();
    fake.abort();
    println!(
        "{}",
        rmcp::serde_json::to_string_pretty(
            &json!({"schema":3,"model":"closed-loop","environment":{"os":std::env::consts::OS,"architecture":std::env::consts::ARCH,"logical_cpus_available":std::thread::available_parallelism().ok().map(|n| n.get()),"package_version":env!("CARGO_PKG_VERSION"),"debug_assertions":cfg!(debug_assertions)},"topology":"single process, loopback, synthetic Discord, temporary SQLite","warmup_requests_per_run":10,"requests_per_worker":100,"fixture_reads":reads,"fixture_sends":sends,"scenarios":results})
        )?
    );
    Ok(())
}
