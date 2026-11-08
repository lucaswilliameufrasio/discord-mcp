use discord_mcp::{
    channels::{ChannelStore, StoreError},
    recovery::prepare_restored_copy,
    rotation_progress::{Consumer, ProgressEvent, Publication},
};

#[test]
fn progress_survives_reopen_and_uncertainty_discards_old_claims() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("vault.db");
    let key = [99; 32];
    let revision = [1; 16];
    let instance = [2; 16];
    let store = ChannelStore::open(&path, &key).unwrap();
    store
        .create_rotation_progress(revision, &[instance])
        .unwrap();
    assert!(
        store
            .create_rotation_progress(revision, &[instance])
            .is_err()
    );
    assert!(
        store
            .advance_rotation_progress(
                revision,
                0,
                ProgressEvent::AdoptionReported {
                    instance,
                    consumer: Consumer::OAuth
                }
            )
            .is_err()
    );
    assert_eq!(
        store
            .advance_rotation_progress(revision, 0, ProgressEvent::PublicationConfirmed)
            .unwrap(),
        1
    );
    assert_eq!(
        store
            .advance_rotation_progress(
                revision,
                1,
                ProgressEvent::AdoptionReported {
                    instance,
                    consumer: Consumer::OAuth
                }
            )
            .unwrap(),
        2
    );
    drop(store);
    let store = ChannelStore::open(&path, &key).unwrap();
    let (version, state) = store.get_rotation_progress(revision).unwrap().unwrap();
    assert_eq!(version, 2);
    assert_eq!(state.publication(), Publication::Confirmed);
    assert!(!state.all_consumers_reported());
    assert_eq!(
        store
            .advance_rotation_progress(revision, 2, ProgressEvent::Uncertain)
            .unwrap(),
        3
    );
    drop(store);
    let store = ChannelStore::open(&path, &key).unwrap();
    let (version, state) = store.get_rotation_progress(revision).unwrap().unwrap();
    assert_eq!(version, 3);
    assert_eq!(state.publication(), Publication::Unknown);
    assert!(!state.all_consumers_reported());
    assert_eq!(
        store
            .advance_rotation_progress(revision, 3, ProgressEvent::PublicationConfirmed)
            .unwrap(),
        4
    );
    assert_eq!(
        store
            .advance_rotation_progress(
                revision,
                4,
                ProgressEvent::AdoptionReported {
                    instance,
                    consumer: Consumer::Admin
                }
            )
            .unwrap(),
        5
    );
    assert!(
        !store
            .get_rotation_progress(revision)
            .unwrap()
            .unwrap()
            .1
            .all_consumers_reported()
    );
    assert_eq!(
        store
            .advance_rotation_progress(revision, 5, ProgressEvent::Revoked)
            .unwrap(),
        6
    );
    assert!(
        store
            .advance_rotation_progress(revision, 6, ProgressEvent::PublicationConfirmed)
            .is_err()
    );
    assert_eq!(
        store
            .get_rotation_progress(revision)
            .unwrap()
            .unwrap()
            .1
            .publication(),
        Publication::Revoked
    );
}

#[test]
fn concurrent_updates_on_same_version_have_exactly_one_winner() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("vault.db");
    let key = [100; 32];
    let revision = [3; 16];
    let store = ChannelStore::open(&path, &key).unwrap();
    store
        .create_rotation_progress(revision, &[[4; 16]])
        .unwrap();
    drop(store);
    let successes = std::thread::scope(|scope| {
        let run = || {
            let store = ChannelStore::open(&path, &key).unwrap();
            store
                .advance_rotation_progress(revision, 0, ProgressEvent::PublicationConfirmed)
                .is_ok()
        };
        let a = scope.spawn(run);
        let b = scope.spawn(run);
        usize::from(a.join().unwrap()) + usize::from(b.join().unwrap())
    });
    assert_eq!(successes, 1);
    assert_eq!(
        ChannelStore::open(&path, &key)
            .unwrap()
            .get_rotation_progress(revision)
            .unwrap()
            .unwrap()
            .0,
        1
    );
}

#[test]
fn restored_copy_never_reactivates_old_tracking_claims() {
    let dir = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let source = dir.path().join("source.db");
    let recovery = dir.path().join("recovery.db");
    let key = [101; 32];
    let revision = [5; 16];
    let store = ChannelStore::open(&source, &key).unwrap();
    store
        .create_rotation_progress(revision, &[[6; 16]])
        .unwrap();
    store
        .advance_rotation_progress(revision, 0, ProgressEvent::PublicationConfirmed)
        .unwrap();
    drop(store);
    prepare_restored_copy(&source, &recovery, &key, 90).unwrap();
    let store = ChannelStore::open_recovery_copy(&recovery, &key).unwrap();
    assert!(matches!(
        store.get_rotation_progress(revision),
        Err(StoreError::RecoveryPending)
    ));
    drop(store);
    let db = rusqlite::Connection::open(&recovery).unwrap();
    db.execute(
        "INSERT INTO rotation_progress(revision,version,state) VALUES (?1,0,X'00')",
        [revision.as_slice()],
    )
    .unwrap();
    drop(db);
    let store = ChannelStore::open_recovery_copy(&recovery, &key).unwrap();
    assert!(store.activate_recovered_copy().is_err());
    drop(store);
    let db = rusqlite::Connection::open(&recovery).unwrap();
    db.execute("DELETE FROM rotation_progress", []).unwrap();
    drop(db);
    let store = ChannelStore::open_recovery_copy(&recovery, &key).unwrap();
    store.activate_recovered_copy().unwrap();
    assert!(store.get_rotation_progress(revision).unwrap().is_none());
    assert!(
        ChannelStore::open(&source, &key)
            .unwrap()
            .get_rotation_progress(revision)
            .unwrap()
            .is_some()
    );
}
