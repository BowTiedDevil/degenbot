from degenbot import next_base_fee


def test_fee_calcs():
    BASE_FEE = 100 * 10**9

    # EIP-1559 target is 50% full blocks, so a 50% full block should return the same base fee
    assert (
        next_base_fee(
            parent_base_fee=BASE_FEE,
            parent_gas_used=15_000_000,
            parent_gas_limit=30_000_000,
        )
        == BASE_FEE
    )

    # Fee should be higher
    assert (
        next_base_fee(
            parent_base_fee=BASE_FEE,
            parent_gas_used=20_000_000,
            parent_gas_limit=30_000_000,
        )
        == 104166666666
    )

    # Fee should be lower
    assert (
        next_base_fee(
            parent_base_fee=BASE_FEE,
            parent_gas_used=10_000_000,
            parent_gas_limit=30_000_000,
        )
        == 95833333334
    )

    MIN_BASE_FEE = 95 * 10**9

    # Enforce minimum fee
    assert (
        next_base_fee(
            parent_base_fee=BASE_FEE,
            parent_gas_used=0,
            parent_gas_limit=30_000_000,
            min_base_fee=MIN_BASE_FEE,
        )
        == MIN_BASE_FEE
    )
