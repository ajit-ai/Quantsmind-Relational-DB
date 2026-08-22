//! M1 integration: buffer pool + WAL crash semantics working together.

use qmind_kernel::wal::{WalReader, WalRecord, WalWriter};
use qmind_kernel::{BufferPool, PageHeader};
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
    // txn 1 commits rows 100..105; txn 2 aborts row 200; txn 3 never finishes.
    let mut sink = Vec::new();
    {
        let mut w = WalWriter::new(&mut sink);
        w.append(&WalRecord::Begin { txn: 1 });
        for row in 100..105u64 {
            w.append(&WalRecord::Insert {
                txn: 1,
                page: row,
                slot: 0,
            });
        }
        w.append(&WalRecord::Commit { txn: 1 });

        w.append(&WalRecord::Begin { txn: 2 });
        w.append(&WalRecord::Insert {
            txn: 2,
            page: 200,
            slot: 0,
        });
        w.append(&WalRecord::Abort { txn: 2 });
        w.commit_group().unwrap();

        w.append(&WalRecord::Begin { txn: 3 }); // in-flight at crash
        drop(w);
    }

    let replay = WalReader::replay(Cursor::new(sink)).unwrap();
    assert!(!replay.torn_tail);

    // Recovery model: buffer each txn's rows; only Commit publishes them.
    let mut committed = std::collections::HashSet::new();
    let mut open: std::collections::HashMap<u64, Vec<u64>> = std::collections::HashMap::new();
    for (_, rec) in &replay.records {
        match *rec {
            WalRecord::Begin { txn } => {
                open.insert(txn, Vec::new());
            }
            WalRecord::Insert { txn, page, .. } => {
                if let Some(rows) = open.get_mut(&txn) {
                    rows.push(page);
                }
            }
            WalRecord::Commit { txn } => {
                if let Some(rows) = open.remove(&txn) {
                    committed.extend(rows);
                }
            }
            WalRecord::Abort { txn } => {
                open.remove(&txn); // rows discarded
            }
        }
    }

    assert_eq!(
        committed,
        [100u64, 101, 102, 103, 104].into_iter().collect()
    );
}

#[test]
fn crash_mid_group_loses_only_that_group() {
    let mut sink = Vec::new();
    let mut lsn_of_group_boundary = 0;
    {
        let mut w = WalWriter::new(&mut sink);
        for g in 0..10u64 {
            w.append(&WalRecord::Begin { txn: g + 1 });
            w.append(&WalRecord::Insert {
                txn: g + 1,
                page: g * 10,
                slot: 1,
            });
            w.append(&WalRecord::Commit { txn: g + 1 });
            lsn_of_group_boundary = w.commit_group().unwrap();
        }
        // simulate crash with one group buffered but uncommitted
        w.append(&WalRecord::Begin { txn: 99 });
        w.append(&WalRecord::Insert {
            txn: 99,
            page: 9999,
            slot: 9,
        });
        drop(w);
        assert_eq!(lsn_of_group_boundary, 30);
    }

    let replay = WalReader::replay(Cursor::new(sink)).unwrap();
    assert_eq!(
        replay.records.last().unwrap().0,
        30,
        "LSN 31-32 never reached storage"
    );
    assert!(!replay.torn_tail);

    // Recovery replays only committed transactions.
    let recovered_pages: Vec<u64> = replay
        .records
        .iter()
        .filter_map(|(_, r)| match r {
            WalRecord::Insert { page, .. } => Some(*page),
            _ => None,
        })
        .collect();
    assert_eq!(
        recovered_pages,
        (0..10u64).map(|g| g * 10).collect::<Vec<_>>()
    );
}
