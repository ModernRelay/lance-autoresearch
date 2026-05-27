// SPDX-License-Identifier: Apache-2.0

//! IMMUTABLE. Reference kernel, defines the seek output the agent must match.
//!
//! Verbatim port of upstream Lance's `PostingIterator::next` linear
//! block-scan from `lance-index::scalar::inverted::wand.rs` line 349
//! (SHA `5cf70b27b3ad38ecdcd1547b7af385e05f67598a`). The two-loop
//! structure (sidecar scan to pick block; partition_point within block;
//! advance to next block if the candidate within the chosen block falls
//! short) is preserved exactly.
//!
//! The starting kernel in `kernels.rs` is an exact clone of this code.
//! Any agent optimization must produce bitwise-identical `Option<u32>`
//! output on every seek call across the full ops sequence; no float
//! tolerance because output is integer.

use crate::{BLOCK_SIZE, PostingList};

pub struct PostingSeekReference<'a> {
    list: &'a PostingList,
    index: usize,
}

impl<'a> PostingSeekReference<'a> {
    pub fn new(list: &'a PostingList) -> Self {
        Self { list, index: 0 }
    }

    pub fn reset(&mut self) {
        self.index = 0;
    }

    /// Seek to the smallest doc id `>= least_id` at or after the current
    /// cursor position. Returns `Some(doc_id)` or `None` if the list is
    /// exhausted.
    ///
    /// Matches `wand.rs::next` line 349. Two phases:
    ///   1. Linear scan over the sidecar to pick the candidate block.
    ///   2. Partition-point inside the picked block; if the bound falls
    ///      off the end of this block, advance to the next block and
    ///      retry.
    pub fn next(&mut self, least_id: u32) -> Option<u32> {
        let num_blocks = self.list.num_blocks();
        let length = self.list.num_docs();
        if self.index >= length {
            return None;
        }

        // Phase 1: linear sidecar scan to pick the candidate block. This
        // is the loop the agent is meant to gallop.
        let mut block_idx = self.index / BLOCK_SIZE;
        while block_idx + 1 < num_blocks
            && self.list.block_first_doc_id[block_idx + 1] <= least_id
        {
            block_idx += 1;
        }
        self.index = self.index.max(block_idx * BLOCK_SIZE);

        // Phase 2: partition_point inside the candidate block; advance if
        // the bound runs off the end.
        loop {
            if self.index >= length {
                return None;
            }
            let block_idx = self.index / BLOCK_SIZE;
            let block_offset = self.index % BLOCK_SIZE;
            let block = self.list.block(block_idx);
            let in_block = &block[block_offset..];
            let off_in_block = in_block.partition_point(|&id| id < least_id);
            let new_offset = block_offset + off_in_block;
            if new_offset < block.len() {
                self.index = block_idx * BLOCK_SIZE + new_offset;
                return Some(block[new_offset]);
            }
            if block_idx + 1 >= num_blocks {
                self.index = length;
                return None;
            }
            self.index = (block_idx + 1) * BLOCK_SIZE;
        }
    }
}

/// Compare two seek result sequences. Returns `Some(diff)` describing
/// the first divergence on mismatch, `None` on bitwise equality.
pub fn seek_results_diff(agent: &[Option<u32>], reference: &[Option<u32>]) -> Option<String> {
    if agent.len() != reference.len() {
        return Some(format!(
            "sequence length mismatch: agent={} reference={}",
            agent.len(),
            reference.len()
        ));
    }
    for (i, (a, r)) in agent.iter().zip(reference).enumerate() {
        if a != r {
            return Some(format!("seek #{i}: agent={a:?} reference={r:?}"));
        }
    }
    None
}
