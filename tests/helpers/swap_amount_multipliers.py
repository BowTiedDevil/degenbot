"""Swap-amount multiplier matrices for the pool swap-calculation tests.

Each multiplier is a fraction of the pool's max token reserve and determines
one swap amount as ``(multiplier * max_reserve)``. The matrices are distinct
value sets probing different reserve scales; they are preserved as found, not
unified:

- :data:`BALANCER_TOKEN_AMOUNT_MULTIPLIERS` sweeps a stable-pool range
  (``1e-7`` .. ``0.25``) where every step stays inside the amplification's
  sane band.
- :data:`UNISWAP_TOKEN_AMOUNT_MULTIPLIERS` sweeps the constant-product and
  concentrated-liquidity range (``1e-9`` .. ``0.1``).
- :data:`ONCHAIN_PARITY_TOKEN_AMOUNT_MULTIPLIERS` is the longest ladder
  (``1e-9`` .. ``0.75``) the on-chain parity loops sweep, so one ladder covers
  dust-to-most-of-the-pool on both a thin (USDbC) and a deep (cbETH/WETH) Base
  pool. It is NOT the Uniswap matrix with extras appended: the extra
  ``0.125``/``0.25``/``0.5``/``0.75`` steps exist so the parity comparison
  reaches the large-swap regime, where a rounding or fee-application drift
  between the Rust core and the on-chain quoter first shows up.

Folding a suite onto another's matrix changes the swap sizes it probes, so a
matrix moves here only when its value set is identical to an existing one.
"""

BALANCER_TOKEN_AMOUNT_MULTIPLIERS = [
    0.0000001,
    0.000001,
    0.00001,
    0.0001,
    0.001,
    0.01,
    0.1,
    0.125,
    0.25,
]

UNISWAP_TOKEN_AMOUNT_MULTIPLIERS = [
    0.000000001,
    0.00000001,
    0.0000001,
    0.000001,
    0.00001,
    0.0001,
    0.001,
    0.01,
    0.1,
]

ONCHAIN_PARITY_TOKEN_AMOUNT_MULTIPLIERS = [
    0.000000001,
    0.00000001,
    0.0000001,
    0.000001,
    0.00001,
    0.0001,
    0.001,
    0.01,
    0.1,
    0.125,
    0.25,
    0.5,
    0.75,
]
