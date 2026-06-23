"""Frozen dataclasses + enum ordinals mirroring `IExecutor.sol`.

Field order, names, and wire types match the Solidity definitions verbatim;
reordering a field or relaxing the frozen contract silently breaks ABI
parity with the Executor.

Spec
----
- ``contracts/src/interfaces/IExecutor.sol``  (struct shapes)
- ``contracts/src/interfaces/IFlashLoanInterfaces.sol``  (``FlashProtocol``)
- ``coordinator/src/types/executor.ts``  (TS reference mirror)
"""

from __future__ import annotations

from dataclasses import dataclass
from enum import IntEnum

#: All-zero EVM address. Useful as a sentinel for unused address fields.
ZERO_ADDRESS: str = "0x" + "0" * 40


class FlashProtocol(IntEnum):
    """Flash-route selector mirrored from ``IFlashLoanInterfaces.sol``.

    The on-chain type is ``type FlashProtocol is uint8;`` — a uint8 newtype.
    The numeric ordinals here MUST match the Solidity enum.
    """

    AAVE_V3 = 0
    MORPHO = 1
    ERC3156 = 2
    UNI_V3 = 3
    UNI_V2 = 4
    UNI_V4 = 5


class DexKind(IntEnum):
    """DEX-implementation hint mirrored from the comment block in ``IExecutor.sol``.

    ``type DexKind is uint8;`` — a uint8 newtype. Used by Executor for
    approval + integrity checks on each swap leg.

    Ordinals are append-only; reordering breaks ABI parity with the Solidity
    contract and the TS / Rust mirrors. Phase E (2026-05) added ordinals
    7..10; Phase F.3 added 11..13 for standalone pool-direct promotions;
    the registry POC added 14..28 for protocol-specific router calls.
    PancakeStable / CurveTwoCrypto / CurveTriCrypto remain deferred — see
    ``docs/architecture/dex-kind-expansion.md``.
    """

    UNI_V2_STYLE = 0
    UNI_V3_POOL = 1
    UNI_V4_POOL_MANAGER = 2
    CURVE = 3
    RESERVED = 4
    AGGREGATOR_V6 = 5
    MORPHO_BLUE_ACTION = 6
    # Phase E additions (2026-05).
    ALGEBRA = 7
    SOLIDLY = 8
    CURVE_NG = 9
    BALANCER_V2 = 10
    # Phase F.3 additions (2026-05-12).
    MAVERICK_V2 = 11
    DODO_PMM = 12
    FLUID_DEX = 13
    # Registry POC additions (2026-05-13).
    BALANCER_V3 = 14
    KYBER_ELASTIC = 15
    LFJ_LIQUIDITY_BOOK = 16
    GMX_V2 = 17
    WOMBAT = 18
    BEBOP = 19
    HASHFLOW = 20
    WOOFI = 21
    OKX_DEX = 22
    ENSO = 23
    SQUID = 24
    LIFI = 25
    RANGO = 26
    RUBIC = 27
    NATIVE = 28


@dataclass(frozen=True, slots=True)
class SwapStep:
    """One leg of a swap chain. Field order matches ``IExecutor.SwapStep``."""

    #: Implementation hint used for approval + integrity checks.
    dex_kind: DexKind
    #: Target contract receiving the call. Aggregator routers must be in the
    #: on-chain whitelist (ADR-016).
    router: str
    #: Raw calldata forwarded as ``router.call(callData)``. ``0x``-prefixed hex.
    call_data: str
    #: Asset spent in this leg.
    token_in: str
    #: Asset received in this leg.
    token_out: str
    #: Amount of ``token_in`` to spend; ``0`` = use prior step's output.
    amount_in: int
    #: Minimum acceptable ``token_out`` delta; reverts on shortfall.
    amount_out_min: int


@dataclass(frozen=True, slots=True)
class NativeArbParams:
    """Params for ``executeNativeArb``. Field order matches ``IExecutor.NativeArbParams``."""

    flash_lender: str
    flash_protocol: FlashProtocol
    flash_token: str
    flash_amount: int
    swaps: tuple[SwapStep, ...]
    min_profit: int
    deadline: int


@dataclass(frozen=True, slots=True)
class MatchParams:
    """Params for ``matchInternal``. Field order matches ``IExecutor.MatchParams``.

    ``expected_token_inflows`` and ``expected_token_inflow_min`` are parallel
    arrays — the Executor checks per-token minimums after CoW + UniswapX
    settlement.

    ``settlement_funding_token`` / ``settlement_funding_max`` are the ADR-029
    settlement-funding commitment (audit 2026-06-09 hardening): the buyToken
    and MAXIMUM amount ``transferToSettlement`` may forward to COW_SETTLEMENT
    during this flow. Use ``(ZERO_ADDRESS, 0)`` when there is no CoW leg.

    The 12-attribute count is fixed by the on-chain struct shape; reducing it
    would break ABI parity with the Executor.
    """

    cow_settlement_calldata: str
    uniswapx_batch_calldata: str
    expected_token_inflows: tuple[str, ...]
    expected_token_inflow_min: tuple[int, ...]
    flash_lender: str
    flash_protocol: FlashProtocol
    flash_token: str
    flash_amount: int
    min_profit: int
    deadline: int
    settlement_funding_token: str
    settlement_funding_max: int


@dataclass(frozen=True, slots=True)
class ComposeParams:
    """Params for ``composeFourLeg``. Field order matches ``IExecutor.ComposeParams``.

    Sequence: Across destination fill, native arbitrage swaps, CoW fill,
    UniswapX rebalance — atomically backed by a single flash loan.

    ``settlement_funding_token`` / ``settlement_funding_max`` are the ADR-029
    settlement-funding commitment (audit 2026-06-09 hardening): the buyToken
    and MAXIMUM amount ``transferToSettlement`` may forward to COW_SETTLEMENT
    during Leg 3. Use ``(ZERO_ADDRESS, 0)`` when Leg 3 is empty.

    The 12-attribute count is fixed by the on-chain struct shape; reducing it
    would break ABI parity with the Executor.
    """

    across_fill_calldata: str
    arb_swaps: tuple[SwapStep, ...]
    cow_fill_calldata: str
    uniswapx_rebalance_calldata: str
    flash_lender: str
    flash_protocol: FlashProtocol
    flash_token: str
    flash_amount: int
    min_profit: int
    deadline: int
    settlement_funding_token: str
    settlement_funding_max: int
