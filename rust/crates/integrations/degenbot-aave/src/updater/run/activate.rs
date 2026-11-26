//! The driver's activation stage: `activate_aave_market` /
//! `deactivate_aave_market` + the one-transaction market-seed substrate fns.

use std::path::Path;

use alloy::primitives::Address;
use degenbot_core::canonical_head::ChainAnchor;
use degenbot_core::runtime::get_runtime;
use degenbot_db::{DbError, DegenbotDb};
use degenbot_rpc::chain_evidence::{resolve_terminal_anchor, BlockReads};
use degenbot_rpc::provider::AlloyProvider;
use degenbot_rpc::read_evidence::ReadEvidence;
use rusqlite::Connection;

use super::RunError;
#[cfg(test)]
use rusqlite::params;

// ── Market activation / deactivation ─────────────────────────────
//
// The one-time market-activation path. These substrate fns seed the
// `aave_v3_markets` row + the `POOL_ADDRESS_PROVIDER` contract row + the GHO
// `erc20_tokens` row (with caller-supplied metadata) + a bare `aave_gho_tokens`
// row (token_id only; the discount columns are filled later by the GHO-config
// apply fns). Pure SQL — NO RPC. The orchestrator `activate_aave_market`
// (below) does the RPC fetches (`getMarketId()` + ERC20 metadata) then calls
// `activate_aave_market_on_conn` in ONE transaction.
//
// Idempotent: re-activating an existing market sets `active = true` + does
// NOT insert duplicate contract / GHO rows: activation flips `market.active`
// on an existing market instead.

/// The block the Aave V3 Ethereum mainnet `PoolAddressProvider` was deployed
/// (TX `0x75fb6e6b…`). The activation path stamps `last_update_block` to the
/// PARENT block (`16_291_070`) so the updater's chunk loop starts AT the
/// deployment block (the `ProxyCreated` events the bootstrap pass resolves
/// fire in `16_291_071`).
///
/// A market reset rewinds `last_update_block` to this same block, which is what
/// makes the following update run take the cold-boot path.
pub const ETHEREUM_AAVE_V3_BOOTSTRAP_BLOCK: i64 = 16_291_070;

/// Seed (or re-activate) an Aave V3 market in ONE transaction.
///
/// - If the market `(chain_id, market_name)` already exists: set `active =
///   true` (no duplicate contract / GHO rows).
/// - If new: INSERT `aave_v3_markets` (`active = true`, `last_update_block =
///   bootstrap_block`), then INSERT the `POOL_ADDRESS_PROVIDER` contract row
///   (idempotent), then get-or-create the GHO `erc20_tokens` row (with
///   metadata `name`/`symbol`/`decimals`), then INSERT a bare
///   `aave_gho_tokens` row referencing it (if absent).
///
/// Returns the market `id`. Pure substrate — NO RPC. The orchestrator
/// [`activate_aave_market`] fetches the market name + GHO metadata via RPC
/// then calls this; the market-reset module
/// ([`reset_market_to_cold_boot`](super::reset_market_to_cold_boot)) reuses
/// that same fetch, then calls this on the caller's transaction.
///
/// # Errors
///
/// Returns [`DbError`] on a `SQLite` query failure.
#[expect(clippy::too_many_arguments)]
pub fn activate_aave_market_on_conn(
    conn: &Connection,
    chain_id: i64,
    market_name: &str,
    pool_address_provider: &str,
    gho_token_address: &str,
    gho_name: Option<&str>,
    gho_symbol: Option<&str>,
    gho_decimals: Option<i64>,
    bootstrap_block: i64,
) -> Result<i64, DbError> {
    use rusqlite::OptionalExtension;

    let existing: Option<i64> = conn
        .query_row(
            "SELECT id FROM aave_v3_markets WHERE chain_id = ?1 AND name = ?2",
            rusqlite::params![chain_id, market_name],
            |row| row.get(0),
        )
        .optional()?;

    if let Some(id) = existing {
        conn.execute(
            "UPDATE aave_v3_markets SET active = 1 WHERE id = ?1",
            rusqlite::params![id],
        )?;

        // COMPLETE the row: the auto-registration seam
        // (`DegenbotDb::register_aave_market`) adds supported markets as
        // inactive BARE rows (no contract/GHO substrate, NULL stamp), so the
        // re-activation path must ensure the same substrate the fresh-seed
        // path writes — else an activated bare row reaches the chunk loop
        // without its FK parents and with a NULL bootstrap stamp
        // (`NeedsBootstrap`).
        DegenbotDb::apply_contract_inserted_if_absent_on_conn(
            conn,
            id,
            "POOL_ADDRESS_PROVIDER",
            pool_address_provider,
            None,
        )?;
        let _gho_token_id = DegenbotDb::get_or_create_erc20_token_on_conn(
            conn,
            chain_id,
            gho_token_address,
            gho_name,
            gho_symbol,
            gho_decimals,
        )?;
        DegenbotDb::get_or_create_gho_token_on_conn(conn, chain_id, gho_token_address)?;

        // Stamp the bootstrap block ONLY when the row is still bare — an
        // already-bootstrapped market keeps its committed cursor (re-running
        // `aave activate` must never rewind `last_update_block`).
        let stamp: Option<i64> = conn.query_row(
            "SELECT last_update_block FROM aave_v3_markets WHERE id = ?1",
            rusqlite::params![id],
            |row| row.get::<_, Option<i64>>(0),
        )?;
        if stamp.is_none() {
            DegenbotDb::set_market_last_update_block_on_conn(conn, id, bootstrap_block)?;
        }
        Ok(id)
    } else {
        conn.execute(
            "INSERT INTO aave_v3_markets (chain_id, name, active, last_update_block) \
                 VALUES (?1, ?2, 1, ?3)",
            rusqlite::params![chain_id, market_name, bootstrap_block],
        )?;
        let market_id = conn.last_insert_rowid();

        DegenbotDb::apply_contract_inserted_if_absent_on_conn(
            conn,
            market_id,
            "POOL_ADDRESS_PROVIDER",
            pool_address_provider,
            None,
        )?;

        // Seed the GHO erc20 row WITH metadata (the orchestrator RPC-
        // fetched it). Must precede the gho-token row (FK).
        let _gho_token_id = DegenbotDb::get_or_create_erc20_token_on_conn(
            conn,
            chain_id,
            gho_token_address,
            gho_name,
            gho_symbol,
            gho_decimals,
        )?;
        // Bare `aave_gho_tokens` row (token_id only). `v_token_id` /
        // discount columns are filled later by the GHO-config apply fns.
        DegenbotDb::get_or_create_gho_token_on_conn(conn, chain_id, gho_token_address)?;
        Ok(market_id)
    }
}

/// Set `active = false` for `market_id` in ONE transaction. The deactivation
/// path — pure substrate, NO RPC.
///
/// # Errors
///
/// Returns [`DbError::MissingRow`] if `market_id` doesn't exist, or
/// [`DbError::Sqlite`] on a query failure.
pub(crate) fn deactivate_aave_market_on_conn(
    conn: &Connection,
    market_id: i64,
) -> Result<(), DbError> {
    let updated = conn.execute(
        "UPDATE aave_v3_markets SET active = 0 WHERE id = ?1",
        rusqlite::params![market_id],
    )?;
    if updated == 0 {
        return Err(DbError::MissingRow(format!(
            "aave_v3_markets id={market_id} (deactivate target)"
        )));
    }
    Ok(())
}

/// The result of a successful [`activate_aave_market`] call.
#[derive(Debug, Clone)]
pub struct ActivatedMarket {
    /// The `aave_v3_markets.id` (the FK every updater fn references).
    pub market_id: i64,
    /// The on-chain `getMarketId()` return (e.g. `"Aave Ethereum Market"`).
    pub market_name: String,
    /// `true` if the market was newly created; `false` if it pre-existed
    /// (re-activation — only `active` was flipped to `true`).
    pub created: bool,
}

/// Activate (or re-activate) an Aave V3 market — the ONE-TIME seed the chunk
/// loop's `AaveMarketWriter::update` bootstraps from.
///
/// RPC fetches (`getMarketId()` on the pool address provider + the GHO
/// token's `name()`/`symbol()`/`decimals()`), then ONE transaction seeds the
/// `aave_v3_markets` row (+ the `POOL_ADDRESS_PROVIDER` contract + the GHO
/// `erc20_tokens` + `aave_gho_tokens` rows) via [`activate_aave_market_on_conn`].
/// Idempotent: re-activating an existing market sets `active = true` + inserts
/// no duplicate rows.
///
/// A standalone Rust consumer (`cargo add degenbot`) can activate a market
/// without Python: this fn owns the market-seeding write path.
///
/// # Args
///
/// - `database_path` — the writeable `DegenbotDb` path.
/// - `chain_id` — the chain.
/// - `pool_address_provider` — the `PoolAddressProvider` contract address
///   (checksummed; stored verbatim).
/// - `gho_token_address` — the chain's GHO token address (checksummed).
/// - `provider` — the injected live transport (ADR-068 D5). The core never
///   builds one; the CLI / FFI / recorder shells construct it from their
///   `rpc_url` and hand it in.
///
/// # Errors
///
/// Returns [`RunError::Provider`] on an RPC failure or [`RunError::Db`] on a
/// DB failure.
///
/// # Shared runtime
///
/// The RPC half runs as ONE future under
/// `degenbot_core::runtime::get_runtime().block_on` at this entry fn
/// (mirrors `AaveMarketWriter::update`'s shape). That `block_on` is legal on the bare
/// `PyO3` fleet threads + the CLI main thread (no ambient tokio context); it
/// MUST NOT be called from within ANY `tokio` runtime context — nested
/// `block_on` panics, the shared runtime included.
pub fn activate_aave_market(
    database_path: &Path,
    chain_id: i64,
    pool_address_provider: &str,
    gho_token_address: &str,
    provider: &AlloyProvider,
) -> Result<ActivatedMarket, RunError> {
    let (db, _state) = DegenbotDb::open_for_writes(database_path)?;

    let ap_address: Address = pool_address_provider.parse().map_err(|_| {
        RunError::Db(DbError::Decode(format!(
            "invalid pool_address_provider address: {pool_address_provider}"
        )))
    })?;
    let gho_address: Address = gho_token_address.parse().map_err(|_| {
        RunError::Db(DbError::Decode(format!(
            "invalid gho_token_address: {gho_token_address}"
        )))
    })?;

    // The ONE `get_runtime().block_on` — the RPC steps are `.await`s inside
    // the driver future, not sequential ad-hoc `block_on`s.
    get_runtime().block_on(activate_aave_market_rpc(
        &db,
        chain_id,
        pool_address_provider,
        gho_token_address,
        provider,
        ap_address,
        gho_address,
    ))
}

/// The RPC-fetched metadata one market activation seeds from: the on-chain
/// market name plus the GHO token's `name()` / `symbol()` / `decimals()`.
///
/// Public so a caller that resolves its own anchor (the cassette recorder) can
/// fetch the same facts at it through [`fetch_activation_metadata_at`] and hand
/// them to [`activate_aave_market_on_conn`] itself.
#[derive(Debug, Clone)]
pub struct ActivationMetadata {
    /// The on-chain `getMarketId()` return (e.g. `"Aave Ethereum Market"`).
    pub market_name: String,
    /// The GHO token's `name()`. `None` only for an established NONCONFORMING
    /// token: the getter answered and reverted, so the field does not exist.
    pub gho_name: Option<String>,
    /// The GHO token's `symbol()`, absent under the same rule as
    /// [`Self::gho_name`].
    pub gho_symbol: Option<String>,
    /// The GHO token's `decimals()`. Required: a token without one cannot be
    /// seeded, so it is never persisted as NULL.
    pub gho_decimals: i64,
}

/// Why a required activation identity fact was not established at the
/// activation anchor.
///
/// Each variant carries the read EVIDENCE, not a summary of it: which spelling
/// was read, who answered, and what the answer carried. Nothing is written when
/// one of them is raised — the fetch runs before the seed transaction opens — so
/// a partial metadata failure cannot persist a NULL identity fact.
#[derive(Debug, thiserror::Error)]
pub enum ActivationMetadataError {
    /// `getMarketId()` carried no decodable string for a reason that says
    /// nothing about the contract's methods (a malformed answer, or an address
    /// holding no code), so the uppercase fallback was never attempted.
    #[error("market id at {address}: getMarketId() {evidence}")]
    MarketIdPrimary {
        /// The address provider the read named.
        address: Address,
        /// What the lowercase read carried.
        evidence: ReadEvidence<String>,
    },
    /// `getMarketId()` reverted — the method a pre-upgrade deployment does not
    /// implement — and the ONE uppercase attempt carried no decodable string
    /// either.
    #[error("market id at {address}: getMarketId() {lower}; GETMARKETID() {upper}")]
    MarketIdFallback {
        /// The address provider the reads named.
        address: Address,
        /// What the lowercase read carried.
        lower: ReadEvidence<String>,
        /// What the single uppercase attempt carried.
        upper: ReadEvidence<String>,
    },
    /// A GHO token text field carried no decodable value. A REVERTED getter is
    /// the established nonconforming case and records absence; this refusal is
    /// every other class, because a NULL identity fact is a permanent claim the
    /// evidence does not support.
    #[error("gho {field} at {address}: {evidence}")]
    GhoText {
        /// The token the read named.
        address: Address,
        /// The getter spelling the read asked for.
        field: &'static str,
        /// What the read carried.
        evidence: ReadEvidence<String>,
    },
    /// The GHO token's `decimals()` carried no usable word. Decimals are
    /// required, so absence here is a refusal rather than a NULL column.
    #[error("gho decimals at {address}: {evidence}")]
    GhoDecimals {
        /// The token the read named.
        address: Address,
        /// What the read carried.
        evidence: ReadEvidence<u64>,
    },
    /// The `decimals()` word does not fit the stored column.
    #[error("gho decimals at {address} = {value} does not fit a signed 64-bit column")]
    GhoDecimalsOutOfRange {
        /// The token the read named.
        address: Address,
        /// The word the token answered.
        value: u64,
    },
}

/// The activation seam's RPC half at ONE caller-supplied exact anchor: read the
/// market name (`getMarketId()` on the address provider, a dynamic `string`
/// return) and the GHO token's metadata at `anchor`.
///
/// Every read names the anchor's block hash with canonical membership required,
/// so the seed's identity facts belong to one block instead of to whatever the
/// endpoint happened to serve between two ambient reads. A caller that owns the
/// block — the cassette recorder, whose seed is a function of the capture parent
/// `from - 1` — hands its own anchor; the head-anchored
/// [`fetch_activation_metadata`] resolves the terminal one and delegates
/// here, so the market-id fallback and the GHO conformance rules exist once.
///
/// # Errors
///
/// [`RunError::ExactRead`] when a read cannot be served at that anchor, and
/// [`RunError::ActivationMetadata`] when a required identity fact was not
/// established.
pub async fn fetch_activation_metadata_at(
    provider: &AlloyProvider,
    anchor: ChainAnchor,
    ap_address: &Address,
    gho_address: &Address,
) -> Result<ActivationMetadata, RunError> {
    let reads = BlockReads::at(provider, anchor);
    let market_name = fetch_market_id(&reads, ap_address).await?;
    let metadata = degenbot_rpc::erc20::read_exact(&reads, *gho_address).await?;
    Ok(ActivationMetadata {
        market_name,
        gho_name: optional_gho_text(metadata.name, gho_address, "name()")?,
        gho_symbol: optional_gho_text(metadata.symbol, gho_address, "symbol()")?,
        gho_decimals: required_gho_decimals(metadata.decimals, gho_address)?,
    })
}

/// The head-anchored activation-metadata fetch shared by
/// [`activate_aave_market`] and the market-reset module: resolve ONE exact
/// terminal anchor, then delegate every identity read to
/// [`fetch_activation_metadata_at`].
///
/// # Errors
///
/// [`RunError::TerminalAnchor`] when the head resolves to no usable exact anchor
/// (a missing header, or an endpoint on another chain), plus every error the
/// anchored read raises.
pub(crate) async fn fetch_activation_metadata(
    provider: &AlloyProvider,
    chain_id: u64,
    ap_address: &Address,
    gho_address: &Address,
) -> Result<ActivationMetadata, RunError> {
    let anchor = resolve_terminal_anchor(provider, chain_id, None).await?;
    fetch_activation_metadata_at(provider, anchor, ap_address, gho_address).await
}

/// A GHO text field: the value, an established nonconforming absence (the
/// getter answered and reverted), or a refusal carrying the evidence.
fn optional_gho_text(
    evidence: ReadEvidence<String>,
    address: &Address,
    field: &'static str,
) -> Result<Option<String>, RunError> {
    match evidence {
        ReadEvidence::Value(text) => Ok(Some(text)),
        ReadEvidence::Reverted(_) => Ok(None),
        evidence => Err(ActivationMetadataError::GhoText {
            address: *address,
            field,
            evidence,
        }
        .into()),
    }
}

/// The GHO token's decimals: a required fact, so every non-value class is a
/// refusal rather than a NULL column.
fn required_gho_decimals(evidence: ReadEvidence<u64>, address: &Address) -> Result<i64, RunError> {
    match evidence {
        ReadEvidence::Value(value) => match i64::try_from(value) {
            Ok(decimals) => Ok(decimals),
            Err(_) => Err(ActivationMetadataError::GhoDecimalsOutOfRange {
                address: *address,
                value,
            }
            .into()),
        },
        evidence => Err(ActivationMetadataError::GhoDecimals {
            address: *address,
            evidence,
        }
        .into()),
    }
}

/// The RPC half of [`activate_aave_market`] — the ONE future under
/// `get_runtime().block_on`. The DB seed transaction runs synchronously at
/// the end (no `.await` while the `db.lock()` guard is held — no
/// `await_holding_lock` exposure).
///
/// # Errors
///
/// See [`activate_aave_market`].
async fn activate_aave_market_rpc(
    db: &DegenbotDb,
    chain_id: i64,
    pool_address_provider: &str,
    gho_token_address: &str,
    provider: &AlloyProvider,
    ap_address: Address,
    gho_address: Address,
) -> Result<ActivatedMarket, RunError> {
    // 1-2. RPC: the market name plus the GHO token metadata at ONE exact head
    //      anchor (the same fetch the market-reset module's re-seed uses).
    let chain = u64::try_from(chain_id).map_err(|_| {
        RunError::Db(DbError::Decode(format!(
            "chain id {chain_id} is not a chain identifier"
        )))
    })?;
    let metadata = fetch_activation_metadata(provider, chain, &ap_address, &gho_address).await?;

    // 3. ONE transaction: seed/activate the market + contract + GHO rows.
    let (market_id, created) = {
        use rusqlite::OptionalExtension;
        let mut guard = db.lock();
        let tx = guard.transaction().map_err(DbError::from)?;
        // Detect re-activation BEFORE the substrate fn (it returns the same id
        // either way; we report `created` for the caller).
        let existed = tx
            .query_row(
                "SELECT 1 FROM aave_v3_markets WHERE chain_id = ?1 AND name = ?2",
                rusqlite::params![chain_id, metadata.market_name.as_str()],
                |_| Ok(()),
            )
            .optional()
            .map_err(DbError::from)?
            .is_some();
        let id = activate_aave_market_on_conn(
            &tx,
            chain_id,
            &metadata.market_name,
            pool_address_provider,
            gho_token_address,
            metadata.gho_name.as_deref(),
            metadata.gho_symbol.as_deref(),
            Some(metadata.gho_decimals),
            ETHEREUM_AAVE_V3_BOOTSTRAP_BLOCK,
        )?;
        tx.commit().map_err(DbError::from)?;
        (id, !existed)
    };

    Ok(ActivatedMarket {
        market_id,
        market_name: metadata.market_name,
        created,
    })
}

/// RPC-fetch `getMarketId()` on the `PoolAddressProvider` (a dynamic `string`
/// return), decoded through the shared string-answer decoder
/// (`degenbot_rpc::erc20::decode_string_return` — the strict dynamic form
/// first, then the `bytes32` answer some impls give).
///
/// The lowercase spelling is read first, and `GETMARKETID()` is attempted ONLY
/// on evidence about the CONTRACT — an execution revert, the answer a
/// pre-upgrade deployment gives for a method it does not implement — exactly
/// once. A transport failure or a malformed answer names itself instead of
/// spending a second read on a fact the chain never denied.
async fn fetch_market_id(reads: &BlockReads<'_>, ap_address: &Address) -> Result<String, RunError> {
    let lower = reads
        .read_evidence::<String>(
            *ap_address,
            crate::config_dispatch::encode_no_arg_call("getMarketId()"),
        )
        .await?;
    let lower = match lower {
        ReadEvidence::Value(name) => return Ok(name),
        evidence @ ReadEvidence::Reverted(_) => evidence,
        evidence => {
            return Err(ActivationMetadataError::MarketIdPrimary {
                address: *ap_address,
                evidence,
            }
            .into());
        }
    };
    let upper = reads
        .read_evidence::<String>(
            *ap_address,
            crate::config_dispatch::encode_no_arg_call("GETMARKETID()"),
        )
        .await?;
    match upper {
        ReadEvidence::Value(name) => Ok(name),
        upper => Err(ActivationMetadataError::MarketIdFallback {
            address: *ap_address,
            lower,
            upper,
        }
        .into()),
    }
}

/// Deactivate an Aave V3 market — set `active = false`.
///
/// # Args
///
/// - `database_path` — the writeable `DegenbotDb` path.
/// - `market_id` — the `aave_v3_markets.id` to deactivate.
///
/// # Errors
///
/// Returns [`RunError::MarketNotFound`] if `market_id` doesn't exist, or
/// [`RunError::Db`] on a DB failure.
pub fn deactivate_aave_market(database_path: &Path, market_id: i64) -> Result<(), RunError> {
    let (db, _state) = DegenbotDb::open_for_writes(database_path)?;
    {
        let mut guard = db.lock();
        let tx = guard.transaction().map_err(DbError::from)?;
        deactivate_aave_market_on_conn(&tx, market_id).map_err(|e| match e {
            DbError::MissingRow(_) => RunError::MarketNotFound(market_id),
            other => RunError::Db(other),
        })?;
        tx.commit().map_err(DbError::from)?;
    }
    Ok(())
}

#[cfg(test)]
#[expect(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    use alloy::primitives::{keccak256, Bytes};

    use crate::updater::test_chain::{
        Answer, ScriptedChain, CHAIN_ID, HEAD_HASH, HEAD_NUMBER, HEAD_PARENT,
    };

    const AP_ADDRESS: &str = "0x2f39d218133AFaB8F2B819B1066c7E434Ad94E9e";
    const GHO_ADDRESS: &str = "0x40D16FC0246aD3160Ccc09B8D0D3A2cD28aE6C2f";

    fn ap() -> Address {
        AP_ADDRESS.parse().expect("the fixture address parses")
    }

    fn gho() -> Address {
        GHO_ADDRESS.parse().expect("the fixture address parses")
    }

    fn sel(signature: &str) -> [u8; 4] {
        let hash = keccak256(signature.as_bytes());
        let mut selector = [0u8; 4];
        selector.copy_from_slice(&hash[..4]);
        selector
    }

    /// The ABI dynamic-`string` return for `text`.
    fn text(value: &str) -> Bytes {
        let raw = value.as_bytes();
        let mut out = vec![0u8; 31];
        out.push(0x20);
        let mut length = [0u8; 32];
        length[24..].copy_from_slice(&u64::try_from(raw.len()).unwrap().to_be_bytes());
        out.extend_from_slice(&length);
        let mut data = raw.to_vec();
        data.resize(raw.len().div_ceil(32) * 32, 0);
        out.extend_from_slice(&data);
        Bytes::from(out)
    }

    /// One 32-byte word holding `value`.
    fn word(value: u64) -> Bytes {
        let mut out = [0u8; 32];
        out[24..].copy_from_slice(&value.to_be_bytes());
        Bytes::copy_from_slice(&out)
    }

    /// A return that is neither a dynamic `string` nor a `bytes32` answer: the
    /// malformed evidence every text field refuses.
    fn short() -> Bytes {
        Bytes::from(vec![0x01u8, 0x02])
    }

    /// The activation-metadata refusal a `RunError` carries (the payload is boxed
    /// so `RunError` stays a small `Err`).
    fn activation_refusal(error: &RunError) -> Option<&ActivationMetadataError> {
        match error {
            RunError::ActivationMetadata(inner) => Some(inner),
            _ => None,
        }
    }

    /// The activation reads the fixture serves: the market id plus a
    /// conforming GHO metadata triple.
    fn scripted_activation(chain: ScriptedChain) -> ScriptedChain {
        chain
            .answer(
                ap(),
                sel("getMarketId()"),
                Answer::Return(text("Aave Ethereum Market")),
            )
            .answer(gho(), sel("name()"), Answer::Return(text("Gho Token")))
            .answer(gho(), sel("symbol()"), Answer::Return(text("GHO")))
            .answer(gho(), sel("decimals()"), Answer::Return(word(18)))
    }

    /// Every activation read names ONE exact block: the same `{blockHash,
    /// requireCanonical: true}` pin on the market-id read and on every GHO
    /// metadata read. An ambient (latest) read would name no block at all.
    #[test]
    fn every_activation_read_carries_the_one_resolved_pin() {
        let chain = scripted_activation(ScriptedChain::new());
        let provider = get_runtime().block_on(chain.provider());
        let metadata = get_runtime()
            .block_on(fetch_activation_metadata(
                &provider,
                CHAIN_ID,
                &ap(),
                &gho(),
            ))
            .expect("the scripted chain serves every activation read");

        assert_eq!(metadata.market_name, "Aave Ethereum Market");
        assert_eq!(metadata.gho_name.as_deref(), Some("Gho Token"));
        assert_eq!(metadata.gho_symbol.as_deref(), Some("GHO"));
        assert_eq!(metadata.gho_decimals, 18);

        let pin = serde_json::json!({
            "blockHash": format!("{HEAD_HASH}"),
            "requireCanonical": true,
        });
        let reads = chain.reads();
        assert!(!reads.is_empty(), "the activation read the chain");
        for read in reads {
            assert_eq!(
                read.block_id(),
                Some(&pin),
                "{} did not name the resolved activation anchor",
                read.method
            );
        }
    }

    /// The `_at` form reads at the anchor it is handed and resolves nothing: no
    /// head/header probe, one pin on every read. The recorder calls this at
    /// `from - 1`, so a capture's identity facts belong to the capture parent.
    #[test]
    fn fetch_activation_metadata_at_reads_the_named_anchor_without_resolving_one() {
        let chain = scripted_activation(ScriptedChain::new());
        let provider = get_runtime().block_on(chain.provider());
        let anchor = degenbot_core::canonical_head::ChainAnchor::new(
            CHAIN_ID,
            HEAD_NUMBER,
            HEAD_HASH,
            HEAD_PARENT,
        )
        .expect("the fixture names one exact anchor");

        let metadata = get_runtime()
            .block_on(fetch_activation_metadata_at(
                &provider,
                anchor,
                &ap(),
                &gho(),
            ))
            .expect("the scripted chain serves every activation read at the anchor");

        assert_eq!(metadata.market_name, "Aave Ethereum Market");
        assert_eq!(metadata.gho_name.as_deref(), Some("Gho Token"));
        assert_eq!(metadata.gho_decimals, 18);

        let methods: Vec<String> = chain
            .issued()
            .into_iter()
            .map(|request| request.method)
            .collect();
        assert!(
            methods
                .iter()
                .all(|method| method == "eth_call" || method == "eth_getCode"),
            "the `_at` form resolves no anchor, issued {methods:?}"
        );
        let pin = serde_json::json!({
            "blockHash": format!("{HEAD_HASH}"),
            "requireCanonical": true,
        });
        let reads = chain.reads();
        assert!(!reads.is_empty(), "the activation read the chain");
        for read in reads {
            assert_eq!(
                read.block_id(),
                Some(&pin),
                "{} left the named anchor",
                read.method
            );
        }
    }

    /// The head-anchored form keeps its production shape: it resolves ONE
    /// terminal anchor and delegates every identity read to the `_at` form.
    #[test]
    fn fetch_activation_metadata_resolves_one_terminal_anchor_and_delegates() {
        let chain = scripted_activation(ScriptedChain::new());
        let provider = get_runtime().block_on(chain.provider());

        let metadata = get_runtime()
            .block_on(fetch_activation_metadata(
                &provider,
                CHAIN_ID,
                &ap(),
                &gho(),
            ))
            .expect("the scripted chain serves every activation read");

        assert_eq!(metadata.market_name, "Aave Ethereum Market");
        assert_eq!(metadata.gho_symbol.as_deref(), Some("GHO"));

        let headers = chain
            .issued()
            .into_iter()
            .filter(|request| request.method == "eth_getBlockByNumber")
            .count();
        assert_eq!(
            headers, 1,
            "the head-anchored form resolves exactly one terminal anchor"
        );
        let pin = serde_json::json!({
            "blockHash": format!("{HEAD_HASH}"),
            "requireCanonical": true,
        });
        for read in chain.reads() {
            assert_eq!(
                read.block_id(),
                Some(&pin),
                "{} did not name the resolved anchor",
                read.method
            );
        }
    }

    /// A transport failure keeps its own classification and does NOT spend the
    /// one uppercase market-id attempt: only an execution revert (or a node
    /// stating the method is unsupported) says anything about the CONTRACT.
    #[test]
    fn a_transport_failure_does_not_trigger_the_uppercase_market_id_fallback() {
        let chain = scripted_activation(ScriptedChain::new()).answer(
            ap(),
            sel("getMarketId()"),
            Answer::Refuse {
                code: -32_603,
                message: "the node stopped answering".into(),
            },
        );
        let provider = get_runtime().block_on(chain.provider());
        let error = get_runtime()
            .block_on(fetch_activation_metadata(
                &provider,
                CHAIN_ID,
                &ap(),
                &gho(),
            ))
            .expect_err("a transport failure is not an activation evidenced by the chain");

        assert_eq!(
            chain.calls_to(ap(), sel("GETMARKETID()")).len(),
            0,
            "a transport fault must not read the uppercase spelling"
        );
        assert!(
            matches!(
                &error,
                RunError::ExactRead(degenbot_rpc::chain_evidence::ExactReadError::Failed {
                    cause: degenbot_core::errors::ProviderError::RpcError { code: -32_603, .. },
                    ..
                })
            ),
            "the transport failure keeps its provider classification, got {error:?}"
        );
    }

    /// A lowercase market-id revert is evidence about the CONTRACT (the method a
    /// pre-upgrade deployment does not implement): exactly ONE uppercase attempt
    /// follows, and its answer is the activation's market name.
    #[test]
    fn a_lowercase_market_id_revert_triggers_exactly_one_uppercase_attempt() {
        let chain = scripted_activation(ScriptedChain::new())
            .answer(ap(), sel("getMarketId()"), Answer::Revert)
            .answer(
                ap(),
                sel("GETMARKETID()"),
                Answer::Return(text("Aave Ethereum Market")),
            );
        let provider = get_runtime().block_on(chain.provider());
        let metadata = get_runtime()
            .block_on(fetch_activation_metadata(
                &provider,
                CHAIN_ID,
                &ap(),
                &gho(),
            ))
            .expect("the uppercase spelling names the market");

        assert_eq!(metadata.market_name, "Aave Ethereum Market");
        assert_eq!(chain.calls_to(ap(), sel("getMarketId()")).len(), 1);
        assert_eq!(chain.calls_to(ap(), sel("GETMARKETID()")).len(), 1);
    }

    /// Both spellings reverting is a typed refusal that names both attempts - and
    /// still spends exactly one uppercase read.
    #[test]
    fn a_reverting_uppercase_fallback_refuses_naming_both_attempts() {
        let chain = scripted_activation(ScriptedChain::new())
            .answer(ap(), sel("getMarketId()"), Answer::Revert)
            .answer(ap(), sel("GETMARKETID()"), Answer::Revert);
        let provider = get_runtime().block_on(chain.provider());
        let error = get_runtime()
            .block_on(fetch_activation_metadata(
                &provider,
                CHAIN_ID,
                &ap(),
                &gho(),
            ))
            .expect_err("neither spelling names the market");

        assert!(
            matches!(
                activation_refusal(&error),
                Some(ActivationMetadataError::MarketIdFallback {
                    lower: ReadEvidence::Reverted(_),
                    upper: ReadEvidence::Reverted(_),
                    ..
                })
            ),
            "the refusal carries both attempts' evidence, got {error:?}"
        );
        assert_eq!(chain.calls_to(ap(), sel("GETMARKETID()")).len(), 1);
    }

    /// A malformed market-id answer says nothing about the method, so the
    /// uppercase spelling is never read.
    #[test]
    fn a_malformed_market_id_propagates_without_a_fallback() {
        let chain = scripted_activation(ScriptedChain::new()).answer(
            ap(),
            sel("getMarketId()"),
            Answer::Return(short()),
        );
        let provider = get_runtime().block_on(chain.provider());
        let error = get_runtime()
            .block_on(fetch_activation_metadata(
                &provider,
                CHAIN_ID,
                &ap(),
                &gho(),
            ))
            .expect_err("a malformed answer is not a market name");

        assert!(
            matches!(
                activation_refusal(&error),
                Some(ActivationMetadataError::MarketIdPrimary {
                    evidence: ReadEvidence::Malformed { .. },
                    ..
                })
            ),
            "the malformed answer names itself, got {error:?}"
        );
        assert_eq!(chain.calls_to(ap(), sel("GETMARKETID()")).len(), 0);
    }

    /// Decimals are REQUIRED: a reverting `decimals()` is a typed refusal, never a
    /// NULL column.
    #[test]
    fn a_missing_decimals_refuses_the_activation() {
        let chain = scripted_activation(ScriptedChain::new()).answer(
            gho(),
            sel("decimals()"),
            Answer::Revert,
        );
        let provider = get_runtime().block_on(chain.provider());
        let error = get_runtime()
            .block_on(fetch_activation_metadata(
                &provider,
                CHAIN_ID,
                &ap(),
                &gho(),
            ))
            .expect_err("decimals are required");

        assert!(
            matches!(
                activation_refusal(&error),
                Some(ActivationMetadataError::GhoDecimals {
                    evidence: ReadEvidence::Reverted(_),
                    ..
                })
            ),
            "the missing decimals name their evidence, got {error:?}"
        );
    }

    /// A reverting `name()` is the ONE permitted absence: the getter answered and
    /// reverted, so the field does not exist. The uppercase spelling is not
    /// consulted (a call failure ends its field) and the rest still seeds.
    #[test]
    fn a_reverting_name_is_an_established_nonconforming_absence() {
        let chain =
            scripted_activation(ScriptedChain::new()).answer(gho(), sel("name()"), Answer::Revert);
        let provider = get_runtime().block_on(chain.provider());
        let metadata = get_runtime()
            .block_on(fetch_activation_metadata(
                &provider,
                CHAIN_ID,
                &ap(),
                &gho(),
            ))
            .expect("a nonconforming name still seeds the rest of the metadata");

        assert_eq!(metadata.gho_name, None);
        assert_eq!(metadata.gho_symbol.as_deref(), Some("GHO"));
        assert_eq!(metadata.gho_decimals, 18);
        assert_eq!(chain.calls_to(gho(), sel("NAME()")).len(), 0);
    }

    /// A malformed name falls through to the uppercase spelling, exactly as the
    /// stop-on-call-error metadata contract states.
    #[test]
    fn a_malformed_name_falls_through_to_the_uppercase_spelling() {
        let chain = scripted_activation(ScriptedChain::new())
            .answer(gho(), sel("name()"), Answer::Return(short()))
            .answer(gho(), sel("NAME()"), Answer::Return(text("Gho Token")));
        let provider = get_runtime().block_on(chain.provider());
        let metadata = get_runtime()
            .block_on(fetch_activation_metadata(
                &provider,
                CHAIN_ID,
                &ap(),
                &gho(),
            ))
            .expect("the uppercase spelling names the token");

        assert_eq!(metadata.gho_name.as_deref(), Some("Gho Token"));
        assert_eq!(chain.calls_to(gho(), sel("NAME()")).len(), 1);
    }

    /// A malformed name on BOTH spellings is a refusal: a NULL name is a
    /// permanent identity claim the evidence does not support.
    #[test]
    fn a_malformed_name_on_both_spellings_refuses_the_activation() {
        let chain = scripted_activation(ScriptedChain::new())
            .answer(gho(), sel("name()"), Answer::Return(short()))
            .answer(gho(), sel("NAME()"), Answer::Return(short()));
        let provider = get_runtime().block_on(chain.provider());
        let error = get_runtime()
            .block_on(fetch_activation_metadata(
                &provider,
                CHAIN_ID,
                &ap(),
                &gho(),
            ))
            .expect_err("a malformed name is not an established absence");

        assert!(
            matches!(
                activation_refusal(&error),
                Some(ActivationMetadataError::GhoText {
                    field: "name()",
                    ..
                })
            ),
            "the refusal names the field, got {error:?}"
        );
        assert_eq!(chain.calls_to(gho(), sel("NAME()")).len(), 1);
    }

    /// A GHO address holding no code at the activation anchor refuses, and issues
    /// no metadata call at all.
    #[test]
    fn a_code_less_gho_token_refuses_before_reading_metadata() {
        let chain = scripted_activation(ScriptedChain::new()).no_code(gho());
        let provider = get_runtime().block_on(chain.provider());
        let error = get_runtime()
            .block_on(fetch_activation_metadata(
                &provider,
                CHAIN_ID,
                &ap(),
                &gho(),
            ))
            .expect_err("a code-less token carries no metadata");

        assert!(
            matches!(
                activation_refusal(&error),
                Some(ActivationMetadataError::GhoText {
                    evidence: ReadEvidence::NoCode { .. },
                    ..
                })
            ),
            "the refusal states the missing code, got {error:?}"
        );
        assert_eq!(chain.calls_to(gho(), sel("name()")).len(), 0);
    }

    /// An endpoint serving another chain cannot state the market's identity facts.
    #[test]
    fn a_cross_chain_endpoint_refuses_the_activation() {
        let chain = scripted_activation(ScriptedChain::new()).chain(2);
        let provider = get_runtime().block_on(chain.provider());
        let error = get_runtime()
            .block_on(fetch_activation_metadata(
                &provider,
                CHAIN_ID,
                &ap(),
                &gho(),
            ))
            .expect_err("an endpoint on chain 2 is not the market's chain");

        assert!(
            matches!(&error, RunError::TerminalAnchor(_)),
            "a cross-chain endpoint is a terminal-anchor refusal, got {error:?}"
        );
    }

    /// A metadata failure writes NOTHING: the fetch runs before the seed
    /// transaction opens, so no market row (and no NULL identity fact) survives.
    #[test]
    fn a_metadata_failure_persists_no_market_row() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("activation.db");
        {
            let (_db, _state) =
                DegenbotDb::open_for_writes(&path).expect("the empty database initializes");
        }
        let chain = scripted_activation(ScriptedChain::new()).answer(
            gho(),
            sel("decimals()"),
            Answer::Revert,
        );
        let provider = get_runtime().block_on(chain.provider());

        let error = activate_aave_market(&path, 1, AP_ADDRESS, GHO_ADDRESS, &provider)
            .expect_err("a missing decimals answer refuses the activation");
        assert!(
            activation_refusal(&error).is_some(),
            "the activation refuses on its metadata evidence, got {error:?}"
        );

        let conn = Connection::open(&path).expect("the activation database opens");
        let markets: i64 = conn
            .query_row("SELECT COUNT(*) FROM aave_v3_markets", [], |row| row.get(0))
            .expect("the market table exists");
        assert_eq!(markets, 0, "a refused activation writes no market row");
        let gho_rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM aave_gho_tokens", [], |row| row.get(0))
            .expect("the GHO table exists");
        assert_eq!(gho_rows, 0, "a refused activation writes no GHO row");
    }

    // ── Market activation substrate ──────────────────────────────

    /// A fresh in-memory writeable DB with NO pre-seeded market (the
    /// activation path seeds it from scratch).
    fn fresh_db_empty() -> DegenbotDb {
        DegenbotDb::open_in_memory_for_writes().unwrap().0
    }

    #[test]
    fn activate_completes_a_bare_pre_registered_market() {
        // The auto-registration seam (`register_aave_market`) adds supported
        // markets as INACTIVE, BARE rows (no contract/GHO substrate, NULL
        // stamp). Activation must COMPLETE such a row: flip it active, ensure
        // the POOL_ADDRESS_PROVIDER + GHO rows, and — the critical part —
        // stamp the bootstrap block only when the row has none (a row that an
        // earlier bootstrap already stamped keeps its stamp; re-running
        // activate must never rewind `last_update_block`).
        let db = fresh_db_empty();
        let pool_ap = "0x2f39d218133AFaB8F2B819B1066c7E434Ad94E9e";
        let gho_addr = "0x40D16FC0246aD3160Ccc09B8D0D3A2cD28aE6C2f";

        // The bare pre-registered row (the auto-registration shape).
        let market_id = db
            .register_aave_market(1, "Aave Ethereum Market")
            .unwrap()
            .0;

        {
            let mut guard = db.lock();
            let tx = guard.transaction().unwrap();
            let id = activate_aave_market_on_conn(
                &tx,
                1,
                "Aave Ethereum Market",
                pool_ap,
                gho_addr,
                Some("GHO Token"),
                Some("GHO"),
                Some(18),
                16_291_070,
            )
            .unwrap();
            assert_eq!(id, market_id, "activation reuses the registered row");
            tx.commit().unwrap();
        }

        let conn = db.lock();
        let (active, stamp): (bool, Option<i64>) = conn
            .query_row(
                "SELECT active, last_update_block FROM aave_v3_markets WHERE id = ?1",
                params![market_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert!(active);
        assert_eq!(
            stamp,
            Some(16_291_070),
            "a bare row gains its bootstrap stamp on activation"
        );

        // The substrate the bare row lacked is now in place.
        let n_contracts: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM aave_v3_contracts \
                 WHERE market_id = ?1 AND name = 'POOL_ADDRESS_PROVIDER' AND address = ?2",
                params![market_id, pool_ap],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n_contracts, 1);
        let n_gho: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM aave_gho_tokens \
                 WHERE token_id IN (SELECT id FROM erc20_tokens WHERE address = ?1)",
                params![gho_addr],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n_gho, 1);
    }

    #[test]
    fn activate_aave_market_on_conn_seeds_market_contract_gho_token() {
        let db = fresh_db_empty();
        let pool_ap = "0x2f39d218133AFaB8F2B819B1066c7E434Ad94E9e";
        let gho_addr = "0x40D16FC0246aD3160Ccc09B8D0D3A2cD28aE6C2f";

        let market_id = {
            let mut guard = db.lock();
            let tx = guard.transaction().unwrap();
            let id = activate_aave_market_on_conn(
                &tx,
                1,
                "Aave Ethereum Market",
                pool_ap,
                gho_addr,
                Some("GHO Token"),
                Some("GHO"),
                Some(18),
                16_291_070,
            )
            .unwrap();
            tx.commit().unwrap();
            id
        };

        let conn = db.lock();
        // Market row: active + stamped.
        let (name, active, stamp): (String, bool, Option<i64>) = conn
            .query_row(
                "SELECT name, active, last_update_block FROM aave_v3_markets WHERE id = ?1",
                params![market_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(name, "Aave Ethereum Market");
        assert!(active);
        assert_eq!(stamp, Some(16_291_070));

        // POOL_ADDRESS_PROVIDER contract row.
        let n_contracts: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM aave_v3_contracts \
                 WHERE market_id = ?1 AND name = 'POOL_ADDRESS_PROVIDER' AND address = ?2",
                params![market_id, pool_ap],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n_contracts, 1);

        // GHO erc20 token row with metadata.
        let (sym, dec): (String, i64) = conn
            .query_row(
                "SELECT symbol, decimals FROM erc20_tokens \
                 WHERE chain = 1 AND address = ?1",
                params![gho_addr],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(sym, "GHO");
        assert_eq!(dec, 18);

        // aave_gho_tokens row referencing the GHO erc20 token.
        let n_gho: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM aave_gho_tokens g \
                 JOIN erc20_tokens e ON e.id = g.token_id \
                 WHERE e.chain = 1 AND e.address = ?1",
                params![gho_addr],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n_gho, 1);
    }

    #[test]
    fn activate_aave_market_on_conn_reactivates_existing_market_idempotently() {
        let db = fresh_db_empty();
        let pool_ap = "0x2f39d218133AFaB8F2B819B1066c7E434Ad94E9e";
        let gho_addr = "0x40D16FC0246aD3160Ccc09B8D0D3A2cD28aE6C2f";

        let first_id = {
            let mut guard = db.lock();
            let tx = guard.transaction().unwrap();
            let id = activate_aave_market_on_conn(
                &tx,
                1,
                "Aave Ethereum Market",
                pool_ap,
                gho_addr,
                Some("GHO Token"),
                Some("GHO"),
                Some(18),
                16_291_070,
            )
            .unwrap();
            tx.commit().unwrap();
            id
        };

        // Deactivate to prove re-activation flips active back to true.
        {
            let mut guard = db.lock();
            let tx = guard.transaction().unwrap();
            deactivate_aave_market_on_conn(&tx, first_id).unwrap();
            tx.commit().unwrap();
        }

        // Second activation: market exists → set active=true, no dup rows.
        let second_id = {
            let mut guard = db.lock();
            let tx = guard.transaction().unwrap();
            let id = activate_aave_market_on_conn(
                &tx,
                1,
                "Aave Ethereum Market",
                pool_ap,
                gho_addr,
                Some("GHO Token"),
                Some("GHO"),
                Some(18),
                16_291_070,
            )
            .unwrap();
            tx.commit().unwrap();
            id
        };
        assert_eq!(
            first_id, second_id,
            "re-activation returns the same market_id"
        );

        let conn = db.lock();
        let active: bool = conn
            .query_row(
                "SELECT active FROM aave_v3_markets WHERE id = ?1",
                params![first_id],
                |r| r.get(0),
            )
            .unwrap();
        assert!(active, "re-activation sets active=true");

        // No duplicate contract rows.
        let n_contracts: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM aave_v3_contracts \
                 WHERE market_id = ?1 AND name = 'POOL_ADDRESS_PROVIDER'",
                params![first_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n_contracts, 1);

        // No duplicate gho token rows.
        let n_gho: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM aave_gho_tokens g \
                 JOIN erc20_tokens e ON e.id = g.token_id \
                 WHERE e.chain = 1 AND e.address = ?1",
                params![gho_addr],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n_gho, 1);
    }

    #[test]
    fn deactivate_aave_market_on_conn_sets_active_false() {
        let db = fresh_db_empty();
        let pool_ap = "0x2f39d218133AFaB8F2B819B1066c7E434Ad94E9e";
        let gho_addr = "0x40D16FC0246aD3160Ccc09B8D0D3A2cD28aE6C2f";

        let market_id = {
            let mut guard = db.lock();
            let tx = guard.transaction().unwrap();
            let id = activate_aave_market_on_conn(
                &tx,
                1,
                "Aave Ethereum Market",
                pool_ap,
                gho_addr,
                Some("GHO Token"),
                Some("GHO"),
                Some(18),
                16_291_070,
            )
            .unwrap();
            tx.commit().unwrap();
            id
        };

        {
            let mut guard = db.lock();
            let tx = guard.transaction().unwrap();
            deactivate_aave_market_on_conn(&tx, market_id).unwrap();
            tx.commit().unwrap();
        }

        let conn = db.lock();
        let active: bool = conn
            .query_row(
                "SELECT active FROM aave_v3_markets WHERE id = ?1",
                params![market_id],
                |r| r.get(0),
            )
            .unwrap();
        assert!(!active, "deactivate sets active=false");
    }
}
