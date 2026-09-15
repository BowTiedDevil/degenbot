//! The `--to-block` block-identifier port (ergo FTVJ6L).
//!
//! Mirrors `cli/pool.py::_resolve_to_block` and its `cli/aave.py` twin exactly:
//!
//! - a concrete integer is used as-is;
//! - a pure tag (`latest`, `safe`, …) with no offset resolves to `None` — the
//!   Rust core fetches the chain tip via `eth_blockNumber` itself;
//! - a tag with an offset (`latest:-64`, `safe:128`) or a concrete integer
//!   resolves to a concrete block number (the tag's block + the offset).
//!
//! Parsing is split from resolution so the semantics are unit-testable offline:
//! [`parse_to_block`] is pure, [`resolve_to_block`] performs the one RPC read a
//! tag+offset needs.

use std::future::Future;

use alloy::eips::BlockNumberOrTag;
use alloy::primitives::Address;
use degenbot_rpc::provider::AlloyProvider;

use crate::error::CliError;

/// The `--chunk` default (`10_000`), ported from `cli/pool.py`.
pub const DEFAULT_CHUNK_SIZE: u64 = 10_000;

/// The `--to-block` default (`latest:-64`), ported from `cli/pool.py`.
pub const DEFAULT_TO_BLOCK: &str = "latest:-64";

/// The `--verify-all-interval` default (`1_000_000`), ported from `cli/pool.py`.
pub const DEFAULT_VERIFY_ALL_INTERVAL: u64 = 1_000_000;

/// The RPC retry budget the console's spot reads use.
const RPC_MAX_RETRIES: u32 = 5;

/// A block tag accepted by `--to-block`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockTag {
    /// `latest`.
    Latest,
    /// `earliest`.
    Earliest,
    /// `pending`.
    Pending,
    /// `safe`.
    Safe,
    /// `finalized`.
    Finalized,
}

impl BlockTag {
    /// Parse one of the five accepted tags (`_BLOCK_TAGS` in the Python CLI).
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "latest" => Some(Self::Latest),
            "earliest" => Some(Self::Earliest),
            "pending" => Some(Self::Pending),
            "safe" => Some(Self::Safe),
            "finalized" => Some(Self::Finalized),
            _ => None,
        }
    }

    /// The tag's wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Latest => "latest",
            Self::Earliest => "earliest",
            Self::Pending => "pending",
            Self::Safe => "safe",
            Self::Finalized => "finalized",
        }
    }

    /// The alloy tag.
    #[must_use]
    pub const fn to_alloy(self) -> BlockNumberOrTag {
        match self {
            Self::Latest => BlockNumberOrTag::Latest,
            Self::Earliest => BlockNumberOrTag::Earliest,
            Self::Pending => BlockNumberOrTag::Pending,
            Self::Safe => BlockNumberOrTag::Safe,
            Self::Finalized => BlockNumberOrTag::Finalized,
        }
    }
}

/// A parsed `--to-block` value, before the tag's block number is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToBlockSpec {
    /// A concrete block number.
    Number(u64),
    /// A pure tag (or the empty effect of `offset == 0`): the core resolves the
    /// chain tip.
    Tip,
    /// A tag plus a signed offset: the tag's block number plus `offset`.
    TagOffset {
        /// The block tag.
        tag: BlockTag,
        /// The signed offset.
        offset: i64,
    },
}

/// Parse a `--to-block` string into a [`ToBlockSpec`].
///
/// # Errors
///
/// [`CliError::InvalidBlockTag`] for a malformed tag or offset — the Python
/// `ValueError("Invalid block tag: …")` refusal.
pub fn parse_to_block(raw: &str) -> Result<ToBlockSpec, CliError> {
    if !raw.is_empty() && raw.bytes().all(|b| b.is_ascii_digit()) {
        return raw
            .parse::<u64>()
            .map(ToBlockSpec::Number)
            .map_err(|_| CliError::InvalidBlockTag(raw.to_string()));
    }
    let (tag_raw, offset) = match raw.split_once(':') {
        Some((tag, offset)) => (
            tag,
            offset
                .trim()
                .parse::<i64>()
                .map_err(|_| CliError::InvalidBlockTag(tag.to_string()))?,
        ),
        None => (raw, 0),
    };
    let tag =
        BlockTag::parse(tag_raw).ok_or_else(|| CliError::InvalidBlockTag(tag_raw.to_string()))?;
    if offset == 0 {
        Ok(ToBlockSpec::Tip)
    } else {
        Ok(ToBlockSpec::TagOffset { tag, offset })
    }
}

/// Resolve a parsed spec to an optional concrete block number.
///
/// `None` means "advance to the chain tip" (the core fetches it). A tag+offset
/// reads the tag's block number over RPC and adds the offset.
///
/// # Errors
///
/// [`CliError::BlockResolution`] when the RPC read fails, or when the resolved
/// block number is negative / out of range.
pub fn resolve_to_block(spec: ToBlockSpec, rpc_url: &str) -> Result<Option<u64>, CliError> {
    match spec {
        ToBlockSpec::Number(block) => Ok(Some(block)),
        ToBlockSpec::Tip => Ok(None),
        ToBlockSpec::TagOffset { tag, offset } => {
            let base = fetch_tag_block_number(tag, rpc_url)?;
            let signed = i128::from(base) + i128::from(offset);
            let resolved = u64::try_from(signed).map_err(|_| {
                CliError::BlockResolution(format!(
                    "block tag {}:{offset} resolved out of range (tag block {base})",
                    tag.as_str()
                ))
            })?;
            Ok(Some(resolved))
        }
    }
}

/// Read the block number for `tag` over RPC.
fn fetch_tag_block_number(tag: BlockTag, rpc_url: &str) -> Result<u64, CliError> {
    let provider = match block_on(AlloyProvider::new(rpc_url, RPC_MAX_RETRIES)) {
        Ok(Ok(provider)) => provider,
        Ok(Err(err)) => return Err(CliError::BlockResolution(err.to_string())),
        Err(err) => return Err(err),
    };
    let arc = provider.provider_arc();
    let result = block_on(async move { arc.get_block_by_number(tag.to_alloy()).await })?;
    let block = result
        .map_err(|err| CliError::BlockResolution(err.to_string()))?
        .ok_or_else(|| {
            CliError::BlockResolution(format!(
                "eth_getBlockByNumber({}) returned no block",
                tag.as_str()
            ))
        })?;
    Ok(block.header.number)
}

/// Drive `fut` on a self-built runtime — the updater cores own their runtime
/// and must not nest, so the arms hold the same constraint.
///
/// # Errors
///
/// [`CliError::RuntimeNested`] when called from inside an existing runtime, or
/// [`CliError::BlockResolution`] when the runtime cannot be built.
pub(crate) fn block_on<F: Future>(fut: F) -> Result<F::Output, CliError> {
    if tokio::runtime::Handle::try_current().is_ok() {
        return Err(CliError::RuntimeNested);
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|err| CliError::BlockResolution(err.to_string()))?;
    Ok(runtime.block_on(fut))
}

/// Parse a chain selector (`base`, `ethereum`, `arbitrum`, or a numeric id).
///
/// # Errors
///
/// [`CliError::UnknownChain`] when the selector names no known chain slug and
/// is not a number.
pub fn resolve_chain_selector(selector: &str) -> Result<u64, CliError> {
    let lower = selector.trim().to_ascii_lowercase();
    if !lower.is_empty() {
        if let Ok(chain_id) = lower.parse::<u64>() {
            return Ok(chain_id);
        }
    }
    match lower.as_str() {
        "base" => Ok(8453),
        "ethereum" | "eth" | "mainnet" => Ok(1),
        "arbitrum" | "arb" | "arb1" => Ok(42_161),
        _ => Err(CliError::UnknownChain {
            chain: selector.to_string(),
        }),
    }
}

/// EIP-55-checksum an address for a DB key / RPC target.
#[must_use]
pub fn checksum(address: Address) -> String {
    address.to_checksum(None)
}
