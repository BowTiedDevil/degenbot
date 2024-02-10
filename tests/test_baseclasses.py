from degenbot.baseclasses import PoolHelper
from hexbytes import HexBytes


def test_address_comparisons():
    P1_ADDRESS = "0x0a1b2c3d"
    P2_ADDRESS = "0x3d2c1b0a"

    p1 = PoolHelper()
    p2 = PoolHelper()

    p1.address = P1_ADDRESS
    p2.address = P2_ADDRESS

    assert p1 != p2
    assert p1 < p2
    assert p2 > p1

    assert p1 == P1_ADDRESS
    assert p2 == P2_ADDRESS

    assert p1 == P1_ADDRESS.lower()
    assert p1 == P1_ADDRESS.upper()
    assert p1 == HexBytes(P1_ADDRESS)

    assert p2 == P2_ADDRESS.lower()
    assert p2 == P2_ADDRESS.upper()
    assert p2 == HexBytes(P2_ADDRESS)
