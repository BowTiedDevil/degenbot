//! `TxSigner` — holds the operator private key ONCE via
//! [`alloy::signers::local::PrivateKeySigner`] (a `LocalSigner<k256 SigningKey>`)
//! and signs type-2 EIP-1559 transactions against a pinned `chain_id`.
//!
//! # Architectural role (ADR-005)
//!
//! Today the Python oracle crosses the operator private key into
//! `eth_account.Account.sign_transaction(...)` **per transaction**
//! (`examples/eth_backrun_v2_v3_v4_rust.py` L2638), a security smell: the key
//! material round-trips into Python on every submission. [`TxSigner`]
//! receives the key bytes/hex **once** at construction (via the `PyO3` wrapper)
//! and holds it in Rust for the signer's lifetime — the key never crosses
//! back into Python per-tx. The signing itself is **synchronous ECDSA**
//! (secp256k1, RFC 6979 deterministic nonces) — CPU-bound, no network — so
//! the `PyO3` wrapper releases the GIL around it (`py.detach`).
//!
//! # Parity (ADR-005 §4.2)
//!
//! The §4.2 oracle is `eth_account.Account.sign_transaction(transaction_dict=tx_params,
//! private_key=operator_private_key).raw_transaction`. Because both `eth_account`
//! and `alloy-signer-local` use RFC 6979 deterministic ECDSA over secp256k1, a
//! pinned key + `tx_params` produces **byte-for-byte identical** raw signed
//! `bytes` in Rust and Python — pinned in the §4.2 test against a canonical
//! (anvil account 0) fixture.

use crate::error::{SubmissionError, SubmissionResult};
use crate::params::TxParams;
use alloy::consensus::{
    transaction::SignerRecoverable, SignableTransaction, TxEip1559, TxEnvelope,
};
use alloy::eips::{eip2718::Decodable2718, Encodable2718};
use alloy::network::TxSignerSync;
use alloy::primitives::{Address, Bytes, TxKind};
use alloy::signers::local::PrivateKeySigner;

/// Holds the operator private key once + the chain id for EIP-155 replay
/// protection, and signs type-2 EIP-1559 transactions.
///
/// Construct via [`TxSigner::from_key_bytes`] / [`TxSigner::from_key_hex`];
/// the held [`PrivateKeySigner`] is `Send + Sync` and cloneable (the key
/// bytes live inside `k256::ecdsa::SigningKey`), so a `TxSigner` can live
/// behind an `Arc` for shared access from the submission loop.
#[derive(Clone)]
pub struct TxSigner {
    signer: PrivateKeySigner,
    chain_id: u64,
}

impl TxSigner {
    /// Construct from a raw 32-byte private key scalar + a chain id.
    ///
    /// # Errors
    ///
    /// Returns [`SubmissionError::Sign`] if the bytes are not a valid
    /// secp256k1 scalar (zero or ≥ the curve order).
    pub fn from_key_bytes(key_bytes: &[u8], chain_id: u64) -> SubmissionResult<Self> {
        let signer =
            PrivateKeySigner::from_slice(key_bytes).map_err(alloy::signers::Error::other)?;
        Ok(Self { signer, chain_id })
    }

    /// Construct from a hex-encoded private key (with or without `0x` prefix)
    /// + a chain id.
    ///
    /// # Errors
    ///
    /// Returns [`SubmissionError::Sign`] if the hex is malformed or decodes
    /// to an invalid secp256k1 scalar.
    pub fn from_key_hex(hex: &str, chain_id: u64) -> SubmissionResult<Self> {
        let hex_stripped = hex.trim_start_matches("0x");
        let key_bytes = alloy::hex::decode(hex_stripped).map_err(|e| {
            SubmissionError::Sign(alloy::signers::Error::other(format!(
                "invalid private key hex: {e}"
            )))
        })?;
        let signer =
            PrivateKeySigner::from_slice(&key_bytes).map_err(alloy::signers::Error::other)?;
        Ok(Self { signer, chain_id })
    }

    /// Wrap an already-constructed [`PrivateKeySigner`] (advanced — for tests
    /// or when the key is loaded by another alloy-native path).
    #[must_use]
    pub fn from_signer(signer: PrivateKeySigner, chain_id: u64) -> Self {
        Self { signer, chain_id }
    }

    /// The derived operator address (the secp256k1 public-key→keccak address).
    #[must_use]
    pub fn address(&self) -> Address {
        self.signer.address()
    }

    /// The chain id used for EIP-155 replay protection on every signed tx.
    #[must_use]
    pub const fn chain_id(&self) -> u64 {
        self.chain_id
    }

    /// Sign a type-2 EIP-1559 transaction and return the raw signed
    /// [`TxEnvelope`] bytes ready for `eth_sendRawTransaction`.
    ///
    /// Builds the `TxEip1559` from `tx_params` (`to`/`data`=`execute()`
    /// calldata/`gas`/`nonce`/`value=0`/`accessList` + the two fee fields
    /// finalized by [`crate::fee::finalize_fees`]), uses the signer's
    /// [`chain_id`](Self::chain_id) for EIP-155 replay protection, signs
    /// synchronously via the [`TxSignerSync`] trait (RFC 6979 deterministic
    /// ECDSA — `eth_account`-parity), and encodes as `Typed2718` (`0x02`
    /// prefix).
    ///
    /// Signing is **synchronous** (CPU-bound secp256k1, no network): the
    /// `PyO3` wrapper releases the GIL around this via `py.detach`.
    ///
    /// # Errors
    ///
    /// Returns [`SubmissionError::Sign`] if the ECDSA signing fails (key
    /// corruption; never for a validly-constructed signer).
    pub fn sign_eip1559(&self, tx_params: &TxParams) -> SubmissionResult<Bytes> {
        let mut tx = TxEip1559 {
            chain_id: self.chain_id,
            nonce: tx_params.nonce,
            gas_limit: tx_params.gas_limit,
            max_fee_per_gas: tx_params.max_fee_per_gas,
            max_priority_fee_per_gas: tx_params.max_priority_fee_per_gas,
            to: TxKind::Call(tx_params.to),
            value: tx_params.value,
            access_list: tx_params.access_list.clone(),
            input: tx_params.data.clone(),
        };

        // Synchronous ECDSA signing — RFC 6979 deterministic nonces, matching
        // eth_account byte-for-byte (proven in §4.2 parity test).
        let sig = self.signer.sign_transaction_sync(&mut tx)?;
        let signed = tx.into_signed(sig);
        let envelope: TxEnvelope = signed.into();
        Ok(envelope.encoded_2718().into())
    }

    /// Decode a raw signed [`TxEnvelope`] (the bytes produced by
    /// [`sign_eip1559`](Self::sign_eip1559)) and recover the sender address.
    ///
    /// Exposed as a static helper for the submission loop's receipt-validation
    /// path + the §4.2 round-trip parity test: decode the bytes you signed →
    /// recover the signer → assert it equals [`address`](Self::address).
    ///
    /// # Errors
    ///
    /// Returns [`SubmissionError::Sign`] if the bytes do not decode as a
    /// typed transaction or the signature does not recover.
    pub fn recover_sender(raw_signed: &[u8]) -> SubmissionResult<Address> {
        let envelope = TxEnvelope::decode_2718(&mut &raw_signed[..])?;
        Ok(envelope.recover_signer()?)
    }
}

impl std::fmt::Debug for TxSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TxSigner")
            .field("address", &self.address())
            .field("chain_id", &self.chain_id)
            // The signer intentionally does NOT expose the key bytes — not even
            // in Debug output (do not leak key material in logs/panic output).
            .field("signer", &"<redacted>")
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fee::finalize_fees;
    use alloy::consensus::{Transaction, TxEnvelope};
    use alloy::eips::eip2718::Decodable2718;
    use alloy::primitives::U256;

    // Anvil account 0 — the canonical well-known test private key + address.
    const ANVIL_KEY_HEX: &str = "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
    const ANVIL_ADDR: &str = "0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266";

    // ──Construction + address parity──────────────────────────────────────────

    #[test]
    fn from_key_hex_derives_anvil_address() {
        let signer = TxSigner::from_key_hex(ANVIL_KEY_HEX, 1).unwrap();
        assert_eq!(format!("{:?}", signer.address()), ANVIL_ADDR);
        assert_eq!(signer.chain_id(), 1);
    }

    #[test]
    fn from_key_bytes_derivates_same_address_as_hex() {
        let bytes = alloy::hex::decode(ANVIL_KEY_HEX).unwrap();
        let from_bytes = TxSigner::from_key_bytes(&bytes, 1).unwrap();
        let from_hex = TxSigner::from_key_hex(ANVIL_KEY_HEX, 1).unwrap();
        assert_eq!(from_bytes.address(), from_hex.address());
    }

    #[test]
    fn from_key_hex_accepts_zerox_prefix() {
        let with_prefix = format!("0x{ANVIL_KEY_HEX}");
        let signer = TxSigner::from_key_hex(&with_prefix, 1).unwrap();
        assert_eq!(format!("{:?}", signer.address()), ANVIL_ADDR);
    }

    #[test]
    fn rejects_zero_private_key() {
        let zero_bytes = [0u8; 32];
        assert!(TxSigner::from_key_bytes(&zero_bytes, 1).is_err());
    }

    #[test]
    fn rejects_malformed_hex() {
        assert!(TxSigner::from_key_hex("nothex", 1).is_err());
        assert!(TxSigner::from_key_hex("0xdeadbeef", 1).is_err()); // too short
    }

    #[test]
    fn debug_does_not_leak_key_material() {
        let signer = TxSigner::from_key_hex(ANVIL_KEY_HEX, 1).unwrap();
        let dbg = format!("{signer:?}");
        assert!(!dbg.contains(ANVIL_KEY_HEX));
        assert!(dbg.contains("redacted"));
    }

    // ──§4.2 byte-exact parity vs eth_account─────────────────────────────────

    #[test]
    fn signed_bytes_match_eth_account_oracle_byte_for_byte() {
        // Pinned fixture: anvil key 0, chain 1, nonce 7, gas 250_000,
        // maxFeePerGas 30 gwei, maxPriorityFeePerGas 2 gwei, to=zero,
        // value=0, data=[0x12,0x34,0x56,0x78], empty access list.
        //
        // eth_account.Account.sign_transaction(...) produces EXACTLY:
        let expected = alloy::hex::decode(
            "02f870010784773594008506fc23ac008303d09094\
             000000000000000000000000000000000000000080\
             8412345678c001a0e4d0045433dd450a5f3f754b622551da4ccad4626e10e044d140960f39b37ad8a0\
             724c6ade60f7cae109ccff66ec35e7387977d0bd2e0e94cad3f5809e966ca3f4",
        )
        .unwrap();

        let signer = TxSigner::from_key_hex(ANVIL_KEY_HEX, 1).unwrap();
        let mut params = TxParams::new(
            Address::ZERO,
            Bytes::from_static(&[0x12, 0x34, 0x56, 0x78]),
            250_000,
            7,
        );
        // Pin the fee fields exactly (no finalize_fees indirection — the raw
        // 30 gwei / 2 gwei values are what the oracle signed).
        params.max_fee_per_gas = 30_000_000_000;
        params.max_priority_fee_per_gas = 2_000_000_000;

        let raw = signer.sign_eip1559(&params).unwrap();
        assert_eq!(
            &raw[..],
            &expected[..],
            "raw signed bytes must match eth_account"
        );
    }

    // ──Round-trip decode + signer recovery────────────────────────────────────

    #[test]
    fn signed_tx_decodes_and_recovers_signer() {
        let signer = TxSigner::from_key_hex(ANVIL_KEY_HEX, 1).unwrap();
        let mut params = TxParams::new(
            Address::ZERO,
            Bytes::from_static(&[0xde, 0xad, 0xbe, 0xef]),
            100_000,
            42,
        );
        finalize_fees(&mut params, 10_000_000_000, 1_000_000_000).unwrap();

        let raw = signer.sign_eip1559(&params).unwrap();
        assert_eq!(raw[0], 0x02, "type-2 EIP-1559 prefix");

        let recovered = TxSigner::recover_sender(&raw).unwrap();
        assert_eq!(recovered, signer.address());
    }

    #[test]
    fn round_trip_all_tx_params_fields_preserved() {
        let signer = TxSigner::from_key_hex(ANVIL_KEY_HEX, 1).unwrap();
        let mut params = TxParams::new(
            Address::ZERO,
            Bytes::from_static(&[0xab, 0xcd]),
            500_000,
            99,
        );
        finalize_fees(&mut params, 15_000_000_000, 3_000_000_000).unwrap();
        // Move some value to exercise the value field.
        params.value = U256::from(1_000_000_000_000_000_000u128); // 1 ether

        let raw = signer.sign_eip1559(&params).unwrap();
        let envelope = TxEnvelope::decode_2718(&mut &raw[..]).unwrap();

        match &envelope {
            TxEnvelope::Eip1559(signed_tx) => {
                let tx = signed_tx.tx();
                assert_eq!(tx.nonce(), 99);
                assert_eq!(tx.gas_limit(), 500_000);
                assert_eq!(tx.max_fee_per_gas(), 25_500_000_000); // int(1.5*15gwei)+3gwei = 22.5+3 = 25.5gwei
                assert_eq!(tx.max_priority_fee_per_gas(), Some(3_000_000_000));
                assert_eq!(tx.chain_id(), Some(1));
                assert_eq!(tx.to(), Some(Address::ZERO));
                assert_eq!(tx.value(), U256::from(1_000_000_000_000_000_000u128));
                assert_eq!(tx.input(), &Bytes::from_static(&[0xab, 0xcd]));
            }
            _ => panic!("expected Eip1559 variant"),
        }
        assert_eq!(envelope.recover_signer().unwrap(), signer.address());
    }

    #[test]
    fn chain_id_applied_for_replay_protection() {
        // Same key + tx_params signed against two different chain_ids must
        // produce different raw bytes (EIP-155 replay protection).
        let signer_mainnet = TxSigner::from_key_hex(ANVIL_KEY_HEX, 1).unwrap();
        let signer_sepolia = TxSigner::from_key_hex(ANVIL_KEY_HEX, 11_155_111).unwrap();
        let mut params = TxParams::new(Address::ZERO, Bytes::from_static(&[0x01]), 210_000, 1);
        finalize_fees(&mut params, 10_000_000_000, 1_000_000_000).unwrap();

        let raw_mainnet = signer_mainnet.sign_eip1559(&params).unwrap();
        let raw_sepolia = signer_sepolia.sign_eip1559(&params).unwrap();
        assert_ne!(&raw_mainnet[..], &raw_sepolia[..]);
        // Each recovers to the same operator address (key is the same).
        assert_eq!(
            TxSigner::recover_sender(&raw_mainnet).unwrap(),
            TxSigner::recover_sender(&raw_sepolia).unwrap(),
        );
    }
}
