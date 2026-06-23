"""JSON wire-format deserializers for cross-language fixtures.

The on-disk wire format mirrors what ``coordinator/src/types/fixtures.gen.ts``
emits. Two invariants govern decoding:

1. ``bigint`` arrives as a *plain decimal string* (no ``{__bi}`` envelope —
   that envelope is TS-internal IPC only). We round-trip with ``int(s)``.
2. Field keys are camelCase (TS convention). Each parser maps camelCase to
   the matching frozen-dataclass snake_case attribute, leaving Solidity
   field order intact.

If a parser is asked for a key that does not exist on the wire payload, it
raises ``KeyError`` — schema violations should fail loudly rather than
silently coerce to a default.

Spec
----
- ``coordinator/src/types/README.md``  (cross-language wire-format invariants)
- ``coordinator/src/types/fixtures.gen.ts``  (canonical emitter)
"""

from __future__ import annotations

import json
from collections.abc import Sequence  # noqa: TC003 - Pydantic resolves this at model rebuild.
from typing import Any

from pydantic import BaseModel, ConfigDict, Field

from degenbot.types_solver.executor import (
    ComposeParams,
    DexKind,
    FlashProtocol,
    MatchParams,
    NativeArbParams,
    SwapStep,
)


class MorphoMarketParams(BaseModel):
    model_config = ConfigDict(frozen=True, populate_by_name=True)
    loan_token: str = Field(alias="loanToken")
    collateral_token: str = Field(alias="collateralToken")
    oracle: str
    irm: str
    lltv: int


class MorphoLiquidationOpportunityPayload(BaseModel):
    model_config = ConfigDict(frozen=True, populate_by_name=True)
    market_id: str = Field(alias="marketId")
    market_params: MorphoMarketParams = Field(alias="marketParams")
    borrower: str
    repaid_shares: int = Field(alias="repaidShares")
    expected_seized_assets: int = Field(alias="expectedSeizedAssets")
    ranking_score_bps: int = Field(alias="rankingScoreBps")
    risk_cost_wei: int = Field(alias="riskCostWei")
    bad_debt_mode: str = Field(alias="badDebtMode")


class EngineSwapStep(BaseModel):
    """Swap step mirroring the monitor's wire format."""

    model_config = ConfigDict(frozen=True, populate_by_name=True)
    pool: str
    token_in: str = Field(alias="tokenIn")
    token_out: str = Field(alias="tokenOut")
    amount_in: int = Field(alias="amountIn")
    amount_out_min: int = Field(alias="amountOutMin")
    zero_for_one: bool = Field(alias="zeroForOne")
    dex: str


class Opportunity(BaseModel):
    """Coordinator/engine opportunity envelope consumed by strategy routing.

    This is intentionally narrower than the full IPC JSON envelope. It carries
    the fields currently used by the migrated TypeScript decision and native
    arbitrage strategy modules while preserving raw integer amounts.
    """

    model_config = ConfigDict(frozen=True, populate_by_name=True)

    id: str
    kind: str
    token_in: str = Field(alias="tokenIn")
    token_out: str = Field(alias="tokenOut")
    amount_in: int = Field(alias="amountIn")
    expected_amount_out: int = Field(default=0, alias="expectedAmountOut")
    estimated_profit_wei: int = Field(alias="estimatedProfitWei")
    flash_token: str = Field(alias="flashToken")
    flash_amount: int = Field(alias="flashAmount")
    path: Sequence[EngineSwapStep] = Field(default_factory=list)
    detected_at_ns: int = Field(default=0, alias="detectedAtNs")
    pool_addresses: Sequence[str] = Field(default_factory=list, alias="poolAddresses")
    enrichment: dict[str, Any] | None = None
    morpho_liquidation: MorphoLiquidationOpportunityPayload | None = Field(
        default=None, alias="morphoLiquidation"
    )


Opportunity.model_rebuild()


def from_wire_json(s: str) -> dict[str, Any]:
    """Parse a fixtures-format JSON document.

    BigInt values arrive as decimal strings, addresses as 0x-strings; the
    parser performs no additional coercion at this layer — typed wrappers
    are constructed by the per-struct ``*_from_wire`` helpers below.
    """
    data: Any = json.loads(s)
    if not isinstance(data, dict):
        msg = f"expected JSON object at top level, got {type(data).__name__}"
        raise TypeError(msg)
    return data


def _swap_step_from_wire(d: dict[str, Any]) -> SwapStep:
    """Hydrate a single ``SwapStep`` from a camelCase wire dict."""
    raw_dex = d["dexKind"]
    if isinstance(raw_dex, str):
        # Support common string names from monitor
        dex_map = {
            "UniswapV2": DexKind.UNI_V2_STYLE,
            "UniswapV3": DexKind.UNI_V3_POOL,
            "UniswapV4": DexKind.UNI_V4_POOL_MANAGER,
        }
        if raw_dex in dex_map:
            kind = dex_map[raw_dex]
        else:
            try:
                kind = DexKind[raw_dex]
            except KeyError:
                kind = DexKind(int(raw_dex))
    else:
        kind = DexKind(int(raw_dex))

    return SwapStep(
        dex_kind=kind,
        router=str(d.get("router", d.get("pool"))),
        call_data=str(d["callData"]),
        token_in=str(d["tokenIn"]),
        token_out=str(d["tokenOut"]),
        amount_in=int(d["amountIn"]),
        amount_out_min=int(d["amountOutMin"]),
    )


def native_arb_from_wire(d: dict[str, Any]) -> NativeArbParams:
    """Hydrate ``NativeArbParams`` from a camelCase wire dict."""
    swaps_raw: list[dict[str, Any]] = list(d["swaps"])
    return NativeArbParams(
        flash_lender=str(d["flashLender"]),
        flash_protocol=FlashProtocol(int(d["flashProtocol"])),
        flash_token=str(d["flashToken"]),
        flash_amount=int(d["flashAmount"]),
        swaps=tuple(_swap_step_from_wire(s) for s in swaps_raw),
        min_profit=int(d["minProfit"]),
        deadline=int(d["deadline"]),
    )


def match_params_from_wire(d: dict[str, Any]) -> MatchParams:
    """Hydrate ``MatchParams`` from a camelCase wire dict."""
    inflows_raw: list[str] = list(d["expectedTokenInflows"])
    inflow_min_raw: list[Any] = list(d["expectedTokenInflowMin"])
    return MatchParams(
        cow_settlement_calldata=str(d["cowSettlementCalldata"]),
        uniswapx_batch_calldata=str(d["uniswapxBatchCalldata"]),
        expected_token_inflows=tuple(str(addr) for addr in inflows_raw),
        expected_token_inflow_min=tuple(int(x) for x in inflow_min_raw),
        flash_lender=str(d["flashLender"]),
        flash_protocol=FlashProtocol(int(d["flashProtocol"])),
        flash_token=str(d["flashToken"]),
        flash_amount=int(d["flashAmount"]),
        min_profit=int(d["minProfit"]),
        deadline=int(d["deadline"]),
        settlement_funding_token=str(d["settlementFundingToken"]),
        settlement_funding_max=int(d["settlementFundingMax"]),
    )


def compose_params_from_wire(d: dict[str, Any]) -> ComposeParams:
    """Hydrate ``ComposeParams`` from a camelCase wire dict."""
    swaps_raw: list[dict[str, Any]] = list(d["arbSwaps"])
    return ComposeParams(
        across_fill_calldata=str(d["acrossFillCalldata"]),
        arb_swaps=tuple(_swap_step_from_wire(s) for s in swaps_raw),
        cow_fill_calldata=str(d["cowFillCalldata"]),
        uniswapx_rebalance_calldata=str(d["uniswapxRebalanceCalldata"]),
        flash_lender=str(d["flashLender"]),
        flash_protocol=FlashProtocol(int(d["flashProtocol"])),
        flash_token=str(d["flashToken"]),
        flash_amount=int(d["flashAmount"]),
        min_profit=int(d["minProfit"]),
        deadline=int(d["deadline"]),
        settlement_funding_token=str(d["settlementFundingToken"]),
        settlement_funding_max=int(d["settlementFundingMax"]),
    )
