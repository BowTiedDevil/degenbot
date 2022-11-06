# This test code was written by the `hypothesis.extra.ghostwriter` module
# and is provided under the Creative Commons Zero public domain dedication.

import token
from hypothesis import given, strategies as st

# TODO: replace st.nothing() with appropriate strategies


@given(x=st.characters())
def test_fuzz_ISEOF(x):
    token.ISEOF(x=x)


@given(x=st.integers())
def test_fuzz_ISNONTERMINAL(x):
    token.ISNONTERMINAL(x=x)


@given(x=st.integers())
def test_fuzz_ISTERMINAL(x):
    token.ISTERMINAL(x=x)
