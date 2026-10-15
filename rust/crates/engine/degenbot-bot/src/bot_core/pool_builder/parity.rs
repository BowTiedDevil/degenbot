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

use alloy::primitives::Address;
use degenbot_substrate::state_lock::LockSite;

use crate::bot_core::Bot;

/// The construction identity-parity refusal: the minted identity disagreed
/// with the identity the registry of record holds for the just-registered
/// pool.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("construction identity parity violated for {address}: {detail}")]
pub struct IdentityDivergence {
    /// The pool the divergence concerns (the minted echo's address).
    pub address: Address,
    /// The field-by-field divergence, for the shell's diagnostic message.
    pub detail: String,
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
    Err(IdentityDivergence {
        address: minted.address,
        detail: format!("minted {minted:?} != registered {registered:?}"),
    })
}

/// The V4 construction identity echo `(currency0, currency1, fee,
/// tick_spacing)` — the immutable-key fields the resolver and the builder must
/// agree on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
pub fn verify_v4_identity(
    minted: V4Identity,
    registered: V4Identity,
) -> Result<(), IdentityDivergence> {
    if minted == registered {
        return Ok(());
    }
    Err(IdentityDivergence {
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
    if let Some(i) = state.pool_slice::<degenbot_pools::v2_state::V2PoolIdentity>(pool_id) {
        return Some(AddressIdentity {
            token0: i.token0,
            token1: i.token1,
            address: i.address,
        });
    }
    state
        .pool_slice::<degenbot_pools::v3_state::V3PoolIdentity>(pool_id)
        .map(|i| AddressIdentity {
            token0: i.token0,
            token1: i.token1,
            address: i.address,
        })
}

#[cfg(test)]
mod tests {
    #![expect(clippy::expect_used)] // test assertions read the refusal directly

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
        assert_eq!(err.address, a(3));
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
        assert_eq!(err.address, a(4));
    }
}
