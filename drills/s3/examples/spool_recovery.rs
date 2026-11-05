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
    fn checkpoint_generation_drill(new_generation: bool) {
        use rusqlite::Connection;
        use sqlite_replication_core::{RestoreTarget, capture_once, restore_to_target, snapshot};

        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.db");
        let repository = dir.path().join("repository");
        let wal_path = dir.path().join("source.db-wal");
        let db = Connection::open(&source).unwrap();
        db.execute_batch("PRAGMA page_size=512; PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; CREATE TABLE records(id INTEGER PRIMARY KEY, value TEXT NOT NULL); INSERT INTO records VALUES(1, 'base'); PRAGMA wal_checkpoint(TRUNCATE);").unwrap();
        snapshot(&source, &repository).unwrap();
        db.execute("INSERT INTO records VALUES(2, 'checkpointed')", [])
            .unwrap();
        let old_wal = fs::read(&wal_path).unwrap();
        assert!(old_wal.len() > 32);
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
        assert!(result.txid >= 2);
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
        checkpoint_generation_drill(false);
    }

    #[test]
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    fn capture_reconciles_checkpointed_changes_with_new_wal_generation() {
        checkpoint_generation_drill(true);
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
