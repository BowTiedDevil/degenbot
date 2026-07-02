//! Pure-Rust cmd-executor domain — simulation warmup-slot storage math.
//!
//! A pyo3-free *core leaf* that ports the Solidity storage-slot math from the
//! Python reference (`examples/cmd_stream.py`) feeding `eth_simulateV1`
//! warmup-slot overrides. These pre-warm three storage slots so injected
//! runtime bytecode (no `initialize()` call) sees warm storage, replicating
//! `cmd_executor.initialize()`'s cold-SSTORE avoidance (~22,100 gas/slot).
//!
//! # Storage-layout domain knowledge (mirrors the Python oracle)
//!
//! - **WETH9** `balanceOf` at mapping slot **3** (`name`@0, `symbol`@1,
//!   `decimals`@2 — all occupy storage, not `constant`).
//! - **PoolManager** ERC6909 `balanceOf` at mapping slot **4** (C3 linearization
//!   of `PoolManager is ProtocolFees, ERC6909Claims, …`: `owner`@0,
//!   `pendingOwner`@1, `isOperator`@2, `protocolFeesAccrued`@3, `balanceOf`@4).
//! - **ERC6909 id** = `uint160(currency)` per `CurrencyLibrary.toId()` — the
//!   native id is `uint160(address(0)) = 0`.
//!
//! The [`WarmupSlots`] struct carries the three computed **slot addresses**
//! only (`U256`); the warmed balance values + the `eth_simulateV1`
//! `{address: {"stateDiff": {slot_hex: value_hex}}}` dict shape are a thin
//! PyO3 adapter concern (cutover task), not this leaf.
//!
//! # Parity
//!
//! Byte-for-byte parity vs `examples/cmd_stream.py::compute_simulation_warmup_slots`
//! (§4.2). The Python oracle is retained as the parity reference — it is NOT
//! deleted. Slot hexes are the canonical mainnet WETH (`0xC02…`) / PoolManager
//! (`0x0000…444c`) slots.

// Solidity/ERC identifiers (balanceOf, ERC6909, protocolFeesAccrued, …) are
// ubiquitous in this crate's docs; allow the pedantic doc-markdown lint to
// match the peer math crates (degenbot-solidly-math, -balancer-math, …).
#![allow(clippy::doc_markdown)]

use alloy::primitives::{keccak256, Address, U256};

pub mod composers;
pub mod encoders;

/// The WETH9 `balanceOf` mapping storage slot (`name`@0, `symbol`@1,
/// `decimals`@2, `balanceOf`@3).
pub const WETH9_BALANCE_OF_SLOT: u64 = 3;

/// The PoolManager ERC6909 `balanceOf` mapping storage slot (C3 linearization:
/// `owner`@0, `pendingOwner`@1, `isOperator`@2, `protocolFeesAccrued`@3,
/// `balanceOf`@4).
pub const POOL_MANAGER_ERC6909_BALANCE_OF_SLOT: u64 = 4;

/// The three computed warmup **slot addresses** for `eth_simulateV1`
/// `stateDiff` overrides.
///
/// Carries slot addresses only — the warmed balance value (1 wei) and the
/// `stateDiff` dict shape are the cutover-task PyO3 adapter concern.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WarmupSlots {
    /// WETH9 `balanceOf(executor)` mapping slot — warmed so the executor's WETH
    /// ERC20 balance slot is hot.
    pub weth_balance: U256,
    /// PoolManager ERC6909 `balanceOf(executor, weth_id)` nested-mapping slot —
    /// the primary warmup slot enabling gas-efficient `V4_MINT` profit capture.
    pub erc6909_weth: U256,
    /// PoolManager ERC6909 `balanceOf(executor, native_id)` nested-mapping slot
    /// — warmed for paths that mint native ETH as ERC6909.
    pub erc6909_native: U256,
}

/// Compute a Solidity mapping storage slot: `keccak256(key ‖ base_slot)`,
/// both 32-byte big-endian.
///
/// Ports `examples/cmd_stream.py::mapping_slot` (L884–L891). **Layout note:**
/// the key is concatenated *before* the base slot (`key ‖ base_slot`, not
/// `base_slot ‖ key`) — this is the parity-sensitive point.
#[must_use]
pub fn mapping_slot(base_slot: U256, key: U256) -> U256 {
    let mut preimage = [0u8; 64];
    preimage[0..32].copy_from_slice(&key.to_be_bytes::<32>());
    preimage[32..64].copy_from_slice(&base_slot.to_be_bytes::<32>());
    U256::from_be_bytes(keccak256(preimage).0)
}

/// Compute the storage slot for `mapping[key1][key2]` at `base_slot`:
/// `mapping_slot(mapping_slot(base_slot, key1), key2)`.
///
/// Ports `examples/cmd_stream.py::_nested_mapping_slot` (L894–L916).
#[must_use]
pub fn nested_mapping_slot(base_slot: U256, key1: U256, key2: U256) -> U256 {
    mapping_slot(mapping_slot(base_slot, key1), key2)
}

/// Build a `U256` from a 64-char big-endian hex string (no `0x` prefix), at
/// const-eval time. Used for the parity fixtures + the `MASK_160` const.
const fn u256_from_hex(hex: &str) -> U256 {
    let bytes = hex.as_bytes();
    let mut b = [0u8; 32];
    let mut i = 0;
    while i < 32 {
        let hi = hex_digit(bytes[2 * i]);
        let lo = hex_digit(bytes[2 * i + 1]);
        b[i] = hi * 16 + lo;
        i += 1;
    }
    U256::from_be_bytes(b)
}

const fn hex_digit(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0',
        b'a'..=b'f' => c - b'a' + 10,
        b'A'..=b'F' => c - b'A' + 10,
        _ => 0,
    }
}

/// The low-160-bit mask (`2^160 − 1`) for the ERC6909 `uint160(currency)`
/// truncation. Built as a const so the shift never touches the overflowing
/// `u128` domain.
const MASK_160: U256 =
    u256_from_hex("000000000000000000000000ffffffffffffffffffffffffffffffffffffffff");

/// Derive the ERC6909 token id for a currency: `uint160(currency)` per
/// `CurrencyLibrary.toId()` (the low 160 bits of the address's integer form).
///
/// The native id (`uint160(address(0))`) is `0`.
#[must_use]
pub fn erc6909_id(currency: Address) -> U256 {
    // address(0) → 0; any address → its low 160 bits (a no-op on a valid
    // `Address`, which is type-guaranteed ≤160 bits, but mirrors the Python
    // oracle's `int(addr,16) & ((1<<160)-1)` for fidelity).
    U256::from_be_bytes(currency.into_word().0) & MASK_160
}

/// Compute the three `eth_simulateV1` warmup slot addresses that replicate the
/// effect of `cmd_executor.initialize()`.
///
/// Ports `examples/cmd_stream.py::compute_simulation_warmup_slots`
/// (L919–L1054). Returns the **slot addresses** only (typed struct); the 1-wei
/// warmed values + the `stateDiff` dict shape are the cutover-task PyO3
/// adapter.
///
/// # Arguments
///
/// - `executor` — the cmd_executor contract address.
/// - `weth` — the WETH9 contract address.
/// - `pool_manager` — the Uniswap V4 PoolManager contract address (the
///   ERC6909-balance host); present for API fidelity with the Python oracle.
#[must_use]
pub fn compute_simulation_warmup_slots(
    executor: Address,
    weth: Address,
    _pool_manager: Address,
) -> WarmupSlots {
    let executor_slot_key = U256::from_be_bytes(executor.into_word().0);
    let weth_id = erc6909_id(weth);
    let native_id = U256::ZERO; // uint160(address(0)) = 0

    let base_slot = U256::from(WETH9_BALANCE_OF_SLOT);
    let pm_slot = U256::from(POOL_MANAGER_ERC6909_BALANCE_OF_SLOT);

    WarmupSlots {
        weth_balance: mapping_slot(base_slot, executor_slot_key),
        erc6909_weth: nested_mapping_slot(pm_slot, executor_slot_key, weth_id),
        erc6909_native: nested_mapping_slot(pm_slot, executor_slot_key, native_id),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::too_many_lines)]

    use super::*;
    use alloy::primitives::{address, Address};

    /// Canonical mainnet WETH9 and PoolManager (per the parity corpus
    /// `tests/arbitrage/test_warmup_slots_gas.py`).
    const WETH: Address = address!("c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");
    const PM: Address = address!("000000000004444c5dc75cb358380d2e3de08a90");

    /// `uint160(WETH)` — the ERC6909 WETH id (computed by the Python oracle).
    const WETH_ID: U256 =
        u256_from_hex("000000000000000000000000c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");

    // ---------------------------------------------------------------------------
    // Parity fixtures — generated directly from the Python oracle
    // `examples/cmd_stream.py::compute_simulation_warmup_slots` over the three
    // executor addresses used in the parity corpus:
    //   - `0xAA…AA`  (the canonical `EXECUTOR_ADDRESS` in test_warmup_slots_gas.py)
    //   - `0xDeAd…0001` / `0xDeAd…0002` (the eth_simulateV1 test executors)
    // Each row: (executor, WETH, PM) → (weth_balance, erc6909_weth,
    //           erc6909_native) as U256 + the 064x hex the stateDiff dict uses.
    // ---------------------------------------------------------------------------
    type Fixture = (
        Address,      // executor
        U256,         // weth_balance_slot
        &'static str, // weth_balance_slot_hex (064x)
        U256,         // erc6909_weth_slot
        &'static str, // erc6909_weth_slot_hex
        U256,         // erc6909_native_slot
        &'static str, // erc6909_native_slot_hex
    );

    const FIXTURES: &[Fixture] = &[
        (
            address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            u256_from_hex("ca0453669a7127ce38f304ce121e552d78c30286022ebefeef6884684816084d"),
            "ca0453669a7127ce38f304ce121e552d78c30286022ebefeef6884684816084d",
            u256_from_hex("c87651b1e38cc90cedd910b68ad3a33c54f8132e8e50beaae3ca68aafafe854a"),
            "c87651b1e38cc90cedd910b68ad3a33c54f8132e8e50beaae3ca68aafafe854a",
            u256_from_hex("27b77f9e86613e8da78f13fe38575cf532b4167ab8d8d5303c689cc9fd0ed7ff"),
            "27b77f9e86613e8da78f13fe38575cf532b4167ab8d8d5303c689cc9fd0ed7ff",
        ),
        (
            address!("dead000000000000000000000000000000000001"),
            u256_from_hex("f35974400be343ad66717b2a38de57c05ec39411b31023551415263aad6916c0"),
            "f35974400be343ad66717b2a38de57c05ec39411b31023551415263aad6916c0",
            u256_from_hex("95c453f6b7d6cf8b9823c709b3f0fedc6e6a884d3f9f5bdc510e8822c1fb31d5"),
            "95c453f6b7d6cf8b9823c709b3f0fedc6e6a884d3f9f5bdc510e8822c1fb31d5",
            u256_from_hex("b2e80833314f92ac980f4c8d9255290f04bbb025949387963ffc8a62ffb875c1"),
            "b2e80833314f92ac980f4c8d9255290f04bbb025949387963ffc8a62ffb875c1",
        ),
        (
            address!("dead000000000000000000000000000000000002"),
            u256_from_hex("f189b9f9855f3e9ba1ed2b62d0daf3b7a19d5a80bc3be3cf7a43f4d3f7324366"),
            "f189b9f9855f3e9ba1ed2b62d0daf3b7a19d5a80bc3be3cf7a43f4d3f7324366",
            u256_from_hex("5dff84ecba6fdee6e82672cd2d229a766608bc3d85c630594b0ac4bd1082be3c"),
            "5dff84ecba6fdee6e82672cd2d229a766608bc3d85c630594b0ac4bd1082be3c",
            u256_from_hex("394274c6b1bd4f5ef5f3123f467627e5d50a0fcb9c065c4b31a2b16774aaf2a6"),
            "394274c6b1bd4f5ef5f3123f467627e5d50a0fcb9c065c4b31a2b16774aaf2a6",
        ),
    ];

    #[test]
    fn parity_vs_python_oracle() {
        for &(executor, weth_slot, weth_hex, erc_weth, erc_weth_hex, erc_native, erc_native_hex) in
            FIXTURES
        {
            let slots = compute_simulation_warmup_slots(executor, WETH, PM);
            assert_eq!(
                slots.weth_balance, weth_slot,
                "WETH balance slot (executor {executor:?})"
            );
            assert_eq!(
                slots.erc6909_weth, erc_weth,
                "ERC6909 WETH slot (executor {executor:?})"
            );
            assert_eq!(
                slots.erc6909_native, erc_native,
                "ERC6909 native slot (executor {executor:?})"
            );

            // The stateDiff dict keys are the 064x hex of the slot — confirm the
            // hex round-trip matches the oracle's `f"0x{slot:064x}"`.
            assert_eq!(
                format!("{:064x}", slots.weth_balance),
                weth_hex,
                "WETH balance slot hex (executor {executor:?})"
            );
            assert_eq!(
                format!("{:064x}", slots.erc6909_weth),
                erc_weth_hex,
                "ERC6909 WETH slot hex (executor {executor:?})"
            );
            assert_eq!(
                format!("{:064x}", slots.erc6909_native),
                erc_native_hex,
                "ERC6909 native slot hex (executor {executor:?})"
            );
        }
    }

    // ---------------------------------------------------------------------------
    // mapping_slot / nested_mapping_slot primitives — direct parity + layout.
    // ---------------------------------------------------------------------------

    /// The mapping_slot composition is `keccak256(key ‖ base_slot)` (key FIRST),
    /// matching the Python oracle's `key.to_bytes(32,"big") + base_slot.to_bytes(32,"big")`.
    /// Cross-checks against the parity corpus's manual keccak (L271–L278):
    ///   keccak256(executor_32bytes ‖ 3_32bytes).
    #[test]
    fn mapping_slot_layout_is_key_then_base_slot() {
        let executor = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let executor_u = U256::from_be_bytes(executor.into_word().0);
        let got = mapping_slot(U256::from(3u64), executor_u);

        // Manual: keccak256(executor_bytes32 ‖ 3_bytes32).
        let mut preimage = [0u8; 64];
        preimage[0..32].copy_from_slice(&executor.into_word().0);
        preimage[32..64].copy_from_slice(&U256::from(3u64).to_be_bytes::<32>());
        let want = U256::from_be_bytes(keccak256(preimage).0);

        assert_eq!(got, want);
        // And the mainnet-parity WETH balance slot value:
        assert_eq!(
            got,
            u256_from_hex("ca0453669a7127ce38f304ce121e552d78c30286022ebefeef6884684816084d")
        );
    }

    /// `nested_mapping_slot` = `mapping_slot(mapping_slot(base, k1), k2)`.
    #[test]
    fn nested_mapping_slot_composition() {
        let base = U256::from(4u64);
        let k1 = U256::from_be_bytes(
            address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
                .into_word()
                .0,
        );
        let k2 = WETH_ID;

        let got = nested_mapping_slot(base, k1, k2);
        let want = mapping_slot(mapping_slot(base, k1), k2);
        assert_eq!(got, want);
        // Mainnet parity: the ERC6909 WETH slot for executor 0xAA…AA.
        assert_eq!(
            got,
            u256_from_hex("c87651b1e38cc90cedd910b68ad3a33c54f8132e8e50beaae3ca68aafafe854a")
        );
    }

    /// The nested slot with `key2 = native_id (0)` is *not* the same as a
    /// single-level `mapping_slot(base, executor)` — confirms the double hash.
    #[test]
    fn nested_native_slot_is_double_hashed() {
        let base = U256::from(4u64);
        let executor = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let k1 = U256::from_be_bytes(executor.into_word().0);

        let native = nested_mapping_slot(base, k1, U256::ZERO);
        let single = mapping_slot(base, k1);
        assert_ne!(native, single);
        // Mainnet parity: the ERC6909 native slot for executor 0xAA…AA.
        assert_eq!(
            native,
            u256_from_hex("27b77f9e86613e8da78f13fe38575cf532b4167ab8d8d5303c689cc9fd0ed7ff")
        );
    }

    // ---------------------------------------------------------------------------
    // ERC6909 id derivation.
    // ---------------------------------------------------------------------------

    #[test]
    fn erc6909_id_is_uint160_of_currency() {
        // uint160(WETH) — the canonical mainnet WETH id.
        assert_eq!(erc6909_id(WETH), WETH_ID);
        assert_eq!(
            erc6909_id(WETH),
            U256::from_be_bytes(WETH.into_word().0) & MASK_160
        );
        // Native: uint160(address(0)) == 0.
        assert_eq!(erc6909_id(Address::ZERO), U256::ZERO);
    }

    // ---------------------------------------------------------------------------
    // Property tests.
    // ---------------------------------------------------------------------------

    /// Invariant: `mapping_slot` is deterministic — same inputs → same slot.
    #[test]
    fn property_mapping_slot_deterministic() {
        use proptest::prelude::*;
        proptest!(|(base in 0u64..u64::MAX, key in 0u64..u64::MAX)| {
            let a = mapping_slot(U256::from(base), U256::from(key));
            let b = mapping_slot(U256::from(base), U256::from(key));
            prop_assert_eq!(a, b);
        });
    }

    /// Invariant: `mapping_slot` outputs are uniformly spread across the U256
    /// space (a keccak collision / extreme clustering would fail this).
    #[test]
    fn property_mapping_slot_spread() {
        use proptest::prelude::*;
        proptest!(|(base in 0u64..u64::MAX, key in 0u64..u64::MAX)| {
            let slot = mapping_slot(U256::from(base), U256::from(key));
            // keccak outputs are effectively uniform — assert the slot is not
            // trivially small (it should exceed a u64 for any input, since
            // keccak256 of a 64-byte preimage never lands below ~2^190 in
            // practice; this catches accidental truncation to low bytes).
            prop_assert!(slot > U256::from(u64::MAX));
        });
    }

    /// Invariant: distinct keys yield distinct slots (no collisions over a
    /// modest sweep — keccak behaves as a random oracle).
    #[test]
    fn property_mapping_slot_no_collision_over_sweep() {
        let base = U256::from(3u64);
        let mut seen = std::collections::HashSet::new();
        for key in 0u64..1000 {
            assert!(
                seen.insert(mapping_slot(base, U256::from(key))),
                "collision at base=3, key={key}"
            );
        }
    }
}
