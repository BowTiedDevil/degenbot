//! The neutral opportunity descriptors are the seam's own types (ADR-098 D1).
//!
//! A foreign consumer names them off the `degenbot_execution` crate root
//! alongside the `ExecutionAdapter` contract — the crate that owns the seam
//! owns its Encode intake, so reaching the descriptors never means reaching
//! the built-in command executor.

use alloy::primitives::{address, Address};

use degenbot_execution::solve_result::{HopDescriptor, HopFamily};
use degenbot_execution::{
    ComposeError, ComposerInputs, ExecutionAdapter, HopInfo, PathInfo, ProbeSpecs, SolveResult,
    TransactionRequest, V2HopInfo, V3HopInfo, V4HopInfo,
};
use degenbot_pools::solve::SolvePathResult;

const POOL: Address = address!("88e6a0c2ddd26feeb64f039a2c41296fcb3f5640");

/// The three hop families and the path wrapper are reachable from the seam
/// crate root, with their field shapes unchanged.
#[test]
fn neutral_opportunity_descriptors_live_in_the_seam() {
    let path = PathInfo::new(vec![
        HopInfo::V2(V2HopInfo {
            pool_address: POOL,
            token0_address: Address::ZERO,
            token1_address: POOL,
            fee: 30,
            zfo: true,
        }),
        HopInfo::V3(V3HopInfo {
            pool_address: POOL,
            token0_address: POOL,
            token1_address: Address::ZERO,
            fee: 3000,
            zfo: false,
        }),
        HopInfo::V4(V4HopInfo {
            pool_manager_address: POOL,
            pool_id_hex: "0x01".to_string(),
            currency0_address: Address::ZERO,
            currency1_address: POOL,
            fee: 100,
            tick_spacing: 1,
            hook_address: Address::ZERO,
            zfo: true,
        }),
    ]);
    assert_eq!(path.hops.len(), 3);
    assert!(matches!(
        &path.hops[1],
        HopInfo::V3(hop) if hop.fee == 3000 && !hop.zfo
    ));
    assert!(matches!(
        &path.hops[2],
        HopInfo::V4(hop) if hop.pool_id_hex == "0x01" && hop.tick_spacing == 1
    ));
}

/// The solve-result projection reads the same descriptors, so the seam's view
/// contract keeps one hop vocabulary.
#[test]
fn solve_result_projects_the_neutral_descriptors() {
    let path = PathInfo::new(vec![HopInfo::V2(V2HopInfo {
        pool_address: POOL,
        token0_address: Address::ZERO,
        token1_address: POOL,
        fee: 30,
        zfo: true,
    })]);
    let descriptor = HopDescriptor::from_hop_info(&path.hops[0]);
    assert_eq!(descriptor.family, HopFamily::V2);
    assert_eq!(descriptor.pool_address, POOL);

    let result = SolvePathResult::default();
    let view = SolveResult::from_solve_path(7, &result, &path);
    assert_eq!(view.path_id, 7);
    assert_eq!(view.hop_count, 1);
    assert_eq!(view.hop_descriptors, vec![descriptor]);
}

/// The trait's Encode input is the neutral descriptor, so an implementation
/// written outside this crate never needs the command executor to name it.
#[test]
fn execution_adapter_names_the_neutral_path() {
    struct Foreign;
    impl ExecutionAdapter for Foreign {
        fn encode(
            &self,
            _path: &PathInfo,
            _inputs: &ComposerInputs<'_>,
        ) -> Result<TransactionRequest, ComposeError> {
            Ok(TransactionRequest::new(
                alloy::primitives::Address::ZERO,
                alloy::primitives::Bytes::new(),
                alloy::primitives::U256::ZERO,
            ))
        }

        fn probe_spec(&self) -> &ProbeSpecs {
            static EMPTY: ProbeSpecs = Vec::new();
            &EMPTY
        }
    }

    fn accepts<A: ExecutionAdapter>(_: &A) {}
    accepts(&Foreign);
}
