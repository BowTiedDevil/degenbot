//! Hop direction resolution for enumerated arbitrage cycles.
//!
//! The pure mechanic shared by every driver: given the hop sequence of a
//! discovered cycle, orient each hop (`zero_for_one`) so the swap chain
//! starts and ends at the input token. No driver policy, no pool state, no
//! I/O — the mechanical tail of path construction, co-located with the
//! graph + DFS that produce the cycle (`PoolKind`, `PathGraph`).
//!
//! V4 pools use the zero address for the native currency. For direction
//! resolution the zero address is treated as equivalent to WETH (the
//! caller supplies the WETH address), matching the Python driver's
//! long-standing behavior. Address comparison is ASCII-case-insensitive.

use std::fmt;

/// The zero address (the V4 native-currency sentinel).
const NATIVE_CURRENCY: &str = "0x0000000000000000000000000000000000000000";

/// One hop's orientation inputs for [`resolve_directions`].
///
/// A borrowed view so callers with richer node types (the Python FFI
/// translator, the settlement example's `PoolNode`) never copy their nodes
/// into a core shape.
pub struct DirectionHop<'a> {
    /// Token0 on-chain address (any case).
    pub token0: &'a str,
    /// Token1 on-chain address (any case).
    pub token1: &'a str,
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
        /// The pool's token0 address (lowercased).
        token0: String,
        /// The pool's token1 address (lowercased).
        token1: String,
        /// The token the chain was tracking entering the hop (lowercased).
        tracked: String,
        /// The cycle's input token (lowercased).
        start: String,
    },
    /// The hop chain does not return to the input token.
    Unclosed {
        /// The token the chain ended on (lowercased).
        output: String,
        /// The cycle's input token (lowercased).
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

/// Determine `zero_for_one` for each hop so the cycle closes.
///
/// The cycle: `input_token` → hop 0 → intermediate → hop 1 → ... →
/// `input_token`. Returns one zfo per hop, or a boxed [`DirectionError`]
/// when a hop carries neither tracked token or the cycle does not close.
///
/// # Errors
///
/// [`DirectionError::Mismatch`] on a mid-path token mismatch,
/// [`DirectionError::Unclosed`] when the chain does not return to the
/// input token. The error is boxed to stay inside the
/// `result_large_err` budget (repo convention: box the payload, keep the
/// `Result` small).
pub fn resolve_directions(
    hops: &[DirectionHop<'_>],
    input_token: &str,
    weth: &str,
) -> Result<Vec<bool>, Box<DirectionError>> {
    let start = input_token.to_ascii_lowercase();
    let weth = weth.to_ascii_lowercase();
    let mut tracked = start.clone();
    let mut zfos = Vec::with_capacity(hops.len());

    for (i, hop) in hops.iter().enumerate() {
        let token0 = {
            let t = hop.token0.to_ascii_lowercase();
            if t == NATIVE_CURRENCY {
                weth.clone()
            } else {
                t
            }
        };
        let token1 = {
            let t = hop.token1.to_ascii_lowercase();
            if t == NATIVE_CURRENCY {
                weth.clone()
            } else {
                t
            }
        };

        let zfo = if token0 == tracked {
            true
        } else if token1 == tracked {
            false
        } else {
            return Err(Box::new(DirectionError::Mismatch {
                hop: i,
                hops: hops.len(),
                identity: hop.identity.to_string(),
                token0,
                token1,
                tracked,
                start,
            }));
        };

        tracked = if zfo { token1 } else { token0 };
        zfos.push(zfo);
    }

    if tracked != start {
        return Err(Box::new(DirectionError::Unclosed {
            output: tracked,
            input: start,
            identities: hops.iter().map(|hop| hop.identity.to_string()).collect(),
        }));
    }

    Ok(zfos)
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

    fn hop<'a>(token0: &'a str, token1: &'a str) -> DirectionHop<'a> {
        DirectionHop {
            token0,
            token1,
            identity: "test-pool",
        }
    }

    #[test]
    fn two_hop_cycle_closes_with_expected_zfos() {
        // WETH → USDC → WETH. The input arrives checksummed while the node
        // addresses arrive lowercase: matching is case-insensitive.
        let hops = [
            hop(
                "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48",
                "0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2",
            ),
            hop(
                "0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2",
                "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48",
            ),
        ];
        let zfos = resolve_directions(&hops, WETH, WETH).expect("cycle closes");
        // hop 0 sells token1 (WETH) for token0 (USDC); hop 1 sells token1
        // (USDC) for token0 (WETH) — zfo means "sell token0".
        assert_eq!(zfos, vec![false, false]);
    }

    #[test]
    fn v4_native_sentinel_matches_weth() {
        let native = "0x0000000000000000000000000000000000000000";
        let hops = [hop(native, USDC), hop(USDC, native)];
        let zfos = resolve_directions(&hops, WETH, WETH).expect("cycle closes");
        // hop 0 sells token0 (native→WETH) for token1 (USDC); hop 1 sells
        // token0 (USDC) for token1 (native→WETH).
        assert_eq!(zfos, vec![true, true]);
    }

    #[test]
    fn empty_hop_list_trivially_closes() {
        let zfos = resolve_directions(&[], WETH, WETH).expect("empty cycle closes");
        assert!(zfos.is_empty());
    }

    #[test]
    fn mid_path_mismatch_names_the_hop_and_tokens() {
        let hops = [
            hop(WETH, USDC),
            hop(
                "0x111111111117dc0aa78b770fa6a738034120c302",
                "0x22222222222222fe6d179adbf7e7b9385e0e4313",
            ),
        ];
        let err = resolve_directions(&hops, WETH, WETH).expect_err("second hop orphans the chain");
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
                token0: WETH,
                token1: USDC,
                identity: "pool-a",
            },
            DirectionHop {
                token0: USDC,
                token1: "0x111111111117dc0aa78b770fa6a738034120c302",
                identity: "pool-b",
            },
        ];
        let err = resolve_directions(&hops, WETH, WETH).expect_err("chain ends off-cycle");
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
}
