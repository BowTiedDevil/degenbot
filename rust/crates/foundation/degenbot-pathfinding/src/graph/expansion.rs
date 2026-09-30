//! One pending yield's lazy pool-assignment expansion.
//!
//! `BundledSearch` parks a [`WalkExpansion`] at the end token and emits one
//! concrete pool assignment per advance: a walk whose bundle has `m`
//! candidate pools does not multiply the DFS subtree — the assignment
//! space is enumerated at yield time, in deterministic lexicographic pool
//! order.

/// One pending yield's expansion state: the walk's per-step candidate pool
/// lists (kind-filtered), an odometer over those lists, and same-bundle
/// injectivity validation. Each valid odometer state is one concrete pool
/// assignment of the walk — the bundle-level equivalent of eager per-pool
/// DFS branching.
pub(crate) struct WalkExpansion {
    /// Step `i` of the walk used `slot_bundle[i]`'s bundle: assignments must
    /// choose distinct pools across all slots of one bundle (the trail's
    /// pools are distinct; different bundles share no pool by construction).
    slot_bundle: Vec<u32>,
    /// Per step: candidate pool indices, limited to pools whose kind passes
    /// the depth's filter (full bundle list when unfiltered).
    slots: Vec<Vec<u32>>,
    /// Odometer position per slot.
    cursor: Vec<usize>,
    /// The pool currently assigned to slot `i` (`slots[i][cursor[i]]`).
    chosen: Vec<u32>,
    /// Gate on the first odometer state: `false` until the initial
    /// assignment is installed.
    started: bool,
    /// Gate on the last odometer state: set once the odometer has advanced
    /// past the final combination.
    finished: bool,
}

impl WalkExpansion {
    /// Build the expansion for a walk parked at `end`: the walk's bundle
    /// ids (same-bundle injectivity) and its per-step candidate pool lists
    /// (kind-filtered). The odometer starts at the first assignment,
    /// installed on the first [`Self::next_assignment_into`] call.
    pub(crate) fn new(slot_bundle: Vec<u32>, slots: Vec<Vec<u32>>) -> Self {
        let n = slots.len();
        Self {
            slot_bundle,
            slots,
            cursor: vec![0; n],
            chosen: vec![0; n],
            started: false,
            finished: false,
        }
    }

    /// Advance to the next concrete assignment (mixed-radix over the slot
    /// candidate lists, skimming invalid same-bundle duplicates). On success
    /// the step-ordered pool list is written into `out` (reusing its
    /// capacity — the hot path allocates nothing per assignment).
    pub(crate) fn next_assignment_into(&mut self, out: &mut Vec<u32>) -> bool {
        let n = self.slots.len();
        if n == 0 {
            // An empty walk (min_depth == 0) has exactly one assignment.
            if self.started {
                return false;
            }
            self.started = true;
            out.clear();
            return true;
        }
        loop {
            if self.finished {
                return false;
            }
            if self.started {
                // Increment the odometer from the last slot, carrying left;
                // on a carry, higher slots reset to their first candidate.
                let mut j = n;
                loop {
                    if j == 0 {
                        self.finished = true;
                        return false;
                    }
                    j -= 1;
                    self.cursor[j] += 1;
                    if self.cursor[j] < self.slots[j].len() {
                        self.chosen[j] = self.slots[j][self.cursor[j]];
                        for k in j + 1..n {
                            self.cursor[k] = 0;
                            self.chosen[k] = self.slots[k][0];
                        }
                        break;
                    }
                }
            } else {
                self.started = true;
                for i in 0..n {
                    self.cursor[i] = 0;
                    self.chosen[i] = self.slots[i][0];
                }
            }
            if self.same_bundle_distinct() {
                out.clear();
                out.extend_from_slice(&self.chosen);
                return true;
            }
            // Invalid (two slots of one bundle chose the same pool): keep
            // advancing. The walk-level kind check only proved a necessary
            // condition; this is where exact feasibility is decided.
        }
    }

    fn same_bundle_distinct(&self) -> bool {
        for i in 0..self.slots.len() {
            for j in i + 1..self.slots.len() {
                if self.slot_bundle[i] == self.slot_bundle[j] && self.chosen[i] == self.chosen[j] {
                    return false;
                }
            }
        }
        true
    }
}
