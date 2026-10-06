"""Offline replay proof for the py_oracle oracle-answer corpus.

The fork-bound Uniswap parity tests' oracle truth is committed in two forms:
the per-test golden ints under ``tests/golden/data`` (what the parity tests'
replay asserts against) and, for the same surfaces, the per-block
``OfflineProvider`` corpus under ``tests/fixtures/chain_data``
(``py_oracle_<scenario>_block<N>.json``, recorded by the
``record_py_oracle_corpus`` example through the recording transport). This
module is the drift gate between the two, run fully offline: every recorded
answer must decode back to the exact golden int (or the exact recorded
revert) through the real ``OfflineProvider`` runtime.

The corpus is the oracle, not a re-derivation: nothing here recomputes swap
maths. It decodes wire answers and compares them to the committed golden
ints, so a mutated byte, a wrong pin, or a shrunken surface all fail loudly
- the seeded-divergence probes at the bottom make that failure mode a test.
"""

from __future__ import annotations

import json
import re
import socket
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING

import pytest

from degenbot.exceptions import ContractLogicError
from degenbot.provider import OfflineProvider
from tests.golden.oracle import GOLDEN_ROOT

if TYPE_CHECKING:
    from collections.abc import Callable, Iterator

REPO_ROOT = Path(__file__).resolve().parents[2]
CORPUS_ROOT = REPO_ROOT / "tests" / "fixtures" / "chain_data"

# The golden keys' pool pins (v3/v4 keys embed the pool, not the quoter the
# call targets; camelot keys embed the pool the call targets).
V3_POOL_ADDRESS = "0xCBCdF9626bC03E24f779434178A73a0B4bad62eD"
V4_POOL_ID = "21c67e77068de97969ba93d4aab21826d33ca12bb9f565d8496e8fda8a82ca27"
CAMELOT_POOL_ADDRESS = "0x84652bb2539513BAf36e225c930Fdd8eaa63CE27"

_V3_QUOTER = "b27308f9f90d607463bb33ea1bebb41c27ce5ab6"
_V4_QUOTER = "52f0e24d1c21c8a0cb1e5a5dd6198556bd9e1203"
_CAMELOT_POOL = "84652bb2539513baf36e225c930fdd8eaa63ce27"

_V3_INPUT_SELECTOR = "f7729d43"
_V3_OUTPUT_SELECTOR = "30d07f21"
_V4_INPUT_SELECTOR = "aa9d21cb"
_V4_OUTPUT_SELECTOR = "58733073"

_V3_SYMBOLS = {
    "2260fac5e5542a773aa44fbcfedf7c193bc2c599": "WBTC",
    "c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2": "WETH",
}
_V4_PAIRS = {True: "ETH->USDC", False: "USDC->ETH"}
_CAMELOT_PAIR = "USDC->WETH"

_CALL_KEY_RE = re.compile(r"0x[0-9a-f]{40}:0x[0-9a-f]+")
_CODE_KEY_RE = re.compile(r"0x[0-9a-f]{40}")
_HEX_RE = re.compile(r"[0-9a-f]+")

_OFFLINE_DIAL_MSG = "offline replay must not dial; a network attempt is a defect"

_PENDING_CORPUS_REASON = (
    "py_oracle corpus not recorded: no configured Arbitrum endpoint serves "
    "block 477785000 state (arb1.arbitrum.io: historical state unavailable; "
    "publicnode: archive requires a token). Record with the "
    "record_py_oracle_corpus example once one does."
)


def _words(data_hex: str) -> list[str]:
    """The 32-byte ABI words after a function selector."""
    body = data_hex[8:]
    return [body[i * 64 : (i + 1) * 64] for i in range(len(body) // 64)]


def _v3_golden_key(data_hex: str) -> str:
    words = _words(data_hex)
    method = {
        _V3_INPUT_SELECTOR: "quoteExactInputSingle",
        _V3_OUTPUT_SELECTOR: "quoteExactOutputSingle",
    }[data_hex[:8]]
    token_in = _V3_SYMBOLS[words[0][24:]]
    token_out = _V3_SYMBOLS[words[1][24:]]
    return f"{V3_POOL_ADDRESS}|{method}|{token_in}->{token_out}|{int(words[3], 16)}"


def _v4_golden_key(data_hex: str) -> str:
    words = _words(data_hex)
    method = {
        _V4_INPUT_SELECTOR: "quoteExactInputSingle",
        _V4_OUTPUT_SELECTOR: "quoteExactOutputSingle",
    }[data_hex[:8]]
    zero_for_one = int(words[6], 16) == 1
    return f"{V4_POOL_ID}|{method}|{_V4_PAIRS[zero_for_one]}|{int(words[7], 16)}"


def _camelot_golden_key(data_hex: str) -> str:
    words = _words(data_hex)
    return f"{CAMELOT_POOL_ADDRESS}|getAmountOut|{_CAMELOT_PAIR}|{int(words[0], 16)}"


@dataclass(frozen=True)
class Scenario:
    """One parity surface: its corpus file, its golden file, and the calldata
    -> golden-key decoder its oracle implies."""

    name: str
    chain_id: int
    block: int
    golden_path: Path
    corpus_path: Path
    called_address: str
    key_decoder: Callable[[str], str]


SCENARIOS = (
    Scenario(
        name="uniswap_v3_quoter",
        chain_id=1,
        block=24_407_242,
        golden_path=(
            GOLDEN_ROOT
            / "tests/uniswap/v3/test_uniswap_v3_onchain_parity"
            / "test_cached_calculations_v3_wbtc_weth.json"
        ),
        corpus_path=CORPUS_ROOT / "1" / "py_oracle_uniswap_v3_quoter_block24407242.json",
        called_address=_V3_QUOTER,
        key_decoder=_v3_golden_key,
    ),
    Scenario(
        name="uniswap_v4_quoter",
        chain_id=1,
        block=24_407_242,
        golden_path=(
            GOLDEN_ROOT
            / "tests/uniswap/v4/test_uniswap_v4_onchain_parity"
            / "test_cached_calculations_v4_eth_usdc.json"
        ),
        corpus_path=CORPUS_ROOT / "1" / "py_oracle_uniswap_v4_quoter_block24407242.json",
        called_address=_V4_QUOTER,
        key_decoder=_v4_golden_key,
    ),
    Scenario(
        name="camelot_v2_get_amount_out",
        chain_id=42161,
        block=477_785_000,
        golden_path=(
            GOLDEN_ROOT
            / "tests/uniswap/v2/test_camelot_v2_onchain_parity"
            / "test_create_camelot_v2_pool.json"
        ),
        corpus_path=(
            CORPUS_ROOT / "42161" / "py_oracle_camelot_v2_get_amount_out_block477785000.json"
        ),
        called_address=_CAMELOT_POOL,
        key_decoder=_camelot_golden_key,
    ),
)


def _refuse_connection(*_args: object, **_kwargs: object) -> None:
    raise AssertionError(_OFFLINE_DIAL_MSG)


@pytest.fixture(autouse=True)
def _replay_makes_no_network_calls(monkeypatch: pytest.MonkeyPatch) -> Iterator[None]:
    """Arm a hard dial-block: this module is replay-only, so any connection
    attempt means the offline contract broke."""
    monkeypatch.setattr(socket, "create_connection", _refuse_connection)
    monkeypatch.setattr(socket, "getaddrinfo", _refuse_connection)
    monkeypatch.setattr(socket.socket, "connect", _refuse_connection)
    monkeypatch.setattr(socket.socket, "connect_ex", _refuse_connection)


def _decoded_amount(result_hex: str) -> int:
    """The oracle int in a recorded answer: a bare uint256, or the first word
    of the V4 quoter's ``(uint256, uint256)`` pair."""
    return int(result_hex[:64], 16)


@pytest.mark.onchain_oracle
@pytest.mark.parametrize("scenario", SCENARIOS, ids=lambda s: s.name)
def test_corpus_decodes_to_the_parity_golden(scenario: Scenario) -> None:
    """Every corpus answer decodes to the exact golden int (or revert).

    Set-equality closes the drift a one-way lookup cannot see: a shrunken
    corpus or a stale golden must fail here, not silently pass."""
    if not scenario.corpus_path.exists():
        pytest.skip(_PENDING_CORPUS_REASON)
    golden = json.loads(scenario.golden_path.read_text())["entries"]
    recorded = json.loads(scenario.corpus_path.read_text())
    assert recorded["chain_id"] == scenario.chain_id
    assert recorded["block_number"] == scenario.block

    decoded: dict[str, str | None] = {}
    for call_key, result_hex in recorded["calls"].items():
        to, data_hex = call_key.split(":0x")
        assert to == f"0x{scenario.called_address}"
        decoded[scenario.key_decoder(data_hex)] = result_hex

    assert set(decoded) == set(golden), (
        f"stale={sorted(set(decoded) - set(golden))} missing={sorted(set(golden) - set(decoded))}"
    )
    for key, result_hex in decoded.items():
        entry = golden[key]
        if result_hex is None:
            assert entry.get("reverted") is True, f"{key}: recorded revert, golden holds a value"
        else:
            assert entry.get("reverted") is not True, f"{key}: golden revert, corpus holds a value"
            assert _decoded_amount(result_hex) == entry["value"], key


@pytest.mark.onchain_oracle
@pytest.mark.parametrize("scenario", SCENARIOS, ids=lambda s: s.name)
def test_offline_provider_replays_the_recorded_calls(scenario: Scenario) -> None:
    """The recorded answers replay through the real OfflineProvider runtime.

    Loads the corpus via ``from_json_file`` and drives every recorded call
    through the in-memory transport: values return the exact recorded bytes,
    recorded reverts raise :class:`ContractLogicError` — the same surface a
    live provider presents."""
    if not scenario.corpus_path.exists():
        pytest.skip(_PENDING_CORPUS_REASON)
    recorded = json.loads(scenario.corpus_path.read_text())
    provider = OfflineProvider.from_json_file(scenario.corpus_path)
    assert provider.chain_id == scenario.chain_id
    assert provider.block_number == scenario.block

    for call_key, result_hex in recorded["calls"].items():
        to, data_hex = call_key.split(":0x")
        calldata = bytes.fromhex(data_hex)
        if result_hex is None:
            with pytest.raises(ContractLogicError):
                provider.call(to, calldata, scenario.block)
        else:
            assert provider.call(to, calldata, scenario.block) == bytes.fromhex(result_hex)


@pytest.mark.onchain_oracle
@pytest.mark.parametrize("scenario", SCENARIOS, ids=lambda s: s.name)
def test_corpus_shape_matches_the_offline_provider_wire(scenario: Scenario) -> None:
    """The corpus file keeps the per-block OfflineProvider wire shape.

    Exactly the five established keys; call keys are ``0x<to>:0x<data>`` with
    lowercase hex and no ``0x`` on the recorded results; the code map holds
    the called contract(s)."""
    if not scenario.corpus_path.exists():
        pytest.skip(_PENDING_CORPUS_REASON)
    recorded = json.loads(scenario.corpus_path.read_text())
    assert set(recorded) == {"chain_id", "block_number", "timestamp", "calls", "code"}
    assert isinstance(recorded["timestamp"], int)
    assert recorded["timestamp"] > 0
    call_targets = {key.split(":")[0] for key in recorded["calls"]}
    assert f"0x{scenario.called_address}" in call_targets
    for key, value in recorded["calls"].items():
        assert _CALL_KEY_RE.fullmatch(key), key
        assert value is None or _HEX_RE.fullmatch(value), key
    assert recorded["code"], "the called contract's runtime code should be recorded"
    for addr, code_hex in recorded["code"].items():
        assert _CODE_KEY_RE.fullmatch(addr), addr
        assert _HEX_RE.fullmatch(code_hex), addr


@pytest.mark.onchain_oracle
def test_same_block_corpus_files_agree_on_the_block_timestamp() -> None:
    """Corpus files pinned to one block record one timestamp."""
    stamps = {
        scenario.name: json.loads(scenario.corpus_path.read_text())["timestamp"]
        for scenario in SCENARIOS
        if scenario.corpus_path.exists() and scenario.chain_id == 1
    }
    assert stamps, "no ethereum corpus recorded"
    assert len(set(stamps.values())) == 1, f"corpus files disagree on the pin: {stamps}"


def _mutated_copy(
    scenario: Scenario,
    tmp_path: Path,
    mutate: Callable[[dict], str],
) -> tuple[Path, str]:
    recorded = json.loads(scenario.corpus_path.read_text())
    key = mutate(recorded)
    path = tmp_path / f"mutated_{scenario.name}.json"
    path.write_text(json.dumps(recorded))
    return path, key


@pytest.mark.onchain_oracle
def test_seeded_answer_divergence_breaks_the_golden_agreement(tmp_path: Path) -> None:
    """Negative probe (value side): one flipped answer digit must break the
    decode agreement — a comparison that cannot fail is not a test."""
    scenario = SCENARIOS[0]
    if not scenario.corpus_path.exists():
        pytest.skip(_PENDING_CORPUS_REASON)

    def _flip_one_answer(recorded: dict) -> str:
        key = next(k for k, v in recorded["calls"].items() if v is not None)
        original = recorded["calls"][key]
        flipped = original[:-1] + ("0" if original[-1] != "0" else "1")
        assert flipped != original
        recorded["calls"][key] = flipped
        return key

    path, key = _mutated_copy(scenario, tmp_path, _flip_one_answer)
    provider = OfflineProvider.from_json_file(path)
    to, data_hex = key.split(":0x")
    answer = provider.call(to, bytes.fromhex(data_hex), scenario.block)
    golden = json.loads(scenario.golden_path.read_text())["entries"]
    golden_key = scenario.key_decoder(data_hex)
    assert _decoded_amount(answer.hex()) != golden[golden_key]["value"]


@pytest.mark.onchain_oracle
def test_seeded_revert_divergence_breaks_the_revert_agreement(tmp_path: Path) -> None:
    """Negative probe (revert side): a recorded revert replaced by a bogus
    value must change the replay behaviour and desynchronize from the golden
    revert entry."""
    scenario = SCENARIOS[1]
    if not scenario.corpus_path.exists():
        pytest.skip(_PENDING_CORPUS_REASON)

    def _fill_one_revert(recorded: dict) -> str:
        key = next(k for k, v in recorded["calls"].items() if v is None)
        recorded["calls"][key] = "ab" * 32
        return key

    path, key = _mutated_copy(scenario, tmp_path, _fill_one_revert)
    provider = OfflineProvider.from_json_file(path)
    to, data_hex = key.split(":0x")
    # A replay-time value where record time reverted: the call now returns
    # instead of raising, so any consumer branching on the revert skips on
    # the wrong path — the desync the drift gate exists to catch.
    answer = provider.call(to, bytes.fromhex(data_hex), scenario.block)
    assert answer == bytes.fromhex("ab" * 32)
    golden = json.loads(scenario.golden_path.read_text())["entries"]
    assert golden[scenario.key_decoder(data_hex)].get("reverted") is True
