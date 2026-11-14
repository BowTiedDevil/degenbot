"""Execute every three-hop permutation using production Rust calldata.

The optimized permutation suite proves the contract mechanics with its test
encoder. This suite closes the other half of the contract: the exact command
stream emitted by ``encode_cmd_3_hop`` must execute with its documented
production transfer shape and conserve every token.
"""

import itertools
import json
import shutil
import subprocess  # ruff: ignore[suspicious-subprocess-import] - executes only repository-owned helper binaries
from pathlib import Path

import pytest

from .conftest_shared import make_config, v2_get_amount_out
from .test_cmd_executor_three_hop_optimized import (
    AMOUNT_USDC,
    AMOUNT_WBTC,
    AMOUNT_WETH,
    AMOUNT_WETH_PROFIT,
    V2_FEE,
    _run_and_verify,
    _setup_v2_for_calc,
    _setup_v3,
    _setup_v4_pool,
)

pytestmark = pytest.mark.gas

# The recipe stages the `degenbot-fixture-tools` `encode_three_hop` binary at this
# path (`just build-executor-three-hop-encoder`), so the suite reads the same artifact
# name it read before the helper left `degenbot-executor`'s examples for the tool package.
ROOT = Path(__file__).resolve().parents[2]
ENCODER = ROOT / "rust/target/debug/examples/encode_three_hop"
FAMILIES = ("V2", "V3", "V4")
PERMUTATIONS = tuple(itertools.product(FAMILIES, repeat=3))
EXPECTED_TRANSFERS = {
    combo: (
        1
        if combo == ("V4", "V4", "V4")
        else 3
        if combo
        in {
            ("V2", "V4", "V4"),
            ("V3", "V4", "V4"),
            ("V4", "V3", "V4"),
            ("V4", "V4", "V2"),
            ("V4", "V4", "V3"),
        }
        else 4
    )
    for combo in PERMUTATIONS
}
# Default all-V2 is self-funded (the in-path-flash option is a separate axis),
# and the current V3-V2-V4 producer uses its forward-flash/final-repayment arm.
# Both are valid production shapes, one transfer above the alternative streams
# pinned by the contract-mechanics suite.
EXPECTED_TRANSFERS["V2", "V2", "V2"] = 5
EXPECTED_TRANSFERS["V3", "V2", "V4"] = 5


@pytest.fixture(scope="session", autouse=True)
def build_production_encoder():
    subprocess.run(  # ruff: ignore[subprocess-without-shell-equals-true] - fixed just recipe, no shell
        [shutil.which("just"), "build-executor-three-hop-encoder"],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    )
    assert ENCODER.is_file(), ENCODER


@pytest.fixture
def v2_matrix_pools(project, owner_account, weth, usdc, wbtc):
    pairs = ((weth, usdc), (usdc, wbtc), (wbtc, weth))
    return tuple(
        project.fake_uniswap_v2_pair.deploy(
            *sorted((a.address, b.address), key=str.lower), 0, V2_FEE, sender=owner_account
        )
        for a, b in pairs
    )


@pytest.fixture
def v3_matrix_pools(project, owner_account, weth, usdc, wbtc):
    pairs = ((weth, usdc), (usdc, wbtc), (wbtc, weth))
    return tuple(
        project.fake_uniswap_v3_pool.deploy(
            *sorted((a.address, b.address), key=str.lower), 0, 3000, sender=owner_account
        )
        for a, b in pairs
    )


def production_commands(payload):
    result = subprocess.run(  # ruff: ignore[subprocess-without-shell-equals-true] - fixed Rust helper, JSON fixture argv, no shell
        [str(ENCODER), json.dumps(payload)],
        cwd=ROOT,
        check=False,
        capture_output=True,
        text=True,
    )
    assert result.returncode == 0, (
        f"production encoder failed for {payload['hops']}:\n{result.stdout}\n{result.stderr}"
    )
    return bytes.fromhex(result.stdout.strip())


def request_document():
    """A request that decodes cleanly, so a failure test isolates the rule it drives."""
    return {
        "executor": "0x" + "11" * 20,
        "pool_manager": "0x" + "22" * 20,
        "weth": "0x" + "33" * 20,
        "optimal_input": 1_000,
        "outputs": [900, 800, 1_000],
        "consumed_inputs": [1_000, 900, 800],
        "hops": [
            {
                "family": "V2",
                "address": "0x" + "44" * 20,
                "token0": "0x" + "55" * 20,
                "token1": "0x" + "66" * 20,
                "fee": V2_FEE,
                "zfo": True,
                "tick_spacing": None,
            }
            for _ in range(3)
        ],
    }


def run_encoder(*arguments):
    return subprocess.run(  # ruff: ignore[subprocess-without-shell-equals-true] - fixed Rust helper, argv forwarded verbatim, no shell
        [str(ENCODER), *arguments],
        cwd=ROOT,
        check=False,
        capture_output=True,
        text=True,
    )


def test_encoder_rejects_a_missing_request_argument():
    result = run_encoder()

    assert result.returncode != 0
    assert not result.stdout
    assert "usage: encode_three_hop '<request-json>'" in result.stderr


def test_encoder_rejects_malformed_json():
    result = run_encoder("{")

    assert result.returncode != 0
    assert not result.stdout
    assert result.stderr.strip()


def test_encoder_rejects_a_hop_family_it_cannot_decode():
    payload = request_document()
    payload["hops"][1]["family"] = "V5"

    result = run_encoder(json.dumps(payload))

    assert result.returncode != 0
    assert not result.stdout
    assert "unsupported family V5" in result.stderr


def test_encoder_requires_tick_spacing_for_v4_hops():
    payload = request_document()
    payload["hops"][2]["family"] = "V4"

    result = run_encoder(json.dumps(payload))

    assert result.returncode != 0
    assert not result.stdout
    assert "V4 requires tick_spacing" in result.stderr


def test_encoder_rejects_a_v2_fee_above_u16():
    payload = request_document()
    payload["hops"][0]["fee"] = 70_000

    result = run_encoder(json.dumps(payload))

    assert result.returncode != 0
    assert not result.stdout


@pytest.mark.parametrize("combo", PERMUTATIONS, ids="-".join)
def test_production_calldata_executes_three_hop_permutation(
    combo,
    project,
    owner_account,
    weth,
    usdc,
    wbtc,
    executor,
    v4_pm,
    v2_matrix_pools,
    v3_matrix_pools,
):
    tokens = (weth, usdc, wbtc)
    desired_outputs = (AMOUNT_USDC, AMOUNT_WBTC, AMOUNT_WETH_PROFIT)
    amount_in = AMOUNT_WETH
    consumed_inputs = []
    outputs = []
    hops = []
    tracked_accounts = [executor]

    for index, (family, input_token, output_token, desired_output) in enumerate(
        zip(combo, tokens, tokens[1:] + tokens[:1], desired_outputs, strict=True)
    ):
        consumed_inputs.append(amount_in)
        if family == "V2":
            pool = v2_matrix_pools[index]
            zfo = _setup_v2_for_calc(
                pool, input_token, output_token, owner_account, amount_in, desired_output
            )
            amount_out = v2_get_amount_out(
                amount_in,
                input_token.balanceOf(pool),
                output_token.balanceOf(pool),
                V2_FEE,
            )
            token0, token1, fee, tick_spacing = pool.token0(), pool.token1(), V2_FEE, None
        elif family == "V3":
            pool = v3_matrix_pools[index]
            zfo, amount_out = _setup_v3(
                pool,
                input_token,
                output_token,
                amount_in,
                desired_output,
                owner_account,
            )
            token0, token1, fee, tick_spacing = pool.token0(), pool.token1(), 3000, None
        else:
            pool = v4_pm
            pool_key, zfo = _setup_v4_pool(
                v4_pm,
                input_token.address,
                output_token.address,
                amount_in,
                desired_output,
                owner_account,
                fee=3000,
                tick_spacing=60,
                output_token=output_token,
            )
            amount_out = desired_output
            token0, token1, fee, tick_spacing = (
                pool_key[0],
                pool_key[1],
                pool_key[2],
                pool_key[3],
            )
        tracked_accounts.append(pool)
        hops.append({
            "family": family,
            "address": pool.address,
            "token0": token0,
            "token1": token1,
            "fee": fee,
            "zfo": zfo,
            "tick_spacing": tick_spacing,
        })
        outputs.append(amount_out)
        amount_in = amount_out

    payload = {
        "executor": executor.address,
        "pool_manager": v4_pm.address,
        "weth": weth.address,
        "optimal_input": AMOUNT_WETH,
        "outputs": outputs,
        "consumed_inputs": consumed_inputs,
        "hops": hops,
    }
    commands = production_commands(payload)

    def run_production(_at, command_bytes, owner, *, profit_mode=1, **_kwargs):
        tx = executor.execute(
            command_bytes,
            make_config(profit_mode=profit_mode),
            sender=owner,
            raise_on_revert=False,
        )
        if tx.status == 0:
            tx.show_trace()
            msg = f"production-calldata {'-'.join(combo)} reverted"
            raise ValueError(msg)
        return tx

    label = f"production {'-'.join(combo)}"
    _run_and_verify(
        executor,
        run_production,
        None,
        commands,
        owner_account,
        list(tokens),
        tracked_accounts,
        EXPECTED_TRANSFERS[combo],
        label,
    )
