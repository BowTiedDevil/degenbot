"""Driver configuration + display-only helpers for the settlement-arbitrage ``BotRunner``.

Extracted from ``examples/eth_backrun_helpers.py`` (epic 5TSYKN, task RVSYWB).
This module owns the Python-companion, ``stays-python`` surface that the
runtime driver (``BotRunner``) and its tests consume:

- :class:`BackrunConfig` — the unified frozen config value object (built from a
  dotenv mapping + CLI flags via :meth:`BackrunConfig.from_env`).
- :func:`classify_revert` + :func:`format_failure_breakdown` — simulation
  revert taxonomy / tallies (display-only).
- :func:`filter_thin_margin_results` — display-only thinning of solver results.
- :func:`format_sim_diag_line` — one always-on ``[sim-diag]`` JSON line.

None of these are engine logic; they are orchestration/config/display, so the
three-layer rubric keeps them in the Python companion (``stays-python``).
"""

import dataclasses
import json
import warnings
from collections.abc import Mapping

from degenbot.arbitrage.verification_retry import VerificationRetryPolicy
from degenbot.checksum_cache import get_checksum_address
from degenbot.config import resolve_rpc_uris
from degenbot.constants import ZERO_ADDRESS as _ZERO_ADDRESS

# Arbitrage configuration
# ──────────────────────────────────────────────────────────────────

# Default dispatch tunables — match the example's current operational values
# (eth_backrun_v2_v3_v4_rust.py module-top constants) so the BackrunSession
# bridge (slice 5b) is behavior-preserving. Canonical home for the defaults
# is the config object; the example's constants are its current deployment.
_MIN_PROFIT_NET = 1
_FEE_HISTORY_WINDOW = 10
_FEE_PERCENTILES = (10, 50)
_TARGET_PROFIT_RATIO = 1.25
_BLOCKS_BEFORE_NONCE_EXPIRES = 5
_MAX_SIMULATE_CONCURRENT = 50
_AGE_DECAY_CONSTANT = 0.25
_MIN_PRIORITY_FEE_PERCENTILE = 10
_MAX_PRIORITY_FEE_PERCENTILE = 50
_PATH_SUPPRESS_THRESHOLD = 10
_PATH_SUPPRESS_RETRY_INTERVAL = 100
# VP42BP AC item 4: default verification retry policy knobs — mirror
# ``VerificationRetryPolicy()`` so an unset env reproduces the sane defaults.
# Sane for a local/edge node recovering from a transient transport blip.
_VERIFICATION_RETRY_MAX_ATTEMPTS = 4
_VERIFICATION_RETRY_BASE_DELAY = 0.5
_VERIFICATION_RETRY_MAX_DELAY = 4.0
_VERIFICATION_RETRY_JITTER = 0.5
# Ethereum mainnet default allowed intermediate tokens — mirrors the example's
# ETH_MAINNET_ALLOWED_TOKENS set.
_ALLOWED_INTERMEDIATE_TOKENS = frozenset({
    "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",  # USDC
    "0xdAC17F958D2ee523a2206206994597C13D831ec7",  # USDT
    "0x6B175474E89094C44Da98b954EedeAC495271d0F",  # DAI
    "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599",  # WBTC
    "0x1f9840a85d5aF5bf1D1762F925BDADdC4201F984",  # UNI
    "0x514910771AF9Ca656af840dff83E8264EcF986CA",  # LINK
    "0x6B3595068778DD592e39A122f4f5a5cF09C90fE2",  # SUSHI
    "0xD533a949740bb3306d119CC777fa900bA034cd52",  # CRV
    "0xc00e94Cb662C3520282E6f5717214004A7f26888",  # COMP
    "0x0bc529c00C6401aEF6D220BE8C6Ea1667F6Ad93e",  # YFI
    "0x7D1AfA7B718fb893dB30A3aBc0Cfc608AaCfeBB0",  # MATIC/POL
})
# Default executor deployment constants — mirror the example's env defaults.
_DEFAULT_EXECUTOR_ADDRESS = "0x543C7eF4F2368a9411c94A055e7236E6Dc6f99D5"
_DEFAULT_INJECTED_ADDRESS = "0x0D6d4c3cF3BD3b769De1821f2BE0d7d99913E4F1"
_DEFAULT_EXECUTOR_OWNER = "0x9C56a29c7231974c269E24F9FB3c29203039089E"

# Dry-run operator placeholder: a VALID secp256k1 private key + its derived
# address, used when `mainnet.env` omits `OPERATOR_*` in non-live mode.
# The (now-eager) `TxSigner(key=operator_private_key, chain_id=1)` site
# rejects the former all-zero placeholder (zero is not a valid scalar) and
# raised `ValueError: signature error`. The Anvil account-0 key is a
# well-known valid throwaway that never signs in dry-run: the Rust submit
# leaf's `dry_run` guard skips `sign_eip1559` for every candidate.
_DRY_RUN_OPERATOR_PRIVATE_KEY = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"
_DRY_RUN_OPERATOR_ADDRESS = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266"


def _verification_retry_policy_from_env(env: Mapping[str, str | None]) -> VerificationRetryPolicy:
    """Build the ``VerificationRetryPolicy`` from ``VERIFICATION_RETRY_*`` env vars.

    Unset vars fall back to the module defaults (mirroring
    ``VerificationRetryPolicy()``). Non-integer / non-float values raise
    ``ValueError`` (fail fast — a typo'd env var must NOT silently fall back to
    defaults, masking the misconfiguration).

    Returns:
        A :class:`VerificationRetryPolicy` built from the env overrides.

    """
    raw_attempts = env.get("VERIFICATION_RETRY_MAX_ATTEMPTS")
    raw_base = env.get("VERIFICATION_RETRY_BASE_DELAY")
    raw_max = env.get("VERIFICATION_RETRY_MAX_DELAY")
    raw_jitter = env.get("VERIFICATION_RETRY_JITTER")

    max_attempts = _parse_int_env(raw_attempts, _VERIFICATION_RETRY_MAX_ATTEMPTS, "MAX_ATTEMPTS")
    base_delay = _parse_float_env(raw_base, _VERIFICATION_RETRY_BASE_DELAY, "BASE_DELAY")
    max_delay = _parse_float_env(raw_max, _VERIFICATION_RETRY_MAX_DELAY, "MAX_DELAY")
    jitter = _parse_float_env(raw_jitter, _VERIFICATION_RETRY_JITTER, "JITTER")

    return VerificationRetryPolicy(
        max_attempts=max_attempts,
        base_delay=base_delay,
        max_delay=max_delay,
        jitter=jitter,
    )


def _parse_int_env(raw: str | None, default: int, name_suffix: str) -> int:
    """Parse a ``VERIFICATION_RETRY_*`` int env var, falling back to ``default``.

    Returns:
        ``int(raw)`` when ``raw`` is set/non-empty, else ``default``.

    Raises:
        ValueError: ``raw`` is set but not an integer.

    """
    if raw is None or not raw:
        return default
    try:
        return int(raw)
    except ValueError:
        msg = f"VERIFICATION_RETRY_{name_suffix} must be an integer, got {raw!r}"
        raise ValueError(msg) from None


def _parse_float_env(raw: str | None, default: float, name_suffix: str) -> float:
    """Parse a ``VERIFICATION_RETRY_*`` float env var, falling back to ``default``.

    Returns:
        ``float(raw)`` when ``raw`` is set/non-empty, else ``default``.

    Raises:
        ValueError: ``raw`` is set but not a float.

    """
    if raw is None or not raw:
        return default
    try:
        return float(raw)
    except ValueError:
        msg = f"VERIFICATION_RETRY_{name_suffix} must be a float, got {raw!r}"
        raise ValueError(msg) from None


def _checksum_or_empty(addr: str | None) -> str:
    """Checksum an address, returning "" for empty input.

    Mirrors ``main()``'s ``get_checksum_address`` handling of an unset field.

    Returns:
        The checksummed address, or ``""`` for empty input.

    """
    if not addr:
        return ""
    return get_checksum_address(addr)


@dataclasses.dataclass(frozen=True)
class BackrunConfig:
    """Unified settlement-arbitrage configuration — one object for the ~20 tunables `main()` reads.

    Replaces the three scattered config sources (a ``mainnet.env`` dotenv
    dict, module-top constants, and CLI args) with a single frozen value object.
    Construct via :meth:`from_env`; the bridge onto ``main()`` lives in the
    ``BotRunner`` orchestration. Live defaults (zero-address operator + dummy
    key in dry-run, localhost nodes) reproduce ``main()``'s current behavior
    exactly — no new defaults invented.
    """

    # Operator identity
    operator_address: str
    operator_private_key: str
    # Node endpoints
    node_http: str
    node_ws: str
    # Executor contract + code-injection
    executor_address: str
    executor_owner: str
    inject_executor_code: bool
    injected_address: str
    # Dispatch tunables
    min_profit_net: int
    fee_history_window: int
    fee_percentiles: tuple[int, ...]
    target_profit_ratio: float
    blocks_before_nonce_expires: int
    max_simulate_concurrent: int
    age_decay_constant: float
    min_priority_fee_percentile: int
    max_priority_fee_percentile: int
    path_suppress_threshold: int
    path_suppress_retry_interval: int
    # Path discovery
    allowed_intermediate_tokens: frozenset[str]
    permutation_filter: frozenset[str] | None
    # VP42BP AC item 4: bounded retry-with-backoff for transient verification
    # RPC failures (per-call transport / provider-init). Mismatch stays fatal.
    verification_retry_policy: VerificationRetryPolicy
    # Run mode
    dry_run: bool

    @classmethod
    def from_env(
        cls,
        env: Mapping[str, str | None],
        *,
        live: bool,
        permutation: str | None,
        chain_id: int = 1,
        cli_http: str | None = None,
        cli_ws: str | None = None,
    ) -> "BackrunConfig":
        """Build a BackrunConfig from a dotenv-style env mapping + CLI flags.

        Behavior:
        - operator: live mode requires both OPERATOR_ADDRESS/PRIVATE_KEY
          (raises ValueError); dry-run defaults to ZERO_ADDRESS + a 0x00..00 key.
        - nodes: delegated to :func:`degenbot.config.resolve_rpc_uris` so the
          standard cascade (CLI > OS env ``DEGENBOT_RPC_{HTTP,WS}_CHAINID_{cid}``
          > caller fallback > config.toml > raise) applies. There is **no
          ``localhost`` default** — a chain with no configured endpoint in any
          layer raises :class:`RpcNotConfiguredError`.
        - executor: zero address is a fatal ``ValueError`` (a factory cannot
          return early like ``main()``'s ``return``).
        - inject code: when ``INJECT_EXECUTOR_CODE=="1"``, the executor address
          is overridden to ``INJECTED_EXECUTOR_ADDRESS``.
        - permutation: a CLI string becomes a singleton frozenset; ``None`` stays ``None``.

        Deprecated: ``NODE_HOST_HTTP``/``NODE_PORT_HTTP``/
        ``NODE_HOST_WEBSOCKET``/``NODE_PORT_WEBSOCKET`` (host+port composition)
        are rebuilt into full URIs and injected as the resolver *fallback* slot
        (below OS env, above config.toml), emitting ``DeprecationWarning``. Migrate
        to ``DEGENBOT_RPC_HTTP_CHAINID_{chain_id}`` / ``..._WS_CHAINID_{cid}``.

        Returns:
            A frozen ``BackrunConfig`` with cascade-resolved ``node_http``/``node_ws``.

        Raises:
            ValueError: missing operator in live mode, zero-address executor,
                or ``RpcNotConfiguredError`` (a ``ValueError`` subclass) when no
                RPC endpoint is configured for ``chain_id`` in any cascade layer.

        """
        # ── Operator ──
        operator_address_raw = env.get("OPERATOR_ADDRESS") or ""
        operator_private_key = env.get("OPERATOR_PRIVATE_KEY") or ""
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
            msg = (
                "OPERATOR_ADDRESS and OPERATOR_PRIVATE_KEY must be set in mainnet.env for live mode"
            )
            if not operator_address or not operator_private_key:
                raise ValueError(msg)

        # ── Node URLs — delegated to the library cascade (resolve_rpc_uris) ──
        # Legacy NODE_HOST_*/NODE_PORT_* host+port form is rebuilt into a full
        # URI and passed as the resolver *fallback* slot (below OS env, above
        # config.toml) so existing mainnet.env users keep working — but it emits
        # a DeprecationWarning pointing at the chain-id-discriminated envvar.
        fallback_http: str | None = None
        fallback_ws: str | None = None
        legacy_http_host = env.get("NODE_HOST_HTTP")
        if legacy_http_host:
            fallback_http = f"{legacy_http_host}:{env.get('NODE_PORT_HTTP') or '8545'}"
            warnings.warn(
                f"NODE_HOST_HTTP is deprecated; set DEGENBOT_RPC_HTTP_CHAINID_{chain_id} instead.",
                DeprecationWarning,
                stacklevel=2,
            )
        legacy_ws_host = env.get("NODE_HOST_WEBSOCKET")
        if legacy_ws_host:
            fallback_ws = f"{legacy_ws_host}:{env.get('NODE_PORT_WEBSOCKET') or '8546'}"
            warnings.warn(
                f"NODE_HOST_WEBSOCKET is deprecated; set "
                f"DEGENBOT_RPC_WS_CHAINID_{chain_id} instead.",
                DeprecationWarning,
                stacklevel=2,
            )

        node_http, node_ws = resolve_rpc_uris(
            chain_id,
            cli_http=cli_http,
            cli_ws=cli_ws,
            fallback_http=fallback_http,
            fallback_ws=fallback_ws,
        )

        # ── Executor ──
        executor_address = _checksum_or_empty(
            env.get("EXECUTOR_CONTRACT_ADDRESS") or _DEFAULT_EXECUTOR_ADDRESS
        )
        if executor_address == _ZERO_ADDRESS:
            msg = "EXECUTOR_CONTRACT_ADDRESS is the zero address"
            raise ValueError(msg)

        inject_executor_code = (env.get("INJECT_EXECUTOR_CODE") or "1") == "1"
        injected_address = _checksum_or_empty(
            env.get("INJECTED_EXECUTOR_ADDRESS") or _DEFAULT_INJECTED_ADDRESS
        )
        executor_owner = _checksum_or_empty(
            env.get("EXECUTOR_OWNER_ADDRESS") or _DEFAULT_EXECUTOR_OWNER
        )
        # main() behavior: when INJECT_EXECUTOR_CODE, override executor with injected
        if inject_executor_code:
            executor_address = injected_address

        verification_retry_policy = _verification_retry_policy_from_env(env)

        return cls(
            operator_address=operator_address,
            operator_private_key=operator_private_key,
            node_http=node_http,
            node_ws=node_ws,
            executor_address=executor_address,
            executor_owner=executor_owner,
            inject_executor_code=inject_executor_code,
            injected_address=injected_address,
            min_profit_net=_MIN_PROFIT_NET,
            fee_history_window=_FEE_HISTORY_WINDOW,
            fee_percentiles=_FEE_PERCENTILES,
            target_profit_ratio=_TARGET_PROFIT_RATIO,
            blocks_before_nonce_expires=_BLOCKS_BEFORE_NONCE_EXPIRES,
            max_simulate_concurrent=_MAX_SIMULATE_CONCURRENT,
            age_decay_constant=_AGE_DECAY_CONSTANT,
            min_priority_fee_percentile=_MIN_PRIORITY_FEE_PERCENTILE,
            max_priority_fee_percentile=_MAX_PRIORITY_FEE_PERCENTILE,
            path_suppress_threshold=_PATH_SUPPRESS_THRESHOLD,
            path_suppress_retry_interval=_PATH_SUPPRESS_RETRY_INTERVAL,
            allowed_intermediate_tokens=_ALLOWED_INTERMEDIATE_TOKENS,
            permutation_filter=(frozenset({permutation}) if permutation is not None else None),
            dry_run=not live,
            verification_retry_policy=verification_retry_policy,
        )


# ──────────────────────────────────────────────────────────────────
# Simulation revert taxonomy
# ──────────────────────────────────────────────────────────────────

# Selector → human name for the revert selectors the cmd_executor / V4
# PoolManager emit. Kept as canonical data so both the (verbose) per-fail
# diagnostic decode in the driver and the (short) bucket label produced by
# ``classify_revert`` stay in sync.
_V4_REVERT_SELECTORS: dict[str, str] = {
    "5212cba1": "CurrencyNotSettled()",
    "486aa307": "PoolNotInitialized()",
    "1e048e1d": "InvalidHookResponse()",
    "a3603d66": "SwapQuantityCannotBeZero()",
    "38606b01": "PriceLimitAlreadyExceeded()",
    "30d6072a": "PriceLimitOutOfBounds()",
    "a40afa38": "LockFailure()",
    "5090d6c6": "AlreadyUnlocked()",
    "54e3ca0d": "ManagerLocked()",
}

_EXECUTOR_REVERT_SELECTORS: dict[str, str] = {
    # Legacy (bare assert)
    "4b9dfc58": "!OWNER",
    "49494100": "IIA(insufficient-input-amount)",
    # Custom errors (Vyper 0.5.0a3+)
    "8e4a23d6": "Unauthorized(caller)",
    "b028a63a": "InvalidCallback(caller)",
    "cf479181": "InsufficientBalance(amount,available)",
    "4e88422a": "InsufficientProfit(actual,expected)",
    "83276224": "InvalidCommand(opcode)",
    "60ef0bb0": "BipsTooHigh(bips)",
    "a61be9f0": "InvalidMsgValue(value)",
    "e5b6bf32": "NotPlainEthTransfer()",
}

# Solidity revert selectors shared across all contracts.
_ERROR_STRING_SELECTOR = "08c379a0"  # Error(string)
_PANIC_SELECTOR = "4e487b71"  # Panic(uint256)

# Hex-string layout constants for revert return-data (bytes are hex-encoded,
# so one byte = two chars). Used by ``classify_revert`` below.
_HEX_SELECTOR_LEN = 8  # 4-byte function selector
_HEX_WORD_LEN = 64  # one 32-byte word
_HEX_PANIC_ARG_END = _HEX_SELECTOR_LEN + _HEX_WORD_LEN  # after Panic's uint256 arg


def classify_revert(revert_data: bytes) -> str:
    """Classify raw simulation revert return-data into a short stable label.

    Used by the ``[sim]`` summary to break the ``N failed`` bucket down by root
    cause. Returns the canonical error *name* for custom-error selectors (params
    dropped, so ``InsufficientProfit(1,2)`` and ``InsufficientProfit(3,4)``
    tally together), the decoded message for ``Error(string)``, the panic code
    for ``Panic``, or ``unknown:0x........`` for anything unrecognised.

    Deliberately never raises — a taxonomy must classify every revert, even
    malformed ones, so the summary always adds up.

    Returns:
        A short stable label for the revert (error name, decoded message,
        panic code, or ``unknown:0x<selector>``).

    """
    if not revert_data:
        return "empty"
    hexed = revert_data.hex()
    if len(hexed) < _HEX_SELECTOR_LEN:
        return f"short:{hexed}"
    selector = hexed[:_HEX_SELECTOR_LEN]
    if selector == _PANIC_SELECTOR:
        # Panic(uint256 code) — code is the first 32-byte arg.
        code = (
            int(hexed[_HEX_SELECTOR_LEN:_HEX_PANIC_ARG_END], 16)
            if len(hexed) >= _HEX_PANIC_ARG_END
            else 0
        )
        return f"Panic(0x{code:x})"
    if selector == _ERROR_STRING_SELECTOR:
        # Error(string): [sel][offset:32][len:32][data:N]
        try:
            str_len = int(hexed[8 + 64 : 8 + 128], 16)
            str_start = 8 + 64 + 64
            msg = bytes.fromhex(hexed[str_start : str_start + str_len * 2]).decode(
                "utf-8", errors="replace"
            )
        except (ValueError, IndexError):
            return "Error(string:undecodable)"
        return msg or "Error(string:empty)"
    if selector in _V4_REVERT_SELECTORS:
        return _V4_REVERT_SELECTORS[selector].split("(", 1)[0]
    if selector in _EXECUTOR_REVERT_SELECTORS:
        return _EXECUTOR_REVERT_SELECTORS[selector].split("(", 1)[0]
    # Bare 32-byte numeric revert (Vyper): 0x00..00<value>
    if len(hexed) >= _HEX_WORD_LEN and hexed[:24] == "0" * 24:
        return "numeric-revert"
    return f"unknown:0x{selector}"


def format_failure_breakdown(buckets: dict[str, int]) -> str:
    """Render a ``name=count`` breakdown, highest count first (name breaks ties).

    Returns ``""`` for an empty tally so the caller can skip the suffix when no
    failures were classified.

    Returns:
        ``"name=count name=count…"`` ordered by descending count, or ``""``.

    """
    if not buckets:
        return ""
    ordered = sorted(buckets.items(), key=lambda kv: (-kv[1], kv[0]))
    return " ".join(f"{name}={count}" for name, count in ordered)


# Basis points denominator (10_000 = 100%).
BPS_DENOM = 10_000

# A result row from the Rust engine: (path_id, opt_input, profit, hop_outputs,
# consumed_inputs, solve_block). The filter inspects opt_input + profit only.
EngineResult = tuple[int, int, int, tuple[int, ...], tuple[int, ...], int]


def filter_thin_margin_results(
    results: list[EngineResult],
    min_profit_margin_bps: int,
) -> tuple[list[EngineResult], int]:
    """Drop solver results whose gross-profit margin is too low.

    S1 found that the dominant IIA reverts in V3/V4-heavy perms are razor-thin
    arb (gross profit ≈ $0.001 on ≈ $0.06-$1.30 input = sub-0.2 bps margin)
    that cannot survive 1-block drift. The chain has already arbitraged these
    away by the time the sim runs. Filtering them pre-sim saves an RPC + a
    revert per attempt and keeps them out of the TSV ``Reverts`` column.

    ``min_profit_margin_bps`` is in basis points of ``optimal_input``
    (e.g. ``50`` = 0.5% — a result with profit < 0.5% of its input is dropped).
    ``0`` disables the filter (keeps all — backwards-compatible default).

    Returns ``(kept, dropped_count)``. A dropped result is a genuine
    above-zero-profit path the solver found; dropping it trades a missed
    edge case for a cleaner revert signal + saved sim budget.

    Returns:
        A ``(kept_results, dropped_count)`` tuple; ``kept`` is a new list.

    """
    if min_profit_margin_bps <= 0 or not results:
        return list(results), 0
    threshold_num = min_profit_margin_bps
    kept: list[EngineResult] = []
    dropped = 0
    for row in results:
        _path_id, opt_input, profit, _ho, _ci, _sb = row
        # profit ≥ opt_input * bps / BPS_DENOM  ⟺  profit * BPS_DENOM ≥ opt_input * bps
        # (integer math — no float rounding.)
        if opt_input > 0 and profit * BPS_DENOM >= opt_input * threshold_num:
            kept.append(row)
        elif opt_input == 0:
            # No input basis to ratio against — keep (the solver's profit is
            # absolute; a filter can't ratio it). Rare for arb paths.
            kept.append(row)
        else:
            dropped += 1
    return kept, dropped


def format_sim_diag_line(
    failure: dict[str, object],
    *,
    path_id: int,
    path_type: str,
    solve_block: int,
    block: int,
    age: int,
) -> str:
    """Render one always-on ``[sim-diag]`` JSON line per reverted candidate.

    Ergo epic 63I7WJ (task AM5AJW): re-pointed at the inspector's captured
    swap amounts (the ACTUAL amounts the in-process EVM emitted) vs the
    solver's reported ``hop_outputs`` (the EXPECTED amounts). No
    ``fetch_onchain``, no ``recompute`` — the captured swaps ARE the ground
    truth (proven byte-exact against mainnet receipts by the
    ``swap_capture_correctness`` probe).

    The line is one compact, machine-parseable JSON object (``json.loads`` on
    the text after the ``[sim-diag] `` prefix) carrying: ``path_id``,
    ``path_type``, ``solve_block``, ``block``, ``age``, ``revert_info``
    (the reverting-frame label — the ``failure["bucket"]``), ``optimal_input``
    (the solver's expected input), ``hop_outputs`` (the solver's expected
    per-hop outputs), and ``captured_swaps`` (the inspector-captured actual
    per-swap amounts). ``logs/permutation_analyzer.py::classify_candidate``
    compares ``hop_outputs[i]`` vs the i-th captured swap's output amount to
    classify SolverCalc / Encoding / Unknown. Never raises — a malformed
    failure emits a best-effort line with the fields it has, so emission never
    blocks the revert path.

    Returns:
        The full ``[sim-diag] ``-prefixed JSON line string.

    """
    payload = {
        "path_id": path_id,
        "path_type": path_type,
        "solve_block": solve_block,
        "block": block,
        "age": age,
        "revert_info": failure.get("bucket", "") or "",
        "optimal_input": failure.get("optimal_input"),
        "hop_outputs": failure.get("hop_outputs", []),
        "captured_swaps": failure.get("captured_swaps", []),
    }
    return "[sim-diag] " + json.dumps(payload, default=str, separators=(",", ":"))
