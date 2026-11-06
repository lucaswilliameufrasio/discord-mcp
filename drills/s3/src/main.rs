//! Synthetic Floci-only qualification. Refuses every non-loopback endpoint.
use std::{
    net::{IpAddr, SocketAddr},
    path::Path,
    time::{Duration, Instant},
};

use aws_credential_types::Credentials;
use discord_mcp::{
    channels::{AuthorizationExchange, AuthorizationGrant, ChannelAction, ChannelStore},
    recovery::prepare_restored_copy,
};
use sqlite_replication_core::{RestoreTarget, restore_from_s3, snapshot, sync_to_s3};
use sqlite_replication_storage_s3::S3Store;
use tokio::io::AsyncReadExt;

fn assert_source_unchanged(store: &ChannelStore, token: &str, resource: &str) {
    assert_eq!(store.list().unwrap(), vec!["support"]);
    assert!(store.get_channel("support").is_ok());
    assert!(
        !store
            .is_permitted("101", "support", ChannelAction::Read)
            .unwrap()
    );
    assert!(
        !store
            .is_permitted("101", "support", ChannelAction::Send)
            .unwrap()
    );
    assert!(
        store
            .verify_access_token(token, resource, 1001)
            .unwrap()
            .is_none()
    );
}

fn assert_no_destination(path: &Path) {
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let mut value = path.as_os_str().to_os_string();
        value.push(suffix);
        assert!(
            !Path::new(&value).exists(),
            "unexpected published destination or sidecar"
        );
    }
}

fn s3_config(address: SocketAddr) -> aws_sdk_s3::Config {
    aws_sdk_s3::Config::builder()
        .behavior_version_latest()
        .region(aws_sdk_s3::config::Region::new("us-east-1"))
        .endpoint_url(format!("http://{address}"))
        .force_path_style(true)
        .credentials_provider(Credentials::new("test", "test", None, None, "synthetic"))
        .build()
}

async fn exercise(address: SocketAddr, directory: &Path) -> Result<(), Box<dyn std::error::Error>> {
    if address.ip() != IpAddr::V4(std::net::Ipv4Addr::LOCALHOST) || address.port() == 0 {
        return Err("only an explicit IPv4 loopback endpoint is allowed".into());
    }
    let source = directory.join("source.db");
    let repository = directory.join("repository");
    let restored = directory.join("restored.db");
    let prepared = directory.join("prepared.db");
    let protected = directory.join("protected.db");
    let key = [173; 32];
    let resource = "https://example.invalid/mcp";
    let store = ChannelStore::open(&source, &key)?;
    store.put_bot("support", "123", "synthetic-token")?;
    store.set_permissions("101", "support", true, true)?;
    let token = store.issue_access_token("101", resource, 1000, 60)?;
    let code = store.issue_authorization_code(
        &AuthorizationGrant {
            user_id: "101",
            client_id: "fixture",
            redirect_uri: "http://127.0.0.1:8765/callback",
            resource,
            code_challenge: "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
        },
        1000,
    )?;
    let client = aws_sdk_s3::Client::from_conf(s3_config(address));
    client
        .create_bucket()
        .bucket("discord-mcp-synthetic-drill")
        .send()
        .await?;
    let s3 = S3Store::new(client.clone(), "discord-mcp-synthetic-drill")?;
    snapshot(&source, &repository)?;
    let uploaded = sync_to_s3(&s3, &repository, "synthetic", "isolated-floci").await?;
    assert_eq!(uploaded.txid, 1);
    assert!(uploaded.objects_confirmed >= 1);
    // Changes after the snapshot must not be silently undone on the source.
    store.set_permissions("101", "support", false, false)?;
    store.revoke_access_token(&token)?;
    assert_source_unchanged(&store, &token, resource);

    // A second full snapshot uses a distinct repository and remote prefix.
    // Reusing TXID 1 within the original repository would conflict with its
    // immutable object keys; this is not incremental replication.
    let next_repository = directory.join("repository-next");
    let next_restored = directory.join("next-restored.db");
    let next_prepared = directory.join("next-prepared.db");
    snapshot(&source, &next_repository)?;
    let next_upload = sync_to_s3(
        &s3,
        &next_repository,
        "next-generation",
        "isolated-floci-next",
    )
    .await?;
    assert_eq!(next_upload.txid, 1);
    assert!(next_upload.objects_confirmed >= 1);
    restore_from_s3(
        &s3,
        "next-generation",
        &next_restored,
        RestoreTarget::Txid(1),
    )
    .await?;
    let next_report = prepare_restored_copy(&next_restored, &next_prepared, &key, 90)?;
    assert_eq!(
        (
            next_report.permissions,
            next_report.tokens,
            next_report.codes
        ),
        (0, 0, 1)
    );
    let next = ChannelStore::open(&next_prepared, &key)?;
    assert!(next.recovery_quarantined()?);
    next.activate_recovered_copy()?;
    assert!(!next.is_permitted("101", "support", ChannelAction::Read)?);
    assert!(next.verify_access_token(&token, resource, 1001)?.is_none());
    assert_source_unchanged(&store, &token, resource);
    println!("PASS second independent snapshot and remote sync preserve later revocations");

    // Independent prefixes contain only synthetic objects created by this run.
    let objects = client
        .list_objects_v2()
        .bucket("discord-mcp-synthetic-drill")
        .prefix("synthetic/")
        .send()
        .await?;
    let keys: Vec<String> = objects
        .contents()
        .iter()
        .filter_map(|item| item.key().map(str::to_owned))
        .collect();
    assert_eq!(keys.len(), 2);
    let manifest = keys
        .iter()
        .find(|key| key.ends_with("_meta/repository.json"))
        .ok_or("missing manifest")?;
    let ltx = keys
        .iter()
        .find(|key| key.ends_with(".ltx"))
        .ok_or("missing LTX")?;
    let manifest_bytes = client
        .get_object()
        .bucket("discord-mcp-synthetic-drill")
        .key(manifest)
        .send()
        .await?
        .body
        .collect()
        .await?
        .into_bytes();
    for prefix in ["missing", "corrupt"] {
        client
            .put_object()
            .bucket("discord-mcp-synthetic-drill")
            .key(format!("{prefix}/_meta/repository.json"))
            .body(manifest_bytes.clone().into())
            .send()
            .await?;
    }
    let relative_ltx = ltx.strip_prefix("synthetic/").ok_or("invalid LTX key")?;
    client
        .put_object()
        .bucket("discord-mcp-synthetic-drill")
        .key(format!("corrupt/{relative_ltx}"))
        .body(aws_sdk_s3::primitives::ByteStream::from_static(
            b"truncated synthetic LTX",
        ))
        .send()
        .await?;
    for prefix in ["missing", "corrupt"] {
        let destination = directory.join(format!("{prefix}.db"));
        assert!(
            restore_from_s3(&s3, prefix, &destination, RestoreTarget::Txid(1))
                .await
                .is_err()
        );
        assert_no_destination(&destination);
        assert_source_unchanged(&store, &token, resource);
        println!("PASS {prefix} object: restore rejected, destination absent, source unchanged");
    }

    // Stall a synthetic S3 request, then cancel only after bytes reach the proxy.
    // This tests cancellation during the initial remote request, not mid-object download.
    let stalled = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let stalled_address = stalled.local_addr()?;
    let (seen_tx, seen_rx) = tokio::sync::oneshot::channel();
    let proxy = tokio::spawn(async move {
        let (mut socket, _) = stalled.accept().await.expect("accept synthetic connection");
        let mut bytes = [0u8; 4096];
        let received = socket
            .read(&mut bytes)
            .await
            .expect("read synthetic S3 request");
        assert!(received > 0);
        let _ = seen_tx.send(());
        std::future::pending::<()>().await;
    });
    let stalled_store = S3Store::new(
        aws_sdk_s3::Client::from_conf(s3_config(stalled_address)),
        "discord-mcp-synthetic-drill",
    )?;
    let cancelled_destination = directory.join("cancelled.db");
    let task_destination = cancelled_destination.clone();
    let restore_task = tokio::spawn(async move {
        restore_from_s3(
            &stalled_store,
            "synthetic",
            task_destination,
            RestoreTarget::Txid(1),
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(10), seen_rx).await??;
    restore_task.abort();
    assert!(restore_task.await.is_err_and(|error| error.is_cancelled()));
    proxy.abort();
    let _ = proxy.await;
    assert_no_destination(&cancelled_destination);
    assert_source_unchanged(&store, &token, resource);
    println!(
        "PASS interrupted initial S3 request: restore cancelled, destination absent, source unchanged"
    );
    let sentinel = b"synthetic protected destination";
    std::fs::write(&protected, sentinel)?;
    assert!(
        restore_from_s3(&s3, "synthetic", &protected, RestoreTarget::Txid(1))
            .await
            .is_err()
    );
    assert_eq!(std::fs::read(&protected)?, sentinel);
    restore_from_s3(&s3, "synthetic", &restored, RestoreTarget::Txid(1)).await?;
    // The S3 restore is not directly usable: preparation must verify the
    // independent application key before publishing a quarantined copy.
    let restored_before = std::fs::read(&restored)?;
    assert!(prepare_restored_copy(&restored, &prepared, &[174; 32], 90).is_err());
    assert_no_destination(&prepared);
    assert_eq!(std::fs::read(&restored)?, restored_before);
    assert_source_unchanged(&store, &token, resource);
    println!("PASS S3 restore with wrong application key: no prepared copy, source unchanged");
    let report = prepare_restored_copy(&restored, &prepared, &key, 90)?;
    assert_eq!((report.permissions, report.tokens, report.codes), (1, 1, 1));
    let restored_store = ChannelStore::open(&prepared, &key)?;
    assert!(restored_store.recovery_quarantined()?);
    assert!(restored_store.get_channel("support").is_err());
    assert!(
        restored_store
            .verify_access_token(&token, resource, 1001)?
            .is_none()
    );
    restored_store.activate_recovered_copy()?;
    assert!(restored_store.get_channel("support").is_ok());
    assert!(!restored_store.is_permitted("101", "support", ChannelAction::Read)?);
    assert!(
        restored_store
            .verify_access_token(&token, resource, 1001)?
            .is_none()
    );
    let exchange = AuthorizationExchange {
        code: &code,
        client_id: "fixture",
        redirect_uri: "http://127.0.0.1:8765/callback",
        resource,
        code_verifier: "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk",
    };
    assert!(
        restored_store
            .exchange_authorization_code(&exchange, 1001)
            .is_err()
    );
    assert_source_unchanged(&store, &token, resource);
    assert_eq!(std::fs::read(&protected)?, sentinel);
    println!(
        "PASS snapshot txid=1; uploaded={} objects; S3 restore, quarantine, revocation and no-overwrite verified",
        uploaded.objects_confirmed
    );
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let address: SocketAddr = std::env::args()
        .nth(1)
        .ok_or("missing loopback fixture address")?
        .parse()?;
    let directory = tempfile::tempdir()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))?;
    }
    let started = Instant::now();
    tokio::time::timeout(Duration::from_secs(90), exercise(address, directory.path())).await??;
    println!(
        "Synthetic exercise elapsed: {:.3}s (excludes build and emulator startup; not an RTO)",
        started.elapsed().as_secs_f64()
    );
    Ok(())
}
