//! M1/M2 integration: buffer pool + WAL + MVCC crash semantics together.

use qmind_kernel::wal::{WalReader, WalRecord, WalWriter};
use qmind_kernel::{BufferPool, MvccStore, PageHeader};
use std::io::Cursor;

#[test]
fn flushed_store_survives_pool_replacement() {
    let mut pool = BufferPool::in_memory(2);
    for id in 1..=5u64 {
        let f = pool.create_page(id).unwrap();
        pool.payload_mut(f)[0..8].copy_from_slice(&id.to_le_bytes());
        pool.unpin(f, true);
    }
    pool.flush_all().unwrap();

    // Fresh pool over the same durable store (simulated restart).
    let old = std::mem::replace(&mut pool, BufferPool::in_memory(1));
    let mut fresh: BufferPool<_> = BufferPool::new(old.store_owned(), 4);
    for id in 1..=5u64 {
        let f = fresh.pin(id).unwrap();
        assert_eq!(&fresh.payload(f)[0..8], &id.to_le_bytes());
        let hdr = PageHeader::decode(fresh.bytes(f)).unwrap();
        assert_eq!(hdr.page_id, id);
        fresh.unpin(f, false);
    }
}

#[test]
fn replay_recovers_exactly_the_committed_row_set() {
    // txn 1 commits keys 100..105; txn 2 aborts key 200; txn 3 never finishes.
    let mut sink = Vec::new();
    {
        let mut w = WalWriter::new(&mut sink);
        w.append(&WalRecord::Begin { txn: 1 });
        for row in 100..105u64 {
            w.append(&WalRecord::Put {
                txn: 1,
                key: format!("row{row}").into_bytes(),
                value: row.to_le_bytes().to_vec(),
            });
        }
        w.append(&WalRecord::Commit { txn: 1 });

        w.append(&WalRecord::Begin { txn: 2 });
        w.append(&WalRecord::Put {
            txn: 2,
            key: b"aborted".to_vec(),
            value: vec![200],
        });
        w.append(&WalRecord::Abort { txn: 2 });
        w.commit_group().unwrap();

        w.append(&WalRecord::Begin { txn: 3 }); // in-flight at crash
        drop(w);
    }

    let replay = WalReader::replay(Cursor::new(sink)).unwrap();
    assert!(!replay.torn_tail);

    // Recovery model: buffer each txn's writes; only Commit publishes them.
    let mut committed = std::collections::HashMap::new();
    let mut open: std::collections::HashMap<u64, Vec<(Vec<u8>, Vec<u8>)>> =
        std::collections::HashMap::new();
    for (_, rec) in &replay.records {
        match rec {
            WalRecord::Begin { txn } => {
                open.insert(*txn, Vec::new());
            }
            WalRecord::Put { txn, key, value } => {
                if let Some(rows) = open.get_mut(txn) {
                    rows.push((key.clone(), value.clone()));
                }
            }
            WalRecord::Commit { txn } => {
                if let Some(rows) = open.remove(txn) {
                    committed.extend(rows);
                }
            }
            WalRecord::Abort { txn } => {
                open.remove(txn); // writes discarded
            }
        }
    }

    assert_eq!(committed.len(), 5);
    for row in 100..105u64 {
        assert_eq!(
            committed.get(format!("row{row}").as_bytes()),
            Some(&row.to_le_bytes().to_vec())
        );
    }
    assert!(!committed.contains_key(b"aborted".as_slice()));
}

#[test]
fn mvcc_commit_rides_wal_and_recovery_rebuilds_state() {
    // Live engine run: MVCC store logging through a WAL writer.
    let mut sink = Vec::new();
    let mut db = MvccStore::new();
    {
        let mut wal = WalWriter::new(&mut sink);
        for i in 0..50u64 {
            let (t, _) = db.begin();
            db.set(
                t,
                format!("key{i}").as_bytes(),
                (i * 1000).to_le_bytes().to_vec(),
            );
            let logged = db
                .commit::<()>(t, |recs| {
                    for r in recs {
                        wal.append(r);
                    }
                    wal.commit_group().map(|_| ()).map_err(|_| ())
                })
                .unwrap();
            assert!(logged.is_ok());
        }
        // Crash with an uncommitted group buffered.
        let (t, _) = db.begin();
        db.set(t, b"lost", vec![9, 9, 9]);
        drop(wal); // "lost" never reached the sink
        db.abort(t);
    }

    // Restart: rebuild state purely from the log.
    let replay = WalReader::replay(Cursor::new(sink)).unwrap();
    assert!(!replay.torn_tail);

    let mut recovered = MvccStore::new();
    let mut open: std::collections::HashMap<u64, Vec<(Vec<u8>, Vec<u8>)>> =
        std::collections::HashMap::new();
    for (_, rec) in &replay.records {
        match rec {
            WalRecord::Begin { txn } => {
                open.insert(*txn, Vec::new());
            }
            WalRecord::Put { txn, key, value } => {
                if let Some(rows) = open.get_mut(txn) {
                    rows.push((key.clone(), value.clone()));
                }
            }
            WalRecord::Commit { txn } => {
                if let Some(rows) = open.remove(txn) {
                    let (t, _) = recovered.begin();
                    for (k, v) in rows {
                        recovered.set(t, &k, v);
                    }
                    recovered.commit::<()>(t, |_| Ok(())).unwrap().unwrap();
                }
            }
            WalRecord::Abort { txn } => {
                open.remove(txn);
            }
        }
    }

    let (check, csnap) = recovered.begin();
    for i in 0..50u64 {
        assert_eq!(
            recovered.get(check, format!("key{i}").as_bytes(), &csnap),
            Some((i * 1000).to_le_bytes().to_vec())
        );
    }
    assert_eq!(recovered.get(check, b"lost", &csnap), None);
}
