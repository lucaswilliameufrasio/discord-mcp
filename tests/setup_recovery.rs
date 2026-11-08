//! Synthetic, offline CLI journey from local setup to quarantined recovery.
use std::process::Command;

use discord_mcp::channels::{ChannelAction, ChannelStore};

#[test]
fn setup_and_recovery_preserve_source_and_revoke_restored_authority() {
    let dir = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let source = dir.path().join("source.db");
    let recovered = dir.path().join("recovered.db");
    let occupied = dir.path().join("occupied.db");
    let key = [71u8; 32];
    let binary = env!("CARGO_BIN_EXE_discord-mcp");
    let cli = |database: &std::path::Path, key: [u8; 32], args: &[&std::ffi::OsStr]| {
        Command::new(binary)
            .env_clear()
            .env("DISCORD_MCP_DB", database)
            .env("DISCORD_MCP_KEY", hex::encode(key))
            .args(args)
            .output()
            .unwrap()
    };
    let init = cli(
        &source,
        key,
        &["setup".as_ref(), "local".as_ref(), "--init".as_ref()],
    );
    assert!(init.status.success());
    let store = ChannelStore::open(&source, &key).unwrap();
    store
        .put_bot("alerts", "123", "synthetic-bot-token")
        .unwrap();
    store.set_permissions("101", "alerts", true, true).unwrap();
    let resource = "https://example.invalid/mcp";
    let token = store.issue_access_token("101", resource, 1000, 60).unwrap();
    drop(store);
    let baseline = std::fs::read(&source).unwrap();
    std::fs::write(&occupied, "operator data").unwrap();
    let prepare = |destination: &std::path::Path, key| {
        cli(
            &source,
            key,
            &[
                "recovery-prepare".as_ref(),
                source.as_os_str(),
                destination.as_os_str(),
            ],
        )
    };
    assert!(!prepare(&occupied, key).status.success());
    assert_eq!(std::fs::read(&occupied).unwrap(), b"operator data");
    assert!(!prepare(&recovered, [72; 32]).status.success());
    assert!(!recovered.exists());
    assert!(prepare(&recovered, key).status.success());
    assert_eq!(std::fs::read(&source).unwrap(), baseline);
    assert!(
        !cli(&recovered, key, &["list-channels".as_ref()])
            .status
            .success()
    );
    assert!(
        !cli(&recovered, key, &["recovery-activate".as_ref()])
            .status
            .success()
    );
    assert!(
        cli(
            &recovered,
            key,
            &["recovery-activate".as_ref(), "--confirm".as_ref()]
        )
        .status
        .success()
    );
    let store = ChannelStore::open(&recovered, &key).unwrap();
    assert!(store.get_channel("alerts").is_ok());
    assert!(
        !store
            .is_permitted("101", "alerts", ChannelAction::Read)
            .unwrap()
    );
    assert_eq!(
        store.verify_access_token(&token, resource, 1001).unwrap(),
        None
    );
    assert_eq!(std::fs::read(&source).unwrap(), baseline);
}
