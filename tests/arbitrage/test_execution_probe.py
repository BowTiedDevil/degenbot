from __future__ import annotations

from typing import Any

import pytest

from degenbot.abi import encode_single

from degenbot.runner.session import SessionPhase
from tests.helpers.session import session_for_test


class _Pool:
    def __init__(self, ratio: int) -> None:
        self.ratio = ratio

    def calculate_tokens_out(self, zero_for_one: bool, amount_in: int) -> int:
        assert isinstance(zero_for_one, bool)
        return amount_in * self.ratio


class _PyBot:
    def get_pool(self, pool_id: int) -> _Pool:
        return {12: _Pool(10), 13: _Pool(20), 14: _Pool(30)}[pool_id]


class _Bot:
    _py_bot = _PyBot()


class _Engine:
    def pool_id_for_pool(self, family: str, address: str) -> int:
        return {"v2": 12, "v3": 13}[family]

    def pool_id_for_v4_pool(self, manager: str, pool_id: str) -> int:
        return 14

    def payload_path_info(self, path_id: int) -> dict[str, Any]:
        assert path_id == 7
        return {
            "path_type": "V2-V3-V4",
            "hops": [
                {
                    "family": "v2",
                    "pool_address": "0x0000000000000000000000000000000000000002",
                    "zfo": True,
                },
                {
                    "family": "v3",
                    "pool_address": "0x0000000000000000000000000000000000000003",
                    "token0_address": "0x0000000000000000000000000000000000000010",
                    "token1_address": "0x0000000000000000000000000000000000000011",
                    "fee": 3000,
                    "zfo": False,
                },
                {
                    "family": "v4",
                    "pool_manager_address": "0x0000000000000000000000000000000000000004",
                    "pool_id_hex": "0x1234",
                    "currency0_address": "0x0000000000000000000000000000000000000011",
                    "currency1_address": "0x0000000000000000000000000000000000000012",
                    "fee": 3000,
                    "tick_spacing": 60,
                    "hook_address": "0x0000000000000000000000000000000000000000",
                    "zfo": True,
                },
            ],
        }


class _Registry:
    engine = _Engine()


class _Provider:
    def __init__(self) -> None:
        self.quotes = iter([20_000, 600_000])

    async def get_block(self, block: int) -> dict[str, int]:
        assert block == 12_345
        return {"base_fee_per_gas": 77, "timestamp": 1_700_000_001}

    async def call(self, target: str, data: bytes, block: int) -> bytes:
        assert block == 12_345
        return encode_single("uint256", next(self.quotes))


class _Pipeline:
    def __init__(self) -> None:
        self.kwargs = None

    async def probe_path(self, **kwargs: Any) -> dict[str, str]:
        self.kwargs = kwargs
        return {"outcome": "failed", "bucket": "InsufficientProfit"}


@pytest.mark.asyncio
async def test_probe_path_quotes_exact_inputs_and_uses_the_batch_executor() -> None:
    pipeline = _Pipeline()
    session = session_for_test(bot=_Bot(), engine_registry=_Registry(), async_w3=_Provider())
    session.phase = SessionPhase.RUNNING
    session.probe_pipeline = pipeline

    result = await session.probe_path(7, optimal_input=100)

    assert result == {"outcome": "failed", "bucket": "InsufficientProfit"}
    assert pipeline.kwargs == {
        "path_id": 7,
        "optimal_input": 100,
        "hop_outputs": [1000, 20_000, 600_000],
        "consumed_inputs": [100, 1000, 20_000],
        "current_block": 12_345,
        "base_fee_next": 77,
        "block_timestamp": 1_700_000_001,
    }


@pytest.mark.asyncio
async def test_zero_balance_probe_builds_flash_funded_pipeline_without_capital(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    pipeline = _Pipeline()
    session = session_for_test(bot=_Bot(), engine_registry=_Registry(), async_w3=_Provider())
    session.phase = SessionPhase.RUNNING
    calls = []

    async def build(inputs: Any, **kwargs: Any) -> _Pipeline:
        calls.append(kwargs)
        return pipeline

    monkeypatch.setattr("degenbot.runner.session.build_batch_executor", build)
    monkeypatch.setattr(
        "degenbot.runner.session.ExecutorInputs.from_session", lambda _session: object()
    )
    result = await session.probe_path(7, optimal_input=100, zero_balance=True)

    assert result == {"outcome": "failed", "bucket": "InsufficientProfit"}
    assert calls == [{"self_fund": False, "zero_balance": True}]
    assert session.probe_zero_balance is True
