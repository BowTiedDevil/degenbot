"""TD1 (P1): the Rust CREATE2 derivations are exposed on ``degenbot._ffi``.

The Rust core owns the canonical CREATE2 family
(``degenbot-uniswap/src/create2.rs``) but historically exposed none of it on
the FFI, so the Python companions re-derived the chain in pure Python.
"""

from __future__ import annotations

import degenbot._ffi as _ffi

## Uniswap V2 DAI/WETH mainnet pool (golden, byte-level).
V2_DAI_WETH = "0xA478c2975Ab1Ea89e8196811F51A7B7Ade33eB11"
## Uniswap V3 USDC/WETH mainnet pools (on-chain golden).
V3_USDC_WETH_005 = "0x88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640"
V3_USDC_WETH_03 = "0x8ad599c3A0ff1De082011EFDDc58f1908eb6e6D8"

V2_FACTORY = "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"
V2_INIT = "0x96e8ac4277198ff8b6f785478aa9a39f403cb768dd02cbee326c3e7da348845f"
V3_FACTORY = "0x1F98431c8aD98523631AE4a59f267346ea31F984"
V3_INIT = "0xe34f199b19b2b4f47f68442619d555527d244f78a3297ea89325f843f87b8b54"
DAI = "0x6B175474E89094C44Da98b954EedeAC495271d0F"
WETH = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"


def test_v2_derivation_exposed_on_ffi_matches_golden() -> None:
    got = _ffi.generate_v2_pool_address(V2_FACTORY, DAI, WETH, V2_INIT)
    assert got == V2_DAI_WETH
    # Order-independent (sorted internally).
    assert _ffi.generate_v2_pool_address(V2_FACTORY, WETH, DAI, V2_INIT) == got


def test_v3_derivation_exposed_on_ffi_matches_golden() -> None:
    usdc = "0xA0b86991c6218b36c1d19d4a2e9Eb0cE3606eB48"
    weth = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
    got = _ffi.generate_v3_pool_address(V3_FACTORY, usdc, weth, 500, V3_INIT)
    assert got == V3_USDC_WETH_005
    assert _ffi.generate_v3_pool_address(V3_FACTORY, weth, usdc, 500, V3_INIT) == got
    assert (
        _ffi.generate_v3_pool_address(V3_FACTORY, usdc, weth, 3000, V3_INIT) == V3_USDC_WETH_03
    )


def test_generic_create2_exposed_on_ffi() -> None:
    # The V2 pair derivation chains through this primitive internally; assert
    # a golden produced by walking the chain directly: salt =
    # keccak(abi.encodePacked(t0,t1)) then CREATE2(deployer, salt, init).
    t0, t1 = sorted([bytes.fromhex(DAI[2:]), bytes.fromhex(WETH[2:])])
    salt = _ffi.keccak256(t0 + t1).hex()
    out = _ffi.create2_address(V2_FACTORY, salt, V2_INIT)
    assert out == V2_DAI_WETH
