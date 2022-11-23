from .Helpers import uint128


def addDelta(x: int, y: int) -> int:
    if y < 0:
        z = x - uint128(-y)
        assert z < x, "LS"
    else:
        z = x + uint128(y)
        assert z >= x, "LA"

    return z
