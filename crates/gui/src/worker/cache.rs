//! Bounded LRU cache of loaded timeline blocks (`<= MAX_BLOCKS * BLOCK_SIZE` rows).

use std::collections::{HashMap, VecDeque};

use crate::vm::RowVm;
use crate::worker::Generation;

pub const BLOCK_SIZE: usize = fp_query::PAGE_SIZE;
pub const MAX_BLOCKS: usize = 24;

pub type BlockKey = (u64, bool, usize);

#[derive(Debug, Default)]
pub struct BlockCache {
    blocks: HashMap<BlockKey, Vec<RowVm>>,
    order: VecDeque<BlockKey>,
    pending: HashMap<BlockKey, Generation>,
    errors: HashMap<BlockKey, (Generation, String)>,
}

impl BlockCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn row(&mut self, scope_hash: u64, reverse: bool, row_idx: usize) -> Option<&RowVm> {
        let key = (scope_hash, reverse, row_idx / BLOCK_SIZE);
        if self.blocks.contains_key(&key) {
            self.touch(key);
        }
        self.blocks
            .get(&key)
            .and_then(|rows| rows.get(row_idx % BLOCK_SIZE))
    }

    pub fn has_block(&self, key: BlockKey) -> bool {
        self.blocks.contains_key(&key)
    }

    pub fn error_for(&self, key: BlockKey, generation: Generation) -> Option<&str> {
        self.errors
            .get(&key)
            .filter(|(g, _)| *g == generation)
            .map(|(_, msg)| msg.as_str())
    }

    /// Cursor `(ts_utc, id)` at the end of the preceding block for keyset scrolling.
    pub fn prev_block_cursor(
        &self,
        scope_hash: u64,
        reverse: bool,
        block_idx: usize,
    ) -> Option<(i64, u64)> {
        let prev_idx = block_idx.checked_sub(1)?;
        let prev = self.blocks.get(&(scope_hash, reverse, prev_idx))?;
        let last = prev.last()?;
        Some((last.ts_utc, last.id))
    }

    pub fn mark_pending(&mut self, key: BlockKey, generation: Generation) -> bool {
        if self.blocks.contains_key(&key) {
            return false;
        }
        if self.pending.get(&key) == Some(&generation) {
            return false;
        }
        if self.errors.get(&key).is_some_and(|(g, _)| *g == generation) {
            return false;
        }
        self.pending.insert(key, generation);
        true
    }

    pub fn insert(&mut self, key: BlockKey, rows: Vec<RowVm>) {
        self.pending.remove(&key);
        self.errors.remove(&key);
        self.blocks.insert(key, rows);
        self.touch(key);
        while self.order.len() > MAX_BLOCKS {
            if let Some(oldest) = self.order.pop_front() {
                self.blocks.remove(&oldest);
            }
        }
    }

    pub fn record_error(&mut self, key: BlockKey, generation: Generation, msg: String) {
        self.pending.remove(&key);
        self.errors.insert(key, (generation, msg));
    }

    /// Drops pending entries from older generations so fresh scopes can request cleanly.
    pub fn prune_stale_pending(&mut self, current_generation: Generation) {
        self.pending.retain(|_, g| *g == current_generation);
        self.errors.retain(|_, (g, _)| *g == current_generation);
    }

    pub fn clear(&mut self) {
        self.blocks.clear();
        self.order.clear();
        self.pending.clear();
        self.errors.clear();
    }

    /// Encodes a block key into the worker's `sub` string.
    pub fn encode_sub(key: BlockKey) -> String {
        format!("{}:{}:{}", key.0, u8::from(key.1), key.2)
    }

    pub fn decode_sub(sub: &str) -> Option<BlockKey> {
        let mut parts = sub.split(':');
        let scope_hash = parts.next()?.parse::<u64>().ok()?;
        let reverse = match parts.next()? {
            "0" => false,
            "1" => true,
            _ => return None,
        };
        let block_idx = parts.next()?.parse::<usize>().ok()?;
        if parts.next().is_some() {
            return None;
        }
        Some((scope_hash, reverse, block_idx))
    }

    fn touch(&mut self, key: BlockKey) {
        if self.order.back() == Some(&key) {
            return;
        }
        if let Some(pos) = self.order.iter().position(|k| *k == key) {
            self.order.remove(pos);
        }
        self.order.push_back(key);
    }

    #[cfg(test)]
    fn len_blocks(&self) -> usize {
        self.blocks.len()
    }

    #[cfg(test)]
    fn total_rows(&self) -> usize {
        debug_assert_eq!(self.order.len(), self.blocks.len());
        self.blocks.values().map(Vec::len).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fp_query::TimelineRow;

    fn dummy_row(id: u64, ts_utc: i64) -> RowVm {
        RowVm::from_row(&TimelineRow {
            id,
            run_id: 1,
            ts_utc,
            ts_precision: "exact".into(),
            kind: "shell.command".into(),
            source_kind: "bash".into(),
            app: None,
            profile: None,
            subject: Some(format!("cmd {id}")),
            subject_host: None,
            summary: None,
            confidence: "direct".into(),
            ioc_hits: Vec::new(),
        })
    }

    #[test]
    fn evicts_oldest_blocks_past_cap() {
        let mut cache = BlockCache::new();
        for b in 0..(MAX_BLOCKS + 4) {
            cache.insert((1, false, b), vec![dummy_row(b as u64, b as i64)]);
        }
        assert_eq!(cache.len_blocks(), MAX_BLOCKS);
        assert_eq!(cache.total_rows(), MAX_BLOCKS);
        assert!(!cache.has_block((1, false, 0)));
        assert!(cache.has_block((1, false, MAX_BLOCKS + 3)));
    }

    #[test]
    fn accessing_older_block_refreshes_its_lru_position() {
        let mut cache = BlockCache::new();
        for b in 0..MAX_BLOCKS {
            cache.insert((1, false, b), vec![dummy_row(b as u64, b as i64)]);
        }
        assert!(cache.row(1, false, 0).is_some());
        cache.insert((1, false, MAX_BLOCKS), vec![dummy_row(99, 99)]);
        assert!(cache.has_block((1, false, 0)));
        assert!(!cache.has_block((1, false, 1)));
    }

    #[test]
    fn prev_block_cursor_and_sub_round_trip() {
        let mut cache = BlockCache::new();
        cache.insert((42, true, 0), vec![dummy_row(10, 100), dummy_row(11, 90)]);
        assert_eq!(cache.prev_block_cursor(42, true, 1), Some((90, 11)));
        let key = (42, true, 7);
        let encoded = BlockCache::encode_sub(key);
        assert_eq!(BlockCache::decode_sub(&encoded), Some(key));
        cache.clear();
        assert_eq!(cache.len_blocks(), 0);
    }
}
