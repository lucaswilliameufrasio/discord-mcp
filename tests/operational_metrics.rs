use discord_mcp::{
    channels::{ChannelStore, OperationalEvent},
    http_auth::{AuthState, MetricsState},
};

#[test]
fn aggregation_retention_and_privacy_use_real_sqlite() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("metrics.db");
    let key = [91; 32];
    let store = ChannelStore::open(&path, &key).unwrap();
    store
        .record_operational_event(
            100_000,
            1,
            OperationalEvent::Http {
                failed: true,
                response_ms: 35,
            },
        )
        .unwrap();
    store
        .record_operational_event(100_001, 1, OperationalEvent::Send { failed: true })
        .unwrap();
    store
        .record_operational_event(100_002, 1, OperationalEvent::Read { failed: false })
        .unwrap();
    let summary = store.operational_summary(99_900, 100_100).unwrap();
    assert_eq!(summary.http_requests, 1);
    assert_eq!(summary.http_failures, 1);
    assert_eq!(summary.response_ms_sum, 35);
    assert_eq!((summary.send_completed, summary.send_failures), (1, 1));
    assert_eq!((summary.read_completed, summary.read_failures), (1, 0));
    store.prune_operational_metrics(200_000, 1).unwrap();
    assert_eq!(store.operational_summary(0, 200_000).unwrap().buckets, 0);
    assert!(
        store
            .record_operational_event(1, 0, OperationalEvent::Read { failed: true })
            .is_err()
    );
    drop(store);
    let db = rusqlite::Connection::open(&path).unwrap();
    let mut stmt = db
        .prepare("PRAGMA table_info(operational_metrics)")
        .unwrap();
    let names = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        names,
        vec![
            "minute",
            "http_requests",
            "http_failures",
            "response_ms_sum",
            "send_completed",
            "send_failures",
            "read_completed",
            "read_failures"
        ]
    );
}

#[test]
fn recording_failure_marks_collection_incomplete() {
    let dir = tempfile::tempdir().unwrap();
    let metrics = MetricsState::new(30).unwrap();
    let auth = AuthState {
        database_path: dir.path().join("missing-parent").join("db"),
        encryption_key: [1; 32],
        resource: "https://example.invalid/mcp".into(),
        metrics: metrics.clone(),
        audit: discord_mcp::http_auth::AuditState::default(),
    };
    assert!(!metrics.is_degraded());
    metrics.record(&auth, OperationalEvent::Read { failed: true });
    assert!(metrics.is_degraded());
    assert!(MetricsState::new(0).is_err());
    assert!(MetricsState::new(366).is_err());
}
