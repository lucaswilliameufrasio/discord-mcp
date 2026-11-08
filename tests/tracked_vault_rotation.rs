use discord_mcp::{
    channels::{ChannelStore, EmbeddedSecret},
    rotation_progress::Publication,
};
use std::{
    io::Write,
    process::{Command, Stdio},
};

#[test]
fn rotation_and_tracking_commit_or_rollback_together() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.db");
    let key = [103; 32];
    let secret = EmbeddedSecret::DiscordOAuthClientSecret;
    let store = ChannelStore::open(&path, &key).unwrap();
    store
        .insert_embedded_secret(secret, "synthetic-old")
        .unwrap();
    store.create_rotation_progress([1; 16], &[[2; 16]]).unwrap();
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch("CREATE TRIGGER fail_rotation BEFORE UPDATE ON embedded_secrets BEGIN SELECT RAISE(ABORT, 'synthetic failure'); END;").unwrap();
    drop(db);
    assert!(
        store
            .rotate_embedded_secret_tracked(
                secret,
                "synthetic-old",
                "synthetic-new",
                [3; 16],
                &[[2; 16]]
            )
            .is_err()
    );
    assert_eq!(store.get_embedded_secret(secret).unwrap(), "synthetic-old");
    assert!(store.get_rotation_progress([3; 16]).unwrap().is_none());
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch("DROP TRIGGER fail_rotation;").unwrap();
    drop(db);
    assert!(
        store
            .rotate_embedded_secret_tracked(
                secret,
                "synthetic-old",
                "synthetic-new",
                [1; 16],
                &[[2; 16]]
            )
            .is_err()
    );
    assert_eq!(store.get_embedded_secret(secret).unwrap(), "synthetic-old");
    store
        .rotate_embedded_secret_tracked(
            secret,
            "synthetic-old",
            "synthetic-new",
            [3; 16],
            &[[2; 16]],
        )
        .unwrap();
    let db = rusqlite::Connection::open(&path).unwrap();
    let (commitment, ciphertext): (Vec<u8>, Vec<u8>) = db.query_row("SELECT b.commitment,s.ciphertext FROM rotation_secret_binding b JOIN embedded_secrets s ON s.name=b.name WHERE b.revision=?1", [[3u8; 16].as_slice()], |row| Ok((row.get(0)?,row.get(1)?))).unwrap();
    assert_eq!(commitment.len(), 32);
    assert_ne!(commitment, ciphertext);
    drop(db);
    drop(store);
    let store = ChannelStore::open(&path, &key).unwrap();
    let (version, state) = store.get_rotation_progress([3; 16]).unwrap().unwrap();
    assert_eq!(version, 0);
    assert_eq!(state.publication(), Publication::Unknown);
    assert!(!state.all_consumers_reported());
    assert_eq!(store.get_embedded_secret(secret).unwrap(), "synthetic-new");
}

#[test]
fn tracked_cli_exposes_only_unknown_and_never_prints_credentials() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.db");
    let key = [104; 32];
    let store = ChannelStore::open(&path, &key).unwrap();
    store
        .insert_embedded_secret(EmbeddedSecret::DiscordOAuthClientSecret, "synthetic-old")
        .unwrap();
    drop(store);
    let base = || {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_discord-mcp"));
        cmd.env_clear()
            .env("DISCORD_MCP_DB", &path)
            .env("DISCORD_MCP_KEY", hex::encode(key));
        cmd
    };
    let id = hex::encode([7; 16]);
    let instance = hex::encode([8; 16]);
    let mut child = base()
        .args([
            "setup",
            "local",
            "vault",
            "rotate-oauth-tracked",
            &id,
            &instance,
            "--confirm",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"synthetic-old\nsynthetic-new\n")
        .unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(result.status.success());
    for bytes in [&result.stdout, &result.stderr] {
        let text = String::from_utf8_lossy(bytes);
        assert!(!text.contains("synthetic-old"));
        assert!(!text.contains("synthetic-new"));
    }
    let status = base()
        .args(["setup", "local", "vault", "rotation-status", &id])
        .output()
        .unwrap();
    assert!(status.status.success());
    let text = String::from_utf8_lossy(&status.stdout);
    assert!(text.contains("publication=Unknown"));
    assert!(text.contains("all_consumers_reported=false"));
}

#[test]
fn tracked_candidate_is_bound_to_operation_instance_and_current_ciphertext() {
    use discord_mcp::rotation_progress::ProgressEvent;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.db");
    let key = [105; 32];
    let secret = EmbeddedSecret::DiscordOAuthClientSecret;
    let store = ChannelStore::open(&path, &key).unwrap();
    store
        .insert_embedded_secret(secret, "synthetic-old")
        .unwrap();
    store
        .rotate_embedded_secret_tracked(
            secret,
            "synthetic-old",
            "synthetic-new",
            [11; 16],
            &[[12; 16]],
        )
        .unwrap();
    assert_eq!(
        store
            .get_tracked_embedded_secret([11; 16], [12; 16])
            .unwrap(),
        "synthetic-new"
    );
    assert!(
        store
            .get_tracked_embedded_secret([11; 16], [13; 16])
            .is_err()
    );
    assert!(
        store
            .get_tracked_embedded_secret([14; 16], [12; 16])
            .is_err()
    );
    drop(store);
    let store = ChannelStore::open(&path, &key).unwrap();
    assert_eq!(
        store
            .get_tracked_embedded_secret([11; 16], [12; 16])
            .unwrap(),
        "synthetic-new"
    );
    store
        .rotate_embedded_secret(secret, "synthetic-new", "synthetic-later")
        .unwrap();
    assert!(
        store
            .get_tracked_embedded_secret([11; 16], [12; 16])
            .is_err()
    );
    store
        .rotate_embedded_secret_tracked(
            secret,
            "synthetic-later",
            "synthetic-final",
            [15; 16],
            &[[12; 16]],
        )
        .unwrap();
    store
        .advance_rotation_progress([15; 16], 0, ProgressEvent::Revoked)
        .unwrap();
    assert!(
        store
            .get_tracked_embedded_secret([15; 16], [12; 16])
            .is_err()
    );
    assert_eq!(
        store.get_embedded_secret(secret).unwrap(),
        "synthetic-final"
    );
}

#[test]
fn observations_reject_wrong_or_stale_candidate_and_uncertainty_clears_them() {
    use discord_mcp::rotation_progress::{Consumer, ProgressEvent};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.db");
    let key = [106; 32];
    let secret = EmbeddedSecret::DiscordOAuthClientSecret;
    let store = ChannelStore::open(&path, &key).unwrap();
    store
        .insert_embedded_secret(secret, "synthetic-old")
        .unwrap();
    store
        .rotate_embedded_secret_tracked(
            secret,
            "synthetic-old",
            "synthetic-new",
            [11; 16],
            &[[12; 16]],
        )
        .unwrap();
    store
        .begin_rotation_process([11; 16], [12; 16], [20; 16], "synthetic-new")
        .unwrap();
    assert!(
        store
            .record_rotation_login_observation(
                [11; 16],
                [12; 16],
                [20; 16],
                Consumer::OAuth,
                "synthetic-old"
            )
            .is_err()
    );
    assert!(
        store
            .record_rotation_login_observation(
                [11; 16],
                [13; 16],
                [20; 16],
                Consumer::OAuth,
                "synthetic-new"
            )
            .is_err()
    );
    assert!(
        !store
            .rotation_login_observed([11; 16], [12; 16], Consumer::OAuth)
            .unwrap()
    );
    store
        .record_rotation_login_observation(
            [11; 16],
            [12; 16],
            [20; 16],
            Consumer::OAuth,
            "synthetic-new",
        )
        .unwrap();
    assert!(
        store
            .rotation_login_observed([11; 16], [12; 16], Consumer::OAuth)
            .unwrap()
    );
    store
        .begin_rotation_process([11; 16], [12; 16], [21; 16], "synthetic-new")
        .unwrap();
    assert!(
        !store
            .rotation_login_observed([11; 16], [12; 16], Consumer::OAuth)
            .unwrap()
    );
    assert!(
        store
            .record_rotation_login_observation(
                [11; 16],
                [12; 16],
                [20; 16],
                Consumer::OAuth,
                "synthetic-new"
            )
            .is_err()
    );
    store
        .record_rotation_login_observation(
            [11; 16],
            [12; 16],
            [21; 16],
            Consumer::OAuth,
            "synthetic-new",
        )
        .unwrap();
    drop(store);
    let store = ChannelStore::open(&path, &key).unwrap();
    assert!(
        store
            .rotation_login_observed([11; 16], [12; 16], Consumer::OAuth)
            .unwrap()
    );
    store
        .advance_rotation_progress([11; 16], 0, ProgressEvent::Uncertain)
        .unwrap();
    assert!(
        !store
            .rotation_login_observed([11; 16], [12; 16], Consumer::OAuth)
            .unwrap()
    );
    store
        .advance_rotation_progress([11; 16], 1, ProgressEvent::Revoked)
        .unwrap();
    assert!(
        store
            .record_rotation_login_observation(
                [11; 16],
                [12; 16],
                [21; 16],
                Consumer::OAuth,
                "synthetic-new"
            )
            .is_err()
    );
}

#[test]
fn caller_forged_local_reports_never_become_verified_adoption() {
    use discord_mcp::rotation_progress::{Consumer, ProgressEvent};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.db");
    let key = [107; 32];
    let revision = [21; 16];
    let instance = [22; 16];
    let boot = [23; 16];
    let secret = EmbeddedSecret::DiscordOAuthClientSecret;
    let store = ChannelStore::open(&path, &key).unwrap();
    store
        .insert_embedded_secret(secret, "synthetic-old")
        .unwrap();
    store
        .rotate_embedded_secret_tracked(
            secret,
            "synthetic-old",
            "synthetic-new",
            revision,
            &[instance],
        )
        .unwrap();
    // A local caller with database/key access can fabricate these events without a provider login.
    store
        .begin_rotation_process(revision, instance, boot, "synthetic-new")
        .unwrap();
    for consumer in [Consumer::OAuth, Consumer::Admin] {
        store
            .record_rotation_login_observation(revision, instance, boot, consumer, "synthetic-new")
            .unwrap();
    }
    store
        .advance_rotation_progress(revision, 0, ProgressEvent::PublicationConfirmed)
        .unwrap();
    store
        .advance_rotation_progress(
            revision,
            1,
            ProgressEvent::AdoptionReported {
                instance,
                consumer: Consumer::OAuth,
            },
        )
        .unwrap();
    store
        .advance_rotation_progress(
            revision,
            2,
            ProgressEvent::AdoptionReported {
                instance,
                consumer: Consumer::Admin,
            },
        )
        .unwrap();
    drop(store);
    let mut command = Command::new(env!("CARGO_BIN_EXE_discord-mcp"));
    let output = command
        .env_clear()
        .env("DISCORD_MCP_DB", &path)
        .env("DISCORD_MCP_KEY", hex::encode(key))
        .args([
            "setup",
            "local",
            "vault",
            "rotation-status",
            &hex::encode(revision),
            &hex::encode(instance),
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let output = String::from_utf8_lossy(&output.stdout);
    assert!(output.contains("oauth_login_observed=true"));
    assert!(output.contains("admin_login_observed=true"));
    assert!(output.contains("workload_identity_verified=false"));
    assert!(output.contains("adoption_verified=false"));
    let mut command = Command::new(env!("CARGO_BIN_EXE_discord-mcp"));
    let output = command
        .env_clear()
        .env("DISCORD_MCP_DB", &path)
        .env("DISCORD_MCP_KEY", hex::encode(key))
        .args([
            "setup",
            "local",
            "vault",
            "rotation-status",
            &hex::encode(revision),
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let output = String::from_utf8_lossy(&output.stdout);
    assert!(output.contains("all_consumers_reported=true"));
    assert!(output.contains("adoption_verified=false"));
}
