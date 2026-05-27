// SPDX-License-Identifier: Apache-2.0
//
// AGENT'S PLAYGROUND. This is the file you (the agent) modify.
//
// **STARTING POINT.** Exact clone of `reference.rs`'s linear block-scan
// from Lance's `wand.rs::next` (SHA 5cf70b27, line 349). The agent's
// job is to make `next` faster while producing bitwise-identical
// `Option<u32>` output on every seek call.
//
// **PRIMARY OPPORTUNITY:** The linear `while block_idx + 1 < num_blocks
// && block_first_doc_id[block_idx + 1] <= least_id { block_idx += 1; }`
// scan is O(N) in skip distance. Replace with exponential search +
// bisect over the same sidecar: cost drops to O(log N). On the
// Skip-deep pattern at 80k blocks, that's ~600x theoretical.
//
// PUBLIC API CONTRACT (must remain stable):
//   - `pub struct PostingSeek<'a>`
//   - `PostingSeek::new(list: &'a PostingList) -> Self`
//   - `PostingSeek::reset(&mut self)`
//   - `PostingSeek::next(&mut self, least_id: u32) -> Option<u32>`
//
// What you CAN do:
//   - Add scratch fields (e.g., a copy of `block_first_doc_id` as a
//     `&'a [u32]` to skip the Vec indirection).
//   - Change the algorithm of `next` freely (gallop, branchless bisect,
//     SIMD compare over sidecar words).
//   - Add `#[cfg(target_arch = ...)]`-gated NEON / AVX2 paths with a
//     portable scalar fallback.
//
// What you CANNOT do:
//   - Change the public API above.
//   - Modify `lib.rs` / `reference.rs` / `inputs.rs` / `run_experiment.rs`.
//   - Return different `Option<u32>` than reference on any seek call.

use crate::{BLOCK_SIZE, PostingList};

pub struct PostingSeek<'a> {
    list: &'a PostingList,
    index: usize,
}

impl<'a> PostingSeek<'a> {
    pub fn new(list: &'a PostingList) -> Self {
        Self { list, index: 0 }
    }

    pub fn reset(&mut self) {
        self.index = 0;
    }

    pub fn next(&mut self, least_id: u32) -> Option<u32> {
        let num_blocks = self.list.num_blocks();
        let length = self.list.num_docs();
        if self.index >= length {
            return None;
        }

        // Phase 1: hybrid linear-budget + McIlroy gallop sidecar scan.
        //
        // First LINEAR_BUDGET=4 linear steps keep the cheap-skip cost
        // identical to the baseline scan (1-block skip = 2 sidecar
        // reads; 4-block skip = 5 reads). If the linear budget exhausts
        // AND the next block still qualifies, switch to McIlroy's
        // always-committing exponential search + halving refine for the
        // O(log N) asymptotic win on deep skips.
        const LINEAR_BUDGET: usize = 4;
        let mut block_idx = self.index / BLOCK_SIZE;
        let sidecar = &self.list.block_first_doc_id;

        let mut budget = LINEAR_BUDGET;
        while budget > 0
            && block_idx + 1 < num_blocks
            && sidecar[block_idx + 1] <= least_id
        {
            block_idx += 1;
            budget -= 1;
        }
        if budget == 0 && block_idx + 1 < num_blocks && sidecar[block_idx + 1] <= least_id {
            let mut step = 1usize;
            while block_idx + step < num_blocks && sidecar[block_idx + step] <= least_id {
                block_idx += step;
                step *= 2;
            }
            while step > 1 {
                step /= 2;
                if block_idx + step < num_blocks && sidecar[block_idx + step] <= least_id {
                    block_idx += step;
                }
            }
        }
        self.index = self.index.max(block_idx * BLOCK_SIZE);

        // Phase 2: partition_point inside the candidate block; advance
        // if the bound runs off the end.
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
