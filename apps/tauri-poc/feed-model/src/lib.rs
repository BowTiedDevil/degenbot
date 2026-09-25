use std::collections::{HashMap, VecDeque};

use serde::Serialize;

const MAX_TRACKED_BLOCKS: usize = 256;

#[derive(Clone, Debug, Serialize)]
pub struct BlockSnapshot {
    pub block_number: u64,
    pub block_timestamp: u64,
    pub transaction_count: Option<usize>,
    pub log_count: usize,
    pub base_fee_wei: Option<u64>,
    pub next_base_fee_wei: Option<u64>,
    pub gas_used: u64,
    pub gas_limit: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct LogSnapshot {
    pub block_number: Option<u64>,
    pub log_index: Option<u64>,
    pub address: String,
    pub topic0: Option<String>,
    pub removed: bool,
}

#[derive(Debug)]
struct BlockStats {
    snapshot: BlockSnapshot,
}

#[derive(Default, Debug)]
pub struct BlockFeedModel {
    blocks: HashMap<u64, BlockStats>,
    pending_logs: HashMap<u64, usize>,
    order: VecDeque<u64>,
}

impl BlockFeedModel {
    pub fn on_header(
        &mut self,
        block_number: u64,
        block_timestamp: u64,
        transaction_count: Option<usize>,
        base_fee_wei: Option<u64>,
        next_base_fee_wei: Option<u64>,
        gas_used: u64,
        gas_limit: u64,
    ) -> BlockSnapshot {
        let log_count = self.blocks.get(&block_number).map_or_else(
            || self.pending_logs.remove(&block_number).unwrap_or(0),
            |stats| stats.snapshot.log_count,
        );
        let snapshot = BlockSnapshot {
            block_number,
            block_timestamp,
            transaction_count,
            log_count,
            base_fee_wei,
            next_base_fee_wei,
            gas_used,
            gas_limit,
        };
        if self
            .blocks
            .insert(
                block_number,
                BlockStats {
                    snapshot: snapshot.clone(),
                },
            )
            .is_none()
        {
            self.order.push_back(block_number);
            while self.order.len() > MAX_TRACKED_BLOCKS {
                if let Some(expired) = self.order.pop_front() {
                    self.blocks.remove(&expired);
                }
            }
        }
        snapshot
    }

    pub fn on_log(&mut self, log: &LogSnapshot) -> Option<BlockSnapshot> {
        let block_number = log.block_number?;
        let Some(stats) = self.blocks.get_mut(&block_number) else {
            *self.pending_logs.entry(block_number).or_default() += 1;
            return None;
        };
        stats.snapshot.log_count += 1;
        Some(stats.snapshot.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn log(block_number: u64) -> LogSnapshot {
        LogSnapshot {
            block_number: Some(block_number),
            log_index: Some(0),
            address: "0x0000000000000000000000000000000000000001".to_owned(),
            topic0: None,
            removed: false,
        }
    }

    #[test]
    fn model_preserves_logs_that_arrive_before_header() {
        let mut model = BlockFeedModel::default();
        assert!(model.on_log(&log(42)).is_none());
        let block = model.on_header(
            42,
            123,
            Some(3),
            Some(1_000_000_000),
            Some(900_000_000),
            15_000_000,
            30_000_000,
        );

        assert_eq!(block.log_count, 1);
        assert_eq!(block.transaction_count, Some(3));
    }

    #[test]
    fn model_updates_a_header_after_each_log() {
        let mut model = BlockFeedModel::default();
        let block = model.on_header(
            7,
            123,
            Some(2),
            Some(1_000_000_000),
            Some(900_000_000),
            15_000_000,
            30_000_000,
        );
        assert_eq!(block.log_count, 0);

        let updated = model.on_log(&log(7));
        assert_eq!(updated.map(|block| block.log_count), Some(1));
    }
}
