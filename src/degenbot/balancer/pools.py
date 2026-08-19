"""Balancer V2 weighted pool implementation."""

from __future__ import annotations

from fractions import Fraction
from typing import TYPE_CHECKING, Any, ClassVar, Self

from degenbot.balancer.libraries.constants import PowVersion
from degenbot.balancer.libraries.scaling_helpers import (
    _compute_scaling_factor,
    _downscale_down,
    _downscale_up,
    _upscale,
    _upscale_array,
)
from degenbot.balancer.math import (
    weighted_add_swap_fee_amount as _rs_add_swap_fee_amount,
)
from degenbot.balancer.math import (
    weighted_calc_in_given_out as _rs_calc_in_given_out,
)
from degenbot.balancer.math import (
    weighted_calc_out_given_in as _rs_calc_out_given_in,
)
from degenbot.balancer.math import (
    weighted_subtract_swap_fee_amount as _rs_subtract_swap_fee_amount,
)
from degenbot.balancer.types import (
    BalancerV2PoolState,
    BalancerV2WeightedPoolExternalUpdate,
)
from degenbot.builders.balancer_builder_base import BalancerBuilderBase
from degenbot.checksum_cache import get_checksum_address
from degenbot.erc20 import Erc20Token
from degenbot.exceptions import DegenbotValueError
from degenbot.types.abstract import AbstractLiquidityPool

if TYPE_CHECKING:
    from degenbot.types import LiquidityPool
    from degenbot.types.chain import ChecksummedAddress

# Hex representation of the TWO constant (2e18 = 0x1bc16d674ec80000)
# This value only appears in the bytecode of V2 WeightedPool contracts that include
# the fast-path optimizations for y == ONE / TWO / FOUR in powDown/powUp.
_V2_TWO_HEX = "1bc16d674ec80000"


def detect_pow_version(bytecode: str) -> PowVersion:
    """Detect which FixedPoint library version a pool contract uses from its bytecode.

    V2 (WeightedPool) contracts include fast paths for y == ONE, TWO, FOUR in
    powDown/powUp. These reference the TWO and FOUR constants, which are absent
    from V1 (WeightedPool2Tokens) bytecode.

    The detection works by checking for the TWO constant (0x1bc16d674ec80000)
    in the deployed bytecode. This constant is used in the `y == TWO` fast-path
    comparison and does not appear in V1 contracts.

    Returns:
        The computed value.

    """
    if _V2_TWO_HEX in bytecode:
        return PowVersion.V2
    return PowVersion.V1


class BalancerV2Pool(AbstractLiquidityPool):
    """BalancerV2Pool class."""

    variant: ClassVar[str | None] = "balancer_weighted"
    type PoolState = BalancerV2PoolState
    FEE_DENOMINATOR = 1 * 10**18

    # Instance attributes set in `_from_py_pool` (the only construction seam).
    _py_pool: LiquidityPool
    address: ChecksummedAddress
    pool_id: bytes
    pool_specialization: int
    vault: ChecksummedAddress
    _tokens: tuple[Erc20Token, ...]
    scaling_factors: tuple[int, ...]
    fee: Fraction
    weights: tuple[int, ...]
    pow_version: PowVersion

    def __init__(self, *args: Any, **kwargs: Any) -> None:  # ruff:ignore[unused-method-argument]
        """Direct construction is forbidden.

        ``BalancerV2Pool`` is a Python companion over a Rust-owned
        ``LiquidityPool`` handle. Use the registered entry points instead:

        - Production: ``Bot.build_pool(address)``
        - Tests: ``make_balancer_weighted_pool(...)``

        Both register the pool in Rust, obtain the ``LiquidityPool``
        handle, and wrap it via :meth:`_from_py_pool`.

        Raises:
            TypeError: Always. Direct construction is not supported.

        """
        msg = (
            f"{type(self).__name__} cannot be constructed directly. "
            "Use Bot.build_pool(address) (production) or "
            "make_balancer_weighted_pool(...) (tests) to register the pool "
            "in Rust and obtain the LiquidityPool handle to wrap."
        )
        raise TypeError(msg)

    @classmethod
    def _from_py_pool(cls, py_pool: LiquidityPool) -> Self:
        """Wrap a Rust-owned ``LiquidityPool`` handle as a Python companion.

        Internal seam (ADR-005, Polars-style ``_from_pydf`` pattern). Every
        identity field (vault, pool_id, tokens, weights, scaling_factors,
        swap_fee, pow_version, address) is read off the handle.

        Returns:
            A ``cls`` instance wrapping ``py_pool``.

        Raises:
            DegenbotValueError: If the handle is not a Balancer weighted pool
                or any token is not registered.

        """
        self = cls.__new__(cls)
        self._py_pool = py_pool

        if py_pool.pool_family != "balancer-weighted":
            msg = (
                "LiquidityPool handle is not a Balancer weighted pool "
                f"(got pool_family {py_pool.pool_family!r})"
            )
            raise DegenbotValueError(message=msg)

        self.address = get_checksum_address(py_pool.balancer_address)
        self.pool_id = bytes.fromhex(py_pool.balancer_pool_id_hex.removeprefix("0x"))
        self.pool_specialization = int.from_bytes(self.pool_id[20:22], byteorder="big")
        self.vault = get_checksum_address(py_pool.balancer_vault)

        py_tokens = py_pool.get_balancer_tokens()
        if py_tokens is None:
            msg = (
                "pool tokens must be registered in the same Bot as the pool "
                "(ADR-006): get_balancer_tokens returned None"
            )
            raise DegenbotValueError(message=msg)
        self._tokens = tuple(
            Erc20Token._from_py_token(t)  # ruff:ignore[private-member-access]
            for t in py_tokens
        )
        self.scaling_factors = tuple(_compute_scaling_factor(t) for t in self._tokens)
        self.fee = Fraction(py_pool.balancer_swap_fee, cls.FEE_DENOMINATOR)
        self.weights = tuple(py_pool.balancer_weights)
        self.pow_version = PowVersion.V1 if py_pool.balancer_pow_version == 1 else PowVersion.V2
        return self

    def __repr__(self) -> str:  # pragma: no cover
        """Return the canonical string representation.

        Returns:
            A string representation of the object.

        """
        return f"{self.__class__.__name__}(address={self.address}, tokens={len(self._tokens)})"

    def __str__(self) -> str:  # pragma: no cover
        """Return a human-readable string representation.

        Returns:
            A string representation of the object.

        """
        return f"{self.__class__.__name__} WeightedPool @ {self.address}"

    @property
    def balances(self) -> tuple[int, ...]:
        """Balances.

        Read from the Rust core via the ``LiquidityPool`` handle
        (ADR-005 slice 12b). Rust ``BotState`` is the single source of truth
        for the mutable ``balances`` slot; this getter returns the live tuple.
        """
        return tuple(self._py_pool.balancer_balances)

    @property
    def state(self) -> PoolState:
        """State.

        Built from one atomic Rust snapshot
        (``snapshot_balancer_weighted()`` — ``(balances, block)``) so
        callers see a coherent tuple (no torn read mid-``external_update``).
        Mirrors V3/V4's ``snapshot_v3()`` and Curve's ``snapshot_curve()``
        contract.

        Raises:
            DegenbotValueError: If the Rust snapshot is absent (the pool is
                not registered in Rust as a Balancer weighted pool —
                unreachable for a companion built over a registered handle).

        """
        snap = self._py_pool.snapshot_balancer_weighted()
        # snapshot_balancer_weighted returns None only for a non-Balancer-
        # weighted pool_id; this companion is always built over a registered
        # Balancer weighted handle, so the snapshot is always present.
        # Defensive: treat None as no-state.
        if snap is None:  # pragma: no cover - defensive, unreachable in practice
            msg = f"No Balancer weighted pool state available for {self.address}"
            raise DegenbotValueError(message=msg)
        balances, block = snap
        return BalancerV2PoolState(
            address=self.address,
            balances=tuple(balances),
            block=block,
        )

    @property
    def tokens(self) -> tuple[Erc20Token, ...]:
        """Tokens."""
        return self._tokens

    def calculate_tokens_out_from_tokens_in(
        self,
        token_in: Erc20Token,
        token_out: Erc20Token,
        token_in_quantity: int,
        override_state: PoolState | None = None,
    ) -> int:
        """Calculate tokens out from tokens in.

        Returns:
            The computed integer value.

        """
        token_in_index = self._tokens.index(token_in)
        token_out_index = self._tokens.index(token_out)

        fee_scaled = int(self.fee * self.FEE_DENOMINATOR)

        amount_new = _rs_subtract_swap_fee_amount(
            token_in_quantity,
            fee_scaled,
        )

        if override_state is not None:
            balances = list(override_state.balances)
        else:
            balances = list(self.balances)  # make a copy because _upscale_array will mutate it

        _upscale_array(amounts=balances, scaling_factors=self.scaling_factors)
        amount_new = _upscale(amount_new, scaling_factor=self.scaling_factors[token_in_index])

        amount_out = _rs_calc_out_given_in(
            int(balances[token_in_index]),
            self.weights[token_in_index],
            int(balances[token_out_index]),
            self.weights[token_out_index],
            int(amount_new),
            BalancerBuilderBase.pow_version_to_rust(self.pow_version),
        )

        return int(
            _downscale_down(
                amount=amount_out,
                scaling_factor=self.scaling_factors[token_out_index],
            ),
        )

    def calculate_tokens_in_from_tokens_out(
        self,
        token_in: Erc20Token,
        token_out: Erc20Token,
        token_out_quantity: int,
        override_state: PoolState | None = None,
    ) -> int:
        """Compute how many tokens must be sent to take `token_out_quantity` out.

        Returns:
            The computed integer value.

        """
        token_in_index = self._tokens.index(token_in)
        token_out_index = self._tokens.index(token_out)

        fee_scaled = int(self.fee * self.FEE_DENOMINATOR)

        if override_state is not None:
            balances = list(override_state.balances)
        else:
            balances = list(self.balances)  # make a copy because _upscale_array will mutate it

        _upscale_array(amounts=balances, scaling_factors=self.scaling_factors)
        amount_out_scaled = _upscale(
            token_out_quantity,
            scaling_factor=self.scaling_factors[token_out_index],
        )

        amount_in = _rs_calc_in_given_out(
            int(balances[token_in_index]),
            self.weights[token_in_index],
            int(balances[token_out_index]),
            self.weights[token_out_index],
            int(amount_out_scaled),
            BalancerBuilderBase.pow_version_to_rust(self.pow_version),
        )

        # Downscale first, then add fee — matching Solidity's onSwap GIVEN_OUT path
        amount_in_token = int(
            _downscale_up(
                amount=amount_in,
                scaling_factor=self.scaling_factors[token_in_index],
            ),
        )

        return _rs_add_swap_fee_amount(amount_in_token, fee_scaled)

    def external_update(
        self,
        update: BalancerV2WeightedPoolExternalUpdate,
    ) -> None:
        """Apply an external state update with new balances.

        Delegates to the Rust core
        (``LiquidityPool.apply_balancer_weighted_balance_update``) which
        journals the prior balances (genesis-anchor V2-style discipline) and
        lands the new balances + ``update_block`` atomically
        (ADR-005 slice 12b). The ``_state_lock`` + double-check-after-acquire
        pattern it used to apply is gone — Rust's internal write lock handles
        atomicity; the registration-state precondition is enforced by the
        Rust core's silent-no-op-on-older-block contract.

        Raises:
            DegenbotValueError: If the Rust core rejects the update (the pool
                is not registered as a Balancer weighted pool — unreachable
                for a companion built over a registered handle).

        """
        applied = self._py_pool.apply_balancer_weighted_balance_update(
            list(update.balances),
            update.block_number,
        )
        if not applied:  # pragma: no cover - defensive, unreachable for a weighted handle
            msg = (
                f"external_update rejected for {self.address} "
                "(not a Balancer weighted pool in Rust)"
            )
            raise DegenbotValueError(message=msg)
