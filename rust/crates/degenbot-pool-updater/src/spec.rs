//! `ExchangeSpec` + `load_active_exchange_specs` — the typed per-exchange
//! dispatch descriptor the Rust-owned pool-updater chunk loop consumes
//! (epic `2SFL6I`, task SHFIGX).
//!
//! Per ADR-005 + AGENTS.md "Rust owns the state": Rust reads the `exchanges`
//! table itself at run start (the DB is the source of truth) rather than
//! receiving a Python-built exchange list through FFI. This module joins the
//! DB row ([`degenbot_db::ExchangeRow`], read via
//! [`DegenbotDb::fetch_active_exchanges_by_chain`]) with a **static
//! event-config map** — the Rust equivalent of `src/degenbot/cli/pool.py`'s
//! `_V2_CONFIGS`/`_V3_CONFIGS`/`_V4_CONFIGS` dataclasses — to build a single
//! [`ExchangeSpec`] carrying everything the chunk fn (task `CKXCOB`) needs to
//! dispatch per exchange: the resolved [`PoolFamily`] (decode-leaf
//! discriminator), the per-exchange `PoolCreated` topic0 (from task `SBICJJ`'s
//! `degenbot-decoders` constants), the `fee_denominator`, and (for Aerodrome
//! DEXes) the RPC fee-override call descriptor.
//!
//! ## Three-layer placement
//! [`ExchangeSpec`] + [`load_active_exchange_specs`] live here, in the
//! chunk-loop crate (`degenbot-pool-updater`), NOT in `degenbot-db`. Reason:
//! the spec joins DB data with chunk-loop-specific concerns (the
//! [`PoolFamily`] decode discriminator + the static event-config map +
//! [`RpcFeeCall`]). Putting it in `degenbot-db` would couple the generic DB
//! crate to decode/fetch concerns + force a `degenbot-decoders` dependency on
//! the DB crate (today `degenbot-db` is a pure DB layer with no decoder dep —
//! kept that way). The chunk-loop crate already owns [`PoolFamily`] (task
//! `SBICJJ`'s `fetch.rs`) + the fetchers, so it's the natural home for the
//! resolved spec too. A standalone Rust consumer (`cargo add
//! degenbot-pool-updater`) gets the loader + the spec + the fetchers in one
//! self-contained crate.
//!
//! ## Unknown exchange names
//! [`load_active_exchange_specs`] resolves each row's `name` against the
//! static config map. An unknown name (an exchange the Rust side doesn't yet
//! know about — e.g. a newly-added DEX whose config hasn't been ported) is
//! **silently skipped** (forward-compat: the chunk loop updates the exchanges
//! it can resolve; the unknown ones are left for a future Rust build). A
//! `log::warn!` would be the ideal diagnostic, but this crate deliberately
//! has no `log` dependency yet (kept minimal); when task `CKXCOB` wires up
//! `tracing`, the skip site is the natural warn target.

use alloy::primitives::{Address, B256};
use degenbot_db::rows::ExchangeRow;
use degenbot_db::DegenbotDb;
use degenbot_decoders::pool_created_decoder::{
    AERODROME_V2_POOL_CREATED_TOPIC, AERODROME_V3_POOL_CREATED_TOPIC, V2_PAIR_CREATED_TOPIC,
    V3_POOL_CREATED_TOPIC, V4_INITIALIZE_TOPIC,
};

use crate::fetch::PoolFamily;

/// The typed per-exchange dispatch descriptor the chunk loop consumes.
///
/// Built by joining an [`ExchangeRow`] (from the `exchanges` table — the DB is
/// the source of truth for `id`/`chain_id`/`name`/`factory`) with the static
/// event-config map (the Rust equivalent of `src/degenbot/cli/pool.py`'s
/// per-family `V2PoolUpdateConfig`/`V3PoolUpdateConfig`/`V4PoolUpdateConfig`
/// dataclasses — the source of truth for `family`/`event_topic`/
/// `fee_denominator`/`v2_fee_token`/`rpc_fee_call`, which are NOT in the DB).
///
/// One spec carries everything the chunk fn (task `CKXCOB`) needs to fetch +
/// decode + persist one exchange's pool-creation events without consulting
/// Python.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExchangeSpec {
    /// The `exchanges.id` PK (the `exchange_id` threaded into the V2/V3/V4
    /// pool-row upserts — `db_upsert_v2_pools(exchange_id=…)`, etc.).
    pub id: i64,
    /// The `exchanges.chain_id` (the chain to fetch logs from + the
    /// `chain_id` threaded into the row upserts).
    pub chain_id: i64,
    /// The exchange `name` discriminator (`"uniswap_v2"`, `"uniswap_v3"`,
    /// `"aerodrome_v2"`, ...). Drives [`Self::family`] + the static-config
    /// resolution. Mirrors the Python `exchange.name` dispatch key.
    pub name: String,
    /// The `exchanges.factory` — the `PoolCreated`/`Initialize` emitter (the
    /// `address` filter on `eth_getLogs`). Checksummed.
    pub factory: Address,
    /// The decode-leaf discriminator (which `decode_*_pool_created_log` the
    /// chunk fn dispatches to). Distinct from the DB's
    /// `ExchangeFamily` (a V3/V4-only SQL-suffix helper); [`PoolFamily`]
    /// covers V2/AerodromeV2/V3/V4 because Aerodrome V2's `PoolCreated` is
    /// structurally distinct from the canonical V2 `PairCreated` (different
    /// topic0 + a `stable` indexed flag).
    pub family: PoolFamily,
    /// The per-exchange resolved `PoolCreated`/`Initialize` topic0. From
    /// task `SBICJJ`'s `degenbot-decoders` constants (NOT re-declared here).
    /// Note: [`PoolFamily`] alone is NOT enough to resolve the topic —
    /// `aerodrome_v3` shares the V3 decode structure but has its own topic
    /// ([`AERODROME_V3_POOL_CREATED_TOPIC`] vs the canonical
    /// [`V3_POOL_CREATED_TOPIC`]).
    pub event_topic: B256,
    /// The V2/V3/V4 fee denominator (`1000` for `uniswap_v2`/`sushiswap_v2`/
    /// `swapbased_v2`, `10000` for `pancakeswap_v2`/`aerodrome_v2`,
    /// `1_000_000` for all V3/V4). Threaded into the row upserts as the
    /// `fee_denominator` arg (the DB stores fee numerators, not ratios).
    pub fee_denominator: i64,
    /// V2-only constant fee numerator (the `fee_token0`/`fee_token1` on
    /// `V2PoolUpdateConfig`). All constant-fee V2 DEXes have
    /// `fee_token0 == fee_token1` (e.g. `uniswap_v2`: 3, `pancakeswap_v2`:
    /// 25, `sushiswap_v2`/`swapbased_v2`: 3), so a single `Option<i64>`
    /// captures both. `None` for V3/V4 (the fee comes from the event) + for
    /// `aerodrome_v2` (the fee is RPC-resolved per pool via
    /// [`Self::rpc_fee_call`]). If a future V2 DEX has asymmetric
    /// `fee_token0` != `fee_token1`, this field's shape must change (none
    /// exist today).
    pub v2_fee_token: Option<i64>,
    /// The per-pool RPC fee-override call descriptor. `None` for all
    /// non-Aerodrome DEXes (their fee is either a constant
    /// [`Self::v2_fee_token`] for V2, or read from the event for V3/V4).
    /// `Some` for `aerodrome_v2` (`getFee(address,bool)→uint256`) and
    /// `aerodrome_v3` (`getSwapFee(address)→uint24`) — their fees come from
    /// an on-chain call per pool rather than a constant or the event.
    pub rpc_fee_call: Option<RpcFeeCall>,
}

/// The RPC fee-override call an Aerodrome exchange requires (the Rust
/// equivalent of `V2PoolUpdateConfig.rpc_fee_call` +
/// `rpc_fee_return_types` + `rpc_fee_includes_stable` in
/// `src/degenbot/cli/pool_updater_configs.py`).
///
/// Modeled as a closed enum (not stringly-typed fields) because only two
/// variants exist today (`aerodrome_v2`, `aerodrome_v3`) — encoding the
/// call signature + return type + whether the `stable` arg is threaded as
/// a single discriminant that the chunk fn (task `CKXCOB`) matches on to
/// build the right `degenbot-abi`/`alloy sol!` call. Adding a third
/// variant is a deliberate breaking change (forcing a config review).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RpcFeeCall {
    /// `aerodrome_v2` — `getFee(address pool, bool stable) returns (uint256)`.
    /// The call takes the pool address + the per-pool `stable` flag (decoded
    /// from the `PoolCreated` event's indexed `stable` topic). The returned
    /// `uint256` is the fee numerator for BOTH token0 and token1 (Aerodrome
    /// V2 pools have symmetric fees). Overrides [`ExchangeSpec::v2_fee_token`]
    /// (which is `None` for `aerodrome_v2`).
    AerodromeV2,
    /// `aerodrome_v3` — `getSwapFee(address pool) returns (uint24)`. The call
    /// takes only the pool address (no `stable` arg — Aerodrome V3 pools
    /// aren't stable/volatile-tagged the way V2 is). The returned `uint24`
    /// overrides the event's indexed `fee` topic.
    AerodromeV3,
}

/// The static per-exchange config (everything NOT in the `exchanges` DB
/// table). The Rust equivalent of `src/degenbot/cli/pool.py`'s
/// `_V2_CONFIGS`/`_V3_CONFIGS`/`_V4_CONFIGS` dataclasses.
///
/// Kept private to this crate — callers consume it through [`ExchangeSpec`]
/// (built by [`load_active_exchange_specs`]); the resolver is an internal
/// detail.
#[derive(Clone, Debug, PartialEq, Eq)]
struct StaticExchangeConfig {
    family: PoolFamily,
    event_topic: B256,
    fee_denominator: i64,
    v2_fee_token: Option<i64>,
    rpc_fee_call: Option<RpcFeeCall>,
}

/// Resolve a known exchange `name` to its static config. Returns `None` for
/// unknown names (an exchange the Rust side doesn't yet model —
/// [`load_active_exchange_specs`] skips these).
///
/// The mapping is the faithful Rust port of the three Python config dicts in
/// `src/degenbot/cli/pool.py` (the `fee_denominator`/`fee_token0`/`rpc_fee_call`
/// values + the event-hash constant references are copied directly from
/// `_V2_CONFIGS`/`_V3_CONFIGS`/`_V4_CONFIGS`). Update this `match` when a new
/// DEX is added to the Python configs — the two MUST stay in sync.
#[must_use]
fn resolve_static_config(name: &str) -> Option<StaticExchangeConfig> {
    // V2 family — the canonical `PairCreated` (no `stable` flag).
    let uniswap_v2 = StaticExchangeConfig {
        family: PoolFamily::V2,
        event_topic: V2_PAIR_CREATED_TOPIC,
        fee_denominator: 1000,
        v2_fee_token: Some(3),
        rpc_fee_call: None,
    };
    // V2 Aerodrome family — a distinct `PoolCreated` topic0 + a `stable`
    // indexed flag + an RPC fee override.
    let aerodrome_v2 = StaticExchangeConfig {
        family: PoolFamily::AerodromeV2,
        event_topic: AERODROME_V2_POOL_CREATED_TOPIC,
        fee_denominator: 10_000,
        v2_fee_token: None, // overridden per-pool by the RPC fee call
        rpc_fee_call: Some(RpcFeeCall::AerodromeV2),
    };
    // V3 family — the canonical `PoolCreated` (shared by Uniswap V3,
    // PancakeSwap V3, SushiSwap V3).
    let v3 = StaticExchangeConfig {
        family: PoolFamily::V3,
        event_topic: V3_POOL_CREATED_TOPIC,
        fee_denominator: 1_000_000,
        v2_fee_token: None, // V3 fee comes from the event's indexed fee topic
        rpc_fee_call: None,
    };
    // V3 Aerodrome family — a distinct `PoolCreated` topic0 (its own
    // signature) but structurally decoded like canonical V3 (token0/token1/
    // fee in topics, tickSpacing/pool in data) + an RPC fee override.
    // Note: `family == PoolFamily::V3` here even though the topic differs —
    // the chunk fn uses `spec.event_topic` (NOT `spec.family.topic0()`) for
    // the RPC filter, so the per-exchange topic is faithfully carried. The
    // decode leaf dispatching on `PoolFamily::V3` must additionally accept
    // `AERODROME_V3_POOL_CREATED_TOPIC` (a task `CKXCOB` concern).
    let aerodrome_v3 = StaticExchangeConfig {
        family: PoolFamily::V3,
        event_topic: AERODROME_V3_POOL_CREATED_TOPIC,
        fee_denominator: 1_000_000,
        v2_fee_token: None,
        rpc_fee_call: Some(RpcFeeCall::AerodromeV3),
    };
    // V4 family — the `Initialize` event (degenbot treats V4 `Initialize` as
    // the pool-creation event).
    let v4 = StaticExchangeConfig {
        family: PoolFamily::V4,
        event_topic: V4_INITIALIZE_TOPIC,
        fee_denominator: 1_000_000,
        v2_fee_token: None, // V4 fee comes from the event's non-indexed data
        rpc_fee_call: None,
    };
    match name {
        // V2 family
        "uniswap_v2" => Some(uniswap_v2),
        "pancakeswap_v2" => Some(StaticExchangeConfig {
            fee_denominator: 10_000,
            v2_fee_token: Some(25),
            ..uniswap_v2
        }),
        "sushiswap_v2" | "swapbased_v2" => Some(StaticExchangeConfig { ..uniswap_v2 }),
        "aerodrome_v2" => Some(aerodrome_v2),
        // V3 family
        "uniswap_v3" => Some(v3),
        "pancakeswap_v3" | "sushiswap_v3" => Some(StaticExchangeConfig { ..v3 }),
        "aerodrome_v3" => Some(aerodrome_v3),
        // V4 family
        "uniswap_v4" => Some(v4),
        _ => None,
    }
}

/// Build an [`ExchangeSpec`] from a DB row + the static config map.
fn build_spec(row: ExchangeRow) -> Option<ExchangeSpec> {
    let cfg = resolve_static_config(&row.name)?;
    Some(ExchangeSpec {
        id: row.id,
        chain_id: row.chain_id,
        name: row.name,
        factory: row.factory,
        family: cfg.family,
        event_topic: cfg.event_topic,
        fee_denominator: cfg.fee_denominator,
        v2_fee_token: cfg.v2_fee_token,
        rpc_fee_call: cfg.rpc_fee_call,
    })
}

/// Read the active exchanges for `chain_id` from the `exchanges` table + build
/// a typed [`ExchangeSpec`] for each, joining the DB row with the static
/// event-config map (`family`/`event_topic`/`fee_denominator`/etc.).
///
/// This is the Rust-owned replacement for the Python trajectory's
/// `active_exchanges` query + `POOL_UPDATER[chain_id, exchange.name]` dispatch
/// lookup — Rust reads the table itself (the DB is the source of truth per
/// ADR-003/ADR-005 + AGENTS.md "Rust owns the state") + resolves the
/// per-family config in-process, so the `PyO3` seam (a later task) needs only
/// `(database_path, chain_id, to_block, chunk_size, rpc_url,
/// progress_callback)` — no per-exchange Python list is threaded through FFI.
///
/// Unknown exchange `name`s (a row whose config hasn't been ported to
/// [`resolve_static_config`]) are silently skipped (forward-compat — the chunk
/// loop updates the exchanges it can resolve).
///
/// # Errors
///
/// Returns [`DbError`](degenbot_db::DbError) on a query failure or a malformed
/// row column.
pub fn load_active_exchange_specs(
    db: &DegenbotDb,
    chain_id: i64,
) -> Result<Vec<ExchangeSpec>, degenbot_db::DbError> {
    let rows = db.fetch_active_exchanges_by_chain(chain_id)?;
    Ok(rows.into_iter().filter_map(build_spec).collect::<Vec<_>>())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use alloy::primitives::address;
    use degenbot_db::SchemaState;

    /// A fresh in-memory **write-capable** DB (mirrors the `degenbot-db`
    /// `read`/`discovery` test helpers — `upsert_exchange`/`set_exchange_active`
    /// need the writable handle).
    fn write_db() -> DegenbotDb {
        let (db, state) = DegenbotDb::open_in_memory_for_writes().unwrap();
        assert!(matches!(state, SchemaState::FreshStandalone { .. }));
        db
    }

    // ── resolve_static_config ────────────────────────────────────────────

    #[test]
    fn resolve_v2_families() {
        let uniswap = resolve_static_config("uniswap_v2").unwrap();
        assert_eq!(uniswap.family, PoolFamily::V2);
        assert_eq!(uniswap.event_topic, V2_PAIR_CREATED_TOPIC);
        assert_eq!(uniswap.fee_denominator, 1000);
        assert_eq!(uniswap.v2_fee_token, Some(3));
        assert_eq!(uniswap.rpc_fee_call, None);

        let pancake = resolve_static_config("pancakeswap_v2").unwrap();
        assert_eq!(pancake.fee_denominator, 10_000);
        assert_eq!(pancake.v2_fee_token, Some(25));

        // sushiswap_v2 + swapbased_v2 share the uniswap_v2 config verbatim.
        assert_eq!(
            resolve_static_config("sushiswap_v2").unwrap(),
            resolve_static_config("uniswap_v2").unwrap(),
        );
        assert_eq!(
            resolve_static_config("swapbased_v2").unwrap(),
            resolve_static_config("uniswap_v2").unwrap(),
        );
    }

    #[test]
    fn resolve_aerodrome_v2_has_rpc_fee_no_constant_fee() {
        let cfg = resolve_static_config("aerodrome_v2").unwrap();
        assert_eq!(cfg.family, PoolFamily::AerodromeV2);
        assert_eq!(cfg.event_topic, AERODROME_V2_POOL_CREATED_TOPIC);
        assert_eq!(cfg.fee_denominator, 10_000);
        assert_eq!(cfg.v2_fee_token, None, "aerodrome_v2 fee is RPC-resolved");
        assert_eq!(cfg.rpc_fee_call, Some(RpcFeeCall::AerodromeV2));
    }

    #[test]
    fn resolve_v3_families_share_topic_but_differ_for_aerodrome() {
        let uniswap = resolve_static_config("uniswap_v3").unwrap();
        assert_eq!(uniswap.family, PoolFamily::V3);
        assert_eq!(uniswap.event_topic, V3_POOL_CREATED_TOPIC);
        assert_eq!(uniswap.fee_denominator, 1_000_000);
        assert_eq!(uniswap.rpc_fee_call, None);

        // PancakeSwap V3 + SushiSwap V3 share the canonical V3 config
        // (same topic0 + same fee denominator).
        assert_eq!(
            resolve_static_config("pancakeswap_v3").unwrap(),
            resolve_static_config("uniswap_v3").unwrap(),
        );
        assert_eq!(
            resolve_static_config("sushiswap_v3").unwrap(),
            resolve_static_config("uniswap_v3").unwrap(),
        );

        // Aerodrome V3 has the SAME family (V3 decode structure) but a
        // DISTINCT topic0 + an RPC fee override.
        let aerodrome = resolve_static_config("aerodrome_v3").unwrap();
        assert_eq!(aerodrome.family, PoolFamily::V3);
        assert_eq!(aerodrome.event_topic, AERODROME_V3_POOL_CREATED_TOPIC);
        assert_ne!(aerodrome.event_topic, uniswap.event_topic);
        assert_eq!(aerodrome.rpc_fee_call, Some(RpcFeeCall::AerodromeV3));
    }

    #[test]
    fn resolve_v4() {
        let cfg = resolve_static_config("uniswap_v4").unwrap();
        assert_eq!(cfg.family, PoolFamily::V4);
        assert_eq!(cfg.event_topic, V4_INITIALIZE_TOPIC);
        assert_eq!(cfg.fee_denominator, 1_000_000);
        assert_eq!(cfg.v2_fee_token, None);
        assert_eq!(cfg.rpc_fee_call, None);
    }

    #[test]
    fn resolve_unknown_name_returns_none() {
        assert!(resolve_static_config("unknown_dex").is_none());
        assert!(resolve_static_config("uniswap_v5").is_none());
    }

    // ── load_active_exchange_specs (the DB→spec round trip) ──────────────

    #[test]
    fn load_active_exchange_specs_round_trips_uniswap_v2_and_v3() {
        let db = write_db();
        let v2_factory = address!("0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f");
        let v3_factory = address!("0x1F98431c8aD98523631AE4a59f267346ea31F984");

        let v2 = db
            .upsert_exchange(1, "uniswap_v2", v2_factory, None)
            .unwrap();
        let v3 = db
            .upsert_exchange(1, "uniswap_v3", v3_factory, None)
            .unwrap();
        // A second-chain exchange — must NOT appear in the chain=1 specs.
        let _other = db
            .upsert_exchange(8453, "uniswap_v3", v3_factory, None)
            .unwrap();

        // Initially empty (neither chain-1 exchange is active).
        assert!(load_active_exchange_specs(&db, 1).unwrap().is_empty());

        db.set_exchange_active(v2.id, true).unwrap();
        db.set_exchange_active(v3.id, true).unwrap();

        let specs = load_active_exchange_specs(&db, 1).unwrap();
        assert_eq!(specs.len(), 2);

        // Ordered by id (v2 inserted first).
        let (v2_spec, v3_spec) = (&specs[0], &specs[1]);

        // V2 spec: family/factory/fee resolution.
        assert_eq!(v2_spec.id, v2.id);
        assert_eq!(v2_spec.chain_id, 1);
        assert_eq!(v2_spec.name, "uniswap_v2");
        assert_eq!(v2_spec.factory, v2_factory);
        assert_eq!(v2_spec.family, PoolFamily::V2);
        assert_eq!(v2_spec.event_topic, V2_PAIR_CREATED_TOPIC);
        assert_eq!(v2_spec.fee_denominator, 1000);
        assert_eq!(v2_spec.v2_fee_token, Some(3));
        assert_eq!(v2_spec.rpc_fee_call, None);

        // V3 spec: family discrimination (V3, not V2) + the V3 topic0.
        assert_eq!(v3_spec.id, v3.id);
        assert_eq!(v3_spec.name, "uniswap_v3");
        assert_eq!(v3_spec.factory, v3_factory);
        assert_eq!(v3_spec.family, PoolFamily::V3);
        assert_ne!(v3_spec.family, v2_spec.family, "family discriminates V2/V3");
        assert_eq!(v3_spec.event_topic, V3_POOL_CREATED_TOPIC);
        assert_eq!(v3_spec.fee_denominator, 1_000_000);
        assert_eq!(v3_spec.v2_fee_token, None, "V3 has no constant V2 fee");
        assert_eq!(v3_spec.rpc_fee_call, None);
    }

    #[test]
    fn load_active_exchange_specs_skips_unknown_names_silently() {
        let db = write_db();
        let factory = address!("0x0000000000000000000000000000000000000001");
        // An exchange whose name isn't in the static config map — forward-compat
        // skip (no spec produced, but the row is in the table + active).
        let unknown = db.upsert_exchange(1, "future_dex", factory, None).unwrap();
        db.set_exchange_active(unknown.id, true).unwrap();
        // Plus a known one alongside it.
        let v3 = db.upsert_exchange(1, "uniswap_v3", factory, None).unwrap();
        db.set_exchange_active(v3.id, true).unwrap();

        let specs = load_active_exchange_specs(&db, 1).unwrap();
        assert_eq!(specs.len(), 1, "unknown exchange skipped, known one loaded");
        assert_eq!(specs[0].name, "uniswap_v3");
    }

    #[test]
    fn load_active_exchange_specs_resolves_aerodrome_v2_rpc_fee() {
        let db = write_db();
        let factory = address!("0xae2Fc483527B8EF99EB5D9B44875F005ba1FaE13");
        let row = db
            .upsert_exchange(1, "aerodrome_v2", factory, None)
            .unwrap();
        db.set_exchange_active(row.id, true).unwrap();

        let specs = load_active_exchange_specs(&db, 1).unwrap();
        assert_eq!(specs.len(), 1);
        let spec = &specs[0];
        assert_eq!(spec.family, PoolFamily::AerodromeV2);
        assert_eq!(spec.event_topic, AERODROME_V2_POOL_CREATED_TOPIC);
        assert_eq!(spec.fee_denominator, 10_000);
        assert_eq!(spec.v2_fee_token, None, "aerodrome_v2 fee is RPC-resolved");
        assert_eq!(spec.rpc_fee_call, Some(RpcFeeCall::AerodromeV2));
    }

    #[test]
    fn load_active_exchange_specs_resolves_aerodrome_v3_distinct_topic() {
        let db = write_db();
        let factory = address!("0xae2Fc483527B8EF99EB5D9B44875F005ba1FaE13");
        let row = db
            .upsert_exchange(1, "aerodrome_v3", factory, None)
            .unwrap();
        db.set_exchange_active(row.id, true).unwrap();

        let specs = load_active_exchange_specs(&db, 1).unwrap();
        assert_eq!(specs.len(), 1);
        let spec = &specs[0];
        // Family is V3 (same decode structure as canonical V3)...
        assert_eq!(spec.family, PoolFamily::V3);
        // ...but the topic0 is the Aerodrome V3 sig, NOT the canonical V3 sig.
        assert_eq!(spec.event_topic, AERODROME_V3_POOL_CREATED_TOPIC);
        assert_ne!(spec.event_topic, V3_POOL_CREATED_TOPIC);
        assert_eq!(spec.rpc_fee_call, Some(RpcFeeCall::AerodromeV3));
    }
}
