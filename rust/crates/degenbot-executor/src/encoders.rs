//! Command-stream primitives — opcode constants, `AddressTable`, `pack_config`,
//! and a `pub fn enc_*` builder for every opcode (`0x00`–`0x59`, `0xFF`).
//!
//! A pyo3-free *core leaf* implementing the tightly-packed command-bytecode
//! layout. The authoritative contract layout is `contracts/README.md`
//! "Command-Stream Executor" (opcode | name | encoding | description); Vyper
//! source `~/code/executor/contracts/cmd_executor.vy` is the single source of
//! truth for the wire format. These Rust `enc_*` builders are canonical
//! (ADR-005); the per-opcode encoding tables below describe every field.
//!
//! # Scope
//!
//! The primitive encoders + `AddressTable` + `pack_config` + `make_pool_key`
//! ONLY. The per-path-type composers (in `composers.rs`) and the PyO3
//! wrappers are sibling / cutover tasks. `# Errors` doc sections appear on
//! every `pub fn` returning `Result`.
//!
//! # Parity (§4.2 hard gate)
//!
//! Byte-for-byte parity vs the `cmd_executor` contract layout over a fixture
//! corpus; the expected hex is embedded in the `tests` module below and is
//! re-derived from these `enc_*` primitives, not from any external oracle.
//!
//! # V4 sign convention (§10.2)
//!
//! V4 uses negative `amountSpecified`; the compact uint96 amount accepted by
//! `enc_v4_swap_compact` / `enc_v4_batch` is **positive** (exact-input) — the
//! contract negates it internally. Documented per-fn.

use std::collections::HashMap;

use alloy::primitives::{Address, U256};

// ── Sentinel address indices ────────────────────────────────────────────────
// Resolved by the contract without SET_ADDRESS or TLOAD. Only 4 protocol
// sentinels exist (0xFC–0xFF). Per-path tokens (USDC, WBTC, …) are NOT baked
// into the contract — they go through the t_addresses table via SET_ADDRESS.

/// PoolManager (immutable, set at deploy).
pub const SENTINEL_PM: u8 = 0xFC;
/// `self` / executor address.
pub const SENTINEL_SELF: u8 = 0xFD;
/// WETH (immutable, set at deploy).
pub const SENTINEL_WETH: u8 = 0xFE;
/// `address(0)` / `NATIVE_ADDRESS` — also the "no hooks" flag.
pub const SENTINEL_NATIVE: u8 = 0xFF;
/// `idx >= SENTINEL_THRESHOLD` is a protocol sentinel; `< it` is a table index.
pub const SENTINEL_THRESHOLD: u8 = 0xFC;
/// `t_addresses` table capacity — must match `MAX_INDEXED_ADDRESSES` in
/// `cmd_executor.vy`.
pub const MAX_INDEXED_ADDRESSES: usize = 32;

/// `address(0)` — the native-ETH / "no address" sentinel address.
pub const NATIVE_ADDRESS: Address = Address::ZERO;

/// The largest V4 static `fee` the cmd_executor can encode (ergo DPODAZ).
///
/// Both `V4_SWAP_COMPACT` and `V4_SWAP_DYNAMIC` encode `fee` as a **2-byte**
/// field (`push_u16`); the contract decodes `fee = (pkh >> 32) & 65535`,
/// masking to `u16`. A static fee `> u16::MAX` (65535) is protocol-valid
/// (`< 1 << 24`, not the dynamic-fee flag `0x800000`) but cannot be encoded by
/// the executor. Such pools are also unprofitable (32%+ per swap) and are
/// rejected at V4 admission (`BotState::register_v4_pool`) rather than wasting
/// a solve + encode-fail cycle.
///
/// `0x1_0000 = 65_536` is the first fee value the 2-byte field cannot hold.
pub const V4_FEE_ENCODER_MAX: u32 = 0x1_0000;

// ── Command opcodes ─────────────────────────────────────────────────────────
// Only 0x00 (SET_ADDRESS) and 0xFF (BEGIN_EXECUTION) are preprocessing
// opcodes. 0x01–0x03 are reserved — their old SKIP_PROFIT_CHECK / BRIBE behavior
// moved into the packed `config` ABI param of `execute()`; emitting them
// reverts (InvalidCommand).

/// `SET_ADDRESS` — append an address to the lookup table.
pub const CMD_SET_ADDRESS: u8 = 0x00;

/// `ERC20_TRANSFER` — transfer an ERC-20 (uint96 amount).
pub const CMD_ERC20_TRANSFER: u8 = 0x10;
/// `ERC20_XFER_BALANCE` — transfer an entire ERC-20 balance.
pub const CMD_ERC20_XFER_BALANCE: u8 = 0x11;
/// `WETH_DEPOSIT` — wrap ETH to WETH.
pub const CMD_WETH_DEPOSIT: u8 = 0x12;
/// `WETH_WITHDRAW` — unwrap WETH to ETH.
pub const CMD_WETH_WITHDRAW: u8 = 0x13;
/// `WETH_DEPOSIT_ALL` — wrap all ETH.
pub const CMD_WETH_DEPOSIT_ALL: u8 = 0x14;
/// `WETH_WITHDRAW_ALL` — unwrap all WETH.
pub const CMD_WETH_WITHDRAW_ALL: u8 = 0x15;
/// `SEND_ETH` — send uint96 ETH.
pub const CMD_SEND_ETH: u8 = 0x16;
/// `SEND_ETH_ALL` — send all ETH.
pub const CMD_SEND_ETH_ALL: u8 = 0x17;

/// `V2_SWAP_COMPACT` — V2 swap + forward data (uint96 amount).
pub const CMD_V2_SWAP_COMPACT: u8 = 0x20;
/// `V2_SWAP_CALC` — V2 swap from excess balance.
pub const CMD_V2_SWAP_CALC: u8 = 0x21;
/// `V2_SWAP_DIRECT` — V2 swap, explicit amount.
pub const CMD_V2_SWAP_DIRECT: u8 = 0x22;

/// `V3_SWAP_COMPACT` — V3 swap + auto-pay (uint96 amount).
pub const CMD_V3_SWAP_COMPACT: u8 = 0x30;
/// `V3_SWAP_DELTA` — V3 swap from PM exttload.
pub const CMD_V3_SWAP_DELTA: u8 = 0x31;

/// `V4_SWAP_COMPACT` — V4 swap, explicit amount (uint96).
pub const CMD_V4_SWAP_COMPACT: u8 = 0x40;
/// `V4_SWAP_DYNAMIC` — V4 swap from PM exttload.
pub const CMD_V4_SWAP_DYNAMIC: u8 = 0x41;
/// `V4_BATCH` — multi-swap + auto-settle (max 8).
pub const CMD_V4_BATCH: u8 = 0x42;

/// `V4_UNLOCK` — enter PM unlock context.
pub const CMD_V4_UNLOCK: u8 = 0x50;
/// `V4_TAKE` — take from PM.
pub const CMD_V4_TAKE: u8 = 0x51;
/// `V4_TAKE_COMPACT` — take, uint96 amount.
pub const CMD_V4_TAKE_COMPACT: u8 = 0x52;
/// `V4_TAKE_DELTA` — take from PM exttload.
pub const CMD_V4_TAKE_DELTA: u8 = 0x53;
/// `V4_SYNC` — sync at PM (anytime).
pub const CMD_V4_SYNC: u8 = 0x54;
/// `V4_SETTLE` — settle at PM.
pub const CMD_V4_SETTLE: u8 = 0x55;
/// `V4_SETTLE_DELTA` — settle one currency from exttload.
pub const CMD_V4_SETTLE_DELTA: u8 = 0x56;
/// `V4_SETTLE_ALL` — settle all nonzero deltas.
pub const CMD_V4_SETTLE_ALL: u8 = 0x57;
/// `V4_MINT_COMPACT` — mint ERC6909 (no transfer).
pub const CMD_V4_MINT_COMPACT: u8 = 0x58;
/// `V4_BURN_COMPACT` — burn ERC6909 (no transfer).
pub const CMD_V4_BURN_COMPACT: u8 = 0x59;

/// `BEGIN_EXECUTION` — marks end of preprocessing / start of execution.
pub const BEGIN_EXECUTION: u8 = 0xFF;

/// The exclusive upper bound for a uint96 amount (`2^96`), used to validate
/// every uint96 amount field (amounts ≥ this overflow the 12-byte field).
const UINT96_BOUND: u128 = 1u128 << 96;

/// Errors raised by the command-stream primitive encoders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncoderError {
    /// A uint96 amount field was given a value ≥ `2^96`.
    Uint96Overflow(u128),
    /// A `forward_data` slice exceeded the 1-byte length cap (255 bytes).
    ForwardDataTooLong(usize),
    /// The `AddressTable` is full (`MAX_INDEXED_ADDRESSES` reached).
    AddressTableFull,
    /// A `V4_BATCH` exceeded the contract's 8-swap cap.
    TooManyV4BatchSwaps(usize),
    /// `pack_config` `check_mode` outside `0..=3`.
    InvalidCheckMode(u8),
    /// `pack_config` `bribe_bips` outside `0..=10_000`.
    InvalidBribeBips(u16),
    /// `pack_config` `bribe_recipient_idx` outside `0..MAX_INDEXED_ADDRESSES`.
    InvalidBribeRecipientIdx(u8),
}

impl std::fmt::Display for EncoderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Uint96Overflow(v) => write!(f, "uint96 amount {v} overflows the 12-byte field"),
            Self::ForwardDataTooLong(n) => {
                write!(f, "forward_data length {n} exceeds the 255-byte cap")
            }
            Self::AddressTableFull => write!(
                f,
                "address table full (max {MAX_INDEXED_ADDRESSES} entries)"
            ),
            Self::TooManyV4BatchSwaps(n) => {
                write!(f, "V4_BATCH max 8 swaps, got {n}")
            }
            Self::InvalidCheckMode(v) => write!(f, "check_mode must be 0–3, got {v}"),
            Self::InvalidBribeBips(v) => write!(f, "bribe_bips must be 0–10000, got {v}"),
            Self::InvalidBribeRecipientIdx(v) => {
                write!(f, "bribe_recipient_idx must be 0–31, got {v}")
            }
        }
    }
}

impl std::error::Error for EncoderError {}

// ── Byte-pushing helpers (mirrors Python `_e(v, n, signed)` + `_address_to_bytes`) ──

fn push_u8(out: &mut Vec<u8>, v: u8) {
    out.push(v);
}

fn push_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_be_bytes());
}

/// Push an `int16` tick-spacing as two big-endian bytes. The contract decodes
/// `tick_spacing` as a signed `int16`; V4 tick spacings are positive in
/// practice, but the signed two's-complement layout is the wire format.
fn push_i16(out: &mut Vec<u8>, v: i16) {
    out.extend_from_slice(&v.to_be_bytes());
}

/// Push a uint96 amount (≤ `2^96 − 1`) as 12 big-endian bytes; anything larger
/// is rejected as a `Uint96Overflow` (the 12-byte field cannot hold it).
fn push_u96(out: &mut Vec<u8>, v: u128) -> Result<(), EncoderError> {
    if v >= UINT96_BOUND {
        return Err(EncoderError::Uint96Overflow(v));
    }
    let b = v.to_be_bytes();
    out.extend_from_slice(&b[4..]); // last 12 of the 16-byte u128
    Ok(())
}

/// Push a uint256 amount as 32 big-endian bytes (`_e(amount)`).
fn push_u256(out: &mut Vec<u8>, v: U256) {
    out.extend_from_slice(&v.to_be_bytes::<32>());
}

/// Push a 1-byte `forward_data` length prefix + the data, rejecting slices
/// exceeding the 255-byte cap (the length is itself a `uint8`).
fn push_forward_data(out: &mut Vec<u8>, data: &[u8]) -> Result<(), EncoderError> {
    let len_u8 =
        u8::try_from(data.len()).map_err(|_| EncoderError::ForwardDataTooLong(data.len()))?;
    out.push(len_u8);
    out.extend_from_slice(data);
    Ok(())
}

// ── `pack_config` / `pack_expected_balance` ──────────────────────────────────

/// Pack the `execute()` `config` ABI parameter.
///
/// Layout (matches `cmd_executor.vy` `execute`):
/// - bits `0–7`:   `check_mode` (0 = skip, 1 = WETH+ETH, 2 = ERC6909 WETH)
/// - bits `8–23`:  `bribe_bips` (0 = no bribe, 1–10000 = basis points)
/// - bits `24–31`: `bribe_recipient_idx` (0 = `block.coinbase`, 1–31 = table idx)
/// - bits `32–255`: `expected_value` (pre-tx balance for the selected mode)
///
/// Bribes were moved OUT of the command stream (opcodes 0x02/0x03 are
/// reserved) and into this parameter. Pass the result as the second argument
/// to `execute(commands, config)`.
///
/// When `bribe_bips > 0`, `expected_value` MUST be the real pre-tx balance —
/// the contract computes `profit = combined_after − expected_value`, so
/// `expected_value = 0` with a bribe over-bribes (drains the full balance).
///
/// # Errors
///
/// Returns [`EncoderError::InvalidCheckMode`] if `check_mode` is not `0..=3`,
/// [`EncoderError::InvalidBribeBips`] if `bribe_bips` is not `0..=10_000`, or
/// [`EncoderError::InvalidBribeRecipientIdx`] if `bribe_recipient_idx` is not
/// `0..MAX_INDEXED_ADDRESSES`.
pub fn pack_config(
    check_mode: u8,
    expected_value: U256,
    bribe_bips: u16,
    bribe_recipient_idx: u8,
) -> Result<U256, EncoderError> {
    if check_mode > 3 {
        return Err(EncoderError::InvalidCheckMode(check_mode));
    }
    if bribe_bips > 10_000 {
        return Err(EncoderError::InvalidBribeBips(bribe_bips));
    }
    if bribe_recipient_idx as usize >= MAX_INDEXED_ADDRESSES {
        return Err(EncoderError::InvalidBribeRecipientIdx(bribe_recipient_idx));
    }
    // (expected_value << 32) | (bribe_recipient_idx << 24) | (bribe_bips << 8) | check_mode
    Ok((expected_value << 32)
        | (U256::from(bribe_recipient_idx) << 24)
        | (U256::from(bribe_bips) << 8)
        | U256::from(check_mode))
}

/// Deprecated alias for [`pack_config`] (no bribe fields) — kept for callers
/// that predate the bribe config move.
///
/// # Errors
///
/// Returns [`EncoderError::InvalidCheckMode`] if `check_mode` is not `0..=3`.
pub fn pack_expected_balance(check_mode: u8, expected_value: U256) -> Result<U256, EncoderError> {
    pack_config(check_mode, expected_value, 0, 0)
}

// ── AddressTable ────────────────────────────────────────────────────────────

/// Tracks addresses for compact index-based referencing in the command stream.
///
/// Each address is assigned a sequential index in insertion order
/// (`0..MAX_INDEXED_ADDRESSES − 1`). Sentinel indices (`0xFC`–`0xFF`) resolve
/// to the 4 protocol roles (PM / SELF / WETH / NATIVE) without `SET_ADDRESS` or
/// `TLOAD`, saving ~476 gas per use. The table is built during preprocessing
/// and referenced during execution.
#[derive(Debug, Default)]
pub struct AddressTable {
    addresses: Vec<Address>,
    index_map: HashMap<Address, u8>,
    sentinel_map: HashMap<Address, u8>,
}

impl AddressTable {
    /// Build a new table. `NATIVE_ADDRESS` (`address(0)`) always resolves to
    /// [`SENTINEL_NATIVE`] without an explicit [`Self::add`] — the table is
    /// pre-seeded with `sentinel_map[NATIVE_ADDRESS] = 0xFF` (and the same for
    /// `ZERO_ADDRESS`, the same address).
    #[must_use]
    pub fn new() -> Self {
        let mut sentinel_map = HashMap::new();
        sentinel_map.insert(NATIVE_ADDRESS, SENTINEL_NATIVE);
        // ZERO_ADDRESS == NATIVE_ADDRESS (both address(0)) — inserting both
        // keys is a no-op overwrite of the same value.
        sentinel_map.insert(Address::ZERO, SENTINEL_NATIVE);
        Self {
            addresses: Vec::new(),
            index_map: HashMap::new(),
            sentinel_map,
        }
    }

    /// Build a table pre-seeded with the protocol sentinels: `pool_manager`
    /// → [`SENTINEL_PM`], `executor` → [`SENTINEL_SELF`], `weth` →
    /// [`SENTINEL_WETH`]. Any `None` sentinel is simply not registered.
    #[must_use]
    pub fn with_sentinels(
        weth: Option<Address>,
        executor: Option<Address>,
        pool_manager: Option<Address>,
    ) -> Self {
        let mut table = Self::new();
        if let Some(pm) = pool_manager {
            table.sentinel_map.insert(pm, SENTINEL_PM);
        }
        if let Some(self_) = executor {
            table.sentinel_map.insert(self_, SENTINEL_SELF);
        }
        if let Some(weth) = weth {
            table.sentinel_map.insert(weth, SENTINEL_WETH);
        }
        table
    }

    /// Add an address, returning its index. Idempotent for duplicates.
    ///
    /// Sentinel addresses (WETH, PM, executor, NATIVE) return their fixed
    /// sentinel index without adding to the table.
    ///
    /// # Errors
    ///
    /// Returns [`EncoderError::AddressTableFull`] if the table already holds
    /// [`MAX_INDEXED_ADDRESSES`] entries and `addr` is neither a sentinel nor
    /// already present.
    pub fn add(&mut self, addr: Address) -> Result<u8, EncoderError> {
        // Check sentinel first.
        if let Some(&idx) = self.sentinel_map.get(&addr) {
            return Ok(idx);
        }
        if let Some(&idx) = self.index_map.get(&addr) {
            return Ok(idx);
        }
        let idx = self.addresses.len();
        if idx >= MAX_INDEXED_ADDRESSES {
            return Err(EncoderError::AddressTableFull);
        }
        // `idx < MAX_INDEXED_ADDRESSES (32)` here, so the narrowing cannot fail;
        // `unwrap_or` is panic-free and the fallback is unreachable.
        let idx = u8::try_from(idx).unwrap_or(u8::MAX);
        self.addresses.push(addr);
        self.index_map.insert(addr, idx);
        Ok(idx)
    }

    /// Return the table index (or sentinel index) for `addr`, or `None` if
    /// `addr` was never added and is not a sentinel.
    #[must_use]
    pub fn index_of(&self, addr: Address) -> Option<u8> {
        if let Some(&idx) = self.sentinel_map.get(&addr) {
            Some(idx)
        } else {
            self.index_map.get(&addr).copied()
        }
    }

    /// `true` if `addr` is a sentinel or a table entry.
    #[must_use]
    pub fn contains(&self, addr: Address) -> bool {
        self.sentinel_map.contains_key(&addr) || self.index_map.contains_key(&addr)
    }

    /// Return only table addresses (not sentinels) for `SET_ADDRESS` encoding,
    /// in insertion order.
    #[must_use]
    pub fn addresses(&self) -> &[Address] {
        &self.addresses
    }
}

// ── Preprocessing commands ──────────────────────────────────────────────────

/// `SET_ADDRESS`: `[0x00][address:20]` — 21 bytes.
#[must_use]
pub fn enc_set_address(addr: Address) -> Vec<u8> {
    let mut out = Vec::with_capacity(21);
    out.push(CMD_SET_ADDRESS);
    out.extend_from_slice(addr.as_slice());
    out
}

/// Encode `SET_ADDRESS` commands for all table addresses (skip sentinels).
#[must_use]
pub fn enc_set_addresses(address_table: &AddressTable) -> Vec<u8> {
    let mut out = Vec::with_capacity(address_table.addresses().len() * 21);
    for &addr in address_table.addresses() {
        out.extend_from_slice(&enc_set_address(addr));
    }
    out
}

/// Encode the full preprocessing section + separator: `[SET_ADDRESS commands][0xFF]`.
///
/// The stream starts directly with `SET_ADDRESS` commands — no `0xFE` prefix.
/// Profit check and bribes are NO LONGER encoded in the stream — both are
/// packed into the `config` ABI parameter of `execute()` (see [`pack_config`]).
#[must_use]
pub fn enc_preamble(address_table: &AddressTable) -> Vec<u8> {
    let mut out = enc_set_addresses(address_table);
    out.push(BEGIN_EXECUTION);
    out
}

// ── ERC20 / ETH / Native commands (0x10–0x17) ───────────────────────────────

/// `ERC20_TRANSFER`: `[0x10][token_idx:1][recipient_idx:1][amount:12]` — 15 bytes.
///
/// `amount` is `uint96` (max ~7.9e28 — covers all practical token amounts).
///
/// # Errors
///
/// Returns [`EncoderError::Uint96Overflow`] if `amount ≥ 2^96`.
pub fn enc_erc20_transfer(
    token_idx: u8,
    recipient_idx: u8,
    amount: u128,
) -> Result<Vec<u8>, EncoderError> {
    let mut out = Vec::with_capacity(15);
    out.push(CMD_ERC20_TRANSFER);
    push_u8(&mut out, token_idx);
    push_u8(&mut out, recipient_idx);
    push_u96(&mut out, amount)?;
    Ok(out)
}

/// `ERC20_XFER_BALANCE`: `[0x11][token_idx:1][recipient_idx:1]` — 3 bytes.
#[must_use]
pub fn enc_erc20_xfer_balance(token_idx: u8, recipient_idx: u8) -> Vec<u8> {
    vec![CMD_ERC20_XFER_BALANCE, token_idx, recipient_idx]
}

/// `WETH_DEPOSIT`: `[0x12][amount:32]` — 33 bytes.
#[must_use]
pub fn enc_weth_deposit(amount: U256) -> Vec<u8> {
    let mut out = Vec::with_capacity(33);
    out.push(CMD_WETH_DEPOSIT);
    push_u256(&mut out, amount);
    out
}

/// `WETH_WITHDRAW`: `[0x13][amount:32]` — 33 bytes.
#[must_use]
pub fn enc_weth_withdraw(amount: U256) -> Vec<u8> {
    let mut out = Vec::with_capacity(33);
    out.push(CMD_WETH_WITHDRAW);
    push_u256(&mut out, amount);
    out
}

/// `WETH_DEPOSIT_ALL`: `[0x14]` — 1 byte.
#[must_use]
pub fn enc_weth_deposit_all() -> Vec<u8> {
    vec![CMD_WETH_DEPOSIT_ALL]
}

/// `WETH_WITHDRAW_ALL`: `[0x15]` — 1 byte.
#[must_use]
pub fn enc_weth_withdraw_all() -> Vec<u8> {
    vec![CMD_WETH_WITHDRAW_ALL]
}

/// `SEND_ETH`: `[0x16][recipient_idx:1][amount:12]` — 14 bytes.
///
/// # Errors
///
/// Returns [`EncoderError::Uint96Overflow`] if `amount ≥ 2^96`.
pub fn enc_send_eth(recipient_idx: u8, amount: u128) -> Result<Vec<u8>, EncoderError> {
    let mut out = Vec::with_capacity(14);
    out.push(CMD_SEND_ETH);
    push_u8(&mut out, recipient_idx);
    push_u96(&mut out, amount)?;
    Ok(out)
}

/// `SEND_ETH_ALL`: `[0x17][recipient_idx:1]` — 2 bytes.
#[must_use]
pub fn enc_send_eth_all(recipient_idx: u8) -> Vec<u8> {
    vec![CMD_SEND_ETH_ALL, recipient_idx]
}

// ── V2 commands (0x20–0x22) ─────────────────────────────────────────────────

/// `V2_SWAP_COMPACT`: `[0x20][pool_idx:1][zfo:1][amount_out:12][recipient_idx:1][fee:2][fwd_len:1][fwd:N]` = 19 + N bytes.
///
/// `fee` is a fraction of 10000 (30 = 0.3% UniswapV2, 25 = 0.25% PancakeSwap),
/// written to `t_v2_pair_fee[pool]` before `swap()` for correct auto-pay.
/// `amount_out` is `uint96`. `forward_data` max 255 bytes.
///
/// # Errors
///
/// Returns [`EncoderError::Uint96Overflow`] if `amount_out ≥ 2^96`, or
/// [`EncoderError::ForwardDataTooLong`] if `forward_data.len() > 255`.
pub fn enc_v2_swap_compact(
    pool_idx: u8,
    zfo: bool,
    amount_out: u128,
    recipient_idx: u8,
    fee: u16,
    forward_data: &[u8],
) -> Result<Vec<u8>, EncoderError> {
    let mut out = Vec::with_capacity(19 + forward_data.len());
    out.push(CMD_V2_SWAP_COMPACT);
    push_u8(&mut out, pool_idx);
    push_u8(&mut out, u8::from(zfo));
    push_u96(&mut out, amount_out)?;
    push_u8(&mut out, recipient_idx);
    push_u16(&mut out, fee);
    push_forward_data(&mut out, forward_data)?;
    Ok(out)
}

/// `V2_SWAP_CALC`: `[0x21][pool_idx:1][zfo:1][recipient_idx:1][fee:2]` — 6 bytes.
#[must_use]
pub fn enc_v2_swap_calc(pool_idx: u8, zfo: bool, recipient_idx: u8, fee: u16) -> Vec<u8> {
    let mut out = Vec::with_capacity(6);
    out.push(CMD_V2_SWAP_CALC);
    push_u8(&mut out, pool_idx);
    push_u8(&mut out, u8::from(zfo));
    push_u8(&mut out, recipient_idx);
    push_u16(&mut out, fee);
    out
}

/// `V2_SWAP_DIRECT`: `[0x22][pool_idx:1][zfo:1][amount_out:12][recipient_idx:1]` — 16 bytes.
///
/// V2 swap with explicit amount and no callback. `amount_out` is `uint96`. No
/// fee field — the pair applies its stored fee.
///
/// # Errors
///
/// Returns [`EncoderError::Uint96Overflow`] if `amount_out ≥ 2^96`.
pub fn enc_v2_swap_direct(
    pool_idx: u8,
    zfo: bool,
    amount_out: u128,
    recipient_idx: u8,
) -> Result<Vec<u8>, EncoderError> {
    let mut out = Vec::with_capacity(16);
    out.push(CMD_V2_SWAP_DIRECT);
    push_u8(&mut out, pool_idx);
    push_u8(&mut out, u8::from(zfo));
    push_u96(&mut out, amount_out)?;
    push_u8(&mut out, recipient_idx);
    Ok(out)
}

// ── V3 commands (0x30–0x31) ─────────────────────────────────────────────────

/// `V3_SWAP_COMPACT`: `[0x30][pool_idx:1][zfo:1][amount_specified:12][recipient_idx:1][fwd_len:1][fwd:N]` = 17 + N bytes.
///
/// `amount_specified` is a **positive** `uint96` (exact-input — the contract
/// negates it internally; see §10.2). Sqrt price limit auto-set to widest
/// range. `forward_data` max 255 bytes.
///
/// # Errors
///
/// Returns [`EncoderError::Uint96Overflow`] if `amount_specified ≥ 2^96`, or
/// [`EncoderError::ForwardDataTooLong`] if `forward_data.len() > 255`.
pub fn enc_v3_swap_compact(
    pool_idx: u8,
    zfo: bool,
    amount_specified: u128,
    recipient_idx: u8,
    forward_data: &[u8],
) -> Result<Vec<u8>, EncoderError> {
    let mut out = Vec::with_capacity(17 + forward_data.len());
    out.push(CMD_V3_SWAP_COMPACT);
    push_u8(&mut out, pool_idx);
    push_u8(&mut out, u8::from(zfo));
    push_u96(&mut out, amount_specified)?;
    push_u8(&mut out, recipient_idx);
    push_forward_data(&mut out, forward_data)?;
    Ok(out)
}

/// `V3_SWAP_DELTA`: `[0x31][pool_idx:1][zfo:1][recipient_idx:1]` — 4 bytes.
#[must_use]
pub fn enc_v3_swap_delta(pool_idx: u8, zfo: bool, recipient_idx: u8) -> Vec<u8> {
    let mut out = Vec::with_capacity(4);
    out.push(CMD_V3_SWAP_DELTA);
    push_u8(&mut out, pool_idx);
    push_u8(&mut out, u8::from(zfo));
    push_u8(&mut out, recipient_idx);
    out
}

// ── V4 swap commands (0x40–0x42) ────────────────────────────────────────────

/// `V4_SWAP_COMPACT`: `[0x40][c0_idx:1][c1_idx:1][fee:2][ts:2][hooks_idx:1][zfo:1][amount:12]` — 21 bytes.
///
/// `fee` is `uint16` (e.g. 3000 = 0.3%). `tick_spacing` is `int16` encoded as
/// two big-endian bytes. `amount_u96` is a **positive** `uint96` exact-input
/// amount — the contract negates it to a negative `amountSpecified` (§10.2).
/// Use `hooks_idx = 0xFF` ([`SENTINEL_NATIVE`]) for "no hooks".
///
/// # Errors
///
/// Returns [`EncoderError::Uint96Overflow`] if `amount_u96 ≥ 2^96`.
pub fn enc_v4_swap_compact(
    c0_idx: u8,
    c1_idx: u8,
    fee: u16,
    tick_spacing: i16,
    hooks_idx: u8,
    zfo: bool,
    amount_u96: u128,
) -> Result<Vec<u8>, EncoderError> {
    let mut out = Vec::with_capacity(21);
    out.push(CMD_V4_SWAP_COMPACT);
    push_u8(&mut out, c0_idx);
    push_u8(&mut out, c1_idx);
    push_u16(&mut out, fee);
    push_i16(&mut out, tick_spacing);
    push_u8(&mut out, hooks_idx);
    push_u8(&mut out, u8::from(zfo));
    push_u96(&mut out, amount_u96)?;
    Ok(out)
}

/// `V4_SWAP_DYNAMIC`: `[0x41][c0_idx:1][c1_idx:1][fee:2][ts:2][hooks_idx:1][zfo:1]` — 9 bytes.
///
/// Amount from PM `exttload`. `fee` is `uint16`, `tick_spacing` is `int16`.
/// Use `hooks_idx = 0xFF` ([`SENTINEL_NATIVE`]) for "no hooks".
#[must_use]
pub fn enc_v4_swap_dynamic(
    c0_idx: u8,
    c1_idx: u8,
    fee: u16,
    tick_spacing: i16,
    hooks_idx: u8,
    zfo: bool,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(9);
    out.push(CMD_V4_SWAP_DYNAMIC);
    push_u8(&mut out, c0_idx);
    push_u8(&mut out, c1_idx);
    push_u16(&mut out, fee);
    push_i16(&mut out, tick_spacing);
    push_u8(&mut out, hooks_idx);
    push_u8(&mut out, u8::from(zfo));
    out
}

/// A single entry in a `V4_BATCH` (`[c0_idx:1][c1_idx:1][fee:2][ts:2][hooks_idx:1][zfo:1][amount:12]` — 20 bytes).
///
/// `amount == 0` means dynamic (from PM `exttload`). `amount_u96` is a positive
/// `uint96` exact-input amount (§10.2 — the contract negates internally).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct V4BatchEntry {
    /// Currency-0 table index.
    pub c0_idx: u8,
    /// Currency-1 table index.
    pub c1_idx: u8,
    /// Pool fee (`uint16`).
    pub fee: u16,
    /// Tick spacing (`int16`).
    pub tick_spacing: i16,
    /// Hooks address index (`0xFF` = no hooks).
    pub hooks_idx: u8,
    /// `zero_for_one` direction flag.
    pub zfo: bool,
    /// Positive `uint96` exact-input amount; `0` = dynamic.
    pub amount_u96: u128,
}

/// `V4_BATCH`: `[0x42][num_swaps:1][entry_1:20]...[entry_N:20]`.
///
/// After all swaps, auto-settles native ETH and WETH deltas. Max 8 swaps
/// (contract limit). Each 20-byte entry: `[c0_idx:1][c1_idx:1][fee:2][ts:2]`
/// `[hooks_idx:1][zfo:1][amount:12]` — `amount == 0` means dynamic.
///
/// # Errors
///
/// Returns [`EncoderError::TooManyV4BatchSwaps`] if `swaps.len() > 8`, or
/// [`EncoderError::Uint96Overflow`] if any entry's `amount_u96 ≥ 2^96`.
pub fn enc_v4_batch(swaps: &[V4BatchEntry]) -> Result<Vec<u8>, EncoderError> {
    if swaps.len() > 8 {
        return Err(EncoderError::TooManyV4BatchSwaps(swaps.len()));
    }
    let mut out = Vec::with_capacity(2 + swaps.len() * 20);
    out.push(CMD_V4_BATCH);
    // `swaps.len() ≤ 8` here, so the narrowing cannot fail; `unwrap_or` is
    // panic-free and the fallback is unreachable.
    push_u8(&mut out, u8::try_from(swaps.len()).unwrap_or(u8::MAX));
    for s in swaps {
        push_u8(&mut out, s.c0_idx);
        push_u8(&mut out, s.c1_idx);
        push_u16(&mut out, s.fee);
        push_i16(&mut out, s.tick_spacing);
        push_u8(&mut out, s.hooks_idx);
        push_u8(&mut out, u8::from(s.zfo));
        push_u96(&mut out, s.amount_u96)?;
    }
    Ok(out)
}

// ── V4 settlement / ERC6909 commands (0x50–0x59) ────────────────────────────

/// `V4_UNLOCK`: `[0x50][len:1][data:N]` — 2 + N bytes.
///
/// Forward data max 255 bytes. Enters the PoolManager unlock context.
///
/// # Errors
///
/// Returns [`EncoderError::ForwardDataTooLong`] if `forward_data.len() > 255`.
pub fn enc_v4_unlock(forward_data: &[u8]) -> Result<Vec<u8>, EncoderError> {
    let mut out = Vec::with_capacity(2 + forward_data.len());
    out.push(CMD_V4_UNLOCK);
    push_forward_data(&mut out, forward_data)?;
    Ok(out)
}

/// `V4_TAKE`: `[0x51][currency_idx:1][recipient_idx:1][amount:32]` — 35 bytes.
///
/// Rarely used — prefer [`enc_v4_take_compact`] (15 bytes) or
/// [`enc_v4_take_delta`] (3 bytes).
#[must_use]
pub fn enc_v4_take(currency_idx: u8, recipient_idx: u8, amount: U256) -> Vec<u8> {
    let mut out = Vec::with_capacity(35);
    out.push(CMD_V4_TAKE);
    push_u8(&mut out, currency_idx);
    push_u8(&mut out, recipient_idx);
    push_u256(&mut out, amount);
    out
}

/// `V4_TAKE_COMPACT`: `[0x52][currency_idx:1][recipient_idx:1][amount:12]` — 15 bytes.
///
/// Preferred over [`enc_v4_take`] for all known amounts. `amount_u96` is `uint96`.
///
/// # Errors
///
/// Returns [`EncoderError::Uint96Overflow`] if `amount_u96 ≥ 2^96`.
pub fn enc_v4_take_compact(
    currency_idx: u8,
    recipient_idx: u8,
    amount_u96: u128,
) -> Result<Vec<u8>, EncoderError> {
    let mut out = Vec::with_capacity(15);
    out.push(CMD_V4_TAKE_COMPACT);
    push_u8(&mut out, currency_idx);
    push_u8(&mut out, recipient_idx);
    push_u96(&mut out, amount_u96)?;
    Ok(out)
}

/// `V4_TAKE_DELTA`: `[0x53][currency_idx:1][recipient_idx:1]` — 3 bytes.
#[must_use]
pub fn enc_v4_take_delta(currency_idx: u8, recipient_idx: u8) -> Vec<u8> {
    vec![CMD_V4_TAKE_DELTA, currency_idx, recipient_idx]
}

/// `V4_SYNC`: `[0x54][currency_idx:1]` — 2 bytes.
#[must_use]
pub fn enc_v4_sync(currency_idx: u8) -> Vec<u8> {
    vec![CMD_V4_SYNC, currency_idx]
}

/// `V4_SETTLE`: `[0x55]` — 1 byte.
#[must_use]
pub fn enc_v4_settle() -> Vec<u8> {
    vec![CMD_V4_SETTLE]
}

/// `V4_SETTLE_DELTA`: `[0x56][currency_idx:1]` — 2 bytes.
#[must_use]
pub fn enc_v4_settle_delta(currency_idx: u8) -> Vec<u8> {
    vec![CMD_V4_SETTLE_DELTA, currency_idx]
}

/// `V4_SETTLE_ALL`: `[0x57]` — 1 byte.
#[must_use]
pub fn enc_v4_settle_all() -> Vec<u8> {
    vec![CMD_V4_SETTLE_ALL]
}

/// `V4_MINT_COMPACT`: `[0x58][currency_idx:1][recipient_idx:1][amount:12]` — 15 bytes.
///
/// Convert a positive PM delta into an ERC6909 balance for `recipient` (no
/// physical token transfer — the asset stays inside PoolManager). `amount_u96`
/// is `uint96`.
///
/// # Errors
///
/// Returns [`EncoderError::Uint96Overflow`] if `amount_u96 ≥ 2^96`.
pub fn enc_v4_mint_compact(
    currency_idx: u8,
    recipient_idx: u8,
    amount_u96: u128,
) -> Result<Vec<u8>, EncoderError> {
    let mut out = Vec::with_capacity(15);
    out.push(CMD_V4_MINT_COMPACT);
    push_u8(&mut out, currency_idx);
    push_u8(&mut out, recipient_idx);
    push_u96(&mut out, amount_u96)?;
    Ok(out)
}

/// `V4_BURN_COMPACT`: `[0x59][currency_idx:1][amount:12]` — 14 bytes.
///
/// Convert an ERC6909 balance into a payable PM delta (offsets a debt). No
/// physical token transfer. `amount_u96` is `uint96`.
///
/// # Errors
///
/// Returns [`EncoderError::Uint96Overflow`] if `amount_u96 ≥ 2^96`.
pub fn enc_v4_burn_compact(currency_idx: u8, amount_u96: u128) -> Result<Vec<u8>, EncoderError> {
    let mut out = Vec::with_capacity(14);
    out.push(CMD_V4_BURN_COMPACT);
    push_u8(&mut out, currency_idx);
    push_u96(&mut out, amount_u96)?;
    Ok(out)
}

// ── Pool key helper ──────────────────────────────────────────────────────────

/// A V4 pool key with currencies sorted so `currency0 < currency1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct V4PoolKey {
    /// The numerically-smaller currency.
    pub currency0: Address,
    /// The numerically-larger currency.
    pub currency1: Address,
    /// Pool fee (`uint24` in the on-chain `PoolKey`; the compact encoders take
    /// a `uint16` view).
    pub fee: u32,
    /// Tick spacing (signed; the compact encoders take an `int16` view).
    pub tick_spacing: i32,
    /// Hooks address (`address(0)` = no hooks).
    pub hooks: Address,
}

/// Create a V4 pool key with currencies sorted by address.
///
/// Returns `(currency0, currency1, fee, tick_spacing, hooks)` with
/// `currency0 < currency1` (lexicographic on the raw 20 bytes — equivalent to
/// `Address`'s `Ord`, which compares the big-endian numeric value). Mirrors the
/// currency-sort in [`crate`]'s `create2` precedent.
#[must_use]
pub fn make_pool_key(
    currency0: Address,
    currency1: Address,
    fee: u32,
    tick_spacing: i32,
    hooks: Address,
) -> V4PoolKey {
    let (c0, c1) = if currency0 <= currency1 {
        (currency0, currency1)
    } else {
        (currency1, currency0)
    };
    V4PoolKey {
        currency0: c0,
        currency1: c1,
        fee,
        tick_spacing,
        hooks,
    }
}

#[cfg(test)]
#[expect(clippy::unwrap_used, clippy::cast_possible_truncation)]
mod tests {
    use super::*;
    use alloy::primitives::{address, U256};

    // ── uint96 boundary + overflow rejection ──

    #[test]
    fn uint96_max_is_accepted_overflow_is_rejected() {
        // 2^96 − 1 is the largest valid uint96.
        let max = u128::MAX >> 32;
        assert_eq!(max, (1u128 << 96) - 1);
        assert!(enc_erc20_transfer(1, 2, max).is_ok());
        // 2^96 overflows the 12-byte field.
        assert_eq!(
            enc_erc20_transfer(1, 2, 1u128 << 96).unwrap_err(),
            EncoderError::Uint96Overflow(1u128 << 96)
        );
    }

    // ── AddressTable: sentinel resolution, dedup, cap ──

    #[test]
    fn address_table_sentinels_resolve_without_adding() {
        let pm = address!("000000000004444c5dc75cB358380D2e3dE08A90");
        let exec = address!("DeAd0000000000000000000000000000000000Be");
        let weth = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
        let mut table = AddressTable::with_sentinels(Some(weth), Some(exec), Some(pm));

        assert_eq!(table.add(weth).unwrap(), SENTINEL_WETH);
        assert_eq!(table.add(pm).unwrap(), SENTINEL_PM);
        assert_eq!(table.add(exec).unwrap(), SENTINEL_SELF);
        assert_eq!(table.add(Address::ZERO).unwrap(), SENTINEL_NATIVE);
        // Sentinels are NOT listed for SET_ADDRESS.
        assert!(table.addresses().is_empty());
    }

    #[test]
    fn address_table_dedups_insertion_order() {
        let usdc = address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48");
        let wbtc = address!("2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599");
        let mut table = AddressTable::new();

        assert_eq!(table.add(usdc).unwrap(), 0);
        assert_eq!(table.add(wbtc).unwrap(), 1);
        // Duplicate returns the same index — no new entry.
        assert_eq!(table.add(usdc).unwrap(), 0);
        assert_eq!(table.add(wbtc).unwrap(), 1);
        assert_eq!(table.addresses(), &[usdc, wbtc]);
        // index_of mirrors add for present addresses.
        assert_eq!(table.index_of(usdc), Some(0));
        assert_eq!(table.index_of(wbtc), Some(1));
        assert!(table
            .index_of(address!("DeAd000000000000000000000000000000000001"))
            .is_none());
    }

    #[test]
    fn address_table_cap_rejects_beyond_32() {
        // Fill the table to MAX_INDEXED_ADDRESSES, then the next add fails.
        let mut table = AddressTable::new();
        for i in 0u8..MAX_INDEXED_ADDRESSES as u8 {
            // +1 so byte 0 (address(0) / NATIVE sentinel) is never produced.
            let addr = Address::with_last_byte(i + 1);
            assert_eq!(table.add(addr).unwrap(), i);
        }
        // Full.
        let extra = Address::with_last_byte(0xAA);
        assert_eq!(
            table.add(extra).unwrap_err(),
            EncoderError::AddressTableFull
        );
        assert_eq!(table.addresses().len(), MAX_INDEXED_ADDRESSES);
    }

    // ── pack_config bit-packing roundtrip ──

    #[test]
    fn pack_config_layout_roundtrips() {
        for &(check_mode, bribe_bips, bribe_recipient_idx) in
            &[(0u8, 0u16, 0u8), (1, 500, 2), (2, 10_000, 31), (0, 1, 1)]
        {
            let expected_value = U256::from((1u128 << 100) | 0xAB); // spans the high 224 bits
            let packed =
                pack_config(check_mode, expected_value, bribe_bips, bribe_recipient_idx).unwrap();
            // Reconstruct via the documented bit layout and compare.
            let manual = (expected_value << 32u32)
                | (U256::from(bribe_recipient_idx) << 24u32)
                | (U256::from(bribe_bips) << 8u32)
                | U256::from(check_mode);
            assert_eq!(packed, manual);
            // Field extraction (bits 0–7, 8–23, 24–31, 32–255).
            assert_eq!((packed & U256::from(0xFFu8)).to::<u8>(), check_mode);
            let bips = ((packed >> 8u32) & U256::from(0xFFFFu32)).to::<u128>();
            assert_eq!(bips as u16, bribe_bips);
            let rcpt = ((packed >> 24u32) & U256::from(0xFFu8)).to::<u8>();
            assert_eq!(rcpt, bribe_recipient_idx);
            assert_eq!(packed >> 32u32, expected_value);
        }
    }

    #[test]
    fn pack_config_rejects_out_of_range() {
        // check_mode must be 0–3 (3=SWEEP). 4 is rejected.
        assert_eq!(
            pack_config(4, U256::ZERO, 0, 0).unwrap_err(),
            EncoderError::InvalidCheckMode(4)
        );
        // bribe_bips must be 0–10000.
        assert_eq!(
            pack_config(0, U256::ZERO, 10_001, 0).unwrap_err(),
            EncoderError::InvalidBribeBips(10_001)
        );
        // bribe_recipient_idx must be 0–31.
        assert_eq!(
            pack_config(0, U256::ZERO, 0, MAX_INDEXED_ADDRESSES as u8).unwrap_err(),
            EncoderError::InvalidBribeRecipientIdx(MAX_INDEXED_ADDRESSES as u8)
        );
        // pack_expected_balance is the no-bribe alias.
        assert_eq!(
            pack_expected_balance(0, U256::ZERO).unwrap(),
            pack_config(0, U256::ZERO, 0, 0).unwrap()
        );
    }

    // ── make_pool_key currency sort (proper property) ──

    #[test]
    fn make_pool_key_sorts_and_is_symmetric() {
        use proptest::prelude::*;
        proptest!(|(a in 0u64..u64::MAX, b in 0u64..u64::MAX)| {
            let ca = Address::with_last_byte((a & 0xFF) as u8);
            let cb = Address::with_last_byte((b & 0xFF) as u8);
            let k13 = make_pool_key(ca, cb, 3000, 60, Address::ZERO);
            let k31 = make_pool_key(cb, ca, 3000, 60, Address::ZERO);
            // Symmetric in argument order.
            prop_assert_eq!(k13, k31);
            // currency0 < currency1.
            prop_assert!(k13.currency0 <= k13.currency1);
        });
    }

    // ── proptest: AddressTable dedup is order-independent in membership ──

    #[test]
    fn property_address_table_membership_stable_under_reorder() {
        use proptest::prelude::*;
        proptest!(|(a in 0u64..256, b in 0u64..256, c in 0u64..256)| {
            // The SET of members doesn't depend on insertion order.
            let addrs: [Address; 3] = [
                Address::with_last_byte(a as u8),
                Address::with_last_byte(b as u8),
                Address::with_last_byte(c as u8),
            ];
            let mut t1 = AddressTable::new();
            let mut t2 = AddressTable::new();
            for a_ in &addrs { t1.add(*a_).ok(); }
            for a_ in addrs.iter().rev() { t2.add(*a_).ok(); }
            // Same membership set.
            for a_ in &addrs {
                prop_assert_eq!(t1.contains(*a_), t2.contains(*a_));
            }
        });
    }
}
