# This test code was written by the `hypothesis.extra.ghostwriter` module
# and is provided under the Creative Commons Zero public domain dedication.

import chainlink
from hypothesis import given, strategies as st


@given(address=st.text())
def test_fuzz_ChainlinkPriceContract(address):
    chainlink.ChainlinkPriceContract(address=address)
