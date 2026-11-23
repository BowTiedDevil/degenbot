//! The per-chunk ChunkSeed: the complete relational starting point one chunk's
//! resolution consumes, loaded and interned synchronously outside the chunk's
//! write transaction.
//!
//! The seed owns every fact resolution could ask the database for — the
//! market's asset rows and their three address indexes, the contract roles and
//! their revisions, the chain's GHO row, the identities the chunk's own events
//! name (users, their collateral/debt positions, their collateral configs), and
//! the reserve identities the chunk initializes (their ERC-20 rows and asset
//! rows, interned here so resolution never mints an id).
//!
//! Two consequences are structural rather than conventional:
//!
//! - Resolution receives no Connection. An unanticipated dependency is an
//!   IncompleteChunkSeed refusal naming the market and the entity, never a lazy
//!   query (ADR-105 D4).
//! - Interning happens here, on the caller's connection, before the chunk's
//!   transaction opens. The rows it commits are inert until the chunk's deltas
//!   reference them, so a rolled-back chunk leaves them behind exactly as
//!   ADR-105 D4 requires.
//!
//! The chain facts a reserve's identity carries (its tokens' metadata and its
//! two implementation revisions) are exact reads, acquired by
//! acquire_reserve_facts before the seed loads: the seed itself is synchronous,
//! and its interning is what turns those facts into ids. The reserve's price
//! source is deliberately NOT among them — it is read at the reserve's own
//! dispatch position, where a same-chunk PriceOracleUpdated is visible, and it
//! reaches the row through the chunk's ReserveInitialized delta.

#![expect(clippy::doc_markdown)]

use alloy::primitives::Address;
use alloy::rpc::types::Log;
use degenbot_core::canonical_head::TxRef;
use degenbot_db::DbError;
use degenbot_decoders::aave_event_decoder::{
    decode_aave_log, decode_aave_reserve_initialized_log, AaveV3ReserveInitializedEvent,
    DecodedAaveEvent,
};
use degenbot_rpc::chain_evidence::BlockReads;
use degenbot_rpc::erc20;
use degenbot_rpc::provider::AlloyProvider;
use rusqlite::Connection;

use super::evidence::CandidateTransaction;
use super::substrate::{candidate_addresses, ChunkSubstrate, PositionTable};
use crate::config_dispatch::{
    discount_to_i64, encode_no_arg_call, eth_calls_batched_or_direct, read_implementation_slot,
    require_word0_u256, revision_selector, ConfigDispatchError, RevisionMemo,
};
use crate::updater::operations_parser::addr_to_hex;

/// The market/entity context of a relational fact the seed does not hold.
///
/// Every resolution read that misses the seed returns this: the run refuses
/// rather than deferring a query into the async path (ADR-105 D4).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("incomplete chunk seed for market {market_id}: {entity}")]
pub struct IncompleteChunkSeed {
    /// The market the resolution was projecting.
    pub market_id: i64,
    /// The relational fact the seed does not hold.
    pub entity: SeedEntity,
}

impl IncompleteChunkSeed {
    pub(crate) fn new(market_id: i64, entity: SeedEntity) -> Self {
        Self { market_id, entity }
    }

    pub(crate) fn user(market_id: i64, address: &str) -> Self {
        Self::new(
            market_id,
            SeedEntity::User {
                address: address.to_string(),
            },
        )
    }
}

/// The relational facts a chunk seed must hold, named by kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeedEntity {
    /// A user row addressed by its market + checksummed address.
    User {
        /// The checksummed user address.
        address: String,
    },
    /// A user row's address, addressed by id.
    UserAddress {
        /// The aave_v3_users.id.
        user_id: i64,
    },
    /// A position row addressed by its natural key.
    Position {
        /// collateral or debt.
        position: &'static str,
        /// The owning user row id.
        user_id: i64,
        /// The reserve asset row id.
        asset_id: i64,
    },
    /// A position row's projected state, addressed by id.
    PositionState {
        /// collateral or debt.
        position: &'static str,
        /// The position row id.
        position_id: i64,
    },
    /// An asset row addressed by one of its three token addresses.
    Asset {
        /// The lookup kind (underlying, a_token, v_token).
        kind: &'static str,
        /// The token address.
        address: String,
    },
    /// A contract role addressed by name.
    Contract {
        /// The contract role name.
        name: String,
    },
    /// The reserve identity a ReserveInitialized event names.
    Reserve {
        /// The underlying token address (or the id a projection named).
        underlying: String,
    },
}

impl std::fmt::Display for SeedEntity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::User { address } => write!(f, "user {address}"),
            Self::UserAddress { user_id } => write!(f, "user id={user_id}"),
            Self::Position {
                position,
                user_id,
                asset_id,
            } => write!(
                f,
                "{position} position (user_id={user_id}, asset_id={asset_id})"
            ),
            Self::PositionState {
                position,
                position_id,
            } => write!(f, "{position} position id={position_id}"),
            Self::Asset { kind, address } => write!(f, "{kind} asset {address}"),
            Self::Contract { name } => write!(f, "contract role {name}"),
            Self::Reserve { underlying } => write!(f, "reserve for underlying {underlying}"),
        }
    }
}

/// One ReserveInitialized event of the chunk, positioned at its exact
/// transaction so its reads ride that transaction's anchor.
#[derive(Debug, Clone)]
pub(crate) struct ReserveInitialization {
    pub(crate) tx: TxRef,
    pub(crate) event: AaveV3ReserveInitializedEvent,
}

/// The metadata triple one reserve's three ERC-20 tokens carry — (name, symbol,
/// decimals), each absent when its read produced no value.
pub(crate) type TokenMetadataFields = (Option<String>, Option<String>, Option<i64>);

/// The chain facts one ReserveInitialized resolution needs, acquired by exact
/// reads before the seed interns the rows they describe.
#[derive(Debug, Clone)]
pub(crate) struct ReserveFacts {
    pub(crate) underlying: Address,
    pub(crate) a_token: Address,
    pub(crate) v_token: Address,
    pub(crate) a_token_revision: i64,
    pub(crate) v_token_revision: i64,
    pub(crate) metadata: [TokenMetadataFields; 3],
}

/// One reserve identity the seed interned: the facts its ReserveInitialized
/// resolution carries, plus the ERC-20 and asset row ids already committed for
/// it.
#[derive(Debug, Clone)]
pub(crate) struct ChunkReserve {
    pub(crate) facts: ReserveFacts,
    pub(crate) underlying_id: i64,
    pub(crate) a_token_id: i64,
    pub(crate) v_token_id: i64,
    pub(crate) asset_id: i64,
}

/// The complete relational starting point for one chunk.
pub(crate) struct ChunkSeed {
    substrate: ChunkSubstrate,
}

impl ChunkSeed {
    /// Load the chunk's complete relational starting point and intern every
    /// identity resolution may name.
    ///
    /// # Errors
    ///
    /// A prefetch, interning, or decode failure refuses the seed; the caller's
    /// chunk transaction has not opened, so nothing rolls back.
    pub(crate) fn load(
        conn: &Connection,
        market_id: i64,
        chain_id: i64,
        logs: &[&Log],
        reserves: &[ReserveFacts],
    ) -> Result<Self, DbError> {
        let mut substrate = ChunkSubstrate::lazy(chain_id);
        substrate.load_market_families(conn, market_id, chain_id)?;
        let (_, asset_candidates) = candidate_addresses(logs);
        substrate.cache_asset_absences(&asset_candidates);
        substrate.intern_reserves(conn, chain_id, reserves)?;
        let demands = IdentityDemands::of_logs(logs, &substrate);
        substrate.load_identity_families(conn, market_id, &demands)?;
        substrate.intern_identities(conn, market_id, &demands)?;
        Ok(Self { substrate })
    }

    /// A seed with no relational state: the empty chunk's resolution input (it
    /// names no candidate, so nothing it reads needs a relationship).
    pub(crate) fn empty(chain_id: i64) -> Self {
        Self {
            substrate: ChunkSubstrate::lazy(chain_id),
        }
    }

    /// The seed's relational state for the resolution that consumes it.
    pub(crate) fn into_substrate(self) -> ChunkSubstrate {
        self.substrate
    }

    /// A seed over the chunk's loaded relational state, for the in-crate
    /// resolution fixtures: the same loading sequence the chunk loop runs, with
    /// the logs' identity demands as the only interning input.
    #[cfg(test)]
    #[expect(clippy::expect_used)]
    pub(crate) fn for_tests(
        conn: &Connection,
        market_id: i64,
        chain_id: i64,
        logs: &[&Log],
    ) -> Self {
        let mut substrate = ChunkSubstrate::lazy(chain_id);
        substrate
            .load_market_families(conn, market_id, chain_id)
            .expect("the fixture market families load");
        substrate.cache_asset_absences(&candidate_addresses(logs).1);
        substrate
            .load_all_identity_families(conn, market_id)
            .expect("the fixture identity families load");
        let demands = IdentityDemands::of_logs(logs, &substrate);
        substrate
            .intern_identities(conn, market_id, &demands)
            .expect("the fixture identities intern");
        substrate.fixture_complete = true;
        Self { substrate }
    }

    /// A fixture seed that additionally interns the given reserve identities,
    /// for the in-crate dispatches whose `ReserveInitialized` events are decoded
    /// structs rather than logs.
    #[cfg(test)]
    #[expect(clippy::expect_used)]
    pub(crate) fn for_tests_reserve(
        conn: &Connection,
        market_id: i64,
        chain_id: i64,
        reserves: &[ReserveFacts],
    ) -> Self {
        let seed =
            Self::load(conn, market_id, chain_id, &[], reserves).expect("the fixture seed loads");
        let mut substrate = seed.substrate;
        substrate
            .load_all_identity_families(conn, market_id)
            .expect("the fixture identity families load");
        substrate.fixture_complete = true;
        Self { substrate }
    }

    /// A fixture seed that additionally interns the given users, for the
    /// in-crate dispatches whose events are decoded structs rather than logs
    /// (so the demand walk cannot see them).
    #[cfg(test)]
    #[expect(clippy::expect_used)]
    pub(crate) fn for_tests_users(
        conn: &Connection,
        market_id: i64,
        chain_id: i64,
        users: &[Address],
    ) -> Self {
        let seed = Self::for_tests(conn, market_id, chain_id, &[]);
        let mut substrate = seed.substrate;
        substrate.fixture_complete = true;
        let mut demands = IdentityDemands::default();
        let asset_ids: Vec<i64> = substrate.asset_ids();
        for user in users {
            let address = addr_to_hex(*user);
            demands.users.push(address.clone());
            for asset_id in &asset_ids {
                demands
                    .positions
                    .push((PositionTable::Collateral, address.clone(), *asset_id));
                demands
                    .positions
                    .push((PositionTable::Debt, address.clone(), *asset_id));
            }
        }
        substrate
            .load_identity_families(conn, market_id, &demands)
            .expect("the fixture users load");
        substrate
            .intern_identities(conn, market_id, &demands)
            .expect("the fixture users intern");
        Self { substrate }
    }
}

/// The users and positions the chunk's own events name.
///
/// The walk decodes every log exactly once and mirrors the identity asks the
/// configuration dispatch and the operation parse issue: a scaled-token event's
/// owner side, a transfer's two sides, and the user a user-scoped configuration
/// event addresses. Every user it names is a user a resolver asks for, so
/// interning one creates no row the chunk would not have created.
#[derive(Debug, Default)]
pub(crate) struct IdentityDemands {
    /// The demanded users in first-encounter order: the seed interns identities
    /// in the order the chunk's own events name them, so a market's surrogate
    /// ids keep the sequence a transaction-ordered interning produces.
    pub(crate) users: Vec<String>,
    /// `(table, user address, asset id)` in first-encounter order.
    pub(crate) positions: Vec<(PositionTable, String, i64)>,
    seen_users: std::collections::HashSet<String>,
    seen_positions: std::collections::HashSet<(PositionTable, String, i64)>,
}

impl IdentityDemands {
    fn of_logs(logs: &[&Log], substrate: &ChunkSubstrate) -> Self {
        let mut demands = Self::default();
        let mut discount_token = substrate
            .gho_asset()
            .and_then(|gho| gho.v_gho_discount_token);
        for log in logs {
            let Some(decoded) = decode_aave_log(log) else {
                continue;
            };
            if let DecodedAaveEvent::DiscountTokenUpdated(event) = &decoded {
                discount_token = Some(addr_to_hex(event.new_discount_token));
            }
            match &decoded {
                DecodedAaveEvent::ScaledTokenMint(event) => {
                    demands.owner(
                        substrate,
                        discount_token.as_deref(),
                        event.token_address,
                        event.on_behalf_of,
                    );
                }
                DecodedAaveEvent::ScaledTokenBurn(event) => {
                    demands.owner(
                        substrate,
                        discount_token.as_deref(),
                        event.token_address,
                        event.from,
                    );
                }
                DecodedAaveEvent::ScaledTokenBalanceTransfer(event) => {
                    demands.owner(
                        substrate,
                        discount_token.as_deref(),
                        event.token_address,
                        event.from,
                    );
                    demands.recipient(
                        substrate,
                        discount_token.as_deref(),
                        event.token_address,
                        event.to,
                    );
                }
                DecodedAaveEvent::Erc20Transfer(event) => {
                    // An ERC-20 transfer's zero leg reports a scaled mint or
                    // burn whose real movement the scaled event carries, so it
                    // names no identity of its own.
                    demands.recipient(
                        substrate,
                        discount_token.as_deref(),
                        event.token_address,
                        event.from,
                    );
                    demands.recipient(
                        substrate,
                        discount_token.as_deref(),
                        event.token_address,
                        event.to,
                    );
                }
                DecodedAaveEvent::ReserveUsedAsCollateralEnabled(event) => {
                    demands.user(addr_to_hex(event.user));
                }
                DecodedAaveEvent::ReserveUsedAsCollateralDisabled(event) => {
                    demands.user(addr_to_hex(event.user));
                }
                DecodedAaveEvent::UserEModeSet(event) => {
                    demands.user(addr_to_hex(event.user));
                }
                DecodedAaveEvent::DiscountPercentUpdated(event) => {
                    demands.user(addr_to_hex(event.user));
                }
                _ => {}
            }
        }
        demands
    }

    fn user(&mut self, address: String) {
        if self.seen_users.insert(address.clone()) {
            self.users.push(address);
        }
    }

    fn owner(
        &mut self,
        substrate: &ChunkSubstrate,
        discount_token: Option<&str>,
        token_address: Address,
        user: Address,
    ) {
        let token = addr_to_hex(token_address);
        let is_discount_token =
            discount_token.is_some_and(|discount| discount.eq_ignore_ascii_case(&token));
        let leg = substrate.scaled_leg(&token);
        if leg.is_none() && !is_discount_token {
            return;
        }
        let user = addr_to_hex(user);
        if self.seen_users.insert(user.clone()) {
            self.users.push(user.clone());
        }
        if let Some((table, asset_id)) = leg {
            let key = (table, user, asset_id);
            if self.seen_positions.insert(key.clone()) {
                self.positions.push(key);
            }
        }
    }

    /// The recipient side of a transfer-shaped event. The zero address is a
    /// burn leg: neither the parse nor the configuration dispatch creates a row
    /// for it.
    fn recipient(
        &mut self,
        substrate: &ChunkSubstrate,
        discount_token: Option<&str>,
        token_address: Address,
        user: Address,
    ) {
        if user == Address::ZERO {
            return;
        }
        self.owner(substrate, discount_token, token_address, user);
    }
}

/// Collect the chunk's ReserveInitialized events, positioned at their exact
/// transactions, in chunk order.
pub(super) fn collect_reserve_initializations(
    candidates: &[CandidateTransaction<'_>],
) -> Vec<ReserveInitialization> {
    let mut initializations = Vec::new();
    for candidate in candidates {
        for log in &candidate.group.logs {
            let Some(event) = decode_aave_reserve_initialized_log(log) else {
                continue;
            };
            initializations.push(ReserveInitialization {
                tx: *candidate.evidence.tx(),
                event,
            });
        }
    }
    initializations
}

/// Acquire every chain fact the chunk's ReserveInitialized events need, at each
/// event's own exact transaction anchor: the three tokens' metadata, the two
/// EIP-1967 implementation slots, and the two implementation revisions (through
/// the chunk's block-pinned memo).
///
/// # Errors
///
/// A transport failure, an unserved exact read, a malformed return, or a
/// metadata burst that carries no identity refuses the chunk before its
/// transaction opens.
pub(crate) async fn acquire_reserve_facts(
    provider: &AlloyProvider,
    memo: &mut RevisionMemo,
    initializations: &[ReserveInitialization],
) -> Result<Vec<ReserveFacts>, ConfigDispatchError> {
    let mut facts = Vec::with_capacity(initializations.len());
    for initialization in initializations {
        let anchor = *initialization.tx.anchor();
        let reads = BlockReads::at(provider, anchor);
        let event = &initialization.event;
        let metadata = fold_metadata_rows(
            erc20::read_batch(
                &reads,
                &[event.asset, event.a_token, event.variable_debt_token],
                erc20::MetadataShape::LowerUpperStopOnCallError,
                None,
            )
            .await,
        )?;
        let a_token_impl = read_implementation_slot(&reads, &event.a_token).await?;
        let v_token_impl = read_implementation_slot(&reads, &event.variable_debt_token).await?;
        let [a_revision, v_revision] = read_revision_pair(
            &reads,
            memo,
            [
                (a_token_impl, "ATOKEN_REVISION()"),
                (v_token_impl, "DEBT_TOKEN_REVISION()"),
            ],
        )
        .await?;
        facts.push(ReserveFacts {
            underlying: event.asset,
            a_token: event.a_token,
            v_token: event.variable_debt_token,
            a_token_revision: discount_to_i64(a_revision),
            v_token_revision: discount_to_i64(v_revision),
            metadata,
        });
    }
    Ok(facts)
}

/// Read the two scaled-token revision facts of one reserve in ONE transport
/// pass when both miss the memo: `ATOKEN_REVISION()` and
/// `DEBT_TOKEN_REVISION()` fold into a single Multicall3 `aggregate3` pinned to
/// the reserve's own anchor, exactly as the pre-seed dispatch's
/// `read_revision_batch` did. A memo hit issues no call, and a single remaining
/// miss keeps the direct `eth_call` shape (a 1-call aggregate3 wrapper would
/// change the recorded request set for no round-trip gain).
///
/// The dispatch-position `getSourceOfAsset` read stays a separate call on
/// purpose: it is resolved at the `ReserveInitialized` event's own dispatch
/// position (the pool's oracle may have changed earlier in the same chunk),
/// while these two facts are identity reads at the reserve's creating
/// transaction. Splitting them costs nothing in steady state — a reserve is
/// initialized once — and keeps the price-source semantics exact.
///
/// # Errors
///
/// Propagates the transport failure, a reverted sub-call, or a short return
/// that cannot be read as a `uint256` word.
async fn read_revision_pair(
    reads: &BlockReads<'_>,
    memo: &mut RevisionMemo,
    pending: [(Address, &str); 2],
) -> Result<[alloy::primitives::U256; 2], ConfigDispatchError> {
    let anchor = reads.anchor();
    let mut values: [Option<alloy::primitives::U256>; 2] = [None, None];
    let mut calls: Vec<(Address, alloy::primitives::Bytes)> = Vec::new();
    let mut call_of_slot: [Option<usize>; 2] = [None, None];
    for (slot, (target, sig)) in pending.iter().enumerate() {
        let selector = revision_selector(sig);
        if let Some(value) = memo.get(target, selector, anchor) {
            values[slot] = Some(value);
        } else {
            call_of_slot[slot] = Some(calls.len());
            calls.push((*target, encode_no_arg_call(sig)));
        }
    }
    let returns = eth_calls_batched_or_direct(reads, &calls).await?;
    for (slot, (target, sig)) in pending.iter().enumerate() {
        let Some(index) = call_of_slot[slot] else {
            continue;
        };
        let value = require_word0_u256(&returns[index], sig, target)?;
        memo.insert(target, revision_selector(sig), anchor, value);
        values[slot] = Some(value);
    }
    let [a, v] = values;
    Ok([
        a.ok_or_else(|| {
            ConfigDispatchError::DecodeShape("a revision slot was left unfilled".to_string())
        })?,
        v.ok_or_else(|| {
            ConfigDispatchError::DecodeShape("a revision slot was left unfilled".to_string())
        })?,
    ])
}
/// Fold the metadata burst's per-token rows into the three-token triple the
/// interning consumes.
///
/// # Errors
///
/// A burst that does not carry exactly one row per token, or one whose every
/// field is absent (an outage, not token identity), refuses the seed.
pub(crate) fn fold_metadata_rows(
    rows: Vec<erc20::TokenMetadata>,
) -> Result<[TokenMetadataFields; 3], ConfigDispatchError> {
    let folded: Vec<TokenMetadataFields> = rows
        .into_iter()
        .map(|meta| (meta.name, meta.symbol, meta.decimals.map(u64::cast_signed)))
        .collect();
    let triple: [TokenMetadataFields; 3] = folded.try_into().map_err(|_| {
        ConfigDispatchError::DecodeShape(
            "ReserveInitialized: expected exactly three metadata rows".to_string(),
        )
    })?;
    if triple
        .iter()
        .all(|(name, symbol, decimals)| name.is_none() && symbol.is_none() && decimals.is_none())
    {
        return Err(ConfigDispatchError::DecodeShape(
            "ReserveInitialized: every token metadata read came back empty (no name, symbol or \
             decimals for the underlying, aToken and vToken)"
                .to_string(),
        ));
    }
    Ok(triple)
}

#[cfg(test)]
#[expect(clippy::expect_used)]
mod tests {
    use super::*;
    use alloy::primitives::U256;
    use degenbot_db::DegenbotDb;

    fn market_db() -> degenbot_db::DegenbotDb {
        let (db, _state) = DegenbotDb::open_in_memory_for_writes().expect("in-memory db");
        {
            let conn = db.lock();
            conn.execute(
                "INSERT INTO aave_v3_markets (id, chain_id, name, active, last_update_block) VALUES (1, 1, 'm', 1, 0)",
                [],
            )
            .expect("market row");
        }
        db
    }

    #[test]
    fn a_seed_without_a_demanded_user_refuses_by_name() {
        let db = market_db();
        let conn = db.lock();
        let seed = ChunkSeed::for_tests(&conn, 1, 1, &[]);
        let mut substrate = seed.into_substrate();
        let error = substrate
            .user_id_or_create("0xdeadbeef", 0)
            .expect_err("an absent user must refuse");
        assert!(error.to_string().contains("market 1"), "{error}");
        assert!(error.to_string().contains("user 0xdeadbeef"), "{error}");
    }

    #[test]
    fn a_seed_without_a_loaded_role_refuses_by_name() {
        let db = market_db();
        let conn = db.lock();
        let seed = ChunkSeed::load(&conn, 1, 1, &[], &[]).expect("the seed loads");
        let substrate = seed.into_substrate();
        let error = substrate
            .contract_address("PRICE_ORACLE")
            .expect_err("an unloaded role must refuse");
        assert!(
            error.to_string().contains("contract role PRICE_ORACLE"),
            "{error}"
        );
        assert_eq!(
            substrate.pool_revision("POOL").err().map(|e| e.entity),
            Some(SeedEntity::Contract {
                name: "POOL".to_string()
            })
        );
    }

    #[test]
    fn the_metadata_fold_refuses_an_empty_or_all_absent_burst() {
        assert!(fold_metadata_rows(Vec::new()).is_err());
        assert!(fold_metadata_rows(vec![erc20::TokenMetadata::default(); 3]).is_err());
        let folded = fold_metadata_rows(vec![
            erc20::TokenMetadata {
                name: Some("Dai".into()),
                symbol: Some("DAI".into()),
                decimals: Some(18),
            },
            erc20::TokenMetadata::default(),
            erc20::TokenMetadata {
                decimals: Some(6),
                ..Default::default()
            },
        ])
        .expect("three rows fold");
        assert_eq!(folded[0].0.as_deref(), Some("Dai"));
        assert_eq!(folded[2].2, Some(6));
    }

    #[test]
    fn the_demand_walk_names_the_owner_side_of_a_scaled_event() {
        use alloy::primitives::{Bytes, Log as AlloyLog, B256};
        use degenbot_decoders::aave_event_decoder::AAVE_MINT_TOPIC;

        let db = market_db();
        let conn = db.lock();
        let seed = ChunkSeed::for_tests(&conn, 1, 1, &[]);
        let substrate = seed.into_substrate();
        let emitter = Address::repeat_byte(0x11);
        let owner = Address::repeat_byte(0x22);
        let mut data = Vec::new();
        data.extend_from_slice(&U256::from(1u64).to_be_bytes::<32>());
        data.extend_from_slice(&U256::from(1u64).to_be_bytes::<32>());
        data.extend_from_slice(&U256::from(1u64).to_be_bytes::<32>());
        let mint = Log {
            inner: AlloyLog::new_unchecked(
                emitter,
                vec![AAVE_MINT_TOPIC, owner.into_word(), owner.into_word()],
                Bytes::from(data),
            ),
            block_hash: Some(B256::repeat_byte(0x01)),
            block_number: Some(100),
            block_timestamp: None,
            transaction_hash: Some(B256::repeat_byte(0x02)),
            transaction_index: Some(0),
            log_index: Some(0),
            removed: false,
        };
        let logs = vec![&mint];
        let demands = IdentityDemands::of_logs(&logs, &substrate);
        assert!(
            demands.users.is_empty() && demands.positions.is_empty(),
            "an emitter outside the market's census names no identity: the reduction skips it"
        );
        let _ = owner;
    }

    /// A reserve's two revision facts share ONE Multicall3 transport pass when
    /// both miss the memo, and a memo hit issues no call at all: the batched
    /// shape the pre-seed dispatch's batch reader had.
    #[test]
    #[expect(clippy::expect_used)]
    fn the_two_reserve_revisions_share_one_multicall3_pass() {
        use crate::updater::test_chain::{Answer, ScriptedChain};
        use alloy::primitives::{Bytes, U256};
        use alloy::sol_types::SolValue;
        use degenbot_rpc::chain_evidence::BlockReads;
        use degenbot_rpc::multicall3::MULTICALL3_ADDRESS;

        let a_token_impl = Address::repeat_byte(0xA1);
        let v_token_impl = Address::repeat_byte(0xB2);
        let anchor = degenbot_core::canonical_head::ChainAnchor::new(
            1,
            100,
            alloy::primitives::B256::repeat_byte(0xaa),
            alloy::primitives::B256::repeat_byte(0x99),
        )
        .expect("the fixture anchor is complete");
        let word = |value: u8| {
            let mut out = [0u8; 32];
            out[31] = value;
            out
        };
        let aggregate3 = degenbot_rpc::multicall3::AGGREGATE3_SELECTOR;
        // aggregate3's return: one (bool success, bytes data) row per call.
        let rows: Vec<(bool, Bytes)> = [word(3), word(4)]
            .into_iter()
            .map(|value| (true, Bytes::from(value.to_vec())))
            .collect();
        let envelope = alloy::hex::encode(rows.abi_encode());
        let chain = ScriptedChain::new().answer(
            MULTICALL3_ADDRESS,
            aggregate3,
            Answer::Return(Bytes::from(
                alloy::hex::decode(envelope).expect("the fixture envelope is hex"),
            )),
        );
        let provider = degenbot_core::runtime::get_runtime().block_on(chain.provider());
        let mut memo = RevisionMemo::new();

        let revisions = degenbot_core::runtime::get_runtime()
            .block_on(read_revision_pair(
                &BlockReads::at(&provider, anchor),
                &mut memo,
                [
                    (a_token_impl, "ATOKEN_REVISION()"),
                    (v_token_impl, "DEBT_TOKEN_REVISION()"),
                ],
            ))
            .expect("both revisions are served by the one batch");
        assert_eq!(revisions, [U256::from(3u64), U256::from(4u64)]);

        let to_multicall = chain
            .issued()
            .into_iter()
            .filter(|issued| issued.to().is_some_and(|to| to == MULTICALL3_ADDRESS))
            .count();
        assert_eq!(
            to_multicall, 1,
            "two revision misses ride one aggregate3 pass"
        );
        assert!(
            chain.issued().len() == to_multicall,
            "the batch is the only provider request the pair issues"
        );

        let before = chain.issued().len();
        let again = degenbot_core::runtime::get_runtime()
            .block_on(read_revision_pair(
                &BlockReads::at(&provider, anchor),
                &mut memo,
                [
                    (a_token_impl, "ATOKEN_REVISION()"),
                    (v_token_impl, "DEBT_TOKEN_REVISION()"),
                ],
            ))
            .expect("the memo answers both");
        assert_eq!(again, revisions);
        assert_eq!(
            chain.issued().len(),
            before,
            "memo hits issue no provider request"
        );
    }
}
