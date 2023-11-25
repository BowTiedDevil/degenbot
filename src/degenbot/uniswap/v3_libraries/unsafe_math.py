from . import yul_operations as yul


def divRoundingUp(x, y) -> int:
    return yul.add(yul.div(x, y), yul.gt(yul.mod(x, y), 0))
