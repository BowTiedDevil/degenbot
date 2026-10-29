//! The construction identity-parity guard.
//!
//! A fresh construction MINTS an identity at the build site and echoes it to
//! the driver, which wraps the registered handle. The two must agree: the echo
//! is what the driver trusts, and the registered identity is what every later
//! read observes. This module is the guard's core-side home — the enforcement
//! lives where the identity is minted, so a mismatched construction is refused
//! before the echo ever crosses the FFI seam. The driver's return-surface
//! checks stay as a redundant cross-seam assertion, never the sole protection.
//!
//! A divergence is a core/driver seam bug (the echoed handle would disagree
//! with shared state), refused LOUD rather than silently returned.
//!
//! The module also owns the shared `CREATE2` / EIP-1167 identity verify the
//! register path and the build path both consume (ADR-077 D3), so an
//! identity mismatch has one verdict and one shape whichever door reached it.

use alloy::primitives::Address;
use degenbot_substrate::state_lock::LockSite;
use degenbot_uniswap::deployments::AddressMismatch;

use crate::bot_core::Bot;

/// The construction identity-parity refusal — the ONE verdict the register
/// path and the build path both consume (ADR-077 D3).
///
/// A recomputed, minted, or echoed identity that disagrees with the declared
/// or registered one is the SAME fact whichever entry raised it: the register
/// path's `CREATE2` recomputation, the builder's `CREATE2` check, and the
/// minted-vs-registered guard below all answer with this type.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IdentityDivergence {
    /// A `CREATE2` / EIP-1167 recomputation disagreed with the declared
    /// address. Display is the source rendering verbatim, so the register
    /// path's historical `ValueError` text is unchanged.
    #[error(transparent)]
    Create2(AddressMismatch),
    /// A freshly minted construction identity disagreed with the identity the
    /// registry of record holds for the just-registered pool.
    #[error("construction identity parity violated for {address}: {detail}")]
    MintedVsRegistered {
        /// The pool the divergence concerns (the minted echo's address).
        address: Address,
        /// The field-by-field divergence, for the shell's diagnostic message.
        detail: String,
    },
}

// ── The shared `CREATE2` identity verify ────────────────────────────────────
//
// The register path (`Bot::register_v2/v3/aerodrome_pool`) and the build path
// (`builder::build_v2/v3/aerodrome_v2`) both recompute a pool's `CREATE2` /
// EIP-1167 identity against the shipped deployment row and refuse a
// divergence. The recompute itself lives in the deployment tables; these
// wrappers are the ONE verdict both entries consume (ADR-077 D3), so an
// identity mismatch has one shape whichever door reached it.

/// Recompute the V2 `CREATE2` address and refuse a divergence.
///
/// # Errors
///
/// [`IdentityDivergence::Create2`] when the declared address disagrees with
/// the recomputed one. A `(chain_id, factory)` row the deployment table does
/// not ship verifies as a match (the ad-hoc path).
pub fn verify_v2_create2(
    chain_id: u64,
    factory: Address,
    address: Address,
    token0: Address,
    token1: Address,
) -> Result<(), IdentityDivergence> {
    degenbot_uniswap::deployments::verify_v2_pool_address(
        chain_id, factory, address, token0, token1,
    )
    .map_err(IdentityDivergence::Create2)
}

/// Recompute the V3 `CREATE2` address (the salt includes the fee) and refuse
/// a divergence. The V3 twin of [`verify_v2_create2`].
///
/// # Errors
///
/// [`IdentityDivergence::Create2`] on a verified mismatch.
pub fn verify_v3_create2(
    chain_id: u64,
    factory: Address,
    address: Address,
    token0: Address,
    token1: Address,
    fee: u32,
) -> Result<(), IdentityDivergence> {
    degenbot_uniswap::deployments::verify_v3_pool_address(
        chain_id, factory, address, token0, token1, fee,
    )
    .map_err(IdentityDivergence::Create2)
}

/// Recompute the Aerodrome V2 EIP-1167 address (deployer + implementation +
/// the `stable` flag's salt) and refuse a divergence.
///
/// # Errors
///
/// [`IdentityDivergence::Create2`] on a verified mismatch.
pub fn verify_aerodrome_v2_create2(
    chain_id: u64,
    factory: Address,
    address: Address,
    token0: Address,
    token1: Address,
    stable: bool,
) -> Result<(), IdentityDivergence> {
    degenbot_uniswap::deployments::verify_aerodrome_v2_pool_address(
        chain_id, factory, address, token0, token1, stable,
    )
    .map_err(IdentityDivergence::Create2)
}

/// The address-keyed construction identity `(token0, token1, address)` — the
/// echo shape of the V2 / V3 / Aerodrome families.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AddressIdentity {
    /// The pool's first token.
    pub token0: Address,
    /// The pool's second token.
    pub token1: Address,
    /// The pool contract address.
    pub address: Address,
}

/// Refuse an address-keyed identity that diverges from the registered one.
///
/// # Errors
///
/// [`IdentityDivergence`] when any field differs.
pub fn verify_address_identity(
    minted: AddressIdentity,
    registered: AddressIdentity,
) -> Result<(), IdentityDivergence> {
    if minted == registered {
        return Ok(());
    }
    Err(IdentityDivergence::MintedVsRegistered {
        address: minted.address,
        detail: format!("minted {minted:?} != registered {registered:?}"),
    })
}

/// The V4 construction identity echo `(currency0, currency1, fee,
/// tick_spacing)` — the immutable-key fields the resolver and the builder must
/// agree on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(test), expect(dead_code))]
pub struct V4Identity {
    /// The pool key's first currency.
    pub currency0: Address,
    /// The pool key's second currency.
    pub currency1: Address,
    /// The pool key's fee.
    pub fee: u32,
    /// The pool key's tick spacing.
    pub tick_spacing: i32,
}

/// Refuse a V4 identity that diverges from the registered one.
///
/// # Errors
///
/// [`IdentityDivergence`] when any field differs.
#[cfg_attr(not(test), expect(dead_code))]
pub fn verify_v4_identity(
    minted: V4Identity,
    registered: V4Identity,
) -> Result<(), IdentityDivergence> {
    if minted == registered {
        return Ok(());
    }
    Err(IdentityDivergence::MintedVsRegistered {
        address: minted.currency0,
        detail: format!("minted {minted:?} != registered {registered:?}"),
    })
}

/// Read the registered address-keyed identity for `pool_id` off the shared
/// state (the V2 or V3 identity, whichever the pool registered as). `None` when
/// no address-keyed identity is registered for the id.
#[must_use]
pub fn registered_address_identity(bot: &Bot, pool_id: u64) -> Option<AddressIdentity> {
    let core = bot.state_arc();
    let state = core.read_at(LockSite::Core);
    if let Some(i) = state.pool_slice::<degenbot_pools::V2PoolIdentity>(pool_id) {
        return Some(AddressIdentity {
            token0: i.token0,
            token1: i.token1,
            address: i.address,
        });
    }
    state
        .pool_slice::<degenbot_pools::V3PoolIdentity>(pool_id)
        .map(|i| AddressIdentity {
            token0: i.token0,
            token1: i.token1,
            address: i.address,
        })
}

#[cfg(test)]
mod tests {
    #![expect(clippy::expect_used, clippy::panic)] // test assertions read the refusal directly; the divergence arms assert the variant by construction

    use super::*;

    fn a(b: u8) -> Address {
        Address::new([b; 20])
    }

    #[test]
    fn address_identity_parity_accepts_a_match_and_refuses_a_divergence() {
        let registered = AddressIdentity {
            token0: a(1),
            token1: a(2),
            address: a(3),
        };
        assert!(verify_address_identity(registered, registered).is_ok());

        // Fault injection: a swapped minted identity (the wrong identity) must
        // be refused — this is the address-keyed red path.
        let minted = AddressIdentity {
            token0: a(2),
            token1: a(1),
            address: a(3),
        };
        let err = verify_address_identity(minted, registered).expect_err("divergence refused");
        match err {
            IdentityDivergence::MintedVsRegistered { address, detail } => {
                assert_eq!(address, a(3));
                assert!(detail.contains("minted"), "{detail}");
            }
            other @ IdentityDivergence::Create2(_) => {
                panic!("a minted-vs-registered divergence: {other:?}")
            }
        }
    }

    #[test]
    fn v4_identity_parity_accepts_a_match_and_refuses_a_wrong_fee() {
        let registered = V4Identity {
            currency0: a(4),
            currency1: a(5),
            fee: 3000,
            tick_spacing: 60,
        };
        assert!(verify_v4_identity(registered, registered).is_ok());

        // Fault injection: a wrong fee decode (the wrong identity) must be
        // refused — this is the V4 red path.
        let minted = V4Identity {
            fee: 500,
            ..registered
        };
        let err = verify_v4_identity(minted, registered).expect_err("divergence refused");
        match err {
            IdentityDivergence::MintedVsRegistered { address, .. } => assert_eq!(address, a(4)),
            other @ IdentityDivergence::Create2(_) => {
                panic!("a minted-vs-registered divergence: {other:?}")
            }
        }
    }

    /// The shared `CREATE2` verdict renders the deployment table's own
    /// mismatch text verbatim, so the register path's historical `ValueError`
    /// is unchanged by the collapse (ADR-077 D3).
    #[test]
    fn create2_divergence_renders_the_source_rendering_verbatim() {
        let mismatch = AddressMismatch {
            chain_id: 1,
            factory: a(1),
            deployer: a(2),
            init_hash: alloy::primitives::B256::ZERO,
            expected: a(3),
            computed: a(4),
        };
        let divergence = IdentityDivergence::Create2(mismatch);
        assert_eq!(divergence.to_string(), mismatch.to_string());
        assert!(divergence.to_string().contains("CREATE2"));
    }
}
