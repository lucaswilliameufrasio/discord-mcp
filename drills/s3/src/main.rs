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
    let config = aws_sdk_s3::Config::builder()
        .behavior_version_latest()
        .region(aws_sdk_s3::config::Region::new("us-east-1"))
        .endpoint_url(format!("http://{address}"))
        .force_path_style(true)
        .credentials_provider(Credentials::new("test", "test", None, None, "synthetic"))
        .build();
    let client = aws_sdk_s3::Client::from_conf(config);
    client
        .create_bucket()
        .bucket("discord-mcp-synthetic-drill")
        .send()
        .await?;
    let s3 = S3Store::new(client, "discord-mcp-synthetic-drill")?;
    snapshot(&source, &repository)?;
    let uploaded = sync_to_s3(&s3, &repository, "synthetic", "isolated-floci").await?;
    assert_eq!(uploaded.txid, 1);
    assert!(uploaded.objects_confirmed >= 1);
    // Changes after the snapshot must not be silently undone on the source.
    store.set_permissions("101", "support", false, false)?;
    store.revoke_access_token(&token)?;
    assert!(!store.is_permitted("101", "support", ChannelAction::Read)?);
    assert!(store.verify_access_token(&token, resource, 1001)?.is_none());
    let sentinel = b"synthetic protected destination";
    std::fs::write(&protected, sentinel)?;
    assert!(
        restore_from_s3(&s3, "synthetic", &protected, RestoreTarget::Txid(1))
            .await
            .is_err()
    );
    assert_eq!(std::fs::read(&protected)?, sentinel);
    restore_from_s3(&s3, "synthetic", &restored, RestoreTarget::Txid(1)).await?;
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
    assert!(!store.is_permitted("101", "support", ChannelAction::Read)?);
    assert!(store.verify_access_token(&token, resource, 1001)?.is_none());
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
