//! Traversal-plan assembly for the DFS driver.
//!
//! A search request names a set of start tokens and a set of end tokens.
//! The plan is the Cartesian product of those sets, with two refinements:
//!
//! 1. **Reverse consolidation.** When several tokens serve as both a start
//!    and an end, a forward path `a -> b` already implies the reverse
//!    `b -> a`; one traversal yielding both directions replaces the two
//!    separate traversals `a -> b` and `b -> a`.
//! 2. **Filter depth floor.** A per-depth pool-kind filter of length `N`
//!    describes an exactly `N`-hop permutation, so the effective minimum
//!    depth is floored at `N` (a shorter cycle would only prefix-match the
//!    first depths).
//!
//! This is pure assembly over token ids. DB resolution of ids and the DFS
//! itself live elsewhere.

use std::collections::{HashMap, HashSet};

/// One `(start, end, direction)` DFS traversal over a prepared graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TraversalSpec {
    /// The DFS start token id.
    pub start_token_id: u64,
    /// The DFS end token id.
    pub end_token_id: u64,
    /// Whether each found cycle is also yielded reversed.
    pub include_reverse: bool,
    /// The effective minimum hop depth for this traversal.
    pub min_depth: usize,
}

/// Assemble the traversal plan for a search request.
///
/// `start_token_ids` / `end_token_ids` are deduplicated here while preserving
/// first-seen order, so the caller may pass raw resolved-id lists. When
/// `filter_len` is `Some(n)`, the effective minimum depth is
/// `max(min_depth, n)`; otherwise it is `min_depth`.
#[must_use]
pub fn prepare_traversal_plan(
    start_token_ids: &[u64],
    end_token_ids: &[u64],
    min_depth: usize,
    filter_len: Option<usize>,
) -> Vec<TraversalSpec> {
    let effective_min_depth = filter_len.map_or(min_depth, |n| min_depth.max(n));

    let starts = dedup_ordered(start_token_ids);
    let ends = dedup_ordered(end_token_ids);

    // Cartesian product, in input order. `None` marks an entry the reverse
    // consolidation removed.
    let mut entries: Vec<Option<(u64, u64, bool)>> = Vec::with_capacity(starts.len() * ends.len());
    let mut positions: HashMap<(u64, u64), usize> = HashMap::new();
    for &start in &starts {
        for &end in &ends {
            positions.insert((start, end), entries.len());
            entries.push(Some((start, end, false)));
        }
    }

    // Tokens that appear in BOTH boundary sets. A forward traversal between
    // any pair covers the reverse as well, so merge `(a -> b)` and `(b -> a)`
    // into one `FORWARD_AND_REVERSE` traversal.
    let end_set: HashSet<u64> = ends.iter().copied().collect();
    let mut common: Vec<u64> = Vec::new();
    let mut seen: HashSet<u64> = HashSet::new();
    for &token in &starts {
        if end_set.contains(&token) && seen.insert(token) {
            common.push(token);
        }
    }
    if common.len() > 1 {
        for i in 0..common.len() {
            for j in (i + 1)..common.len() {
                let (a, b) = (common[i], common[j]);
                if let Some(&index) = positions.get(&(a, b)) {
                    if let Some(entry) = entries.get_mut(index).and_then(Option::as_mut) {
                        entry.2 = true;
                    }
                }
                if let Some(&index) = positions.get(&(b, a)) {
                    entries[index] = None;
                }
            }
        }
    }

    entries
        .into_iter()
        .flatten()
        .map(
            |(start_token_id, end_token_id, include_reverse)| TraversalSpec {
                start_token_id,
                end_token_id,
                include_reverse,
                min_depth: effective_min_depth,
            },
        )
        .collect()
}

/// Deduplicate preserving first-seen order.
fn dedup_ordered(ids: &[u64]) -> Vec<u64> {
    let mut seen = HashSet::with_capacity(ids.len());
    let mut out = Vec::with_capacity(ids.len());
    for &id in ids {
        if seen.insert(id) {
            out.push(id);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(start: u64, end: u64, reverse: bool, min_depth: usize) -> TraversalSpec {
        TraversalSpec {
            start_token_id: start,
            end_token_id: end,
            include_reverse: reverse,
            min_depth,
        }
    }

    #[test]
    fn cartesian_product_is_forward() {
        let plan = prepare_traversal_plan(&[1, 2], &[3, 4], 2, None);
        assert_eq!(
            plan,
            vec![
                spec(1, 3, false, 2),
                spec(1, 4, false, 2),
                spec(2, 3, false, 2),
                spec(2, 4, false, 2),
            ]
        );
    }

    #[test]
    fn common_tokens_consolidate_into_one_reverse_traversal() {
        // start == end == {1, 2}: the product has four entries; the (1, 2)
        // forward traversal absorbs the reverse of (2, 1), which is removed.
        let plan = prepare_traversal_plan(&[1, 2], &[1, 2], 2, None);
        assert_eq!(
            plan,
            vec![
                spec(1, 1, false, 2),
                spec(1, 2, true, 2),
                spec(2, 2, false, 2)
            ]
        );
    }

    #[test]
    fn single_common_token_is_not_consolidated() {
        let plan = prepare_traversal_plan(&[1], &[1], 2, None);
        assert_eq!(plan, vec![spec(1, 1, false, 2)]);
    }

    #[test]
    fn filter_length_floors_the_min_depth() {
        let plan = prepare_traversal_plan(&[1], &[1], 2, Some(3));
        assert_eq!(plan, vec![spec(1, 1, false, 3)]);
    }

    #[test]
    fn filter_length_below_min_depth_keeps_min_depth() {
        let plan = prepare_traversal_plan(&[1], &[1], 4, Some(2));
        assert_eq!(plan, vec![spec(1, 1, false, 4)]);
    }

    #[test]
    fn duplicate_boundary_ids_are_deduplicated() {
        let plan = prepare_traversal_plan(&[1, 1, 2], &[2, 2], 2, None);
        assert_eq!(plan, vec![spec(1, 2, false, 2), spec(2, 2, false, 2)]);
    }
}
