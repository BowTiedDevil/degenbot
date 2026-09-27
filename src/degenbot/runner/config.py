"""Driver configuration for the settlement-arbitrage ``BotRunner``.

Extracted from ``examples/eth_backrun_helpers.py`` (epic 5TSYKN, task RVSYWB).
This module owns the Python-companion, ``stays-python`` surface that the
runtime driver (``BotRunner``) and its tests consume:

- :class:`ArbitrageConfig` — the unified frozen config value object (built from a
  the resolved verdict + CLI flags via :meth:`ArbitrageConfig.from_env`;
  the operator/executor identity it carries is read from the process
  environment).
- :func:`classify_revert` — the canonical simulation-revert labeler
  (public leaf; the dual-driver parity test imports it directly).

The display renderers (sim-diag / sim-fail / failure-breakdown) moved to
:mod:`degenbot.runner._render`, and the helpers that served only the deleted
legacy ``main()`` (``filter_thin_margin_results`` with its ``BPS_DENOM`` /
``EngineResult`` pair) were deleted (epic Y7PA5A, task 34XJ6C).
"""

import dataclasses
import os
from pathlib import Path
from typing import Any

from degenbot.arbitrage.verification_retry import VerificationRetryPolicy
from degenbot.checksum_cache import get_checksum_address
from degenbot.config import resolve_rpc_uris, resolved_config
from degenbot.constants import ZERO_ADDRESS as _ZERO_ADDRESS
from degenbot.runner.diag import DiagConfig

# Arbitrage configuration
# ──────────────────────────────────────────────────────────────────

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
# address, used when the process environment omits `OPERATOR_*` in non-live mode.
# The (now-eager) `TxSigner(key=operator_private_key, chain_id=1)` site
# rejects the former all-zero placeholder (zero is not a valid scalar) and
# raised `ValueError: signature error`. The Anvil account-0 key is a
# well-known valid throwaway that never signs in dry-run: the Rust submit
# leaf's `dry_run` guard skips `sign_eip1559` for every candidate.
_DRY_RUN_OPERATOR_PRIVATE_KEY = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"
_DRY_RUN_OPERATOR_ADDRESS = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266"

# Private keys the repository publishes. A live run must never sign with one:
# it would submit real transactions under a key anyone can read. Compared
# case-insensitively against the normalized (0x-prefixed) spelling.
_PLACEHOLDER_OPERATOR_PRIVATE_KEYS = frozenset({
    "0x" + "0" * 64,  # the former all-zero dry-run placeholder
    _DRY_RUN_OPERATOR_PRIVATE_KEY,  # the Anvil account-0 throwaway
})


def _declared(path: str) -> Any:
    """One declared key's resolved value, addressed by its dotted TOML path.

    The verdict is the driver's only configuration authority, so every
    ``DEGENBOT_*`` stance this module carries is read here rather than
    resolved a second time. The projection is schema-driven, so a key added to
    the core needs no edit in this file.

    Args:
        path: The declared key's dotted TOML path.

    Returns:
        The resolved value, in the Python type its declared kind names. The
        static type is deliberately loose: the projection is schema-driven, so
        a value's kind is a property of the declaration rather than of this
        accessor, and each caller narrows it where it consumes it.

    """
    return resolved_config().values[path]


def _verification_retry_policy() -> VerificationRetryPolicy:
    """The bounded verification retry policy, from the resolved verdict.

    The four ``verify.verify_retry_*`` keys are declared in the core schema
    (``VERIFICATION_RETRY_*`` in the env layer), so the operator file and the
    environment reach them through the one cascade and the declared default is
    the only fallback left. A value the schema cannot parse is refused by the
    loader at boot rather than here, which is the same fail-loud outcome at an
    earlier point.

    Returns:
        The resolved :class:`VerificationRetryPolicy`.

    """
    return VerificationRetryPolicy(
        max_attempts=int(_declared("verify.verify_retry_max_attempts")),
        base_delay=float(_declared("verify.verify_retry_base_delay")),
        max_delay=float(_declared("verify.verify_retry_max_delay")),
        jitter=float(_declared("verify.verify_retry_jitter")),
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
class RpcCascadeOverrides:
    """The :func:`degenbot.config.resolve_rpc_uris` inputs for :meth:`ArbitrageConfig.from_env`.

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
    Construct via :meth:`from_env`; the bridge onto ``main()`` lives in the
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
    allowed_intermediate_tokens: frozenset[str]
    permutation_filter: frozenset[str] | None
    # Bounded retry-with-backoff for transient verification RPC failures
    # (per-call transport / provider-init). Mismatch stays fatal.
    verification_retry_policy: VerificationRetryPolicy
    # The declared driver stances (the `dispatch.*` and `pathfinding.*` keys).
    # Each arrives from the resolved verdict, so the operator file and the
    # environment reach them through the one cascade `degenbot-config` owns.
    erc6909_profit: bool
    min_profit_margin_bps: int
    reg_progress_secs: float
    max_registered_paths: int
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
    def from_env(
        cls,
        *,
        live: bool,
        permutation: str | None,
        rpc: RpcCascadeOverrides | None = None,
    ) -> "ArbitrageConfig":
        """Build an ArbitrageConfig from the process environment + CLI flags + the verdict.

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

        inject_executor_code = bool(_declared("simulation.inject_executor_code"))
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

        verification_retry_policy = _verification_retry_policy()
        executor_runtime = os.environ.get("EXECUTOR_RUNTIME") or None
        # The incident probes' intervals come from the declared
        # `diagnostics.*` keys; zero is the declared default, so there is no
        # second "off" spelling here.
        diag = DiagConfig(
            tracemalloc_secs=float(_declared("diagnostics.tracemalloc_secs")),
            procmem_secs=float(_declared("diagnostics.procmem_secs")),
            procmem_csv=str(_declared("diagnostics.procmem_csv")),
            faulthandler_timeout_secs=float(_declared("diagnostics.faulthandler_timeout_secs")),
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
            allowed_intermediate_tokens=_ALLOWED_INTERMEDIATE_TOKENS,
            permutation_filter=(frozenset({permutation}) if permutation is not None else None),
            dry_run=not live,
            erc6909_profit=bool(_declared("dispatch.erc6909_profit")),
            min_profit_margin_bps=int(_declared("dispatch.min_profit_margin_bps")),
            reg_progress_secs=float(_declared("pathfinding.reg_progress_secs")),
            max_registered_paths=int(_declared("pathfinding.max_registered_paths")),
            verification_retry_policy=verification_retry_policy,
            executor_runtime=executor_runtime,
            diag=diag,
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
    return _classify_selector(hexed[:_HEX_SELECTOR_LEN], hexed)


def _classify_selector(selector: str, hexed: str) -> str:
    """Label one full revert payload from its leading 4-byte selector."""
    if selector == _PANIC_SELECTOR:
        return _decode_panic(hexed)
    if selector == _ERROR_STRING_SELECTOR:
        return _decode_error_string(hexed)
    named = _V4_REVERT_SELECTORS.get(selector) or _EXECUTOR_REVERT_SELECTORS.get(selector)
    if named is not None:
        return named.split("(", 1)[0]
    # Bare 32-byte numeric revert (Vyper): 0x00..00<value>
    if len(hexed) >= _HEX_WORD_LEN and hexed[:24] == "0" * 24:
        return "numeric-revert"
    return f"unknown:0x{selector}"


def _decode_panic(hexed: str) -> str:
    """Decode ``Panic(uint256)``'s code (0 when the arg is missing)."""
    # Panic(uint256 code) — code is the first 32-byte arg.
    code = (
        int(hexed[_HEX_SELECTOR_LEN:_HEX_PANIC_ARG_END], 16)
        if len(hexed) >= _HEX_PANIC_ARG_END
        else 0
    )
    return f"Panic(0x{code:x})"


def _decode_error_string(hexed: str) -> str:
    """Decode ``Error(string)``'s message, best-effort (never raises)."""
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


BPS_DENOM = 10_000

EngineResult = tuple[int, int, int, tuple[int, ...], tuple[int, ...], int]
