"""
Test the local GHO discount rate calculation against the on-chain contract.

The GhoDiscountRateStrategy contract at 0x4C38Ec4D1D2068540DfC11DFa4de41F733DDF812
implements the discount rate calculation. This test verifies that the local Python
implementation matches the contract's calculation for various input combinations.
"""

import random

import pytest

from degenbot.checksum_cache import get_checksum_address
from degenbot.cli.aave import (
    DISCOUNT_RATE_BPS,
    MIN_DEBT_TOKEN_BALANCE,
    MIN_DISCOUNT_TOKEN_BALANCE,
    calculate_gho_discount_rate,
)
from degenbot.functions import encode_function_calldata, raw_call

# Mainnet contract address for GhoDiscountRateStrategy
GHO_DISCOUNT_RATE_STRATEGY = get_checksum_address("0x4C38Ec4D1D2068540DfC11DFa4de41F733DDF812")


@pytest.mark.ethereum
def test_calculate_discount_rate_below_minimum_balances(fork_mainnet_archive):
    """
    Test that the discount rate is 0 when balances are below minimum thresholds.

    The contract returns 0 if:
    - discountTokenBalance < MIN_DISCOUNT_TOKEN_BALANCE (1e15)
    - debtBalance < MIN_DEBT_TOKEN_BALANCE (1e18)
    """
    w3 = fork_mainnet_archive.w3

    # Test below minimum debt balance
    debt_balance = MIN_DEBT_TOKEN_BALANCE - 1
    discount_token_balance = MIN_DISCOUNT_TOKEN_BALANCE + 10**18

    (contract_result,) = raw_call(
        w3=w3,
        address=GHO_DISCOUNT_RATE_STRATEGY,
        calldata=encode_function_calldata(
            function_prototype="calculateDiscountRate(uint256,uint256)",
            function_arguments=[debt_balance, discount_token_balance],
        ),
        return_types=["uint256"],
    )

    local_result = calculate_gho_discount_rate(debt_balance, discount_token_balance)

    assert contract_result == 0
    assert local_result == 0
    assert local_result == contract_result

    # Test below minimum discount token balance
    debt_balance = MIN_DEBT_TOKEN_BALANCE + 10**18
    discount_token_balance = MIN_DISCOUNT_TOKEN_BALANCE - 1

    (contract_result,) = raw_call(
        w3=w3,
        address=GHO_DISCOUNT_RATE_STRATEGY,
        calldata=encode_function_calldata(
            function_prototype="calculateDiscountRate(uint256,uint256)",
            function_arguments=[debt_balance, discount_token_balance],
        ),
        return_types=["uint256"],
    )

    local_result = calculate_gho_discount_rate(debt_balance, discount_token_balance)

    assert contract_result == 0
    assert local_result == 0
    assert local_result == contract_result


@pytest.mark.ethereum
def test_calculate_discount_rate_full_discount(fork_mainnet_archive):
    """
    Test that the maximum discount rate (DISCOUNT_RATE_BPS) is returned when
    the discounted balance covers the entire debt.

    This occurs when:
    discountedBalance = discountTokenBalance * GHO_DISCOUNTED_PER_DISCOUNT_TOKEN
    discountedBalance >= debtBalance
    """
    w3 = fork_mainnet_archive.w3

    # Setup: discount token balance is large enough to cover debt
    # GHO_DISCOUNTED_PER_DISCOUNT_TOKEN = 100e18
    # So if debt = 100e18 and discount_token = 1e18, discounted = 100e18 >= debt
    debt_balance = 100 * 10**18  # 100 GHO
    discount_token_balance = 2 * 10**18  # 2 stkAAVE - provides 200 GHO discount

    (contract_result,) = raw_call(
        w3=w3,
        address=GHO_DISCOUNT_RATE_STRATEGY,
        calldata=encode_function_calldata(
            function_prototype="calculateDiscountRate(uint256,uint256)",
            function_arguments=[debt_balance, discount_token_balance],
        ),
        return_types=["uint256"],
    )

    local_result = calculate_gho_discount_rate(debt_balance, discount_token_balance)

    assert contract_result == DISCOUNT_RATE_BPS  # 3000 = 30%
    assert local_result == DISCOUNT_RATE_BPS
    assert local_result == contract_result


@pytest.mark.ethereum
def test_calculate_discount_rate_partial_discount(fork_mainnet_archive):
    """
    Test that a proportional discount rate is returned when the discounted balance
    covers only part of the debt.

    Formula: (discountedBalance * DISCOUNT_RATE) / debtBalance
    """
    w3 = fork_mainnet_archive.w3

    # Setup: discount token provides partial coverage
    # debt = 100e18, discount_token = 0.5e18
    # discounted = 0.5e18 * 100e18 = 50e18 (covers 50% of debt)
    # result = (50e18 * 3000) / 100e18 = 1500 (15% discount)
    debt_balance = 100 * 10**18
    discount_token_balance = 5 * 10**17  # 0.5 stkAAVE

    (contract_result,) = raw_call(
        w3=w3,
        address=GHO_DISCOUNT_RATE_STRATEGY,
        calldata=encode_function_calldata(
            function_prototype="calculateDiscountRate(uint256,uint256)",
            function_arguments=[debt_balance, discount_token_balance],
        ),
        return_types=["uint256"],
    )

    local_result = calculate_gho_discount_rate(debt_balance, discount_token_balance)

    assert local_result == contract_result
    # Verify it's a partial discount (between 0 and DISCOUNT_RATE_BPS)
    assert 0 < local_result < DISCOUNT_RATE_BPS


@pytest.mark.ethereum
def test_calculate_discount_rate_edge_cases(fork_mainnet_archive):
    """
    Test edge cases including exact minimum balances and proportional calculations.
    """
    w3 = fork_mainnet_archive.w3

    test_cases = [
        # (debt_balance, discount_token_balance, description)
        (MIN_DEBT_TOKEN_BALANCE, MIN_DISCOUNT_TOKEN_BALANCE, "exact minimum balances"),
        (
            10 * MIN_DEBT_TOKEN_BALANCE,
            MIN_DISCOUNT_TOKEN_BALANCE,
            "10x min debt, min discount token",
        ),
        (
            MIN_DEBT_TOKEN_BALANCE,
            10 * MIN_DISCOUNT_TOKEN_BALANCE,
            "min debt, 10x min discount token",
        ),
        (10**22, 10**19, "large debt, moderate discount token"),
        (10**20, 10**18, "moderate debt, small discount token"),
    ]

    for debt_balance, discount_token_balance, description in test_cases:
        (contract_result,) = raw_call(
            w3=w3,
            address=GHO_DISCOUNT_RATE_STRATEGY,
            calldata=encode_function_calldata(
                function_prototype="calculateDiscountRate(uint256,uint256)",
                function_arguments=[debt_balance, discount_token_balance],
            ),
            return_types=["uint256"],
        )

        local_result = calculate_gho_discount_rate(debt_balance, discount_token_balance)

        assert local_result == contract_result, (
            f"Mismatch for {description}: "
            f"debt={debt_balance}, discount_token={discount_token_balance}, "
            f"contract={contract_result}, local={local_result}"
        )


@pytest.mark.ethereum
def test_calculate_discount_rate_exact_coverage_threshold(fork_mainnet_archive):
    """
    Test the exact threshold where discounted balance equals debt.

    At this point, discount rate should transition from partial to full.
    """
    w3 = fork_mainnet_archive.w3

    # Find the exact point where discounted_balance == debt_balance
    # discounted_balance = discount_token_balance * GHO_DISCOUNTED_PER_DISCOUNT_TOKEN / WAD
    # We want: discount_token_balance * 100e18 = debt_balance * 1e18
    # So: discount_token_balance = debt_balance / 100

    debt_balance = 1000 * 10**18  # 1000 GHO
    # Exact amount needed for full coverage
    exact_discount_token = debt_balance // 100

    (contract_result,) = raw_call(
        w3=w3,
        address=GHO_DISCOUNT_RATE_STRATEGY,
        calldata=encode_function_calldata(
            function_prototype="calculateDiscountRate(uint256,uint256)",
            function_arguments=[debt_balance, exact_discount_token],
        ),
        return_types=["uint256"],
    )

    local_result = calculate_gho_discount_rate(debt_balance, exact_discount_token)

    assert local_result == contract_result
    # Should be at or near full discount rate
    assert local_result <= DISCOUNT_RATE_BPS


@pytest.mark.ethereum
def test_calculate_discount_rate_fuzz_random_values(fork_mainnet_archive):
    """
    Test with random valid values to ensure consistent behavior.
    """
    w3 = fork_mainnet_archive.w3
    random.seed(42)  # Reproducible test

    for _ in range(20):
        # Generate random values above minimum thresholds
        debt_balance = random.randint(MIN_DEBT_TOKEN_BALANCE, 10**25)
        discount_token_balance = random.randint(MIN_DISCOUNT_TOKEN_BALANCE, 10**22)

        (contract_result,) = raw_call(
            w3=w3,
            address=GHO_DISCOUNT_RATE_STRATEGY,
            calldata=encode_function_calldata(
                function_prototype="calculateDiscountRate(uint256,uint256)",
                function_arguments=[debt_balance, discount_token_balance],
            ),
            return_types=["uint256"],
        )

        local_result = calculate_gho_discount_rate(debt_balance, discount_token_balance)

        assert local_result == contract_result, (
            f"Mismatch: debt={debt_balance}, discount_token={discount_token_balance}, "
            f"contract={contract_result}, local={local_result}"
        )
