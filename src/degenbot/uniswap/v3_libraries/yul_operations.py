def gt(x: int, y: int) -> int:
    return 1 if x > y else 0


def mod(x: int, y: int) -> int:
    return 0 if y == 0 else x % y


def mul(x: int, y: int) -> int:
    return x * y


def shl(x: int, y: int) -> int:
    return y << x


def shr(x: int, y: int) -> int:
    return y >> x


def or_(x: int, y: int) -> int:
    return x | y


def add(x: int, y: int) -> int:
    return x + y


def div(x: int, y: int) -> int:
    return 0 if y == 0 else x // y
