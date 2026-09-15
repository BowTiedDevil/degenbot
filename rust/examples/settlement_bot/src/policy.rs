//! Driver-side path-composition policy — parity-ledger row 13
//!
//! Mirrors `src/degenbot/arbitrage/policy.py` (`touched_tokens`, the
//! `PathPolicy` rule order, the checksum-normalized token allow/deny sets,
//! the duplicate-pool guard keyed off the hop identity) plus the
//! permutation filter from `src/degenbot/runner/build_paths.py`
//! (`_parse_permutation_filter` / `_pool_types_from_filter`).
//!
//! The policy is deliberately driver-owned (AGENTS.md: Python is a driver
//! shell — and so is this Rust example). The core's pool *admission* floor
//! (V4 dynamic-fee / fee-encoder-limit / hooked refusal) stays in the Rust
//! core; this module only composes already-built hop candidates.
//!
//! Rule order (first violation raises, matching `PathPolicy.evaluate`):
//! 1. hop-count bounds (min then max)
//! 2. token denylist / allowlist
//! 3. duplicate-pool guard

use std::collections::BTreeSet;

use degenbot::pathfinding::PoolKind;

/// One oriented hop of a candidate path: the pool identity, its family, and
/// the `(token_in, token_out)` pair the hop swaps between.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HopView {
    /// Stable pool identity — checksummed/lowercase address for V2/V3, the
    /// V4 `pool_id` hex for V4 (mirrors `policy._pool_identity`).
    pub pool_identity: String,
    /// The hop's pool family (used by the per-depth permutation filter).
    pub pool_kind: PoolKind,
    /// The token sold into the pool (`token0` when `zfo`, else `token1`).
    pub token_in: String,
    /// The token bought from the pool (`token1` when `zfo`, else `token0`).
    pub token_out: String,
}

/// A deterministic path-composition refusal (mirrors the Python
/// `PathRejectedError` subtype family — classified by variant, never string).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PathRejection {
    /// `HopCountInsufficientError`.
    HopCountInsufficient { hop_count: usize, min_hops: usize },
    /// `HopCountExceededError`.
    HopCountExceeded { hop_count: usize, max_hops: usize },
    /// `TokenDenylistedError` (denylisted, or absent from a set allowlist).
    TokenDenylisted { token: String },
    /// `DuplicatePoolError`.
    DuplicatePool { pool: String },
}

impl PathRejection {
    /// The bounded metric/skip tag for this refusal (closed vocabulary).
    #[must_use]
    pub fn tag(&self) -> &'static str {
        match self {
            Self::HopCountInsufficient { .. } => "hop-count-insufficient",
            Self::HopCountExceeded { .. } => "hop-count-exceeded",
            Self::TokenDenylisted { .. } => "token-denylisted",
            Self::DuplicatePool { .. } => "duplicate-pool",
        }
    }
}

impl std::fmt::Display for PathRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::HopCountInsufficient {
                hop_count,
                min_hops,
            } => {
                write!(
                    f,
                    "path has {hop_count} hops, below the minimum of {min_hops}"
                )
            }
            Self::HopCountExceeded {
                hop_count,
                max_hops,
            } => {
                write!(
                    f,
                    "path has {hop_count} hops, exceeding the maximum of {max_hops}"
                )
            }
            Self::TokenDenylisted { token } => write!(f, "token {token} is not allowed"),
            Self::DuplicatePool { pool } => write!(f, "path repeats pool {pool}"),
        }
    }
}

impl std::error::Error for PathRejection {}

/// The ordered, de-duplicated tokens a path touches (input + each hop's
/// output), mirroring `policy.touched_tokens`.
#[must_use]
pub fn touched_tokens(hops: &[HopView]) -> Vec<String> {
    let mut touched: Vec<String> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for (idx, hop) in hops.iter().enumerate() {
        if idx == 0 && seen.insert(hop.token_in.clone()) {
            touched.push(hop.token_in.clone());
        }
        if seen.insert(hop.token_out.clone()) {
            touched.push(hop.token_out.clone());
        }
    }
    touched
}

/// Composable path-composition policy (the Rust twin of `PathPolicy`).
///
/// Token sets are normalized to lowercase; pool identities carry the
/// family-specific key shape produced by [`pool_identity`].
#[derive(Clone, Debug)]
pub struct PathPolicy {
    /// Denylisted token addresses (lowercase).
    pub disallowed_tokens: BTreeSet<String>,
    /// When `Some`, every touched token must appear (lowercase).
    pub allowed_tokens: Option<BTreeSet<String>>,
    /// Minimum hop count (inclusive).
    pub min_hops: usize,
    /// Maximum hop count (inclusive).
    pub max_hops: usize,
    /// Reject a path that traverses the same pool identity twice.
    pub reject_duplicate_pools: bool,
}

impl Default for PathPolicy {
    /// Python `PathPolicy` defaults (`min_hops=1`, `max_hops=usize::MAX`,
    /// no token sets, duplicate guard on).
    fn default() -> Self {
        Self {
            disallowed_tokens: BTreeSet::new(),
            allowed_tokens: None,
            min_hops: 1,
            max_hops: usize::MAX,
            reject_duplicate_pools: true,
        }
    }
}

impl PathPolicy {
    /// Evaluate every enabled rule in the fixed order; the first violation
    /// yields `Err`.
    ///
    /// # Errors
    ///
    /// Returns the typed [`PathRejection`] for the first violated rule.
    pub fn evaluate(&self, hops: &[HopView]) -> Result<(), PathRejection> {
        let hop_count = hops.len();
        if hop_count < self.min_hops {
            return Err(PathRejection::HopCountInsufficient {
                hop_count,
                min_hops: self.min_hops,
            });
        }
        if hop_count > self.max_hops {
            return Err(PathRejection::HopCountExceeded {
                hop_count,
                max_hops: self.max_hops,
            });
        }

        if !self.disallowed_tokens.is_empty() || self.allowed_tokens.is_some() {
            for token in touched_tokens(hops) {
                if self.disallowed_tokens.contains(&token) {
                    return Err(PathRejection::TokenDenylisted { token });
                }
                if let Some(allowed) = &self.allowed_tokens {
                    if !allowed.contains(&token) {
                        return Err(PathRejection::TokenDenylisted { token });
                    }
                }
            }
        }

        if self.reject_duplicate_pools {
            let mut seen: BTreeSet<String> = BTreeSet::new();
            for hop in hops {
                if !seen.insert(hop.pool_identity.clone()) {
                    return Err(PathRejection::DuplicatePool {
                        pool: hop.pool_identity.clone(),
                    });
                }
            }
        }

        Ok(())
    }
}

/// The stable pool identity used by the duplicate-pool guard (mirrors
/// `policy._pool_identity`: V2/V3 → checksummed/lowercase address, V4 →
/// `pool_id` hex, since the V4 manager address is shared).
#[must_use]
pub fn pool_identity(kind: PoolKind, address: Option<&str>, pool_hash: Option<&str>) -> String {
    match kind {
        PoolKind::V4 => pool_hash.unwrap_or_default().to_lowercase(),
        _ => address.unwrap_or_default().to_lowercase(),
    }
}

/// The parsed permutation filter (`{'V3-V4-V3'}` → per-depth allowed sets).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PermutationFilter {
    /// Allowed pool kinds at each depth; `None` = any kind at that depth.
    pub per_depth: Vec<Option<Vec<PoolKind>>>,
    /// The pool kinds mentioned anywhere in the filter (the discovery
    /// `pool_types` analogue — `_pool_types_from_filter`).
    pub pool_kinds: Vec<PoolKind>,
}

/// Parse a set of permutation strings (`V2`/`V3`/`V4` joined by `-`) into a
/// [`PermutationFilter`].
///
/// Mirrors `_parse_permutation_filter` + `_pool_types_from_filter`: every
/// permutation must have the same depth; a depth where all permutations
/// allow every kind collapses to `None`; the pool-kind set is every kind
/// mentioned.
///
/// # Errors
///
/// Returns a human-readable error for an unknown version tag or mixed depths
/// (the Python `ValueError` messages).
pub fn parse_permutation_filter<S: AsRef<str>>(
    perms: &BTreeSet<S>,
) -> Result<Option<PermutationFilter>, String> {
    if perms.is_empty() {
        return Ok(None);
    }
    let mut parsed: Vec<Vec<PoolKind>> = Vec::with_capacity(perms.len());
    for perm in perms {
        let parts: Vec<&str> = perm.as_ref().split('-').collect();
        let mut kinds: Vec<PoolKind> = Vec::with_capacity(parts.len());
        for part in &parts {
            let kind = match *part {
                "V2" => PoolKind::V2,
                "V3" => PoolKind::V3,
                "V4" => PoolKind::V4,
                other => {
                    return Err(format!(
                        "Invalid permutation '{}': unknown version tag {other}",
                        perm.as_ref()
                    ));
                }
            };
            kinds.push(kind);
        }
        parsed.push(kinds);
    }
    let depth = parsed[0].len();
    if parsed.iter().any(|p| p.len() != depth) {
        let rendered: Vec<&str> = perms.iter().map(AsRef::as_ref).collect();
        return Err(format!(
            "All permutations must have the same depth, got: {rendered:?}"
        ));
    }

    let all_kinds = [PoolKind::V2, PoolKind::V3, PoolKind::V4];
    let mut per_depth: Vec<Option<Vec<PoolKind>>> = Vec::with_capacity(depth);
    for d in 0..depth {
        let mut allowed: Vec<PoolKind> = Vec::new();
        for perm in &parsed {
            if !allowed.contains(&perm[d]) {
                allowed.push(perm[d]);
            }
        }
        if allowed.len() == all_kinds.len() {
            per_depth.push(None);
        } else {
            per_depth.push(Some(allowed));
        }
    }

    let mut pool_kinds: Vec<PoolKind> = Vec::new();
    for perm in &parsed {
        for kind in perm {
            if !pool_kinds.contains(kind) {
                pool_kinds.push(*kind);
            }
        }
    }

    Ok(Some(PermutationFilter {
        per_depth,
        pool_kinds,
    }))
}

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "tests assert on known-valid inputs")]
mod tests {
    use super::*;

    fn hop(identity: &str, kind: PoolKind, token_in: &str, token_out: &str) -> HopView {
        HopView {
            pool_identity: identity.to_string(),
            pool_kind: kind,
            token_in: token_in.to_string(),
            token_out: token_out.to_string(),
        }
    }

    #[test]
    fn touched_tokens_orders_and_dedups() {
        let hops = vec![
            hop("p:a", PoolKind::V3, "weth", "usdc"),
            hop("p:b", PoolKind::V3, "usdc", "dai"),
            hop("p:c", PoolKind::V3, "dai", "weth"),
        ];
        assert_eq!(touched_tokens(&hops), vec!["weth", "usdc", "dai"]);
    }

    #[test]
    fn hop_count_floor_and_cap() {
        let policy = PathPolicy {
            min_hops: 2,
            max_hops: 3,
            ..PathPolicy::default()
        };
        let one = vec![hop("p:a", PoolKind::V3, "weth", "weth")];
        assert!(matches!(
            policy.evaluate(&one),
            Err(PathRejection::HopCountInsufficient {
                hop_count: 1,
                min_hops: 2
            })
        ));
        let four = vec![
            hop("p:a", PoolKind::V3, "w", "x"),
            hop("p:b", PoolKind::V3, "x", "y"),
            hop("p:c", PoolKind::V3, "y", "z"),
            hop("p:d", PoolKind::V3, "z", "w"),
        ];
        assert!(matches!(
            policy.evaluate(&four),
            Err(PathRejection::HopCountExceeded {
                hop_count: 4,
                max_hops: 3
            })
        ));
    }

    #[test]
    fn denylist_and_allowlist() {
        let hops = vec![
            hop("p:a", PoolKind::V3, "weth", "usdc"),
            hop("p:b", PoolKind::V3, "usdc", "weth"),
        ];
        let deny = PathPolicy {
            disallowed_tokens: BTreeSet::from(["usdc".to_string()]),
            ..PathPolicy::default()
        };
        assert!(matches!(
            deny.evaluate(&hops),
            Err(PathRejection::TokenDenylisted { token }) if token == "usdc"
        ));
        let allow = PathPolicy {
            allowed_tokens: Some(BTreeSet::from(["weth".to_string()])),
            ..PathPolicy::default()
        };
        assert!(allow.evaluate(&hops).is_err());
        let allow_ok = PathPolicy {
            allowed_tokens: Some(BTreeSet::from(["weth".to_string(), "usdc".to_string()])),
            ..PathPolicy::default()
        };
        assert!(allow_ok.evaluate(&hops).is_ok());
    }

    #[test]
    fn duplicate_pool_guard() {
        let policy = PathPolicy::default();
        let hops = vec![
            hop("0xabc", PoolKind::V3, "w", "x"),
            hop("0xabc", PoolKind::V3, "x", "w"),
        ];
        assert!(matches!(
            policy.evaluate(&hops),
            Err(PathRejection::DuplicatePool { pool }) if pool == "0xabc"
        ));
        let no_dup = PathPolicy {
            reject_duplicate_pools: false,
            ..PathPolicy::default()
        };
        assert!(no_dup.evaluate(&hops).is_ok());
    }

    #[test]
    fn permutation_parse_single_per_depth() {
        let perms: BTreeSet<String> = BTreeSet::from(["V3-V4-V3".to_string()]);
        let filter = parse_permutation_filter(&perms).unwrap().unwrap();
        assert_eq!(
            filter.per_depth,
            vec![
                Some(vec![PoolKind::V3]),
                Some(vec![PoolKind::V4]),
                Some(vec![PoolKind::V3]),
            ]
        );
        assert_eq!(filter.pool_kinds, vec![PoolKind::V3, PoolKind::V4]);
    }

    #[test]
    fn permutation_parse_union_and_collapse() {
        let perms: BTreeSet<String> = BTreeSet::from(["V2-V3".to_string(), "V3-V4".to_string()]);
        let filter = parse_permutation_filter(&perms).unwrap().unwrap();
        assert_eq!(
            filter.per_depth,
            vec![
                Some(vec![PoolKind::V2, PoolKind::V3]),
                Some(vec![PoolKind::V3, PoolKind::V4]),
            ]
        );
        // Witness the None collapse: all three kinds at a depth is
        // indistinguishable from no filter, so the implementation stores None.
        let all_perms: BTreeSet<String> = BTreeSet::from([
            "V2-V3-V4".to_string(),
            "V3-V4-V2".to_string(),
            "V4-V2-V3".to_string(),
        ]);
        let all = parse_permutation_filter(&all_perms).unwrap().unwrap();
        assert_eq!(all.per_depth.len(), 3);
        assert!(all.per_depth.iter().all(Option::is_none));
    }

    #[test]
    fn permutation_unknown_tag_and_mixed_depth_error() {
        let bad: BTreeSet<String> = BTreeSet::from(["V9-V3".to_string()]);
        assert!(parse_permutation_filter(&bad).is_err());
        let mixed: BTreeSet<String> = BTreeSet::from(["V2-V3".to_string(), "V3".to_string()]);
        assert!(parse_permutation_filter(&mixed)
            .unwrap_err()
            .contains("same depth"));
    }

    #[test]
    fn pool_identity_v4_uses_hash() {
        assert_eq!(
            pool_identity(PoolKind::V4, None, Some("0xABCDEF")),
            "0xabcdef"
        );
        assert_eq!(
            pool_identity(PoolKind::V3, Some("0xDeadBeef"), None),
            "0xdeadbeef"
        );
    }

    #[test]
    fn rejection_tags_are_stable() {
        assert_eq!(
            PathRejection::HopCountExceeded {
                hop_count: 9,
                max_hops: 3
            }
            .tag(),
            "hop-count-exceeded"
        );
    }
}
