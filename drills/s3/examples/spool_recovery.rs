//! Synthetic LTX spool research only; no application or remote data is used.
#[cfg(test)]
use std::{
    fs::{self, File},
    io::Cursor,
    path::Path,
};

#[cfg(test)]
use sqlite_replication_ltx::{DatabaseState, Header, Page, encode_delta, encode_snapshot};
#[cfg(test)]
use sqlite_replication_spool::{FaultPoint, publish_transactions, read_watermark, recover};
#[cfg(test)]
use sqlite_replication_storage_fs::{ensure_repository, ltx_path};
#[cfg(test)]
use sqlite_replication_wal::translate::{Position, State, Transaction};

#[cfg(test)]
fn fixture(root: &Path) -> (Vec<u8>, Transaction, State) {
    let mut image = vec![0_u8; 1024];
    image[..16].copy_from_slice(b"SQLite format 3\0");
    image[16..18].copy_from_slice(&512_u16.to_be_bytes());
    image[28..32].copy_from_slice(&2_u32.to_be_bytes());
    let base = DatabaseState::load(&mut Cursor::new(&image), 1).unwrap();
    ensure_repository(root).unwrap();
    let snapshot = ltx_path(root, 1, 1);
    fs::create_dir_all(snapshot.parent().unwrap()).unwrap();
    let mut file = File::create(snapshot).unwrap();
    encode_snapshot(&mut Cursor::new(&image), &mut file, 1).unwrap();
    file.sync_all().unwrap();
    publish_transactions(
        root,
        &[],
        &State {
            database: base.clone(),
            wal: Position::default(),
        },
        |_, _| Ok(()),
    )
    .unwrap();
    let header = Header {
        flags: 0,
        page_size: 512,
        commit: 2,
        min_txid: 2,
        max_txid: 2,
        timestamp_millis: 1,
        pre_apply_checksum: base.checksum,
        wal_offset: 32,
        wal_size: 568,
        wal_salt_1: 1,
        wal_salt_2: 2,
        node_id: 0,
    };
    let pages = vec![Page {
        page_no: 2,
        data: vec![0x56; 512],
    }];
    let updated = base.apply_pages(header, &pages).unwrap();
    let mut encoded = Vec::new();
    encode_delta(header, &pages, updated.checksum, &mut encoded).unwrap();
    let transaction = Transaction {
        start_frame: 1,
        end_frame: 1,
        header,
        pages,
        post_checksum: updated.checksum,
        encoded,
    };
    let state = State {
        database: updated,
        wal: Position {
            initialized: true,
            checkpoint_sequence: 1,
            salt_1: 1,
            salt_2: 2,
            frame_count: 1,
            checksum_1: 3,
            checksum_2: 4,
        },
    };
    (image, transaction, state)
}

fn main() {
    println!("Run with cargo test --manifest-path drills/s3/Cargo.toml --example spool_recovery");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    fn image_state(source: &rusqlite::Connection, path: &Path) -> (u32, u32, u64) {
        use rusqlite::{
            Connection,
            backup::{Backup, StepResult},
        };
        let mut destination = Connection::open(path).unwrap();
        {
            let backup = Backup::new(source, &mut destination).unwrap();
            assert_eq!(backup.step(-1).unwrap(), StepResult::Done);
        }
        drop(destination);
        let state = DatabaseState::load(&mut File::open(path).unwrap(), 1).unwrap();
        (state.page_size, state.commit, state.checksum)
    }

    // Fixture is single-threaded: the test itself is the only writer and does
    // not write during capture. This is not an application write gate.
    struct IsolatedGate;
    impl sqlite_replication_core::WriteGate for IsolatedGate {
        type Permit = IsolatedGate;

        fn acquire(&self) -> io::Result<Self::Permit> {
            Ok(IsolatedGate)
        }
    }
    impl sqlite_replication_core::WritePermit for IsolatedGate {
        fn release(self) -> io::Result<()> {
            Ok(())
        }
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    fn checkpoint_generation_drill(new_generation: bool, partial_publication: bool) {
        use rusqlite::Connection;
        use sqlite_replication_core::{RestoreTarget, capture_once, restore_to_target, snapshot};

        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.db");
        let repository = dir.path().join("repository");
        let wal_path = dir.path().join("source.db-wal");
        let db = Connection::open(&source).unwrap();
        db.execute_batch("PRAGMA page_size=512; PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; CREATE TABLE records(id INTEGER PRIMARY KEY, value TEXT NOT NULL); INSERT INTO records VALUES(1, 'base'); PRAGMA wal_checkpoint(TRUNCATE);").unwrap();
        snapshot(&source, &repository).unwrap();
        let base = recover(&repository, &[]).unwrap();
        db.execute("INSERT INTO records VALUES(2, 'checkpointed')", [])
            .unwrap();
        let old_wal = fs::read(&wal_path).unwrap();
        assert!(old_wal.len() > 32);
        if partial_publication {
            let (transactions, state) =
                sqlite_replication_wal::translate::translate_wal(&old_wal, &base, 200).unwrap();
            assert_eq!(transactions.len(), 1);
            assert_eq!(state.database.txid, 2);
            assert!(
                publish_transactions(&repository, &transactions, &state, |point, _| {
                    if point == FaultPoint::AfterLtxPublish {
                        return Err(io::Error::other("synthetic interruption after publication"));
                    }
                    Ok(())
                })
                .is_err()
            );
            assert_eq!(read_watermark(&repository).unwrap().txid, 1);
            assert!(ltx_path(&repository, 2, 2).exists());
            db.execute("INSERT INTO records VALUES(4, 'after-publication')", [])
                .unwrap();
        }
        let checkpoint: (i64, i64, i64) = db
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .unwrap();
        assert_eq!(checkpoint.0, 0);
        if new_generation {
            db.execute("INSERT INTO records VALUES(3, 'new-generation')", [])
                .unwrap();
            let current_wal = fs::read(&wal_path).unwrap();
            assert!(current_wal.len() > 32);
            assert_ne!(&old_wal[16..24], &current_wal[16..24]);
        }
        let expected: Vec<(i64, String)> = db
            .prepare("SELECT id, value FROM records ORDER BY id")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        let keep_open = if !new_generation {
            drop(db);
            assert!(
                !wal_path.exists(),
                "SQLite should remove the WAL when the last connection closes"
            );
            None
        } else {
            // No writes occur during capture; retain this connection so the new
            // WAL generation stays present until the library reads it.
            Some(db)
        };
        let result = capture_once(&source, &repository, &IsolatedGate).unwrap();
        if partial_publication {
            assert_eq!(result.txid, 3);
        } else {
            assert!(result.txid >= 2);
        }
        assert_eq!(read_watermark(&repository).unwrap().txid, result.txid);
        let repeated = capture_once(&source, &repository, &IsolatedGate).unwrap();
        assert_eq!(repeated.txid, result.txid);
        assert_eq!(repeated.transactions, 0);
        let restored = dir.path().join("restored.db");
        restore_to_target(&repository, &restored, RestoreTarget::Txid(result.txid)).unwrap();
        let restored_db = Connection::open(&restored).unwrap();
        let restored_rows: Vec<(i64, String)> = restored_db
            .prepare("SELECT id, value FROM records ORDER BY id")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(restored_rows, expected);
        let integrity: String = restored_db
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .unwrap();
        assert_eq!(integrity, "ok");
        let source_db = Connection::open(&source).unwrap();
        let rows_after: Vec<(i64, String)> = source_db
            .prepare("SELECT id, value FROM records ORDER BY id")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(rows_after, expected);
        drop(keep_open);
    }

    #[test]
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    fn capture_reconciles_checkpointed_changes_when_wal_is_absent() {
        checkpoint_generation_drill(false, false);
    }

    #[test]
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    fn capture_reconciles_checkpointed_changes_with_new_wal_generation() {
        checkpoint_generation_drill(true, false);
    }

    #[test]
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    fn capture_recovers_published_ltx_after_checkpoint_and_missing_wal() {
        checkpoint_generation_drill(false, true);
    }

    #[test]
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    fn capture_recovers_published_ltx_after_wal_generation_change() {
        checkpoint_generation_drill(true, true);
    }

    #[test]
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    fn missing_uncheckpointed_wal_can_reconcile_to_stale_image() {
        use rusqlite::Connection;
        use sqlite_replication_core::{RestoreTarget, capture_once, restore_to_target, snapshot};
        use sqlite_replication_wal::translate::translate_wal;

        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.db");
        let stale = dir.path().join("incomplete-copy.db");
        let repository = dir.path().join("repository");
        let db = Connection::open(&source).unwrap();
        db.execute_batch("PRAGMA page_size=512; PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; CREATE TABLE records(id INTEGER PRIMARY KEY, value TEXT NOT NULL); INSERT INTO records VALUES(1, 'base'); PRAGMA wal_checkpoint(TRUNCATE);").unwrap();
        snapshot(&source, &repository).unwrap();
        let base = recover(&repository, &[]).unwrap();
        db.execute("INSERT INTO records VALUES(2, 'committed-in-wal')", [])
            .unwrap();
        let wal_path = dir.path().join("source.db-wal");
        let wal = fs::read(&wal_path).unwrap();
        assert!(wal.len() > 32);
        let (transactions, state) = translate_wal(&wal, &base, 200).unwrap();
        assert_eq!(transactions.len(), 1);
        assert!(
            publish_transactions(&repository, &transactions, &state, |point, _| {
                if point == FaultPoint::AfterLtxPublish {
                    return Err(io::Error::other("synthetic interruption"));
                }
                Ok(())
            })
            .is_err()
        );
        assert_eq!(read_watermark(&repository).unwrap().txid, 1);
        let published = fs::read(ltx_path(&repository, 2, 2)).unwrap();
        let source_before: i64 = db
            .query_row("SELECT COUNT(*) FROM records", [], |row| row.get(0))
            .unwrap();
        assert_eq!(source_before, 2);
        // Deliberately incomplete copy to model a lost WAL. Never modify the
        // original source or its live WAL and never use this as a backup method.
        fs::copy(&source, &stale).unwrap();
        assert!(!dir.path().join("incomplete-copy.db-wal").exists());
        let stale_db = Connection::open(&stale).unwrap();
        let stale_count: i64 = stale_db
            .query_row("SELECT COUNT(*) FROM records", [], |row| row.get(0))
            .unwrap();
        assert_eq!(stale_count, 1);
        drop(stale_db);
        let capture = capture_once(&stale, &repository, &IsolatedGate).unwrap();
        // A successful reconciliation to the stale image does not detect loss.
        assert_eq!(capture.txid, 3);
        assert_eq!(read_watermark(&repository).unwrap().txid, 3);
        assert_eq!(fs::read(ltx_path(&repository, 2, 2)).unwrap(), published);
        let retained = dir.path().join("retained-txid-2.db");
        restore_to_target(&repository, &retained, RestoreTarget::Txid(2)).unwrap();
        let retained_db = Connection::open(&retained).unwrap();
        assert_eq!(
            retained_db
                .query_row("SELECT COUNT(*) FROM records", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            2
        );
        let restored = dir.path().join("restored.db");
        restore_to_target(&repository, &restored, RestoreTarget::Txid(capture.txid)).unwrap();
        let result = Connection::open(&restored).unwrap();
        let restored_count: i64 = result
            .query_row("SELECT COUNT(*) FROM records", [], |row| row.get(0))
            .unwrap();
        let integrity: String = result
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .unwrap();
        assert_eq!(integrity, "ok");
        assert_eq!(restored_count, 1);
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM records", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            source_before
        );
        assert_eq!(fs::read(&wal_path).unwrap(), wal);
    }

    #[test]
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    fn independent_image_witness_blocks_stale_source_before_spool_mutation() {
        use rusqlite::Connection;
        use sqlite_replication_core::{RestoreTarget, capture_once, restore_to_target, snapshot};
        use sqlite_replication_wal::translate::translate_wal;

        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.db");
        let stale = dir.path().join("stale.db");
        let repository = dir.path().join("repository");
        let db = Connection::open(&source).unwrap();
        db.execute_batch("PRAGMA page_size=512; PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; CREATE TABLE records(id INTEGER PRIMARY KEY, value TEXT NOT NULL); INSERT INTO records VALUES(1, 'base'); PRAGMA wal_checkpoint(TRUNCATE);").unwrap();
        snapshot(&source, &repository).unwrap();
        let base = recover(&repository, &[]).unwrap();
        db.execute("INSERT INTO records VALUES(2, 'committed-in-wal')", [])
            .unwrap();
        let wal = fs::read(dir.path().join("source.db-wal")).unwrap();
        let (transactions, state) = translate_wal(&wal, &base, 200).unwrap();
        assert_eq!(transactions.len(), 1);
        assert!(
            publish_transactions(&repository, &transactions, &state, |point, _| {
                if point == FaultPoint::AfterLtxPublish {
                    return Err(io::Error::other("synthetic interruption"));
                }
                Ok(())
            })
            .is_err()
        );
        // The witness is obtained independently of spool recovery while the
        // complete source and its committed WAL are still available.
        let witness = image_state(&db, &dir.path().join("witness.db"));
        fs::copy(&source, &stale).unwrap();
        let candidate = Connection::open(&stale).unwrap();
        let candidate_state = image_state(&candidate, &dir.path().join("candidate-image.db"));
        drop(candidate);
        let snapshot_path = ltx_path(&repository, 1, 1);
        let delta_path = ltx_path(&repository, 2, 2);
        let snapshot_before = fs::read(&snapshot_path).unwrap();
        let delta_before = fs::read(&delta_path).unwrap();
        let watermark_before = read_watermark(&repository).unwrap();
        let gate = |expected: Option<(u32, u32, u64)>, actual| -> io::Result<()> {
            match expected {
                Some(value) if value == actual => Ok(()),
                _ => Err(io::Error::other("missing or mismatched continuity witness")),
            }
        };
        let guarded_capture = |path: &Path, expected, actual| {
            gate(expected, actual)?;
            capture_once(path, &repository, &IsolatedGate)
        };
        assert!(guarded_capture(&stale, None, candidate_state).is_err());
        assert!(guarded_capture(&stale, Some(witness), candidate_state).is_err());
        assert_eq!(fs::read(snapshot_path).unwrap(), snapshot_before);
        assert_eq!(fs::read(delta_path).unwrap(), delta_before);
        assert_eq!(read_watermark(&repository).unwrap(), watermark_before);
        assert!(!ltx_path(&repository, 3, 3).exists());
        assert_eq!(fs::read(dir.path().join("source.db-wal")).unwrap(), wal);
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM records", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            2
        );

        let retained = dir.path().join("retained.db");
        restore_to_target(&repository, &retained, RestoreTarget::Txid(2)).unwrap();
        let restored = Connection::open(retained).unwrap();
        assert_eq!(
            restored
                .query_row("SELECT COUNT(*) FROM records", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            2
        );
        // Positive control: no writers run between evidence and capture.
        let complete = image_state(&db, &dir.path().join("complete-image.db"));
        let result = guarded_capture(&source, Some(witness), complete).unwrap();
        assert_eq!(result.txid, 2);
    }

    #[test]
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    fn witness_check_can_become_stale_before_capture() {
        use rusqlite::Connection;
        use sqlite_replication_core::{RestoreTarget, capture_once, restore_to_target, snapshot};
        use std::{sync::mpsc, time::Duration};

        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.db");
        let repository = dir.path().join("repository");
        let db = Connection::open(&source).unwrap();
        db.execute_batch("PRAGMA page_size=512; PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; CREATE TABLE records(id INTEGER PRIMARY KEY, value TEXT NOT NULL); INSERT INTO records VALUES(1, 'base'); PRAGMA wal_checkpoint(TRUNCATE);").unwrap();
        snapshot(&source, &repository).unwrap();
        recover(&repository, &[]).unwrap();
        let watermark_before = read_watermark(&repository).unwrap();
        let snapshot_before = fs::read(ltx_path(&repository, 1, 1)).unwrap();
        let witness = image_state(&db, &dir.path().join("witness.db"));
        let verified = image_state(&db, &dir.path().join("verified.db"));
        assert_eq!(verified, witness);
        // Synchronize a real second connection to commit precisely after the
        // witness was accepted, but before the later capture call.
        let (start_tx, start_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let writer_path = source.clone();
        let writer = std::thread::spawn(move || {
            start_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            let writer_db = Connection::open(writer_path).unwrap();
            writer_db
                .execute("INSERT INTO records VALUES(2, 'after-check')", [])
                .unwrap();
            done_tx.send(()).unwrap();
        });
        start_tx.send(()).unwrap();
        done_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        writer.join().unwrap();
        let changed = image_state(&db, &dir.path().join("changed.db"));
        assert_ne!(changed, witness);
        // A fresh check would reject this legitimate change without touching
        // the spool; the earlier successful check is now stale.
        let current_check = if changed == witness {
            Ok(())
        } else {
            Err("stale witness")
        };
        assert!(current_check.is_err());
        assert_eq!(read_watermark(&repository).unwrap(), watermark_before);
        assert_eq!(
            fs::read(ltx_path(&repository, 1, 1)).unwrap(),
            snapshot_before
        );
        assert!(!ltx_path(&repository, 2, 2).exists());
        // Model a caller that trusts the previously successful comparison:
        // no-op IsolatedGate cannot fence the write that already happened.
        assert_eq!(verified, witness);
        let result = capture_once(&source, &repository, &IsolatedGate).unwrap();
        assert_eq!(result.txid, 2);
        let restored = dir.path().join("restored.db");
        restore_to_target(&repository, &restored, RestoreTarget::Txid(2)).unwrap();
        let restored_db = Connection::open(restored).unwrap();
        assert_eq!(
            restored_db
                .query_row("SELECT COUNT(*) FROM records", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            2
        );
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM records", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            2
        );
    }

    #[test]
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    fn continuity_barrier_worker() {
        use rusqlite::Connection;
        use sqlite_replication_core::capture_once;
        let Some(dir) = std::env::var_os("CONTINUITY_FIXTURE_DIR") else {
            return;
        };
        let dir = Path::new(&dir);
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(dir.join("writers.lock"))
            .unwrap();
        lock.lock().unwrap();
        let db = Connection::open(dir.join("source.db")).unwrap();
        let image = tempfile::Builder::new()
            .prefix("worker-image-")
            .tempfile_in(dir)
            .unwrap();
        let actual = image_state(&db, image.path());
        let witness = fs::read_to_string(dir.join("witness.txt")).unwrap();
        if format!("{actual:?}") != witness {
            eprintln!("MARK mismatch");
            return;
        }
        eprintln!("MARK verified");
        let mut command = String::new();
        std::io::stdin().read_line(&mut command).unwrap();
        assert_eq!(command.trim(), "capture");
        let result =
            capture_once(dir.join("source.db"), dir.join("repository"), &IsolatedGate).unwrap();
        assert_eq!(result.txid, 1);
        eprintln!("MARK captured");
        drop(db);
        drop(lock);
    }

    #[test]
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    fn continuity_authority_worker() {
        use std::{
            io::{BufRead, BufReader, Write},
            os::unix::net::UnixListener,
            sync::{Arc, Mutex},
        };
        let Some(dir) = std::env::var_os("CONTINUITY_AUTHORITY_DIR") else {
            return;
        };
        let dir = std::path::PathBuf::from(dir);
        let listener = UnixListener::bind(dir.join("authority.sock")).unwrap();
        // (open, capture-running, capture-completed); every process starts closed.
        let state = Arc::new(Mutex::new((false, false, false)));
        eprintln!("MARK ready");
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let state = state.clone();
            let dir = dir.clone();
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut command = String::new();
                reader.read_line(&mut command).unwrap();
                match command.trim() {
                    "WRITE" => {
                        let guard = state.lock().unwrap();
                        writeln!(stream, "{}", if guard.0 { "ADMITTED" } else { "DENIED" })
                            .unwrap();
                    }
                    "OPEN" => {
                        let mut guard = state.lock().unwrap();
                        if guard.1 || !guard.2 {
                            writeln!(stream, "DENIED").unwrap();
                            return;
                        }
                        let lock = fs::OpenOptions::new()
                            .read(true)
                            .write(true)
                            .open(dir.join("writers.lock"))
                            .unwrap();
                        if lock.try_lock().is_err() {
                            writeln!(stream, "BUSY").unwrap();
                            return;
                        }
                        guard.0 = true;
                        writeln!(stream, "OPENED").unwrap();
                    }
                    "CAPTURE" | "CAPTURE_FAULT" => {
                        {
                            let mut guard = state.lock().unwrap();
                            if guard.1 {
                                writeln!(stream, "BUSY").unwrap();
                                return;
                            }
                            guard.0 = false;
                            guard.1 = true;
                            guard.2 = false;
                        }
                        let lock = fs::OpenOptions::new()
                            .read(true)
                            .write(true)
                            .open(dir.join("writers.lock"))
                            .unwrap();
                        if lock.try_lock().is_err() {
                            state.lock().unwrap().1 = false;
                            writeln!(stream, "BUSY").unwrap();
                            return;
                        }
                        let db = rusqlite::Connection::open(dir.join("source.db")).unwrap();
                        let image = tempfile::Builder::new()
                            .prefix("authority-image-")
                            .tempfile_in(&dir)
                            .unwrap();
                        let actual = image_state(&db, image.path());
                        if format!("{actual:?}")
                            != fs::read_to_string(dir.join("witness.txt")).unwrap()
                        {
                            state.lock().unwrap().1 = false;
                            writeln!(stream, "MISMATCH").unwrap();
                            return;
                        }
                        writeln!(stream, "VERIFIED").unwrap();
                        let mut next = String::new();
                        if reader.read_line(&mut next).is_err() || next.trim() != "GO" {
                            state.lock().unwrap().1 = false;
                            return;
                        }
                        if command.trim() == "CAPTURE_FAULT" {
                            // Fixture-only path: the public capture_once API has
                            // no fault hook, so exercise the same spool boundary
                            // directly while the authority holds its lock.
                            let repository = dir.join("repository");
                            let wal = fs::read(dir.join("source.db-wal")).unwrap();
                            let base = recover(&repository, &wal).unwrap();
                            let (transactions, next_state) =
                                sqlite_replication_wal::translate::translate_wal(&wal, &base, 200)
                                    .unwrap();
                            assert_eq!(transactions.len(), 1);
                            let _ = publish_transactions(
                                &repository,
                                &transactions,
                                &next_state,
                                |point, _| {
                                    if point == FaultPoint::AfterLtxPublish {
                                        writeln!(stream, "PUBLISHED")?;
                                        let mut release = String::new();
                                        reader.read_line(&mut release)?;
                                        return Err(io::Error::other("synthetic interruption"));
                                    }
                                    Ok(())
                                },
                            );
                            return;
                        }
                        let result = sqlite_replication_core::capture_once(
                            dir.join("source.db"),
                            dir.join("repository"),
                            &IsolatedGate,
                        );
                        let mut guard = state.lock().unwrap();
                        guard.1 = false;
                        if result.is_ok() {
                            guard.2 = true;
                            writeln!(stream, "CAPTURED").unwrap();
                        } else {
                            writeln!(stream, "FAILED").unwrap();
                        }
                        drop(lock);
                    }
                    _ => {}
                }
            });
        }
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    async fn barrier_worker(
        dir: &Path,
    ) -> (
        tokio::process::Child,
        tokio::io::BufReader<tokio::process::ChildStderr>,
    ) {
        use std::process::Stdio;
        let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
            .env_clear()
            .env("CONTINUITY_FIXTURE_DIR", dir)
            .args(["--exact", "tests::continuity_barrier_worker", "--nocapture"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let stderr = tokio::io::BufReader::new(child.stderr.take().unwrap());
        (child, stderr)
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    async fn start_continuity_authority(dir: &Path) -> tokio::process::Child {
        use std::process::Stdio;
        let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
            .env_clear()
            .env("CONTINUITY_AUTHORITY_DIR", dir)
            .args([
                "--exact",
                "tests::continuity_authority_worker",
                "--nocapture",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut stderr = tokio::io::BufReader::new(child.stderr.take().unwrap());
        barrier_marker(&mut stderr, "ready").await;
        child
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    async fn authority_command(
        dir: &Path,
        command: &str,
    ) -> tokio::io::BufReader<tokio::net::UnixStream> {
        use tokio::io::AsyncWriteExt;
        let mut stream = tokio::net::UnixStream::connect(dir.join("authority.sock"))
            .await
            .unwrap();
        stream
            .write_all(format!("{command}\n").as_bytes())
            .await
            .unwrap();
        tokio::io::BufReader::new(stream)
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    async fn authority_line(reader: &mut tokio::io::BufReader<tokio::net::UnixStream>) -> String {
        use tokio::{
            io::AsyncBufReadExt,
            time::{Duration, timeout},
        };
        let mut line = String::new();
        let count = timeout(Duration::from_secs(5), reader.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        assert!(count > 0);
        line.trim().to_owned()
    }

    #[tokio::test]
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    async fn authority_starts_closed_survives_capture_crash_and_requires_reverification() {
        use tokio::{
            io::AsyncWriteExt,
            time::{Duration, timeout},
        };
        let (dir, db) = barrier_fixture();
        let witness = fs::read(dir.path().join("witness.txt")).unwrap();
        let mut authority = start_continuity_authority(dir.path()).await;
        assert_eq!(
            authority_line(&mut authority_command(dir.path(), "WRITE").await).await,
            "DENIED"
        );
        assert_eq!(
            authority_line(&mut authority_command(dir.path(), "OPEN").await).await,
            "DENIED"
        );
        fs::write(dir.path().join("witness.txt"), b"invalid synthetic witness").unwrap();
        assert_eq!(
            authority_line(&mut authority_command(dir.path(), "CAPTURE").await).await,
            "MISMATCH"
        );
        assert_eq!(
            authority_line(&mut authority_command(dir.path(), "OPEN").await).await,
            "DENIED"
        );
        fs::write(dir.path().join("witness.txt"), witness).unwrap();
        let mut capture = authority_command(dir.path(), "CAPTURE").await;
        assert_eq!(authority_line(&mut capture).await, "VERIFIED");
        let writer_lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(dir.path().join("writers.lock"))
            .unwrap();
        assert!(writer_lock.try_lock_shared().is_err());
        assert_eq!(
            authority_line(&mut authority_command(dir.path(), "WRITE").await).await,
            "DENIED"
        );
        authority.kill().await.unwrap();
        authority.wait().await.unwrap();
        writer_lock.try_lock_shared().unwrap();
        assert!(
            tokio::net::UnixStream::connect(dir.path().join("authority.sock"))
                .await
                .is_err()
        );
        drop(writer_lock);
        // Only this fixture-owned socket is removed after its process was reaped.
        fs::remove_file(dir.path().join("authority.sock")).unwrap();
        let mut restarted = start_continuity_authority(dir.path()).await;
        assert_eq!(
            authority_line(&mut authority_command(dir.path(), "WRITE").await).await,
            "DENIED"
        );
        assert_eq!(
            authority_line(&mut authority_command(dir.path(), "OPEN").await).await,
            "DENIED"
        );
        let mut capture = authority_command(dir.path(), "CAPTURE").await;
        assert_eq!(authority_line(&mut capture).await, "VERIFIED");
        capture.get_mut().write_all(b"GO\n").await.unwrap();
        assert_eq!(authority_line(&mut capture).await, "CAPTURED");
        assert_eq!(
            authority_line(&mut authority_command(dir.path(), "WRITE").await).await,
            "DENIED"
        );
        assert_eq!(
            authority_line(&mut authority_command(dir.path(), "OPEN").await).await,
            "OPENED"
        );
        let writer_lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(dir.path().join("writers.lock"))
            .unwrap();
        writer_lock.lock_shared().unwrap();
        assert_eq!(
            authority_line(&mut authority_command(dir.path(), "WRITE").await).await,
            "ADMITTED"
        );
        db.execute("INSERT INTO records VALUES(2, 'after-open')", [])
            .unwrap();
        drop(writer_lock);
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM records", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            2
        );
        restarted.kill().await.unwrap();
        timeout(Duration::from_secs(5), restarted.wait())
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    async fn authority_death_after_ltx_publish_requires_explicit_spool_reconciliation() {
        use tokio::{
            io::AsyncWriteExt,
            time::{Duration, timeout},
        };
        let (dir, db) = barrier_fixture();
        let repository = dir.path().join("repository");
        db.execute("INSERT INTO records VALUES(2, 'pending-publication')", [])
            .unwrap();
        let witness = image_state(&db, &dir.path().join("updated-witness.db"));
        fs::write(dir.path().join("witness.txt"), format!("{witness:?}")).unwrap();
        let source_count: i64 = db
            .query_row("SELECT COUNT(*) FROM records", [], |row| row.get(0))
            .unwrap();
        assert_eq!(source_count, 2);
        let mut authority = start_continuity_authority(dir.path()).await;
        let mut capture = authority_command(dir.path(), "CAPTURE_FAULT").await;
        assert_eq!(authority_line(&mut capture).await, "VERIFIED");
        capture.get_mut().write_all(b"GO\n").await.unwrap();
        assert_eq!(authority_line(&mut capture).await, "PUBLISHED");
        let delta_path = ltx_path(&repository, 2, 2);
        let delta = fs::read(&delta_path).unwrap();
        assert_eq!(read_watermark(&repository).unwrap().txid, 1);
        assert_eq!(
            authority_line(&mut authority_command(dir.path(), "WRITE").await).await,
            "DENIED"
        );
        authority.kill().await.unwrap();
        authority.wait().await.unwrap();
        assert!(
            tokio::net::UnixStream::connect(dir.path().join("authority.sock"))
                .await
                .is_err()
        );
        fs::remove_file(dir.path().join("authority.sock")).unwrap();
        let mut restarted = start_continuity_authority(dir.path()).await;
        assert_eq!(
            authority_line(&mut authority_command(dir.path(), "WRITE").await).await,
            "DENIED"
        );
        assert_eq!(
            authority_line(&mut authority_command(dir.path(), "OPEN").await).await,
            "DENIED"
        );
        assert_eq!(read_watermark(&repository).unwrap().txid, 1);
        let mut reconcile = authority_command(dir.path(), "CAPTURE").await;
        assert_eq!(authority_line(&mut reconcile).await, "VERIFIED");
        assert_eq!(
            authority_line(&mut authority_command(dir.path(), "WRITE").await).await,
            "DENIED"
        );
        reconcile.get_mut().write_all(b"GO\n").await.unwrap();
        assert_eq!(authority_line(&mut reconcile).await, "CAPTURED");
        assert_eq!(read_watermark(&repository).unwrap().txid, 2);
        assert_eq!(fs::read(delta_path).unwrap(), delta);
        assert_eq!(
            authority_line(&mut authority_command(dir.path(), "WRITE").await).await,
            "DENIED"
        );
        let restored = dir.path().join("restored-after-crash.db");
        sqlite_replication_core::restore_to_target(
            &repository,
            &restored,
            sqlite_replication_core::RestoreTarget::Txid(2),
        )
        .unwrap();
        let recovered = rusqlite::Connection::open(restored).unwrap();
        let count: i64 = recovered
            .query_row("SELECT COUNT(*) FROM records", [], |row| row.get(0))
            .unwrap();
        let integrity: String = recovered
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, source_count);
        assert_eq!(integrity, "ok");
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM records", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            source_count
        );
        assert_eq!(
            authority_line(&mut authority_command(dir.path(), "OPEN").await).await,
            "OPENED"
        );
        restarted.kill().await.unwrap();
        timeout(Duration::from_secs(5), restarted.wait())
            .await
            .unwrap()
            .unwrap();
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    async fn barrier_marker(
        stderr: &mut tokio::io::BufReader<tokio::process::ChildStderr>,
        marker: &str,
    ) {
        use tokio::{
            io::AsyncBufReadExt,
            time::{Duration, timeout},
        };
        let mut line = String::new();
        timeout(Duration::from_secs(5), stderr.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(line.trim(), format!("MARK {marker}"));
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    fn barrier_fixture() -> (tempfile::TempDir, rusqlite::Connection) {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.db");
        let db = rusqlite::Connection::open(&source).unwrap();
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; CREATE TABLE records(id INTEGER PRIMARY KEY, value TEXT NOT NULL); INSERT INTO records VALUES(1, 'base'); PRAGMA wal_checkpoint(TRUNCATE);").unwrap();
        sqlite_replication_core::snapshot(&source, dir.path().join("repository")).unwrap();
        recover(&dir.path().join("repository"), &[]).unwrap();
        fs::write(dir.path().join("writers.lock"), []).unwrap();
        let witness = image_state(&db, &dir.path().join("witness.db"));
        fs::write(dir.path().join("witness.txt"), format!("{witness:?}")).unwrap();
        (dir, db)
    }

    #[tokio::test]
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    async fn barrier_blocks_cooperative_writer_through_verification_and_capture() {
        use tokio::{
            io::AsyncWriteExt,
            sync::oneshot,
            time::{Duration, timeout},
        };
        let (dir, db) = barrier_fixture();
        let (mut child, mut stderr) = barrier_worker(dir.path()).await;
        barrier_marker(&mut stderr, "verified").await;
        let path = dir.path().to_path_buf();
        let (blocked_tx, blocked_rx) = oneshot::channel();
        let (done_tx, done_rx) = oneshot::channel();
        let writer = std::thread::spawn(move || {
            let lock = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(path.join("writers.lock"))
                .unwrap();
            assert!(lock.try_lock_shared().is_err());
            blocked_tx.send(()).unwrap();
            lock.lock_shared().unwrap();
            let writer_db = rusqlite::Connection::open(path.join("source.db")).unwrap();
            writer_db
                .execute("INSERT INTO records VALUES(2, 'after-capture')", [])
                .unwrap();
            drop(writer_db);
            drop(lock);
            done_tx.send(()).unwrap();
        });
        timeout(Duration::from_secs(5), blocked_rx)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM records", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(b"capture\n")
            .await
            .unwrap();
        barrier_marker(&mut stderr, "captured").await;
        assert!(
            timeout(Duration::from_secs(5), child.wait())
                .await
                .unwrap()
                .unwrap()
                .success()
        );
        timeout(Duration::from_secs(5), done_rx)
            .await
            .unwrap()
            .unwrap();
        writer.join().unwrap();
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM records", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            2
        );
        let restored = dir.path().join("restored.db");
        sqlite_replication_core::restore_to_target(
            dir.path().join("repository"),
            &restored,
            sqlite_replication_core::RestoreTarget::Txid(1),
        )
        .unwrap();
        let result = rusqlite::Connection::open(restored).unwrap();
        assert_eq!(
            result
                .query_row("SELECT COUNT(*) FROM records", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }

    #[tokio::test]
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    async fn barrier_process_death_releases_lock_and_restart_rejects_stale_witness() {
        use tokio::time::{Duration, timeout};
        let (dir, db) = barrier_fixture();
        let watermark = read_watermark(&dir.path().join("repository")).unwrap();
        let (mut child, mut stderr) = barrier_worker(dir.path()).await;
        barrier_marker(&mut stderr, "verified").await;
        child.kill().await.unwrap();
        child.wait().await.unwrap();
        // This intentionally demonstrates that process death releases the lock:
        // the fixture has no fail-closed authority while it is down.
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(dir.path().join("writers.lock"))
            .unwrap();
        lock.try_lock_shared().unwrap();
        db.execute("INSERT INTO records VALUES(2, 'after-crash')", [])
            .unwrap();
        drop(lock);
        let (mut restarted, mut stderr) = barrier_worker(dir.path()).await;
        barrier_marker(&mut stderr, "mismatch").await;
        assert!(
            timeout(Duration::from_secs(5), restarted.wait())
                .await
                .unwrap()
                .unwrap()
                .success()
        );
        assert_eq!(
            read_watermark(&dir.path().join("repository")).unwrap(),
            watermark
        );
        assert!(!ltx_path(&dir.path().join("repository"), 2, 2).exists());
    }

    #[tokio::test]
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    async fn barrier_capture_failure_releases_lock_and_can_retry_after_repair() {
        use tokio::{
            io::AsyncWriteExt,
            time::{Duration, timeout},
        };
        let (dir, _db) = barrier_fixture();
        let repository = dir.path().join("repository");
        let watermark = read_watermark(&repository).unwrap();
        let snapshot = fs::read(ltx_path(&repository, 1, 1)).unwrap();
        let staging = repository.join(".staging");
        let saved = repository.join(".staging-saved");
        fs::rename(&staging, &saved).unwrap();
        fs::write(&staging, b"synthetic directory failure").unwrap();
        let (mut child, mut stderr) = barrier_worker(dir.path()).await;
        barrier_marker(&mut stderr, "verified").await;
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(b"capture\n")
            .await
            .unwrap();
        assert!(
            !timeout(Duration::from_secs(5), child.wait())
                .await
                .unwrap()
                .unwrap()
                .success()
        );
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(dir.path().join("writers.lock"))
            .unwrap();
        lock.try_lock_shared().unwrap();
        drop(lock);
        assert_eq!(read_watermark(&repository).unwrap(), watermark);
        assert_eq!(fs::read(ltx_path(&repository, 1, 1)).unwrap(), snapshot);
        fs::remove_file(&staging).unwrap();
        fs::rename(&saved, &staging).unwrap();
        let (mut retry, mut stderr) = barrier_worker(dir.path()).await;
        barrier_marker(&mut stderr, "verified").await;
        retry
            .stdin
            .as_mut()
            .unwrap()
            .write_all(b"capture\n")
            .await
            .unwrap();
        barrier_marker(&mut stderr, "captured").await;
        assert!(
            timeout(Duration::from_secs(5), retry.wait())
                .await
                .unwrap()
                .unwrap()
                .success()
        );
        assert_eq!(read_watermark(&repository).unwrap(), watermark);
    }

    #[test]
    fn real_sqlite_wal_reconciles_published_delta_and_restores_rows() {
        use rusqlite::Connection;
        use sqlite_replication_core::{RestoreTarget, restore_to_target, snapshot};
        use sqlite_replication_wal::translate::translate_wal;

        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.db");
        let repository = dir.path().join("repository");
        let connection = Connection::open(&source).unwrap();
        connection.execute_batch("PRAGMA page_size=512; PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; CREATE TABLE records(id INTEGER PRIMARY KEY, value TEXT NOT NULL); INSERT INTO records VALUES(1, 'base'); PRAGMA wal_checkpoint(TRUNCATE);").unwrap();
        snapshot(&source, &repository).unwrap();
        let base = recover(&repository, &[]).unwrap();
        assert_eq!(base.database.txid, 1);
        connection
            .execute("INSERT INTO records VALUES (2, 'synthetic')", [])
            .unwrap();
        let wal = fs::read(dir.path().join("source.db-wal")).unwrap();
        assert!(wal.len() > 32);
        let (transactions, expected) = translate_wal(&wal, &base, 200).unwrap();
        assert_eq!(transactions.len(), 1);
        assert_eq!(expected.database.txid, 2);
        let baseline: Vec<(i64, String)> = connection
            .prepare("SELECT id, value FROM records ORDER BY id")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert!(
            publish_transactions(&repository, &transactions, &expected, |point, _| {
                if point == FaultPoint::AfterLtxPublish {
                    return Err(io::Error::other("synthetic interruption after LTX publish"));
                }
                Ok(())
            })
            .is_err()
        );
        let before = read_watermark(&repository).unwrap();
        assert_eq!(before.txid, 1);
        let mut corrupt_wal = wal.clone();
        corrupt_wal[0] ^= 0xff;
        assert!(recover(&repository, &corrupt_wal).is_err());
        assert_eq!(read_watermark(&repository).unwrap(), before);
        let recovered = recover(&repository, &wal).unwrap();
        assert_eq!(recovered.database.txid, 2);
        assert_eq!(recovered.database.checksum, expected.database.checksum);
        assert!(recovered.wal.initialized);
        assert_eq!(recovered.wal.frame_count, expected.wal.frame_count);
        assert_eq!(
            recover(&repository, &wal).unwrap().database,
            recovered.database
        );
        let restored = dir.path().join("restored.db");
        restore_to_target(&repository, &restored, RestoreTarget::Txid(2)).unwrap();
        let restored_db = Connection::open(&restored).unwrap();
        let integrity: String = restored_db
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .unwrap();
        assert_eq!(integrity, "ok");
        let restored_rows: Vec<(i64, String)> = restored_db
            .prepare("SELECT id, value FROM records ORDER BY id")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(restored_rows, baseline);
        let source_rows: Vec<(i64, String)> = connection
            .prepare("SELECT id, value FROM records ORDER BY id")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(source_rows, baseline);
    }

    #[test]
    fn published_ltx_before_watermark_is_verified_and_reconciled_idempotently() {
        let dir = tempfile::tempdir().unwrap();
        let (image, transaction, state) = fixture(dir.path());
        let source = dir.path().join("source-image");
        fs::write(&source, &image).unwrap();
        let result = publish_transactions(dir.path(), &[transaction], &state, |point, _| {
            if point == FaultPoint::AfterLtxPublish {
                return Err(io::Error::other("synthetic interruption"));
            }
            Ok(())
        });
        assert!(result.is_err());
        assert_eq!(read_watermark(dir.path()).unwrap().txid, 1);
        assert!(ltx_path(dir.path(), 2, 2).exists());
        let recovered = recover(dir.path(), &[]).unwrap();
        assert_eq!(recovered.database.txid, 2);
        assert_eq!(recovered.database.checksum, state.database.checksum);
        assert_eq!(read_watermark(dir.path()).unwrap().txid, 2);
        assert_eq!(
            recover(dir.path(), &[]).unwrap().database,
            recovered.database
        );
        assert_eq!(fs::read(source).unwrap(), image);
    }

    #[test]
    fn truncated_published_ltx_is_rejected_without_advancing_watermark() {
        let dir = tempfile::tempdir().unwrap();
        let (image, transaction, state) = fixture(dir.path());
        let source = dir.path().join("source-image");
        fs::write(&source, &image).unwrap();
        assert!(
            publish_transactions(dir.path(), &[transaction], &state, |point, _| {
                if point == FaultPoint::AfterLtxPublish {
                    return Err(io::Error::other("synthetic interruption"));
                }
                Ok(())
            })
            .is_err()
        );
        let delta = ltx_path(dir.path(), 2, 2);
        let bytes = fs::read(&delta).unwrap();
        fs::write(&delta, &bytes[..bytes.len() / 2]).unwrap();
        let before = read_watermark(dir.path()).unwrap();
        assert_eq!(before.txid, 1);
        assert!(recover(dir.path(), &[]).is_err());
        assert_eq!(read_watermark(dir.path()).unwrap(), before);
        assert_eq!(fs::read(source).unwrap(), image);
    }
}
