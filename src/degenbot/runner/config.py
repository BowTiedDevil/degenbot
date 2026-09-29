"""Driver configuration for the settlement-arbitrage ``BotRunner``.

This module owns the Python-companion, ``stays-python`` surface that the
runtime driver (``BotRunner``) and its tests consume:

- :class:`ArbitrageConfig` — the unified frozen config value object (built from
  the resolved verdict + CLI flags via :meth:`ArbitrageConfig.build`;
  the operator/executor identity it carries is read from the process
  environment; the deployment defaults and the checksum helper live in
  :mod:`degenbot.runner.identity`).

The display renderers (sim-diag / sim-fail / failure-breakdown) live in
:mod:`degenbot.runner._render`. The thin-margin solver-result pre-filter of the
legacy ``main()`` path is core-owned: the drop lives in the Rust dispatch leaf
alongside its basis-points denominator and candidate tuple, so this module
carries no filtering state.
"""

import dataclasses
import os
from pathlib import Path

from degenbot.arbitrage import RetryPolicy
from degenbot.config import ConfigValues, resolve_rpc_uris, resolved_config
from degenbot.constants import ZERO_ADDRESS as _ZERO_ADDRESS
from degenbot.runner.diag import DiagConfig
from degenbot.runner.identity import (
    _DEFAULT_EXECUTOR_ADDRESS,
    _DEFAULT_EXECUTOR_OWNER,
    _DEFAULT_INJECTED_ADDRESS,
    _DRY_RUN_OPERATOR_ADDRESS,
    _DRY_RUN_OPERATOR_PRIVATE_KEY,
    _PLACEHOLDER_OPERATOR_PRIVATE_KEYS,
    _checksum_or_empty,
)


def _verification_retry_policy(values: ConfigValues) -> RetryPolicy:
    """The bounded verification retry policy, from the resolved verdict.

    The four ``verify.verify_retry_*`` keys are declared in the core schema
    (``VERIFICATION_RETRY_*`` in the env layer), so the operator file and the
    environment reach them through the one cascade and the declared default is
    the only fallback left. The FFI ``RetryPolicy`` constructor validates the
    parsed knobs against the core's own bounds, so a misconfigured budget is
    refused while the config is built. A value the schema cannot parse is
    refused by the loader at boot rather than here, which is the same
    fail-loud outcome at an earlier point.

    Returns:
        The resolved :class:`~degenbot.arbitrage.RetryPolicy`.

    """
    # Named-property reads over the typed seam projection: each value arrives
    # in the Python type its declared kind names, so there is no narrowing
    # cast to hide a kind drift behind.
    return RetryPolicy(
        max_attempts=values.verify.verify_retry_max_attempts,
        base_delay=values.verify.verify_retry_base_delay,
        max_delay=values.verify.verify_retry_max_delay,
        jitter=values.verify.verify_retry_jitter,
    )


#: The crawl shell's sizing knobs, retired at the PRG-5 hard cutover: pool
#: builds and the registration work host on the fleet's duty-counted
#: ``PoolStateUpdater`` intake seats (ADR-042), so there is no crawl queue or
#: worker pool left to size.
_RETIRED_SHELL_KNOBS = ("DEGENBOT_REG_QUEUE_BOUND", "DEGENBOT_REG_WORKERS")


def _refuse_retired_shell_knobs() -> None:
    """Refuse a retired crawl-shell knob at config load, not at import.

    A closed list read through one computed name, so the refusal is one site
    rather than two literals. Presence is the whole signal: no value would be
    honored even if one were offered, and a tool that never builds a config is
    not a misconfigured bot.

    Raises:
        ValueError: A retired knob is present in the OS environment.

    """
    for name in _RETIRED_SHELL_KNOBS:
        if name in os.environ:
            msg = (
                f"{name} was retired at the PRG-5 hard cutover: the registration "
                "crawl is hosted by the fleet PoolStateUpdater intake "
                "(fleet.pool_state_updater_slots) — there is no crawl queue or "
                "worker pool to size. Remove the knob."
            )
            raise ValueError(msg)


#: The retired bare spelling of the injection stance, and the declared key
#: that replaced it. The bare name is refused anywhere it can still appear:
#: the process environment.
_RETIRED_INJECTION_KEY = "INJECT_EXECUTOR_CODE"
_TYPED_INJECTION_KEY = "DEGENBOT_INJECT_EXECUTOR_CODE"

#: One message for both layers, so the refusal names the replacement and the
#: divergence the bare name cost whichever layer the operator reached for.
_RETIRED_INJECTION_KEY_REFUSAL = (
    f"{_RETIRED_INJECTION_KEY} is retired in every layer: set "
    f"{_TYPED_INJECTION_KEY} instead (typed key simulation.inject_executor_code, "
    "docs/rust-config-keys.md). The example dotenv mapping is no longer a layer "
    "for the injection stance, and the bare name used to be honored there with "
    "the opposite default the OS environment refused — a divergence that "
    "produced a live bot which never submitted."
)


def _refuse_retired_injection_key() -> None:
    """Refuse the retired bare injection name in the process environment.

    The bare name was a hard ``ValueError`` from the OS environment but
    authoritative from the example dotenv mapping, so the same spelling meant
    two different things depending on the layer and an operator's dotenv-only
    ``INJECT_EXECUTOR_CODE=1`` produced a live bot that never submitted (the
    divergence surfaced as WARNING-only skip lines, not an error). One
    declaration means one name: the dotenv mapping is gone, the bare spelling
    is refused in the process environment, and the stance itself is the
    declared ``simulation.inject_executor_code`` key.

    Raises:
        ValueError: The retired bare name is present in the environment.

    """
    if _RETIRED_INJECTION_KEY in os.environ:
        raise ValueError(_RETIRED_INJECTION_KEY_REFUSAL)


@dataclasses.dataclass(frozen=True)
class RpcCascadeOverrides:
    """The :func:`degenbot.config.resolve_rpc_uris` inputs for :meth:`ArbitrageConfig.build`.

    The chain identity and the explicit-override endpoint travel together: they
    are exactly the arguments the RPC cascade consumes, so bundling them keeps
    the factory's keyword surface one concept per parameter. One ``node`` fills
    one transport slot and the core classifies which, so a driver that needs
    both an HTTP and a WS endpoint declares one of them here and takes the other
    from the file or the environment.
    """

    chain_id: int = 1
    node: str | None = None


@dataclasses.dataclass(frozen=True)
class ArbitrageConfig:
    """Unified settlement-arbitrage configuration — one object for the ~20 tunables `main()` reads.

    Replaces the scattered config sources (the example's dotenv file,
    module-top constants, and CLI flags) with a single frozen value object.
    Construct via :meth:`build`; the bridge onto ``main()`` lives in the
    ``BotRunner`` orchestration. The operational stances resolve through the
    core verdict, and the operator/executor identity is read from the process
    environment. Live defaults reproduce ``main()``'s behavior exactly — no
    new defaults invented.
    """

    # Operator identity
    operator_address: str
    operator_private_key: str
    # The chain identity: used for the RPC cascade AND stamped onto every
    # signed transaction by the submit leaf's TxSigner — signing against a
    # different chain than sims/RPC would be a silent replay-fail.
    chain_id: int
    # Node endpoints, one per capability: the request endpoint (pool reads,
    # eth_callMany, submission) and the subscription endpoint a feed holds
    # open. Each resolves independently through the same four layers.
    node_http: str
    node_ws: str
    # Executor contract + code-injection
    executor_address: str
    executor_owner: str
    inject_executor_code: bool
    injected_address: str
    # Dispatch policy is absent by design: the profit floor, fee percentiles,
    # priority-fee pricing, the sim fan-out cap, and the path-suppression
    # thresholds are core-owned and applied in the core, so a driver-side copy
    # of any of them could only be a value the core ignores.
    # Path discovery
    permutation_filter: frozenset[str] | None
    # Bounded retry-with-backoff for transient verification RPC failures
    # (per-call transport / provider-init). Mismatch stays fatal.
    verification_retry_policy: RetryPolicy
    # The declared driver stances (the `dispatch.*` and `pathfinding.*` keys).
    # Each arrives from the resolved verdict, so the operator file and the
    # environment reach them through the one cascade `degenbot-config` owns.
    erc6909_profit: bool
    min_profit_margin_bps: int
    reg_progress_secs: float
    max_registered_paths: int
    # The runtime values the driver's leaves consume. Resolved once here, at
    # the construction boundary, so a leaf reads the value it was handed rather
    # than reaching for the process verdict — the Python companion to the
    # engine's instance-scoped `SolveRuntimeConfig`.
    discovery_batch_size: int
    contracts_dir: str
    sim_pipeline_concurrency: int
    sim_exit_on_fail: bool
    sim_exit_ignore_buckets: str
    # Run mode
    dry_run: bool
    # The incident probes the cockpit arms at start(). Required, because the
    # declared `diagnostics.*` keys are the only place their defaults live.
    diag: DiagConfig
    # Explicit executor-runtime bytecode path (file containing 0x-prefixed hex).
    # None -> the `dispatch.contracts_dir` key -> one computed source-layout
    # candidate (NO filesystem walk). Wheel installs: pass this explicitly.
    executor_runtime: str | Path | None = None

    @classmethod
    def build(
        cls,
        *,
        live: bool,
        permutation: str | None,
        rpc: RpcCascadeOverrides | None = None,
        values: ConfigValues | None = None,
    ) -> "ArbitrageConfig":
        """Build an ArbitrageConfig from the resolved verdict + CLI flags + process identity.

        The dotenv file is no longer a source: operator/executor identity is
        read from the process environment (the launch shell exports it from
        ``bot.env``), and every ``DEGENBOT_*`` operational stance is a declared
        schema key that arrives from :func:`degenbot.config.resolved_config`
        and reaches the operator file as well as the environment; nothing here
        re-resolves a layer.

        Behavior:
        - operator: live mode requires both OPERATOR_ADDRESS/OPERATOR_PRIVATE_KEY
          from the process environment and refuses a known placeholder key
          (raises ValueError); dry-run defaults to a valid throwaway key + its
          derived address.
        - nodes: delegated to :func:`degenbot.config.resolve_rpc_uris`, so the
          four-layer cascade (explicit ``node`` > OS env
          ``DEGENBOT_RPC_{IPC,WS,HTTP}_CHAINID_{cid}`` > the operator file's
          ``[nodes.*]`` tables > a declared default) applies, per capability.
          There is **no ``localhost`` default** — a chain with no configured
          endpoint in any layer raises :class:`RpcNotConfiguredError`.
        - executor: zero address is a fatal ``ValueError`` (a factory cannot
          return early like ``main()``'s ``return``).
        - inject code: when ``simulation.inject_executor_code`` resolves true,
          the executor address is overridden to ``INJECTED_EXECUTOR_ADDRESS``.
        - permutation: a CLI string becomes a singleton frozenset; ``None`` stays ``None``.

        Returns:
            A frozen ``ArbitrageConfig`` with cascade-resolved ``node_http``/``node_ws``.

        Raises:
            ValueError: missing or placeholder operator in live mode, zero-address executor, a
                retired knob present, or ``RpcNotConfiguredError`` (a
                ``ValueError`` subclass) when no RPC endpoint is configured for
                ``chain_id`` in any cascade layer.

        """
        _refuse_retired_shell_knobs()
        _refuse_retired_injection_key()
        overrides = rpc if rpc is not None else RpcCascadeOverrides()

        # ── Operator ──
        operator_address_raw = os.environ.get("OPERATOR_ADDRESS") or ""
        operator_private_key = os.environ.get("OPERATOR_PRIVATE_KEY") or ""
        operator_address = _checksum_or_empty(operator_address_raw) if operator_address_raw else ""
        if not live:
            # dry-run: allow missing operator → a valid throwaway key + its
            # derived address (must be a real secp256k1 scalar so the eagerly
            # constructed `TxSigner` doesn't reject it — see the constants'
            # docstring). The key never signs: the leaf's `dry_run` guard
            # skips every candidate before reaching `sign_eip1559`.
            if not operator_address:
                operator_address = _DRY_RUN_OPERATOR_ADDRESS
            if not operator_private_key:
                operator_private_key = _DRY_RUN_OPERATOR_PRIVATE_KEY
        else:
            if not operator_address or not operator_private_key:
                msg = (
                    "OPERATOR_ADDRESS and OPERATOR_PRIVATE_KEY must be set in the process "
                    "environment (the launch shell exports them from bot.env) for live mode"
                )
                raise ValueError(msg)
            if operator_private_key.lower() in _PLACEHOLDER_OPERATOR_PRIVATE_KEYS:
                msg = (
                    "OPERATOR_PRIVATE_KEY is a known placeholder (the dry-run throwaway or "
                    "the all-zero scalar): refusing to run live with a key the repository "
                    "publishes. Set the real operator key in the process environment."
                )
                raise ValueError(msg)

        # ── Node URLs — delegated to the library cascade (resolve_rpc_uris) ──

        node_http, node_ws = resolve_rpc_uris(overrides.chain_id, node=overrides.node)

        # ── Executor ──
        executor_address = _checksum_or_empty(
            os.environ.get("EXECUTOR_CONTRACT_ADDRESS") or _DEFAULT_EXECUTOR_ADDRESS
        )
        if executor_address == _ZERO_ADDRESS:
            msg = "EXECUTOR_CONTRACT_ADDRESS is the zero address"
            raise ValueError(msg)

        resolved_values = resolved_config().values if values is None else values
        inject_executor_code = bool(resolved_values.simulation.inject_executor_code)
        injected_address = _checksum_or_empty(
            os.environ.get("INJECTED_EXECUTOR_ADDRESS") or _DEFAULT_INJECTED_ADDRESS
        )
        # Owner defaulting follows the deployment invariant: a real
        # deployment makes the owner the deployer, and the operator IS the
        # deployer (bot.env key), so an unset owner is the operator — with
        # the dry-run placeholder pair as the only non-live fallback.
        default_owner = operator_address if live else _DEFAULT_EXECUTOR_OWNER
        executor_owner = _checksum_or_empty(
            os.environ.get("EXECUTOR_OWNER_ADDRESS") or default_owner
        )
        if live and executor_owner != operator_address:
            msg = (
                "EXECUTOR_OWNER_ADDRESS must equal OPERATOR_ADDRESS in live mode "
                f"(owner {executor_owner} != operator {operator_address}): the "
                "executor's owner gate and the sim's caller both key off this address"
            )
            raise ValueError(msg)
        if live and inject_executor_code:
            # Injected bytecode exists only inside the simulator's overlay:
            # a live dispatch to it targets empty code on-chain. Previously
            # this combination was carried by a second flag layer whose skip
            # veto made it *look* like a live run; refuse it at config load.
            msg = (
                "live mode requires a deployed executor: the injection stance is active "
                "(simulation.inject_executor_code / DEGENBOT_INJECT_EXECUTOR_CODE). "
                "Set it to 0 and deploy the executor first."
            )
            raise ValueError(msg)
        # The injection stance swaps in the overlay address the sim injects
        # bytecode at, so the executor the bot dispatches to follows it.
        if inject_executor_code:
            executor_address = injected_address

        verification_retry_policy = _verification_retry_policy(resolved_values)
        executor_runtime = os.environ.get("EXECUTOR_RUNTIME") or None
        # The incident probes' intervals come from the declared
        # `diagnostics.*` keys; zero is the declared default, so there is no
        # second "off" spelling here.
        diag = DiagConfig(
            tracemalloc_secs=resolved_values.diagnostics.tracemalloc_secs,
            procmem_secs=resolved_values.diagnostics.procmem_secs,
            procmem_csv=resolved_values.diagnostics.procmem_csv,
            faulthandler_timeout_secs=resolved_values.diagnostics.faulthandler_timeout_secs,
        )

        return cls(
            operator_address=operator_address,
            operator_private_key=operator_private_key,
            chain_id=overrides.chain_id,
            node_http=node_http,
            node_ws=node_ws,
            executor_address=executor_address,
            executor_owner=executor_owner,
            inject_executor_code=inject_executor_code,
            injected_address=injected_address,
            permutation_filter=(frozenset({permutation}) if permutation is not None else None),
            dry_run=not live,
            erc6909_profit=resolved_values.dispatch.erc6909_profit,
            min_profit_margin_bps=resolved_values.dispatch.min_profit_margin_bps,
            reg_progress_secs=resolved_values.pathfinding.reg_progress_secs,
            max_registered_paths=resolved_values.pathfinding.max_registered_paths,
            # The ONE clamp owner for the batch size: the shell getter's twin
            # is gone, so a zero/garbage value degrades to the legacy
            # per-path delivery exactly once, here.
            discovery_batch_size=max(1, resolved_values.pathfinding.discovery_batch_size),
            contracts_dir=resolved_values.dispatch.contracts_dir or "",
            sim_pipeline_concurrency=max(1, resolved_values.simulation.pipeline_concurrency),
            sim_exit_on_fail=resolved_values.simulation.sim_exit_on_fail,
            sim_exit_ignore_buckets=resolved_values.simulation.exit_ignore_buckets,
            verification_retry_policy=verification_retry_policy,
            executor_runtime=executor_runtime,
            diag=diag,
        )
