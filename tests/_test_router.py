# This test code was written by the `hypothesis.extra.ghostwriter` module
# and is provided under the Creative Commons Zero public domain dedication.
# TODO: fix ModuleNotFoundError: No module named 'router'

import router
from brownie.network.account import Account, LocalAccount
from hypothesis import given, strategies as st


@given(
    address=st.text(),
    name=st.text(),
    user=st.builds(LocalAccount),
    abi=st.one_of(st.none(), st.builds(list)),
)
def test_fuzz_Router(address, name, user, abi):
    router.Router(address=address, name=name, user=user, abi=abi)
