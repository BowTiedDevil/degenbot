//! Hop direction resolution for enumerated arbitrage cycles.
//!
//! The pure mechanic shared by every driver: given the hop sequence of a
//! discovered cycle, orient each hop (`zero_for_one`) so the swap chain
//! starts and ends at the input token. No driver policy, no pool state, no
//! I/O — the mechanical tail of path construction, co-located with the
//! graph + DFS that produce the cycle (`PoolKind`, `PathGraph`).
//!
//! Tokens are the CONCEPTUAL identities (ADR-097 D1): the chain's native
//! asset is [`DirectionToken::Native`], never a protocol sentinel address —
//! raw zero is confined to V4 execution and never crosses this public input.
//! WETH stays a distinct ERC-20. When a cycle's value chain crosses the
//! native/WETH representation gap (V4 native currency against a reserve
//! pair's WETH), the cycle closes EXPLICITLY: the resolution records a
//! [`CurrencyBridge`] at each boundary needing a wrap/unwrap instead of
//! aliasing the two identities (ADR-097 D5). Address comparison is
//! ASCII-case-insensitive.

use std::fmt;

/// One hop's token spelling for direction resolution: the chain's
/// conceptual native asset, or an ERC-20 by address spelling.
///
/// A borrowed view so callers with richer node types (the Python FFI
/// translator, the crawl's hop facts) never copy their nodes into a core
/// shape. The native asset arrives as `Native` because it has no address of
/// its own; a raw protocol sentinel (V4 zero, Curve all-Es) is never a way
/// to spell it here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectionToken<'a> {
    /// The chain's conceptual native asset.
    Native,
    /// The ERC-20 token at this address spelling (any case).
    Erc20(&'a str),
}

impl fmt::Display for DirectionToken<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Native => f.write_str("native token"),
            Self::Erc20(address) => f.write_str(&address.to_ascii_lowercase()),
        }
    }
}

/// The explicit native↔WETH representation conversion one cycle boundary
/// needs (ADR-097 D5).
///
/// The same classification axis the executor's
/// `degenbot_executor::composers::CurrencyBridge` encodes: `Wrap` converts
/// native → WETH (`WETH_DEPOSIT`), `Unwrap` converts WETH → native
/// (`WETH_WITHDRAW`). WETH and the native asset stay DISTINCT identities; a
/// boundary that crosses the gap is recorded here so the conversion is
/// never hidden through registry equivalence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CurrencyBridge {
    /// No conversion — both sides carry the same representation.
    None,
    /// The upstream side holds native, the downstream side consumes WETH.
    Wrap,
    /// The upstream side holds WETH, the downstream side consumes native.
    Unwrap,
}

impl CurrencyBridge {
    /// Classify the boundary `(upstream, downstream)` — the representation
    /// conversion required to feed `downstream` from `upstream`.
    ///
    /// Only the native/WETH pair can require a bridge; any other pairing is
    /// either identical or not a representation pair at all (a `Mismatch`
    /// is the caller's verdict for those).
    #[must_use]
    pub fn between(
        upstream: &DirectionToken<'_>,
        downstream: &DirectionToken<'_>,
        weth: &str,
    ) -> Self {
        let up_native = matches!(upstream, DirectionToken::Native);
        let down_native = matches!(downstream, DirectionToken::Native);
        let up_is_weth =
            matches!(upstream, DirectionToken::Erc20(a) if a.eq_ignore_ascii_case(weth));
        let down_is_weth =
            matches!(downstream, DirectionToken::Erc20(a) if a.eq_ignore_ascii_case(weth));
        match (up_native, down_native, up_is_weth, down_is_weth) {
            (true, false, _, true) => Self::Wrap,
            (false, true, true, _) => Self::Unwrap,
            _ => Self::None,
        }
    }
}

/// One hop's orientation inputs for [`resolve_directions`].
pub struct DirectionHop<'a> {
    /// Token0 on-chain (the conceptual spelling, any case for addresses).
    pub token0: DirectionToken<'a>,
    /// Token1 on-chain (the conceptual spelling, any case for addresses).
    pub token1: DirectionToken<'a>,
    /// The stable pool identity used verbatim in error messages.
    pub identity: &'a str,
}

/// Why [`resolve_directions`] refused the cycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirectionError {
    /// A hop carries neither the tracked input token nor its complement:
    /// the constructed pool disagrees with the enumerated edge. A fatal
    /// invariant violation, never a skip-and-continue.
    Mismatch {
        /// The zero-based hop index.
        hop: usize,
        /// The total hop count.
        hops: usize,
        /// The offending pool's identity.
        identity: String,
        /// The pool's token0 spelling.
        token0: String,
        /// The pool's token1 spelling.
        token1: String,
        /// The token the chain was tracking entering the hop.
        tracked: String,
        /// The cycle's input token.
        start: String,
    },
    /// The hop chain does not return to the input token (exactly, or across
    /// the explicit native/WETH representation bridge).
    Unclosed {
        /// The token the chain ended on.
        output: String,
        /// The cycle's input token.
        input: String,
        /// Every hop's identity, in order, for operator diagnosis.
        identities: Vec<String>,
    },
}

impl fmt::Display for DirectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Mismatch {
                hop,
                hops,
                identity,
                token0,
                token1,
                tracked,
                start,
            } => write!(
                f,
                "hop {hop}/{hops}: pool {identity} has token0={token0} token1={token1}; \
expected either to carry the tracked input token {tracked} (path starts at {start})"
            ),
            Self::Unclosed {
                output,
                input,
                identities,
            } => write!(
                f,
                "cycle does not close: final output {output} != input {input}; \
pools=[{}]",
                identities.join(", ")
            ),
        }
    }
}

/// A resolved cycle: one `zero_for_one` per hop, plus the explicit
/// representation bridges the value chain needs.
///
/// # Fields
///
/// - `zero_for_one`: the per-hop orientation, in hop order.
/// - `bridges`: one entry per boundary of the value chain, `hops.len() + 1`
///   in walk order. `bridges[0]` is the seed-side boundary (the cycle input →
///   hop 0's input); `bridges[i]` (`0 < i < n`) is hop `i-1`'s output → hop
///   `i`'s input; `bridges[n]` is the capture-side boundary (hop `n-1`'s
///   output → the cycle input). Every entry is [`CurrencyBridge::None`] when
///   the boundary carries one representation end to end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedDirections {
    /// The per-hop orientation, in hop order.
    pub zero_for_one: Vec<bool>,
    /// The per-boundary representation bridges, in walk order (`n + 1`).
    pub bridges: Vec<CurrencyBridge>,
}

/// Exact identity match: the same conceptual token (ERC-20 spellings compare
/// ASCII-case-insensitively). Native and WETH are DISTINCT identities — they
/// match here only when both sides are the same one.
fn exact(a: &DirectionToken<'_>, b: &DirectionToken<'_>) -> bool {
    match (a, b) {
        (DirectionToken::Native, DirectionToken::Native) => true,
        (DirectionToken::Erc20(a), DirectionToken::Erc20(b)) => a.eq_ignore_ascii_case(b),
        _ => false,
    }
}

/// The native/WETH representation pair: one side native, the other side the
/// WETH ERC-20 (`weth` spelling). The ONE pairing a cycle may bridge.
fn representation_pair(a: &DirectionToken<'_>, b: &DirectionToken<'_>, weth: &str) -> bool {
    matches!(
        CurrencyBridge::between(a, b, weth),
        CurrencyBridge::Wrap | CurrencyBridge::Unwrap
    )
}

/// Determine `zero_for_one` for each hop so the cycle closes.
///
/// The cycle: `input_token` → hop 0 → intermediate → hop 1 → ... →
/// `input_token`. Each hop must carry the tracked token on one side: an
/// exact identity match orients the hop with no bridge, and a native/WETH
/// representation pair matches WITH the explicit [`CurrencyBridge`] the
/// boundary needs. Exact identity wins over the representation pair when a
/// hop carries both (the native/WETH pair pool), so the cycle prefers the
/// representation it can enter without conversion.
///
/// Returns the [`ResolvedDirections`] (per-hop orientations + per-boundary
/// bridges), or a boxed [`DirectionError`] when a hop carries neither the
/// tracked token nor its representation pair or the cycle does not close.
/// The error is boxed to stay inside the `result_large_err` budget (repo
/// convention: box the payload, keep the `Result` small).
///
/// # Errors
///
/// [`DirectionError::Mismatch`] on a mid-path token mismatch,
/// [`DirectionError::Unclosed`] when the chain does not return to the
/// input token (not even across the representation bridge).
pub fn resolve_directions(
    hops: &[DirectionHop<'_>],
    input_token: &DirectionToken<'_>,
    weth: &str,
) -> Result<ResolvedDirections, Box<DirectionError>> {
    let start = *input_token;
    let mut tracked = start;
    let mut zfos = Vec::with_capacity(hops.len());
    let mut bridges = Vec::with_capacity(hops.len() + 1);

    for (i, hop) in hops.iter().enumerate() {
        // Exact identity first (both endpoints), then the representation
        // pair: a native/WETH pair hop carries BOTH spellings of the value
        // asset, and the exact side is the one entered without conversion.
        let oriented = if exact(&hop.token0, &tracked) {
            Some((true, CurrencyBridge::None))
        } else if exact(&hop.token1, &tracked) {
            Some((false, CurrencyBridge::None))
        } else if representation_pair(&hop.token0, &tracked, weth) {
            Some((true, CurrencyBridge::between(&tracked, &hop.token0, weth)))
        } else if representation_pair(&hop.token1, &tracked, weth) {
            Some((false, CurrencyBridge::between(&tracked, &hop.token1, weth)))
        } else {
            None
        };

        let Some((zfo, bridge)) = oriented else {
            return Err(Box::new(DirectionError::Mismatch {
                hop: i,
                hops: hops.len(),
                identity: hop.identity.to_string(),
                token0: hop.token0.to_string(),
                token1: hop.token1.to_string(),
                tracked: tracked.to_string(),
                start: start.to_string(),
            }));
        };

        bridges.push(bridge);
        tracked = if zfo { hop.token1 } else { hop.token0 };
        zfos.push(zfo);
    }

    // The capture-side boundary: the chain closes exactly, or across the
    // explicit representation bridge (recorded, never aliased).
    let close = if exact(&tracked, &start) {
        CurrencyBridge::None
    } else if representation_pair(&tracked, &start, weth) {
        CurrencyBridge::between(&tracked, &start, weth)
    } else {
        return Err(Box::new(DirectionError::Unclosed {
            output: tracked.to_string(),
            input: start.to_string(),
            identities: hops.iter().map(|hop| hop.identity.to_string()).collect(),
        }));
    };
    bridges.push(close);

    Ok(ResolvedDirections {
        zero_for_one: zfos,
        bridges,
    })
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    clippy::panic,
    reason = "tests assert on known-valid fixture contents"
)]
mod tests {
    use super::*;

    const WETH: &str = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2";
    const USDC: &str = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48";
    const USDT: &str = "0xdAC17F958D2ee523a2206206994597C13D831ec7";

    fn erc20(spelling: &str) -> DirectionToken<'_> {
        DirectionToken::Erc20(spelling)
    }

    fn native() -> DirectionToken<'static> {
        DirectionToken::Native
    }

    fn hop<'a>(token0: DirectionToken<'a>, token1: DirectionToken<'a>) -> DirectionHop<'a> {
        DirectionHop {
            token0,
            token1,
            identity: "test-pool",
        }
    }

    // ── the exact-identity core ────────────────────────────────────────────

    #[test]
    fn two_hop_cycle_closes_with_expected_zfos_and_no_bridges() {
        // WETH → USDC → WETH. The input arrives checksummed while the node
        // addresses arrive lowercase: matching is case-insensitive. No
        // representation is crossed, so every bridge is None.
        let hops = [
            hop(
                erc20("0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48"),
                erc20("0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2"),
            ),
            hop(
                erc20("0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2"),
                erc20("0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48"),
            ),
        ];
        let resolved = resolve_directions(&hops, &erc20(WETH), WETH).expect("cycle closes");
        // hop 0 sells token1 (WETH) for token0 (USDC); hop 1 sells token1
        // (USDC) for token0 (WETH) — zfo means "sell token0".
        assert_eq!(resolved.zero_for_one, vec![false, false]);
        assert_eq!(
            resolved.bridges,
            vec![
                CurrencyBridge::None,
                CurrencyBridge::None,
                CurrencyBridge::None
            ]
        );
    }

    #[test]
    fn empty_hop_list_trivially_closes() {
        let resolved = resolve_directions(&[], &erc20(WETH), WETH).expect("empty cycle closes");
        assert!(resolved.zero_for_one.is_empty());
        // n + 1 boundaries: just the (trivially closed) cycle boundary.
        assert_eq!(resolved.bridges, vec![CurrencyBridge::None]);
    }

    // ── V4 -> non-V4 and non-V4 -> V4, both orientations ───────────────────

    #[test]
    fn v4_led_cycle_with_weth_profit_records_the_seed_unwrap() {
        // V4 -> non-V4, native start variant at the hop level: the V4 hop
        // consumes native, so the WETH profit token must unwrap at the seed
        // boundary; the chain closes exactly on WETH.
        let hops = [hop(native(), erc20(USDC)), hop(erc20(USDC), erc20(WETH))];
        let resolved = resolve_directions(&hops, &erc20(WETH), WETH).expect("cycle closes");
        assert_eq!(resolved.zero_for_one, vec![true, true]);
        assert_eq!(
            resolved.bridges,
            vec![
                CurrencyBridge::Unwrap,
                CurrencyBridge::None,
                CurrencyBridge::None
            ]
        );
    }

    #[test]
    fn v4_tail_cycle_with_weth_profit_records_the_capture_wrap() {
        // non-V4 -> V4: the WETH seed feeds the reserve pair directly, the
        // V4 tail delivers native, so the capture boundary wraps back to the
        // WETH profit representation.
        let hops = [hop(erc20(WETH), erc20(USDC)), hop(erc20(USDC), native())];
        let resolved = resolve_directions(&hops, &erc20(WETH), WETH).expect("cycle closes");
        assert_eq!(resolved.zero_for_one, vec![true, true]);
        assert_eq!(
            resolved.bridges,
            vec![
                CurrencyBridge::None,
                CurrencyBridge::None,
                CurrencyBridge::Wrap
            ]
        );
    }

    #[test]
    fn v4_led_cycle_with_native_profit_closes_exactly_on_native() {
        // V4 -> non-V4, native start AND native end variant: the tracked
        // value enters native (no seed conversion), crosses WETH inside the
        // reserve-pair hop, and closes on the native representation with an
        // explicit unwrap at the capture boundary.
        let hops = [hop(native(), erc20(USDC)), hop(erc20(USDC), erc20(WETH))];
        let resolved = resolve_directions(&hops, &native(), WETH).expect("cycle closes");
        assert_eq!(resolved.zero_for_one, vec![true, true]);
        assert_eq!(
            resolved.bridges,
            vec![
                CurrencyBridge::None,
                CurrencyBridge::None,
                CurrencyBridge::Unwrap
            ]
        );
    }

    #[test]
    fn non_v4_led_cycle_with_native_profit_records_the_seed_wrap() {
        // non-V4 -> V4, native start variant: the native profit token wraps
        // to WETH at the seed boundary so the reserve-pair hop can consume
        // WETH; the V4 tail delivers native and closes exactly.
        let hops = [hop(erc20(WETH), erc20(USDC)), hop(erc20(USDC), native())];
        let resolved = resolve_directions(&hops, &native(), WETH).expect("cycle closes");
        assert_eq!(resolved.zero_for_one, vec![true, true]);
        assert_eq!(
            resolved.bridges,
            vec![
                CurrencyBridge::Wrap,
                CurrencyBridge::None,
                CurrencyBridge::None
            ]
        );
    }

    // ── the representation pair stays explicit ─────────────────────────────

    #[test]
    fn interior_representation_gap_is_recorded_not_aliased() {
        // A mixed path whose interior boundary crosses the gap: hop 0
        // delivers WETH, hop 1 consumes native. The hop is oriented through
        // the EXPLICIT bridge, never through identity equivalence.
        let hops = [
            hop(erc20(USDC), erc20(WETH)),
            hop(native(), erc20(USDT)),
            hop(erc20(USDT), erc20(USDC)),
        ];
        let resolved = resolve_directions(&hops, &erc20(USDC), WETH).expect("cycle closes");
        assert_eq!(resolved.zero_for_one, vec![true, true, true]);
        assert_eq!(
            resolved.bridges,
            vec![
                CurrencyBridge::None,
                CurrencyBridge::Unwrap,
                CurrencyBridge::None,
                CurrencyBridge::None,
            ]
        );
    }

    #[test]
    fn native_weth_pair_hop_prefers_the_exact_identity_side() {
        // The V4 native/WETH pair hop carries BOTH representations of the
        // value asset. Exact identity wins: the WETH profit token enters the
        // WETH side without conversion and the capture wraps the native
        // output.
        let hops = [hop(native(), erc20(WETH))];
        let resolved = resolve_directions(&hops, &erc20(WETH), WETH).expect("cycle closes");
        assert_eq!(resolved.zero_for_one, vec![false]);
        assert_eq!(
            resolved.bridges,
            vec![CurrencyBridge::None, CurrencyBridge::Wrap]
        );

        // And the native profit token enters the native side exactly.
        let resolved = resolve_directions(&hops, &native(), WETH).expect("cycle closes");
        assert_eq!(resolved.zero_for_one, vec![true]);
        assert_eq!(
            resolved.bridges,
            vec![CurrencyBridge::None, CurrencyBridge::Unwrap]
        );
    }

    #[test]
    fn native_and_weth_stay_distinct_identities() {
        // A hop carrying WETH does not match a tracked USDC even though the
        // cycle's profit representation is WETH — the representation pair is
        // ONLY native/WETH.
        let hops = [hop(erc20(USDT), erc20(WETH))];
        let err = resolve_directions(&hops, &erc20(USDC), WETH)
            .expect_err("USDC tracks into a WETH/USDT hop");
        assert!(matches!(*err, DirectionError::Mismatch { hop: 0, .. }));

        // Native does not alias any non-WETH ERC-20 either.
        let hops = [hop(erc20(USDT), erc20(USDC))];
        let err = resolve_directions(&hops, &native(), WETH)
            .expect_err("native tracks into a USDT/USDC hop");
        assert!(matches!(*err, DirectionError::Mismatch { hop: 0, .. }));
    }

    #[test]
    fn a_cycle_that_only_closes_through_a_foreign_token_stays_unclosed() {
        // Ends on USDT while the profit token is WETH: no representation
        // bridge can close this.
        let hops = [hop(erc20(WETH), erc20(USDC)), hop(erc20(USDC), erc20(USDT))];
        let err = resolve_directions(&hops, &erc20(WETH), WETH)
            .expect_err("the chain does not return to WETH");
        assert!(matches!(*err, DirectionError::Unclosed { .. }));
    }

    // ── the refusals keep their operator text ──────────────────────────────

    #[test]
    fn mid_path_mismatch_names_the_hop_and_tokens() {
        let hops = [
            hop(erc20(WETH), erc20(USDC)),
            hop(
                erc20("0x111111111117dc0aa78b770fa6a738034120c302"),
                erc20("0x22222222222222fe6d179adbf7e7b9385e0e4313"),
            ),
        ];
        let err = resolve_directions(&hops, &erc20(WETH), WETH)
            .expect_err("second hop orphans the chain");
        match *err {
            DirectionError::Mismatch {
                hop,
                hops,
                identity,
                tracked,
                start,
                ..
            } => {
                assert_eq!(hop, 1);
                assert_eq!(hops, 2);
                assert_eq!(identity, "test-pool");
                assert_eq!(tracked, "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48");
                assert_eq!(start, "0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");
            }
            other @ DirectionError::Unclosed { .. } => panic!("expected Mismatch, got {other:?}"),
        }
    }

    #[test]
    fn unclosed_cycle_reports_output_and_identities() {
        let hops = [
            DirectionHop {
                token0: erc20(WETH),
                token1: erc20(USDC),
                identity: "pool-a",
            },
            DirectionHop {
                token0: erc20(USDC),
                token1: erc20("0x111111111117dc0aa78b770fa6a738034120c302"),
                identity: "pool-b",
            },
        ];
        let err = resolve_directions(&hops, &erc20(WETH), WETH).expect_err("chain ends off-cycle");
        match *err {
            DirectionError::Unclosed {
                output,
                input,
                identities,
            } => {
                assert_eq!(output, "0x111111111117dc0aa78b770fa6a738034120c302");
                assert_eq!(input, "0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");
                assert_eq!(identities, vec!["pool-a".to_string(), "pool-b".to_string()]);
            }
            other @ DirectionError::Mismatch { .. } => panic!("expected Unclosed, got {other:?}"),
        }
    }

    #[test]
    fn error_display_carries_the_operator_message() {
        let err = DirectionError::Unclosed {
            output: "0xabc".to_string(),
            input: "0xdef".to_string(),
            identities: vec!["pool-a".to_string()],
        };
        assert_eq!(
            err.to_string(),
            "cycle does not close: final output 0xabc != input 0xdef; pools=[pool-a]"
        );
    }

    #[test]
    fn the_native_token_renders_without_an_address() {
        // Public results project the conceptual token: the native spelling
        // names the asset, never a raw zero address.
        assert_eq!(native().to_string(), "native token");
        assert!(!native().to_string().contains('0'));
    }

    #[test]
    fn currency_bridge_classification_matches_the_executor_axis() {
        // Wrap: native upstream, WETH downstream.
        assert_eq!(
            CurrencyBridge::between(&native(), &erc20(WETH), WETH),
            CurrencyBridge::Wrap
        );
        // Unwrap: WETH upstream, native downstream.
        assert_eq!(
            CurrencyBridge::between(&erc20(WETH), &native(), WETH),
            CurrencyBridge::Unwrap
        );
        // Same representation on both sides: no bridge.
        assert_eq!(
            CurrencyBridge::between(&native(), &native(), WETH),
            CurrencyBridge::None
        );
        assert_eq!(
            CurrencyBridge::between(&erc20(WETH), &erc20(WETH), WETH),
            CurrencyBridge::None
        );
        // Non-WETH ERC-20s are never a representation pair.
        assert_eq!(
            CurrencyBridge::between(&erc20(USDC), &native(), WETH),
            CurrencyBridge::None
        );
        assert_eq!(
            CurrencyBridge::between(&native(), &erc20(USDC), WETH),
            CurrencyBridge::None
        );
    }
}
