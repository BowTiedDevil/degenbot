"""The one test-side door onto ADR-013's private Rust seam.

``degenbot._ffi`` is the compiled extension's registration surface, not a stable
API: its names follow the Rust core's own module tree, and a rename there is a
normal refactor. Test code nevertheless needs those names — engine handles to
drive, pure math leaves to cross-check against a second implementation, config
verdicts to compare against the Python oracle — so it takes them from here
instead of importing the extension at each call site.

``tests/test_ffi_boundary.py`` scans ``tests/`` and fails on any other ``_ffi``
import, which is what makes a seam rename a one-file edit in this module rather
than a sweep across every test that hand-imported a Rust symbol.

Two deliveries, and the difference between them is the discipline:

- the **symbols** re-exported below, each from the module that owns it (the
  extension root, or the ``add_submodule`` child it is registered on). A test
  adds a name by adding it to the group it belongs to and importing it from
  ``tests.helpers.seam``.
- :data:`REGISTRATION_SURFACE`, the extension module object itself. Only a test
  whose subject *is* the registration surface — the pyclass and stub walkers
  under ``tests/ffi/``, and the public-mirror identity checks — needs to walk
  the module tree (``dir()``, ``__module__``, submodule presence). Those files
  are listed in the boundary test; reaching an ordinary symbol through the
  module is the leak the door exists to prevent.

``degenbot.<domain>`` packages remain the door for *driver* code (ADR-013): a
test that can express its need through a companion API should, and this module
is for the seam-level claims a companion cannot make.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

import degenbot._ffi as _registration_surface

if TYPE_CHECKING:
    # Stub-only aliases: ``_ffi``'s ``.pyi`` declares these for the type checker
    # while the extension registers no runtime object under the name, so the
    # import belongs on the annotation path (a runtime ``from degenbot._ffi
    # import ChecksummedAddress`` raises).
    from degenbot._ffi import ChecksummedAddress  # ruff: ignore[unused-import] — re-export for annotations

# The extension root. Engine handles (ADR-032's module-path disambiguators),
# the address/crypto primitives the pure-Python mirrors are checked against,
# the resolved-config verdict pair, and the registration family vocabulary.
from degenbot._ffi import (
    ArbitrageEngine,
    BalancerFamily,
    Bot,
    BotIo,
    Erc20TokenRow,
    FleetIntakeFaultedError,
    PathGraph,
    Pool,
    PoolBuildDecodeError,
    PoolBuildRpcError,
    PoolConstructionRequest,
    PoolKind,
    PoolTickCoverage,
    RegistrationLedger,
    RegistrationLifecycle,
    ResolvedConfig,
    RetryPolicy,
    StrategyHostError,
    SwapRefusalKind,
    SwapRefused,
    UnconfiguredStrategyError,
    UnknownStrategyError,
    VerificationRpcError,
    build_path_graph,
    call_on_ambient_runtime,
    compute_aerodrome_v2_pool_address,
    compute_aerodrome_v3_pool_address,
    create2_address,
    event_topic,
    execution,
    executor,
    fleet,
    generate_v2_pool_address,
    generate_v3_pool_address,
    keccak256,
    pool_family_tags,
    registration_admission_kinds,
    registration_outcome_tags,
    registration_pool_memo_key,
    registration_unit_kinds,
    reinstall_config_holder_for_tests,
    resolve_hypothetical,
    resolve_hypothetical_chain_id,
    resolve_hypothetical_database_path,
    resolve_hypothetical_node_uri,
    resolved_config,
    resolved_node_uri_for_tests,
    session_phase_next,
    shutdown_log_drainer,
    solve_balancer_weighted_basket,
    to_checksum_address,
)

# aave: the updater's position scan and market activation.
from degenbot._ffi.aave import (
    activate_aave_market,
    deactivate_aave_market,
    run_aave_update,
    verify_touched_positions_on_chain,
)

# abi: the single-value codec leaves.
from degenbot._ffi.abi import decode_single, encode_single

# balancer_math / v2_math / concentrated_liquidity_math / curve_dy: the pure
# math leaves whose Python ports are parity-checked against them.
from degenbot._ffi.balancer_math import (
    fixed_point_div_down,
    fixed_point_div_up,
    fixed_point_mul_down,
)
from degenbot._ffi.cancel import CancelHandle
from degenbot._ffi.concentrated_liquidity_math import (
    MAX_SQRT_RATIO,
    MAX_TICK,
    MIN_SQRT_RATIO,
    MIN_TICK,
    get_sqrt_ratio_at_tick,
    get_tick_at_sqrt_ratio,
    get_tick_word_and_bit_position,
)
from degenbot._ffi.curve_dy import DyCalculationInputs, calculate_dy

# db: the schema migration entry and the row inputs a writer test builds.
from degenbot._ffi.db import LiquidityUpdateEvent, V3PoolRowInput, db_upgrade_database

# deployments / dex_identity: the resolver statics and the identity record.
from degenbot._ffi.deployments import (
    deployer_for,
    init_hash_for,
    resolve_deployer,
    resolve_v2_init_hash,
    resolve_v3_init_hash,
)
from degenbot._ffi.dex_identity import DexIdentity, dex_identity

# provider: the transport the offline harnesses stub.
from degenbot._ffi.provider import AlloyProvider, AsyncAlloyProvider, is_local_rpc_url

# simulation: the batch executor and dispatch surface.
from degenbot._ffi.simulation import (
    CandidateAssembly,
    DispatchCandidate,
    SimulateContext,
    assemble_dispatch_candidates_py,
    build_batch_executor_py,
    executor_policy_py,
    merge_payload_results_py,
    simulate_in_process_revert_probe,
    simulate_in_process_success_probe,
)

# submission: the signer/dispatcher vocabulary a submission test drives.
from degenbot._ffi.submission import (
    Dispatcher,
    DivergentPool,
    SubmitCandidate,
    TxParams,
    TxSigner,
    finalize_fees,
)
from degenbot._ffi.v2_math import calc_exact_in_v2, calc_exact_out_v2

# The module object, for the registration-surface walkers only (see the module
# docstring). Not a general accessor: the boundary test restricts it to the
# tests whose subject is the surface itself.
REGISTRATION_SURFACE = _registration_surface

__all__ = [
    "MAX_SQRT_RATIO",
    "MAX_TICK",
    "MIN_SQRT_RATIO",
    "MIN_TICK",
    "REGISTRATION_SURFACE",
    "AlloyProvider",
    "ArbitrageEngine",
    "AsyncAlloyProvider",
    "BalancerFamily",
    "Bot",
    "BotIo",
    "CancelHandle",
    "CandidateAssembly",
    "DexIdentity",
    "DispatchCandidate",
    "Dispatcher",
    "DivergentPool",
    "DyCalculationInputs",
    "Erc20TokenRow",
    "FleetIntakeFaultedError",
    "LiquidityUpdateEvent",
    "PathGraph",
    "Pool",
    "PoolBuildDecodeError",
    "PoolBuildRpcError",
    "PoolConstructionRequest",
    "PoolKind",
    "PoolTickCoverage",
    "RegistrationLedger",
    "RegistrationLifecycle",
    "ResolvedConfig",
    "RetryPolicy",
    "SimulateContext",
    "StrategyHostError",
    "SubmitCandidate",
    "SwapRefusalKind",
    "SwapRefused",
    "TxParams",
    "TxSigner",
    "UnconfiguredStrategyError",
    "UnknownStrategyError",
    "V3PoolRowInput",
    "VerificationRpcError",
    "activate_aave_market",
    "assemble_dispatch_candidates_py",
    "build_batch_executor_py",
    "build_path_graph",
    "calc_exact_in_v2",
    "calc_exact_out_v2",
    "calculate_dy",
    "call_on_ambient_runtime",
    "compute_aerodrome_v2_pool_address",
    "compute_aerodrome_v3_pool_address",
    "create2_address",
    "db_upgrade_database",
    "deactivate_aave_market",
    "decode_single",
    "deployer_for",
    "dex_identity",
    "encode_single",
    "event_topic",
    "execution",
    "executor",
    "executor_policy_py",
    "finalize_fees",
    "fixed_point_div_down",
    "fixed_point_div_up",
    "fixed_point_mul_down",
    "fleet",
    "generate_v2_pool_address",
    "generate_v3_pool_address",
    "get_sqrt_ratio_at_tick",
    "get_tick_at_sqrt_ratio",
    "get_tick_word_and_bit_position",
    "init_hash_for",
    "is_local_rpc_url",
    "keccak256",
    "merge_payload_results_py",
    "pool_family_tags",
    "registration_admission_kinds",
    "registration_outcome_tags",
    "registration_pool_memo_key",
    "registration_unit_kinds",
    "reinstall_config_holder_for_tests",
    "resolve_deployer",
    "resolve_hypothetical",
    "resolve_hypothetical_chain_id",
    "resolve_hypothetical_database_path",
    "resolve_hypothetical_node_uri",
    "resolve_v2_init_hash",
    "resolve_v3_init_hash",
    "resolved_config",
    "resolved_node_uri_for_tests",
    "run_aave_update",
    "session_phase_next",
    "shutdown_log_drainer",
    "simulate_in_process_revert_probe",
    "simulate_in_process_success_probe",
    "solve_balancer_weighted_basket",
    "to_checksum_address",
    "verify_touched_positions_on_chain",
]
