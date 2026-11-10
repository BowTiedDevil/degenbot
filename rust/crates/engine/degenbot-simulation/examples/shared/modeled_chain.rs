//! The modeled chain behind the offline benchmarks: one deterministic block
//! chain, one modeled write history over the corpus's identities, and the stub
//! JSON-RPC transport that serves both the values and the raw access lists.
//!
//! # Why a modeled write history
//!
//! The recorded corpus is reads only, so the benchmark models what the blocks
//! wrote: each modeled block writes each tracked identity when its per-block
//! write bit selects it, and an identity's value changes only at a block that
//! wrote it. The stub's `eth_getStorageAt` and the modeled list's final writes
//! derive from the SAME chain, so a value is identical whatever policy served
//! it — the cross-leg digest gate then proves the three cache policies replay
//! the same reads, and a carry that leaks a stale parent value cannot pass.
//! The write coverage itself is modeled (the corpus records no writes); real
//! coverage is the live probe's and the broader-corpus experiment's question.
//!
//! # The modeled block chain
//!
//! Block hashes are a deterministic function of the block number, linked
//! parent-to-child like a real chain: `advance_bal` sees an unbroken parent
//! chain through every intermediate block, and the list request is keyed by
//! exact hash.
//!
//! # Real coverage, and the shared value chain
//!
//! A coverage comparison ([`Coverage`]) swaps WHICH write identities each
//! block's list carries — the modeled write bit's, or the captured real
//! fixture's exact identities at their real hashes — while candidate order,
//! schedule, prices, and READ VALUES stay put. Values staying put is the hard
//! part: an identity's value may change only where every served list carries
//! the change, or one policy's snapshot serves a word another policy's fetch
//! would not, and the cross-leg digest gate turns red. With both coverages in
//! play the only drift set that satisfies both lists is the writes BOTH
//! coverages make ([`shared_value`]) — the maximal such set, so the digest
//! gate keeps teeth against a dropped write wherever the shared chain drifts.
//! (Outside a coverage comparison, one coverage is the whole world and values
//! drift at its writes: [`modeled_value`].)

use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};

use alloy::primitives::{keccak256, Address, Bytes, B256, U256};
use alloy::providers::RootProvider;
use alloy::rpc::client::ClientBuilder;
use alloy::rpc::json_rpc::{
    ErrorPayload, RequestPacket, Response, ResponsePacket, ResponsePayload, SerializedRequest,
};
use alloy::transports::{TransportError, TransportFut};
use alloy_eip7928::bal::Bal;
use alloy_eip7928::{AccountChanges, BlockAccessIndex, SlotChanges, StorageChange};
use serde_json::value::to_raw_value;
use serde_json::{json, Value};

use degenbot_rpc::provider::{AccountRead, AlloyProvider};

use crate::real_coverage::RealCoverage;

/// One tracked read identity.
pub type Key = (Address, U256);

/// The byte-tagged preimage every modeled derivation hashes.
fn preimage(tag: &str, number: u64, address: Address, slot: U256) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(tag.len() + 60);
    bytes.extend_from_slice(tag.as_bytes());
    bytes.extend_from_slice(&number.to_be_bytes());
    bytes.extend_from_slice(address.as_slice());
    bytes.extend_from_slice(&slot.to_be_bytes::<32>());
    bytes
}

/// The modeled chain's block hash at `number` — deterministic, and linked:
/// every block names its own identity and the caller passes the parent's hash
/// as the child's parent hash.
#[must_use]
pub fn block_hash(number: u64) -> B256 {
    let mut bytes = Vec::with_capacity(22);
    bytes.extend_from_slice(b"bal-wall-chain");
    bytes.extend_from_slice(&number.to_be_bytes());
    keccak256(bytes)
}

/// Whether the modeled block at `number` writes this identity — one
/// independent bit per `(block, identity)`, so coverage is deterministic but
/// partial, like a real block's.
#[must_use]
pub fn write_selected(number: u64, address: Address, slot: U256) -> bool {
    keccak256(preimage("bal-wall-write", number, address, slot)).0[31] & 1 == 1
}

/// Whether a candidate reads one identity at `number` — one independent bit
/// per `(block, identity)`, so a replay's candidate read stream is
/// deterministic but partial: some identities are read again inside their
/// idle window and some go quiet, the workload retention is measured against.
#[must_use]
pub fn read_selected(number: u64, address: Address, slot: U256) -> bool {
    keccak256(preimage("bal-retention-read", number, address, slot)).0[31] & 1 == 1
}

/// The value a write at `number` sets.
#[must_use]
pub fn write_value(number: u64, address: Address, slot: U256) -> U256 {
    U256::from_be_bytes(keccak256(preimage("bal-wall-value", number, address, slot)).0)
}

/// The modeled value of one identity after the block at `block`: the value the
/// last selecting write set, or the pre-chain base value when no modeled block
/// in `first..=block` wrote it.
#[must_use]
pub fn modeled_value(first: u64, block: u64, address: Address, slot: U256) -> U256 {
    for number in (first..=block).rev() {
        if write_selected(number, address, slot) {
            return write_value(number, address, slot);
        }
    }
    write_value(first.saturating_sub(1), address, slot)
}

/// The shared chain's value of one identity after `block`: the value of the
/// last block up to `block` that BOTH coverages wrote it, or the pre-chain base
/// value when no such block exists. This is the value table a coverage
/// comparison serves whatever lists it replays (module docs).
#[must_use]
pub fn shared_value(
    first: u64,
    block: u64,
    real: &RealCoverage,
    address: Address,
    slot: U256,
) -> U256 {
    for number in (first..=block).rev() {
        if write_selected(number, address, slot) && real.is_write(number, address, slot) {
            return write_value(number, address, slot);
        }
    }
    write_value(first.saturating_sub(1), address, slot)
}

/// One modeled block's final writes over the tracked identities, in key order:
/// exactly the identities its write bit selects, each carrying the value the
/// chain serves after it.
#[must_use]
pub fn block_writes(first: u64, number: u64, keys: &BTreeSet<Key>) -> Vec<(Address, U256, U256)> {
    keys.iter()
        .filter(|(address, slot)| write_selected(number, *address, *slot))
        .map(|(address, slot)| {
            (
                *address,
                *slot,
                modeled_value(first, number, *address, *slot),
            )
        })
        .collect()
}

/// One block's modeled final writes over the tracked identities under the
/// SHARED chain: the same identities [`block_writes`] selects, each carrying
/// the shared value chain's word after it (module docs).
#[must_use]
pub fn shared_block_writes(
    first: u64,
    number: u64,
    keys: &BTreeSet<Key>,
    real: &RealCoverage,
) -> Vec<(Address, U256, U256)> {
    keys.iter()
        .filter(|(address, slot)| write_selected(number, *address, *slot))
        .map(|(address, slot)| {
            (
                *address,
                *slot,
                shared_value(first, number, real, *address, *slot),
            )
        })
        .collect()
}

/// One block's REAL final writes: exactly the captured fixture's write
/// identities at that block (the applied set the live probe applied), each
/// carrying the shared value chain's word after it.
#[must_use]
pub fn real_block_writes(
    first: u64,
    number: u64,
    real: &RealCoverage,
) -> Vec<(Address, U256, U256)> {
    real.writes_at(number)
        .iter()
        .map(|&(address, slot)| {
            (
                address,
                slot,
                shared_value(first, number, real, address, slot),
            )
        })
        .collect()
}

/// Which write identities one coverage mode's lists carry, and which block
/// hashes key them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Coverage {
    /// The modeled write bit's identities at the modeled block hashes.
    Synthetic,
    /// The captured real identities at their exact (real) hashes.
    Real,
}

impl Coverage {
    /// The block hash this coverage's anchors and lists are keyed by.
    ///
    /// # Errors
    ///
    /// Real coverage has no identity outside the captured stretch.
    pub fn hash(self, real: &RealCoverage, number: u64) -> Result<B256, String> {
        match self {
            Self::Synthetic => Ok(block_hash(number)),
            Self::Real => real.hash(number),
        }
    }

    /// The parent hash a child at `number` names: its predecessor's hash —
    /// the modeled chain's derived link, or the captured header's real one.
    ///
    /// # Errors
    ///
    /// Real coverage has no linkage outside the captured stretch.
    pub fn parent_hash(self, real: &RealCoverage, number: u64) -> Result<B256, String> {
        match self {
            Self::Synthetic => Ok(block_hash(number.saturating_sub(1))),
            Self::Real => real.parent_hash(number),
        }
    }
}

/// One block's modeled final writes as raw EIP-7928 list bytes — the exact
/// bytes the stub answers the list request with.
#[must_use]
pub fn bal_bytes(writes: &[(Address, U256, U256)]) -> Vec<u8> {
    let mut accounts: Vec<AccountChanges> = Vec::new();
    for &(address, slot, value) in writes {
        let entry = SlotChanges {
            slot,
            changes: vec![StorageChange {
                block_access_index: BlockAccessIndex(0),
                new_value: value,
            }],
        };
        match accounts
            .iter_mut()
            .find(|account| account.address == address)
        {
            Some(account) => account.storage_changes.push(entry),
            None => accounts.push(AccountChanges {
                address,
                storage_changes: vec![entry],
                storage_reads: Vec::new(),
                balance_changes: Vec::new(),
                nonce_changes: Vec::new(),
                code_changes: Vec::new(),
            }),
        }
    }
    alloy::rlp::encode(Bal::from(accounts))
}

/// The precomputed modeled chain over the corpus's identities: the value of
/// every tracked identity at every modeled block, and every block's raw list
/// keyed by its exact hash.
#[derive(Debug)]
pub struct ChainState {
    values: HashMap<(Key, u64), U256>,
    bals: HashMap<B256, Vec<u8>>,
}

impl ChainState {
    /// Build the chain over `keys` for the blocks `first..=last`.
    ///
    /// # Errors
    ///
    /// An empty key set or an empty block range is not a chain this benchmark
    /// can define over.
    pub fn build(keys: &BTreeSet<Key>, first: u64, last: u64) -> Result<Self, String> {
        if keys.is_empty() {
            return Err("the modeled chain needs at least one tracked identity".to_string());
        }
        if first > last {
            return Err(format!("the modeled block range {first}..={last} is empty"));
        }
        let mut values = HashMap::new();
        let mut bals = HashMap::new();
        let mut current: HashMap<Key, U256> = keys
            .iter()
            .map(|&key @ (address, slot)| {
                (key, write_value(first.saturating_sub(1), address, slot))
            })
            .collect();
        for number in first..=last {
            let writes = block_writes(first, number, keys);
            for &(address, slot, value) in &writes {
                current.insert((address, slot), value);
            }
            for (&key, &value) in &current {
                values.insert((key, number), value);
            }
            bals.insert(block_hash(number), bal_bytes(&writes));
        }
        Ok(Self { values, bals })
    }

    /// The value the modeled chain serves for one identity at one block.
    #[must_use]
    pub fn value_at(&self, address: Address, slot: U256, block: u64) -> Option<U256> {
        self.values.get(&((address, slot), block)).copied()
    }

    /// The raw list bytes for one block hash, when the chain has one there.
    #[must_use]
    pub fn bal_at(&self, hash: B256) -> Option<&[u8]> {
        self.bals.get(&hash).map(Vec::as_slice)
    }

    /// Build the COVERAGE-COMPARISON chain over `keys` for `first..=last`: one
    /// shared value table (values drift only at writes both the modeled bit and
    /// `real` make — [`shared_value`]) with `coverage`'s write identities
    /// served at that coverage's block hashes. Whatever coverage's lists the
    /// run replays, its reads return the same words (module docs).
    ///
    /// # Errors
    ///
    /// An empty key set or an empty block range is not a chain this benchmark
    /// can define over.
    pub fn build_coverage(
        keys: &BTreeSet<Key>,
        first: u64,
        last: u64,
        real: &RealCoverage,
        coverage: Coverage,
    ) -> Result<Self, String> {
        if keys.is_empty() {
            return Err("the modeled chain needs at least one tracked identity".to_string());
        }
        if first > last {
            return Err(format!("the modeled block range {first}..={last} is empty"));
        }
        let mut values = HashMap::new();
        let mut bals = HashMap::new();
        for number in first..=last {
            let writes = match coverage {
                Coverage::Synthetic => shared_block_writes(first, number, keys, real),
                Coverage::Real => real_block_writes(first, number, real),
            };
            for &key in keys {
                let (address, slot) = key;
                values.insert(
                    (key, number),
                    shared_value(first, number, real, address, slot),
                );
            }
            let hash = coverage.hash(real, number)?;
            bals.insert(hash, bal_bytes(&writes));
        }
        Ok(Self { values, bals })
    }
}

/// The stub JSON-RPC transport behind the modeled node: every answer is a
/// function of the read key and the pinned block, and nothing is memoized here
/// (the memo belongs to the hub, as it does in production).
#[derive(Clone, Debug)]
pub struct StubChain {
    /// The modeled chain's values and lists.
    pub chain: Arc<ChainState>,
    /// Raised by the negative probe: storage answers one wei high.
    pub plant: Arc<AtomicBool>,
}

impl StubChain {
    /// The provider the hubs and cold reads dial.
    #[must_use]
    pub fn provider(&self) -> AlloyProvider {
        let client = ClientBuilder::default().transport(self.clone(), true);
        let root: RootProvider = RootProvider::new(client);
        AlloyProvider::from_provider(Arc::new(root))
    }

    fn answer(&self, request: &SerializedRequest) -> Response {
        let payload = match self.serve(request) {
            Ok(value) => {
                ResponsePayload::Success(to_raw_value(&value).expect("a served answer serializes"))
            }
            Err(message) => ResponsePayload::Failure(ErrorPayload {
                code: -32_000,
                message: message.into(),
                data: None,
            }),
        };
        Response {
            id: request.id().clone(),
            payload,
        }
    }

    fn serve(&self, request: &SerializedRequest) -> Result<Value, String> {
        let params = request.params().ok_or("a member carries params")?;
        let params: Value = serde_json::from_str(params.get())
            .map_err(|error| format!("member params: {error}"))?;
        if request.method() == "debug_getRawBlockAccessList" {
            let hash = params
                .get(0)
                .and_then(Value::as_str)
                .ok_or("the block hash is the first param")?
                .parse::<B256>()
                .map_err(|error| format!("member hash: {error}"))?;
            let raw = self
                .chain
                .bal_at(hash)
                .ok_or_else(|| format!("the modeled chain holds no list at {hash}"))?;
            return Ok(json!(format!("0x{}", alloy::hex::encode(raw))));
        }
        let address = params
            .get(0)
            .and_then(Value::as_str)
            .ok_or("the address is the first param")?
            .parse::<Address>()
            .map_err(|error| format!("member address: {error}"))?;
        match request.method() {
            "eth_getStorageAt" => {
                let slot = params
                    .get(1)
                    .and_then(Value::as_str)
                    .ok_or("the slot is the second param")?
                    .parse::<U256>()
                    .map_err(|error| format!("member slot: {error}"))?;
                let block = params
                    .get(2)
                    .and_then(Value::as_str)
                    .ok_or("the pinned block is the third param")?;
                let block = u64::from_str_radix(block.trim_start_matches("0x"), 16)
                    .map_err(|error| format!("member block {block}: {error}"))?;
                let mut word = self
                    .chain
                    .value_at(address, slot, block)
                    .ok_or_else(|| format!("no modeled value for {address}/{slot} at {block}"))?;
                if self.plant.load(Ordering::Relaxed) {
                    word = word.wrapping_add(U256::from(1u8));
                }
                Ok(json!(format!("{word:#x}")))
            }
            "eth_getBalance" => Ok(json!(format!("{:#x}", account_read(address).balance))),
            "eth_getTransactionCount" => Ok(json!(format!("{:#x}", account_read(address).nonce))),
            "eth_getCode" => Ok(json!(format!(
                "0x{}",
                alloy::hex::encode(&account_read(address).code)
            ))),
            other => Err(format!("the modeled node serves no {other}")),
        }
    }
}

impl tower::Service<RequestPacket> for StubChain {
    type Response = ResponsePacket;
    type Error = TransportError;
    type Future = TransportFut<'static>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: RequestPacket) -> Self::Future {
        let chain = self.clone();
        Box::pin(async move {
            let responses = match &request {
                RequestPacket::Single(one) => ResponsePacket::Single(chain.answer(one)),
                RequestPacket::Batch(many) => {
                    ResponsePacket::Batch(many.iter().map(|one| chain.answer(one)).collect())
                }
            };
            Ok(responses)
        })
    }
}

/// One byte-tagged derivation off a key's seed hash.
fn derived(tag: u8, seed: B256) -> [u8; 32] {
    let mut preimage = [0u8; 33];
    preimage[0] = tag;
    preimage[1..].copy_from_slice(seed.as_slice());
    keccak256(preimage).0
}

/// The balance/nonce/code triple the modeled node serves for one account —
/// accounts carry no modeled history: every leg fetches them at its own anchor,
/// and the hub's account memo is per anchor.
#[must_use]
pub fn account_read(address: Address) -> AccountRead {
    let seed = keccak256(address.as_slice());
    let nonce = derived(2, seed);
    let mut nonce_bytes = [0u8; 8];
    nonce_bytes.copy_from_slice(&nonce[..8]);
    AccountRead {
        balance: U256::from_be_bytes(derived(1, seed)),
        nonce: u64::from_be_bytes(nonce_bytes),
        code: Bytes::copy_from_slice(&derived(3, seed)),
    }
}
