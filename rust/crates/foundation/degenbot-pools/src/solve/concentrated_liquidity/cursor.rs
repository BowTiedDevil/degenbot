use std::cell::Cell;

use alloy::primitives::U256;
use hashbrown::HashSet;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum WalkTransition {
    Advance { next: Vec<usize> },
    SkippedTuple { next: Vec<usize> },
    AnchorReentry { next: Vec<usize> },
    Terminal(TerminalKind),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TerminalKind {
    UnboundedRefine,
    StopRefine { climbing: bool },
    DegenerateNoRefine,
    VisitedDuplicate,
    CapExhausted,
}

impl TerminalKind {
    pub(super) fn refine_neighbor(self) -> Option<bool> {
        match self {
            Self::UnboundedRefine | Self::StopRefine { climbing: false } => Some(false),
            Self::StopRefine { climbing: true } => Some(true),
            Self::DegenerateNoRefine | Self::VisitedDuplicate | Self::CapExhausted => None,
        }
    }
}

pub(super) struct WalkCursor {
    ks: Vec<usize>,
    visited: HashSet<Vec<usize>>,
    prev_right_edge: Cell<Option<U256>>,
    right_bracket: Cell<Option<(U256, U256)>>,
    anchor_reentry_active: bool,
}

impl WalkCursor {
    pub(super) fn new(hop_count: usize) -> Self {
        Self {
            ks: vec![0; hop_count],
            visited: HashSet::new(),
            prev_right_edge: Cell::new(None),
            right_bracket: Cell::new(None),
            anchor_reentry_active: false,
        }
    }

    pub(super) fn begin_piece(&mut self) -> Result<(), TerminalKind> {
        if self.visited.insert(self.ks.clone()) {
            Ok(())
        } else {
            Err(TerminalKind::VisitedDuplicate)
        }
    }

    pub(super) fn ks(&self) -> &[usize] {
        &self.ks
    }

    pub(super) fn prev_right_edge(&self) -> Option<U256> {
        self.prev_right_edge.get()
    }

    pub(super) fn right_bracket(&self) -> Option<(U256, U256)> {
        self.right_bracket.get()
    }

    pub(super) fn record_right_edge(&self, edge: U256, confirm_hi: U256) {
        self.prev_right_edge.set(Some(edge));
        self.right_bracket.set(Some((edge, confirm_hi)));
    }

    pub(super) fn anchor_reentry_active(&self) -> bool {
        self.anchor_reentry_active
    }

    pub(super) fn has_visited(&self, landed: &[usize]) -> bool {
        self.visited.contains(landed)
    }

    pub(super) fn apply(&mut self, transition: WalkTransition) {
        match transition {
            WalkTransition::Advance { next } => self.ks = next,
            WalkTransition::SkippedTuple { next } => {
                self.ks = next;
                self.prev_right_edge.set(None);
            }
            WalkTransition::AnchorReentry { next } => {
                self.ks = next;
                self.prev_right_edge.set(None);
                self.right_bracket.set(None);
                self.anchor_reentry_active = true;
            }
            WalkTransition::Terminal(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advance_preserves_consecutive_edge_warm_start() {
        let mut cursor = WalkCursor::new(2);
        cursor.record_right_edge(U256::from(100u64), U256::from(110u64));
        cursor.apply(WalkTransition::Advance { next: vec![1, 0] });
        assert_eq!(cursor.ks(), &[1, 0]);
        assert_eq!(cursor.prev_right_edge(), Some(U256::from(100u64)));
        assert_eq!(
            cursor.right_bracket(),
            Some((U256::from(100u64), U256::from(110u64)))
        );
        assert!(!cursor.anchor_reentry_active());
    }

    #[test]
    fn skipped_tuple_clears_only_previous_edge() {
        let mut cursor = WalkCursor::new(2);
        cursor.record_right_edge(U256::from(100u64), U256::from(110u64));
        cursor.apply(WalkTransition::SkippedTuple { next: vec![2, 0] });
        assert_eq!(cursor.ks(), &[2, 0]);
        assert_eq!(cursor.prev_right_edge(), None);
        assert_eq!(
            cursor.right_bracket(),
            Some((U256::from(100u64), U256::from(110u64)))
        );
    }

    #[test]
    fn anchor_reentry_clears_all_edge_cache_and_latches() {
        let mut cursor = WalkCursor::new(2);
        cursor.record_right_edge(U256::from(100u64), U256::from(110u64));
        cursor.apply(WalkTransition::AnchorReentry { next: vec![3, 1] });
        assert_eq!(cursor.ks(), &[3, 1]);
        assert_eq!(cursor.prev_right_edge(), None);
        assert_eq!(cursor.right_bracket(), None);
        assert!(cursor.anchor_reentry_active());
    }

    #[test]
    fn visited_duplicate_is_terminal_only_for_a_repeated_tuple() {
        let mut cursor = WalkCursor::new(2);
        assert_eq!(cursor.begin_piece(), Ok(()));
        assert_eq!(cursor.begin_piece(), Err(TerminalKind::VisitedDuplicate));
        cursor.apply(WalkTransition::Advance { next: vec![1, 0] });
        assert_eq!(cursor.begin_piece(), Ok(()));
    }

    #[test]
    fn terminal_kinds_preserve_refinement_contract() {
        assert_eq!(TerminalKind::UnboundedRefine.refine_neighbor(), Some(false));
        assert_eq!(
            TerminalKind::StopRefine { climbing: true }.refine_neighbor(),
            Some(true)
        );
        assert_eq!(
            TerminalKind::StopRefine { climbing: false }.refine_neighbor(),
            Some(false)
        );
        for terminal in [
            TerminalKind::DegenerateNoRefine,
            TerminalKind::VisitedDuplicate,
            TerminalKind::CapExhausted,
        ] {
            assert_eq!(terminal.refine_neighbor(), None);
        }
    }
}
