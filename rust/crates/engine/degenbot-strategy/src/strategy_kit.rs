//! ADR-061 D3: the strategy kit — one boot-resolved composition of the
//! clustered cells a strategy drives.
//!
//! The six-slot Strategy vocabulary is the ADR-058 D3 plane construct — a
//! Rust-typed taxonomy of `slot -> meaning`, not a Python-only list; this
//! module is where the *shared machinery* a strategy actually
//! composes is named as a concrete struct. A strategy receives one
//! [`StrategyKit`] from the boot rather than constructing its own ingress,
//! discovery graph, or verifier, so "which strategies compose what" is a
//! static fact rather than an inference from whichever seams happen to be
//! wired.
//!
//! # Cells
//!
//! - **provision** ([`ProvisionCell`]): the single V3 pool-state ingress
//!   (`Db -> Chain` tick-map precedence, per-block memo) the strategy admits
//!   through. The boot builds it from the held DB connection and attaches the
//!   chain arm + chain-sample policy; the verify policy value flows on
//!   [`PoolIngress::set_verify_level`] (ADR-061 D7).
//! - **discovery** ([`DiscoveryHandles`]): the frozen route registry and the
//!   startup discovery graph built from its connector index. `None` keeps the
//!   discovery fan shut — frames observe; connectors are never guessed.
//!
//! # Cells deliberately absent
//!
//! The ADR's sketch also names `simulate`, `submit`, and `react`. None has a
//! genuine boot-resolved per-strategy value today, so inventing placeholder
//! ones would be exactly the "hypothetical seam" the design rule forbids:
//!
//! - **simulate**: `BlockSimHandle` is rebuilt inside the driver loop whenever
//!   the observed head advances ([`build_block_handle`]) and borrows the
//!   loop's own runtime; it is a per-block loop-local resource, not a
//!   per-strategy boot fact. A hosted `StrategyKit` cannot own it without
//!   moving the head watch into the kit.
//! - **submit**: the submission lane (`NonceLane`) is minted by the host and
//!   carried by the boot context; the submission target is resolved per-frame
//!   by `driver_policy` (relay fan-out + builder target). No `SubmitCell`
//!   value is resolved at kit time today.
//! - **react**: the head subscription and pending-tx feed are registered with
//!   the host [`Hub`](degenbot_eventhub::Hub) at drive time inside the driver
//!   loop. Reaction-kind dispatch stays strategy-owned (ADR-061 D6); the
//!   head/feed handles are not a per-strategy kit value.
//!
//! [`build_block_handle`]: crate::frame_pipeline::build_block_handle

use std::sync::Arc;

use degenbot_bot::bot_core::pool_ingress::{
    DbArm, IngressWitness, PoolIngress, TickMapPoolIdentity, TickMapSampleVerifier, VerifyLevel,
};
use degenbot_bot::bot_core::RouteRegistry;
use degenbot_pools::tick_fetch::TickBootstrapRpc;

use crate::anchored_dfs::AnchoredGraph;
use crate::strategy_plane::StrategyName;

/// The provisioning cell: the one V3 pool-state ingress a strategy admits
/// through, with the chain arm and chain-sample policy the boot attached.
pub struct ProvisionCell {
    /// `Db -> Chain` tick-map admission, memoized per block.
    pub ingress: PoolIngress,
}

/// The discovery cell: the frozen route registry and the startup graph built
/// from its connector index.
pub struct DiscoveryHandles {
    /// The boot-time pool world-view (connector index + frozen pool set).
    pub registry: Arc<RouteRegistry>,
    /// The startup discovery graph over the registry's connector edge set.
    pub dfs: AnchoredGraph,
}

/// The boot-resolved composition of clustered cells a strategy drives.
///
/// Fields are concrete where variation is hypothetical; `dyn` appears only at
/// the ingress's real adapter seams (`TickMapDb`, `TickBootstrapRpc`). There
/// are no defaulted traits: a strategy cannot answer a cell with something it
/// built itself.
pub struct StrategyKit {
    /// Pool-state provisioning (always present).
    pub provision: ProvisionCell,
    /// Discovery handles; `None` means the fan is shut.
    pub discovery: Option<DiscoveryHandles>,
}

/// The ingress's backfill witness, written to the offline-review JSONL
/// capture so a soak can count the pools the Db arm advanced to head.
struct TraceBackfillWitness;

impl IngressWitness for TraceBackfillWitness {
    fn db_backfill(
        &self,
        identity: TickMapPoolIdentity,
        from_block: u64,
        to_block: u64,
        events: usize,
    ) {
        let (family, pool) = match identity {
            TickMapPoolIdentity::V3(pool) => ("v3", pool),
            TickMapPoolIdentity::V4 { manager, pool_id } => {
                return crate::frame_pipeline::trace_jsonl(
                    "ingress_stage",
                    serde_json::json!({
                        "pool": format!("0x{}", alloy::hex::encode(manager)),
                        "pool_id": pool_id,
                        "stage": "admit-v4-db-backfill",
                        "from_block": from_block,
                        "to_block": to_block,
                        "events": events,
                    }),
                );
            }
        };
        crate::frame_pipeline::trace_jsonl(
            "ingress_stage",
            serde_json::json!({
                "pool": format!("0x{}", alloy::hex::encode(pool)),
                "stage": format!("admit-{family}-db-backfill"),
                "from_block": from_block,
                "to_block": to_block,
                "events": events,
            }),
        );
    }
}

impl StrategyKit {
    /// The cells this composition can carry, fixed by the struct shape.
    ///
    /// A new shared capability must be added here (and to the declared table
    /// below) in the same reviewable diff; that churn is the audit checkpoint.
    pub const COMPOSED_CELLS: [StrategyCell; 2] =
        [StrategyCell::Provision, StrategyCell::Discovery];

    /// Resolve the kit once at boot: build the ingress over the held DB arm
    /// (whose paired transport closes any Db-to-head lag), attach the chain arm
    /// and the chain-sample policy, and build the discovery graph from the
    /// frozen registry.
    ///
    /// This is the ONE kit resolve site; a strategy composes the result and
    /// never its own ingress.
    #[must_use]
    pub fn resolve(
        registry: Option<Arc<RouteRegistry>>,
        db: Option<DbArm>,
        chain: Option<Arc<dyn TickBootstrapRpc>>,
        verify: VerifyLevel,
        verifier: Option<Arc<dyn TickMapSampleVerifier>>,
    ) -> Self {
        let mut ingress = PoolIngress::new(db, chain);
        ingress.set_verify_level(verify);
        ingress.set_witness(Arc::new(TraceBackfillWitness));
        if let Some(verifier) = verifier {
            ingress.set_verifier(verifier);
        }
        let discovery = registry.map(|registry| DiscoveryHandles {
            dfs: AnchoredGraph::from_connector_index(registry.index()),
            registry,
        });
        Self {
            provision: ProvisionCell { ingress },
            discovery,
        }
    }

    /// The cells this resolved kit actually wires, for the declared==wired pin.
    #[must_use]
    pub fn cells(&self) -> Vec<StrategyCell> {
        let mut cells = vec![StrategyCell::Provision];
        if self.discovery.is_some() {
            cells.push(StrategyCell::Discovery);
        }
        cells
    }

    /// The frozen route registry (`None` when the discovery fan is shut).
    #[must_use]
    pub fn registry(&self) -> Option<&Arc<RouteRegistry>> {
        self.discovery.as_ref().map(|d| &d.registry)
    }

    /// The startup discovery graph (`None` exactly when the registry is).
    #[must_use]
    pub fn dfs(&self) -> Option<&AnchoredGraph> {
        self.discovery.as_ref().map(|d| &d.dfs)
    }

    /// The provisioning ingress.
    #[must_use]
    pub fn ingress(&self) -> &PoolIngress {
        &self.provision.ingress
    }
}

/// One clustered cell of the strategy kit vocabulary.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum StrategyCell {
    /// Pool-state provisioning (the ingress).
    Provision,
    /// Discovery handles (registry + startup graph).
    Discovery,
    /// Per-strategy block simulation handle — deliberately absent (see module
    /// doc).
    Simulate,
    /// Per-strategy submission lane + target — deliberately absent.
    Submit,
    /// Reaction feeds (head tick + pending-tx ring) — deliberately absent.
    React,
}

impl StrategyCell {
    /// Every cell in the vocabulary, including the three the kit documents as
    /// absent, so the declaration pin cannot silently skip one.
    pub const ALL: [Self; 5] = [
        Self::Provision,
        Self::Discovery,
        Self::Simulate,
        Self::Submit,
        Self::React,
    ];

    /// Stable label for diagnostics and test messages.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Provision => "provision",
            Self::Discovery => "discovery",
            Self::Simulate => "simulate",
            Self::Submit => "submit",
            Self::React => "react",
        }
    }
}

/// The declared==wired strategy-kit table: which cells each plane strategy
/// composes.
///
/// The settlement arm composes no kit: its sealed-block work is driven by the
/// engine pump (the pump self-driving special case on the settlement ledger),
/// so it resolves no `StrategyKit`. The two hosted backrun arms share the one
/// resolved composition.
pub const STRATEGY_CELLS: &[(StrategyName, &[StrategyCell])] = &[
    (StrategyName::Settlement, &[]),
    (
        StrategyName::MevblockerBackrun,
        &[StrategyCell::Provision, StrategyCell::Discovery],
    ),
    (
        StrategyName::TxpoolBackrun,
        &[StrategyCell::Provision, StrategyCell::Discovery],
    ),
];

/// The declared cells for one strategy, `None` when the table has no row (a
/// declaration hole, not a silent "none").
#[must_use]
pub fn declared_cells(name: StrategyName) -> Option<&'static [StrategyCell]> {
    STRATEGY_CELLS
        .iter()
        .find_map(|(n, cells)| (*n == name).then_some(*cells))
}
