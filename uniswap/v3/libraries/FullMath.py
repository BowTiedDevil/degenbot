from . import YulOperations as yul


class FullMathException(Exception):
    pass


def mulDiv(a, b, denominator):
    """
    The Solidity implementation is designed to calculate a * b / d without risk of overflowing
    the intermediate result (maximum of 2**256-1). Python does not have this limitation,
    so simply check for exceptional conditions then return the value
    """
    assert denominator, FullMathException("DIVISION BY ZERO")
    result = (a * b) // denominator
    assert result <= 2**256 - 1, FullMathException("uint256 overflow")
    return result


def mulDivRoundingUp(a, b, denominator):
    result = mulDiv(a, b, denominator)
    if yul.mulmod(a, b, denominator) > 0:
        assert result < 2**256 - 1, "FAIL!"
        result += 1
    return result
