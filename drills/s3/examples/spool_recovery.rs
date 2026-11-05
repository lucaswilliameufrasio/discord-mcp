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
