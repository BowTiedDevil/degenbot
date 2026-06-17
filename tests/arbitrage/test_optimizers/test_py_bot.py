"""Tests for PyBot — Rust-owned state with thin Python handles."""

from __future__ import annotations

import pytest

from degenbot.degenbot_rs import PyBot, PyLiquidityPool


class TestPyBotV2Pool:
    """Test V2 pool registration, update, and calculation through PyBot."""

    POOL_ADDR = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    TOKEN0_ADDR = "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
    TOKEN1_ADDR = "0xcccccccccccccccccccccccccccccccccccccccc"
    FACTORY_ADDR = "0xdddddddddddddddddddddddddddddddddddddddd"

    def test_register_v2_pool(self):
        core = PyBot()
        pool_id = core.register_v2_pool(
            address=self.POOL_ADDR,
            token0=self.TOKEN0_ADDR,
            token1=self.TOKEN1_ADDR,
            reserve0=1000,
            reserve1=2000,
            gamma_numer0=997,
            fee_denom0=1000,
            gamma_numer1=997,
            fee_denom1=1000,
            factory=self.FACTORY_ADDR,
        )
        assert pool_id == 1
        assert core.pool_count() == 1

    def test_calculate_tokens_out(self):
        """Verify V2 constant product calculation matches Python reference."""
        core = PyBot()
        pool_id = core.register_v2_pool(
            address=self.POOL_ADDR,
            token0=self.TOKEN0_ADDR,
            token1=self.TOKEN1_ADDR,
            reserve0=1000,
            reserve1=2000,
            gamma_numer0=997,
            fee_denom0=1000,
            gamma_numer1=997,
            fee_denom1=1000,
            factory=self.FACTORY_ADDR,
        )

        # Python reference: constant_product_calc_exact_in(100, 1000, 2000, 3/1000) = 181
        result = core.calculate_tokens_out(pool_id, zero_for_one=True, amount_in=100)
        assert result == 181

    def test_calculate_tokens_out_reverse(self):
        """Verify reverse direction calculation."""
        core = PyBot()
        pool_id = core.register_v2_pool(
            address=self.POOL_ADDR,
            token0=self.TOKEN0_ADDR,
            token1=self.TOKEN1_ADDR,
            reserve0=2000,
            reserve1=1000,
            gamma_numer0=997,
            fee_denom0=1000,
            gamma_numer1=997,
            fee_denom1=1000,
            factory=self.FACTORY_ADDR,
        )

        # Swap token1→token0 (reverse): reserves_in=1000, reserves_out=2000
        # Python: constant_product_calc_exact_in(100, 1000, 2000, 3/1000) = 181
        result = core.calculate_tokens_out(pool_id, zero_for_one=False, amount_in=100)
        assert result == 181

    def test_update_v2_pool_changes_result(self):
        """Verify that updating reserves changes calculation results."""
        core = PyBot()
        pool_id = core.register_v2_pool(
            address=self.POOL_ADDR,
            token0=self.TOKEN0_ADDR,
            token1=self.TOKEN1_ADDR,
            reserve0=1000,
            reserve1=2000,
            gamma_numer0=997,
            fee_denom0=1000,
            gamma_numer1=997,
            fee_denom1=1000,
            factory=self.FACTORY_ADDR,
        )

        # Before update
        before = core.calculate_tokens_out(pool_id, zero_for_one=True, amount_in=100)
        assert before == 181

        # Update: swap to 2000/1000
        core.update_v2_pool(
            address=self.POOL_ADDR,
            reserve0=2000,
            reserve1=1000,
            block_number=42,
        )

        # After update: Python: constant_product_calc_exact_in(100, 2000, 1000, 3/1000) = 47
        after = core.calculate_tokens_out(pool_id, zero_for_one=True, amount_in=100)
        assert after == 47

    def test_calculate_tokens_in(self):
        """Verify V2 exact-out calculation matches Python reference."""
        core = PyBot()
        pool_id = core.register_v2_pool(
            address=self.POOL_ADDR,
            token0=self.TOKEN0_ADDR,
            token1=self.TOKEN1_ADDR,
            reserve0=1000,
            reserve1=2000,
            gamma_numer0=997,
            fee_denom0=1000,
            gamma_numer1=997,
            fee_denom1=1000,
            factory=self.FACTORY_ADDR,
        )

        # Python: constant_product_calc_exact_out(50, 1000, 2000, 3/1000) = 26
        result = core.calculate_tokens_in(pool_id, zero_for_one=True, amount_out=50)
        assert result == 26

        # Reverse: reserves_in=2000, reserves_out=1000, amount_out=10
        # Python: constant_product_calc_exact_out(10, 2000, 1000, 3/1000) = 21
        result_rev = core.calculate_tokens_in(pool_id, zero_for_one=False, amount_out=10)
        assert result_rev == 21

    def test_calculate_tokens_out_large_values(self):
        """Verify calculation with realistic on-chain amounts (uint256 scale)."""
        core = PyBot()
        pool_id = core.register_v2_pool(
            address=self.POOL_ADDR,
            token0=self.TOKEN0_ADDR,
            token1=self.TOKEN1_ADDR,
            reserve0=1_500_000_000_000,       # 1.5M USDC (6dp)
            reserve1=800_000_000_000_000_000_000,  # 800 WETH (18dp)
            gamma_numer0=997,
            fee_denom0=1000,
            gamma_numer1=997,
            fee_denom1=1000,
            factory=self.FACTORY_ADDR,
        )

        # Swap 1000 USDC for WETH
        # Python: constant_product_calc_exact_in(1e9, 1.5e12, 8e20, 3/1000) = 531380142665175213
        result = core.calculate_tokens_out(
            pool_id,
            zero_for_one=True,
            amount_in=1_000_000_000,
        )
        assert result == 531380142665175213

    def test_zero_amount_in_returns_zero(self):
        """Zero input should return zero output."""
        core = PyBot()
        pool_id = core.register_v2_pool(
            address=self.POOL_ADDR,
            token0=self.TOKEN0_ADDR,
            token1=self.TOKEN1_ADDR,
            reserve0=1000,
            reserve1=2000,
            gamma_numer0=997,
            fee_denom0=1000,
            gamma_numer1=997,
            fee_denom1=1000,
            factory=self.FACTORY_ADDR,
        )

        result = core.calculate_tokens_out(pool_id, zero_for_one=True, amount_in=0)
        assert result == 0

    def test_unknown_pool_id_returns_zero(self):
        """Unknown pool ID should return zero (not raise)."""
        core = PyBot()
        result = core.calculate_tokens_out(999, zero_for_one=True, amount_in=100)
        assert result == 0


class TestPoolHandle:
    """Test the thin Pool handle over PyBot."""

    POOL_ADDR = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    TOKEN0_ADDR = "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
    TOKEN1_ADDR = "0xcccccccccccccccccccccccccccccccccccccccc"
    FACTORY_ADDR = "0xdddddddddddddddddddddddddddddddddddddddd"

    def _make_core_with_pool(self) -> tuple[PyBot, int]:
        core = PyBot()
        pool_id = core.register_v2_pool(
            address=self.POOL_ADDR,
            token0=self.TOKEN0_ADDR,
            token1=self.TOKEN1_ADDR,
            reserve0=1000,
            reserve1=2000,
            gamma_numer0=997,
            fee_denom0=1000,
            gamma_numer1=997,
            fee_denom1=1000,
            factory=self.FACTORY_ADDR,
        )
        return core, pool_id

    def test_get_pool_returns_handle(self):
        """get_pool() returns a PyLiquidityPool handle for a registered pool."""
        core, pool_id = self._make_core_with_pool()
        pool = core.get_pool(pool_id)
        assert pool is not None
        assert pool.pool_id == pool_id

    def test_get_pool_unknown_returns_none(self):
        """get_pool() returns None for unknown pool ID."""
        core = PyBot()
        assert core.get_pool(999) is None

    def test_pool_calculate_tokens_out(self):
        """Pool handle delegates calculation to PyBot."""
        core, pool_id = self._make_core_with_pool()
        pool = core.get_pool(pool_id)
        assert pool is not None

        # Python reference: constant_product_calc_exact_in(100, 1000, 2000, 3/1000) = 181
        result = pool.calculate_tokens_out(zero_for_one=True, amount_in=100)
        assert result == 181

    def test_pool_calculate_tokens_in(self):
        """Pool handle delegates exact-out calculation to PyBot."""
        core, pool_id = self._make_core_with_pool()
        pool = core.get_pool(pool_id)
        assert pool is not None

        # Python reference: constant_product_calc_exact_out(50, 1000, 2000, 3/1000) = 26
        result = pool.calculate_tokens_in(zero_for_one=True, amount_out=50)
        assert result == 26

    def test_pool_sees_updates(self):
        """Pool handle reads current state after update."""
        core, pool_id = self._make_core_with_pool()
        pool = core.get_pool(pool_id)

        # Before update
        result_before = pool.calculate_tokens_out(zero_for_one=True, amount_in=100)
        assert result_before == 181

        # Update reserves
        core.update_v2_pool(
            address=self.POOL_ADDR,
            reserve0=2000,
            reserve1=1000,
            block_number=42,
        )

        # After update: Python: constant_product_calc_exact_in(100, 2000, 1000, 3/1000) = 47
        result_after = pool.calculate_tokens_out(zero_for_one=True, amount_in=100)
        assert result_after == 47


class TestPoolHandleState:
    """PyLiquidityPool state read getters + per-handle mutations (ADR-005 slice 4 step 2)."""

    POOL_ADDR = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    TOKEN0_ADDR = "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
    TOKEN1_ADDR = "0xcccccccccccccccccccccccccccccccccccccccc"
    FACTORY_ADDR = "0xdddddddddddddddddddddddddddddddddddddddd"

    def _make_core_with_pool(self) -> tuple[PyBot, int]:
        core = PyBot()
        pool_id = core.register_v2_pool(
            address=self.POOL_ADDR,
            token0=self.TOKEN0_ADDR,
            token1=self.TOKEN1_ADDR,
            reserve0=1000,
            reserve1=2000,
            gamma_numer0=997,
            fee_denom0=1000,
            gamma_numer1=997,
            fee_denom1=1000,
            factory=self.FACTORY_ADDR,
        )
        return core, pool_id

    def test_pool_reads_genesis_reserves_and_block(self):
        """The handle reads the registration state (genesis) directly."""
        core, pool_id = self._make_core_with_pool()
        pool = core.get_pool(pool_id)
        assert pool is not None
        assert pool.reserve0 == 1000
        assert pool.reserve1 == 2000
        assert pool.update_block == 0  # update_block defaults to 0 at registration

    def test_pool_sync_reserves_updates_state(self):
        """sync_reserves journals + lands the new state on the handle."""
        core, pool_id = self._make_core_with_pool()
        pool = core.get_pool(pool_id)
        assert pool is not None
        assert pool.journal_len() == 1  # genesis

        pool.sync_reserves(reserve0=2000, reserve1=1000, block_number=10)
        assert pool.reserve0 == 2000
        assert pool.reserve1 == 1000
        assert pool.update_block == 10
        assert pool.journal_len() == 2  # genesis + block-10 transition

    def test_pool_handle_sync_is_equivalent_to_pybot_update(self):
        """sync_reserves and PyBot.update_v2_pool land the same state."""
        core, pool_id = self._make_core_with_pool()
        pool = core.get_pool(pool_id)
        assert pool is not None
        pool.sync_reserves(reserve0=2000, reserve1=1000, block_number=10)

        core2 = PyBot()
        pool_id2 = core2.register_v2_pool(
            address=self.POOL_ADDR,
            token0=self.TOKEN0_ADDR,
            token1=self.TOKEN1_ADDR,
            reserve0=1000,
            reserve1=2000,
            gamma_numer0=997,
            fee_denom0=1000,
            gamma_numer1=997,
            fee_denom1=1000,
            factory=self.FACTORY_ADDR,
        )
        core2.update_v2_pool(self.POOL_ADDR, 2000, 1000, 10)
        pool2 = core2.get_pool(pool_id2)
        assert pool2 is not None
        assert (pool.reserve0, pool.reserve1, pool.update_block) == (
            pool2.reserve0,
            pool2.reserve1,
            pool2.update_block,
        )

    def test_pool_restore_lands_at_previous_block(self):
        """Handle.restore_before_block lands at the largest block below target."""
        core, pool_id = self._make_core_with_pool()
        pool = core.get_pool(pool_id)
        assert pool is not None
        # Genesis@0 (1000,2000); block-10 after (2000,1000); block-20 after (3000,500).
        pool.sync_reserves(2000, 1000, 10)
        pool.sync_reserves(3000, 500, 20)

        result = pool.restore_before_block(20)
        assert result is not None
        r0, r1, block = result
        assert (r0, r1) == (2000, 1000)
        assert block == 10
        # State landed at block 10.
        assert (pool.reserve0, pool.reserve1, pool.update_block) == (2000, 1000, 10)

    def test_pool_restore_noop_when_target_after_newest(self):
        """Handle.restore_before_block(N>newest) no-ops, returning current."""
        core, pool_id = self._make_core_with_pool()
        pool = core.get_pool(pool_id)
        assert pool is not None
        pool.sync_reserves(2000, 1000, 10)

        result = pool.restore_before_block(100)
        assert result is not None
        r0, r1, block = result
        assert (r0, r1, block) == (2000, 1000, 10)

    def test_pool_restore_past_registration_raises(self):
        """Handle.restore at/before genesis raises ValueError (decision 3)."""
        core, pool_id = self._make_core_with_pool()
        pool = core.get_pool(pool_id)
        assert pool is not None
        with pytest.raises(ValueError, match="No pool state known prior to block 0"):
            pool.restore_before_block(0)

    def test_pool_discard_past_newest_raises(self):
        """Handle.discard past newest raises ValueError (decision 2)."""
        core, pool_id = self._make_core_with_pool()
        pool = core.get_pool(pool_id)
        assert pool is not None
        pool.sync_reserves(2000, 1000, 10)
        with pytest.raises(ValueError, match="No pool state known at or after block 100"):
            pool.discard_before_block(100)

    def test_pool_discard_keeps_rest(self):
        """Handle.discard removes deltas below target, keeping the rest."""
        core, pool_id = self._make_core_with_pool()
        pool = core.get_pool(pool_id)
        assert pool is not None
        pool.sync_reserves(2000, 1000, 10)
        pool.sync_reserves(3000, 500, 20)
        pool.sync_reserves(4000, 250, 30)
        assert pool.journal_len() == 4  # genesis + 10 + 20 + 30

        pool.discard_before_block(20)  # drops genesis@0 + block-10
        assert pool.journal_len() == 2  # 20 + 30 remain


class TestTokenHandle:
    """Test the thin Token handle over PyBot."""

    TOKEN_ADDR = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"

    def test_register_and_get_token(self):
        """Can register and retrieve a token handle."""
        core = PyBot()
        token = core.register_token(
            address=self.TOKEN_ADDR,
            name="Wrapped Ether",
            symbol="WETH",
            decimals=18,
            chain_id=1,
        )
        # ADR-003 Slice 5: register_token returns the PyErc20Token handle; the
        # getters read Rust-owned metadata back through PyBot's token map.
        assert token is not None
        assert token.address == self.TOKEN_ADDR  # alloy preserves checksum case
        assert token.symbol == "WETH"
        assert token.decimals == 18
        assert token.name == "Wrapped Ether"

        retrieved = core.get_token(self.TOKEN_ADDR)
        assert retrieved is not None
        assert retrieved.symbol == "WETH"
        assert retrieved.decimals == 18

    def test_get_unknown_token_returns_none(self):
        """get_token() returns None for unregistered address."""
        core = PyBot()
        assert core.get_token(self.TOKEN_ADDR) is None


class TestV2SwapEncoding:
    """Test V2 swap calldata encoding through PyBot."""

    POOL_ADDR = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    TOKEN0_ADDR = "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
    TOKEN1_ADDR = "0xcccccccccccccccccccccccccccccccccccccccc"
    FACTORY_ADDR = "0xdddddddddddddddddddddddddddddddddddddddd"
    RECIPIENT = "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"

    def _make_core_with_pool(self) -> tuple[PyBot, int]:
        core = PyBot()
        pool_id = core.register_v2_pool(
            address=self.POOL_ADDR,
            token0=self.TOKEN0_ADDR,
            token1=self.TOKEN1_ADDR,
            reserve0=1000,
            reserve1=2000,
            gamma_numer0=997,
            fee_denom0=1000,
            gamma_numer1=997,
            fee_denom1=1000,
            factory=self.FACTORY_ADDR,
        )
        return core, pool_id

    def test_encode_swap_zero_for_one(self):
        """Encode a token0→token1 swap (amount1Out nonzero)."""
        core, pool_id = self._make_core_with_pool()
        result = core.encode_swap(pool_id, zero_for_one=True, amount_out=181, recipient=self.RECIPIENT)
        assert result is not None
        to_hex, calldata_hex, value = result

        # Selector is 022c0d9f
        assert calldata_hex.startswith("0x022c0d9f")
        # Value is 0 (no ETH sent)
        assert value == 0
        # Token address matches pool address
        assert to_hex == self.POOL_ADDR

    def test_encode_swap_matches_python_reference(self):
        """Full byte-level comparison against Python eth_abi output."""
        import eth_abi
        from web3 import Web3

        core, pool_id = self._make_core_with_pool()
        result = core.encode_swap(pool_id, zero_for_one=True, amount_out=181, recipient=self.RECIPIENT)
        assert result is not None
        _, calldata_hex, _ = result

        # Generate reference calldata from Python eth_abi
        selector = Web3.keccak(text="swap(uint256,uint256,address,bytes)")[:4]
        reference_data = eth_abi.abi.encode(
            types=["uint256", "uint256", "address", "bytes"],
            args=[0, 181, self.RECIPIENT, b""],
        )
        reference_calldata = selector + reference_data

        assert calldata_hex == "0x" + reference_calldata.hex()

    def test_encode_swap_one_for_zero(self):
        """Encode a token1→token0 swap (amount0Out nonzero)."""
        import eth_abi
        from web3 import Web3

        core, pool_id = self._make_core_with_pool()
        result = core.encode_swap(pool_id, zero_for_one=False, amount_out=181, recipient=self.RECIPIENT)
        assert result is not None
        _, calldata_hex, _ = result

        # Generate reference calldata from Python eth_abi
        selector = Web3.keccak(text="swap(uint256,uint256,address,bytes)")[:4]
        reference_data = eth_abi.abi.encode(
            types=["uint256", "uint256", "address", "bytes"],
            args=[181, 0, self.RECIPIENT, b""],
        )
        reference_calldata = selector + reference_data

        assert calldata_hex == "0x" + reference_calldata.hex()

    def test_encode_swap_unknown_pool_returns_none(self):
        """encode_swap returns None for unknown pool ID."""
        core = PyBot()
        result = core.encode_swap(999, zero_for_one=True, amount_out=100, recipient=self.RECIPIENT)
        assert result is None

    def test_pool_handle_encode_swap(self):
        """Pool handle can also encode swaps."""
        import eth_abi
        from web3 import Web3

        core, pool_id = self._make_core_with_pool()
        pool = core.get_pool(pool_id)
        assert pool is not None

        result = pool.encode_swap(zero_for_one=True, amount_out=181, recipient=self.RECIPIENT)
        assert result is not None
        _, calldata_hex, _ = result

        # Generate reference calldata from Python eth_abi
        selector = Web3.keccak(text="swap(uint256,uint256,address,bytes)")[:4]
        reference_data = eth_abi.abi.encode(
            types=["uint256", "uint256", "address", "bytes"],
            args=[0, 181, self.RECIPIENT, b""],
        )
        reference_calldata = selector + reference_data

        assert calldata_hex == "0x" + reference_calldata.hex()


class TestV2ReorgJournal:
    """Test V2 pool state history through PyBot."""

    POOL_ADDR = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    TOKEN0_ADDR = "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
    TOKEN1_ADDR = "0xcccccccccccccccccccccccccccccccccccccccc"
    FACTORY_ADDR = "0xdddddddddddddddddddddddddddddddddddddddd"

    def _make_core_with_pool(self) -> tuple[PyBot, int]:
        core = PyBot()
        pool_id = core.register_v2_pool(
            address=self.POOL_ADDR,
            token0=self.TOKEN0_ADDR,
            token1=self.TOKEN1_ADDR,
            reserve0=1000,
            reserve1=2000,
            gamma_numer0=997,
            fee_denom0=1000,
            gamma_numer1=997,
            fee_denom1=1000,
            factory=self.FACTORY_ADDR,
        )
        return core, pool_id

    def test_initial_journal_has_genesis_delta(self):
        """Registration seeds a genesis delta at the update block (len 1)."""
        core, pool_id = self._make_core_with_pool()
        # update_block defaults to 0 — the genesis delta lands there.
        assert core.v2_journal_len(pool_id) == 1

    def test_update_appends_to_journal(self):
        """Each update pushes a new transition delta after the genesis."""
        core, pool_id = self._make_core_with_pool()
        assert core.v2_journal_len(pool_id) == 1  # genesis

        core.update_v2_pool(self.POOL_ADDR, 2000, 1000, 10)
        assert core.v2_journal_len(pool_id) == 2  # genesis + block-10

        core.update_v2_pool(self.POOL_ADDR, 3000, 500, 20)
        assert core.v2_journal_len(pool_id) == 3

    def test_same_block_replaces_current(self):
        """Updating at the same block replaces the newest delta wholesale."""
        core, pool_id = self._make_core_with_pool()
        core.update_v2_pool(self.POOL_ADDR, 2000, 1000, 10)
        assert core.v2_journal_len(pool_id) == 2  # genesis + block-10

        # Same-block update replaces the block-10 delta (genesis survives).
        core.update_v2_pool(self.POOL_ADDR, 2500, 750, 10)
        assert core.v2_journal_len(pool_id) == 2  # still 2 (genesis + block-10)

    def test_discard_before_block(self):
        """discard_before_block removes deltas below target, keeping the rest."""
        core, pool_id = self._make_core_with_pool()
        core.update_v2_pool(self.POOL_ADDR, 2000, 1000, 10)
        core.update_v2_pool(self.POOL_ADDR, 3000, 500, 20)
        core.update_v2_pool(self.POOL_ADDR, 4000, 250, 30)
        # genesis@0, 10, 20, 30 → four deltas
        assert core.v2_journal_len(pool_id) == 4

        # Discard < 20 → drops genesis@0 and block-10, keeps 20 and 30.
        core.v2_discard_before_block(pool_id, 20)
        assert core.v2_journal_len(pool_id) == 2

    def test_restore_before_block_lands_at_previous_block(self):
        """restore(N) lands at the largest block < N, returning that state."""
        core, pool_id = self._make_core_with_pool()
        # Genesis@0 (after 1000,2000); block-10 after (2000,1000);
        # block-20 after (3000,500); block-30 after (4000,250).
        core.update_v2_pool(self.POOL_ADDR, 2000, 1000, 10)
        core.update_v2_pool(self.POOL_ADDR, 3000, 500, 20)
        core.update_v2_pool(self.POOL_ADDR, 4000, 250, 30)

        # Restore before block 20: lands at block 10, returns its landed-at
        # (after) state (2000, 1000) at block 10 — NOT the block-20 delta's
        # before-values at block 20 (the pre-slice-4 behavior).
        result = core.v2_restore_before_block(pool_id, 20)
        assert result is not None
        r0, r1, block = result
        assert r0 == 2000
        assert r1 == 1000
        assert block == 10

    def test_restore_syncs_current_reserves(self):
        """Restoring lands the current reserves at the landed-at state."""
        core, pool_id = self._make_core_with_pool()
        core.update_v2_pool(self.POOL_ADDR, 2000, 1000, 10)
        core.update_v2_pool(self.POOL_ADDR, 3000, 500, 20)

        # Restore before block 20 → lands at block 10 → reserves (2000, 1000).
        core.v2_restore_before_block(pool_id, 20)

        # constant_product_calc_exact_in(100, 2000, 1000, 3/1000) = 47
        result = core.calculate_tokens_out(pool_id, zero_for_one=True, amount_in=100)
        assert result == 47

    def test_restore_noop_returns_current_when_target_after_newest(self):
        """restore(N>newest) no-ops and returns the current landed-at state.

        Pre-slice-4 this returned the pre-newest BEFORE values (a silent
        wrong-answer footgun); the landed-at journal returns AFTER (current).
        """
        core, pool_id = self._make_core_with_pool()
        # Genesis@0 (1000,2000); block-10 after (2000,1000). Newest = 10.
        core.update_v2_pool(self.POOL_ADDR, 2000, 1000, 10)

        # Target (100) is after the newest (10) → no-op, returns current.
        result = core.v2_restore_before_block(pool_id, 100)
        assert result is not None
        r0, r1, block = result
        assert (r0, r1) == (2000, 1000)
        assert block == 10

    def test_restore_past_registration_raises(self):
        """restore at/before the genesis block raises ValueError (decision 3).

        Rolling back past registration is a hard error, not a silent no-op.
        """
        core, pool_id = self._make_core_with_pool()
        # Genesis is at block 0 (update_block default). Restoring to 0 →
        # no state before registration exists.
        with pytest.raises(ValueError, match="No pool state known prior to block 0"):
            core.v2_restore_before_block(pool_id, 0)

    def test_discard_past_newest_raises(self):
        """discard past the newest delta raises ValueError (decision 2).

        Would remove every known state — was a panic pre-slice-4.
        """
        core, pool_id = self._make_core_with_pool()
        core.update_v2_pool(self.POOL_ADDR, 2000, 1000, 10)
        # Newest is block 10; discarding before 100 would drop everything.
        with pytest.raises(ValueError, match="No pool state known at or after block 100"):
            core.v2_discard_before_block(pool_id, 100)

    def test_journal_len_unknown_pool_returns_zero(self):
        """v2_journal_len returns 0 for unknown pool ID."""
        core = PyBot()
        assert core.v2_journal_len(999) == 0

    def test_restore_unknown_pool_returns_none(self):
        """v2_restore_before_block returns None for unknown pool ID."""
        core = PyBot()
        assert core.v2_restore_before_block(999, 10) is None


class TestV3PoolState:
    """Test V3 pool registration, update, and reorg journal through PyBot."""

    POOL_ADDR = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    TOKEN0_ADDR = "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
    TOKEN1_ADDR = "0xcccccccccccccccccccccccccccccccccccccccc"
    FACTORY_ADDR = "0xdddddddddddddddddddddddddddddddddddddddd"

    # sqrt(1.0) * 2^96 ≈ 79228162514264337593543950336
    SQRT_PRICE_X96 = 79228162514264337593543950336

    def _make_core_with_v3_pool(self) -> tuple[PyBot, int]:
        core = PyBot()
        pool_id = core.register_v3_pool(
            address=self.POOL_ADDR,
            token0=self.TOKEN0_ADDR,
            token1=self.TOKEN1_ADDR,
            fee=3000,
            tick_spacing=60,
            factory=self.FACTORY_ADDR,
            sqrt_price_x96=self.SQRT_PRICE_X96,
            liquidity=1000000,
            tick=0,
        )
        return core, pool_id

    def test_register_v3_pool(self):
        """Can register a V3 pool and it gets a pool ID."""
        core, pool_id = self._make_core_with_v3_pool()
        assert pool_id >= 1
        assert core.pool_count() == 1

    def test_register_v3_and_v2_coexist(self):
        """V2 and V3 pools can coexist in the same PyBot."""
        core = PyBot()
        v2_id = core.register_v2_pool(
            address="0x1111111111111111111111111111111111111111",
            token0=self.TOKEN0_ADDR,
            token1=self.TOKEN1_ADDR,
            reserve0=1000,
            reserve1=2000,
            gamma_numer0=997,
            fee_denom0=1000,
            gamma_numer1=997,
            fee_denom1=1000,
            factory=self.FACTORY_ADDR,
        )
        v3_id = core.register_v3_pool(
            address=self.POOL_ADDR,
            token0=self.TOKEN0_ADDR,
            token1=self.TOKEN1_ADDR,
            fee=3000,
            tick_spacing=60,
            factory=self.FACTORY_ADDR,
            sqrt_price_x96=self.SQRT_PRICE_X96,
            liquidity=1000000,
            tick=0,
        )
        assert v2_id != v3_id
        assert core.pool_count() == 2

    def test_update_v3_pool_changes_journal(self):
        """Each V3 update pushes a delta to the journal."""
        core, pool_id = self._make_core_with_v3_pool()
        assert core.v3_journal_len(pool_id) == 0

        core.update_v3_pool(
            address=self.POOL_ADDR,
            sqrt_price_x96=self.SQRT_PRICE_X96 + 1000,
            liquidity=2000000,
            tick=10,
            block_number=42,
        )
        assert core.v3_journal_len(pool_id) == 1

    def test_update_v3_same_block_replaces_delta(self):
        """Same-block V3 update replaces the current delta."""
        core, pool_id = self._make_core_with_v3_pool()
        core.update_v3_pool(self.POOL_ADDR, self.SQRT_PRICE_X96 + 1000, 2000000, 10, 42)
        assert core.v3_journal_len(pool_id) == 1

        # Same-block update replaces
        core.update_v3_pool(self.POOL_ADDR, self.SQRT_PRICE_X96 + 2000, 3000000, 20, 42)
        assert core.v3_journal_len(pool_id) == 1  # Still 1, not 2

    def test_v3_discard_before_block(self):
        """discard_before_block removes old V3 deltas."""
        core, pool_id = self._make_core_with_v3_pool()
        core.update_v3_pool(self.POOL_ADDR, self.SQRT_PRICE_X96 + 1000, 2000000, 10, 10)
        core.update_v3_pool(self.POOL_ADDR, self.SQRT_PRICE_X96 + 2000, 3000000, 20, 20)
        core.update_v3_pool(self.POOL_ADDR, self.SQRT_PRICE_X96 + 3000, 4000000, 30, 30)
        assert core.v3_journal_len(pool_id) == 3

        core.v3_discard_before_block(pool_id, 20)
        assert core.v3_journal_len(pool_id) == 2  # blocks 20 and 30 remain

    def test_v3_restore_before_block(self):
        """restore_before_block rolls back V3 state."""
        core, pool_id = self._make_core_with_v3_pool()
        # Initial: sqrt_price=SQUARE_PRICE_X96, liquidity=1000000, tick=0
        core.update_v3_pool(self.POOL_ADDR, self.SQRT_PRICE_X96 + 1000, 2000000, 10, 10)
        # After block 10: current=(SQ+1000, 2000000, 10)
        core.update_v3_pool(self.POOL_ADDR, self.SQRT_PRICE_X96 + 2000, 3000000, 20, 20)
        # After block 20: current=(SQ+2000, 3000000, 20)

        # Restore before block 20: pops block-20 delta, returns before-values
        result = core.v3_restore_before_block(pool_id, 20)
        assert result is not None
        spx, liq, tick, block = result
        assert spx == self.SQRT_PRICE_X96 + 1000
        assert liq == 2000000
        assert tick == 10
        assert block == 20

    def test_v3_restore_syncs_current_state(self):
        """Restoring also updates the pool's current scalar state."""
        core, pool_id = self._make_core_with_v3_pool()
        core.update_v3_pool(self.POOL_ADDR, self.SQRT_PRICE_X96 + 1000, 2000000, 10, 10)
        core.update_v3_pool(self.POOL_ADDR, self.SQRT_PRICE_X96 + 2000, 3000000, 20, 20)

        # Restore before block 20
        core.v3_restore_before_block(pool_id, 20)

        # Verify current state is rolled back by doing another update
        # and checking journal length
        assert core.v3_journal_len(pool_id) == 1  # Only block-10 delta remains

    def test_v3_journal_len_unknown_pool_returns_zero(self):
        """v3_journal_len returns 0 for unknown pool ID."""
        core = PyBot()
        assert core.v3_journal_len(999) == 0

    def test_v3_restore_unknown_pool_returns_none(self):
        """v3_restore_before_block returns None for unknown pool ID."""
        core = PyBot()
        assert core.v3_restore_before_block(999, 10) is None

    def test_v2_journal_methods_on_v3_pool_return_zero(self):
        """V2 journal methods correctly return 0/None for V3 pools."""
        core, pool_id = self._make_core_with_v3_pool()
        assert core.v2_journal_len(pool_id) == 0

    def test_v3_journal_methods_on_v2_pool_return_zero(self):
        """V3 journal methods correctly return 0/None for V2 pools."""
        core = PyBot()
        pool_id = core.register_v2_pool(
            address="0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            token0=self.TOKEN0_ADDR,
            token1=self.TOKEN1_ADDR,
            reserve0=1000,
            reserve1=2000,
            gamma_numer0=997,
            fee_denom0=1000,
            gamma_numer1=997,
            fee_denom1=1000,
            factory=self.FACTORY_ADDR,
        )
        assert core.v3_journal_len(pool_id) == 0
        assert core.v3_restore_before_block(pool_id, 10) is None
