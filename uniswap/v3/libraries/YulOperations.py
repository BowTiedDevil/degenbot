def gt(x, y):
    return 1 if x > y else 0


def lt(x, y):
    return 1 if x < y else 0


def mod(x, y):
    return 0 if y == 0 else x % y


def mul(x, y):
    return x * y


def mulmod(x, y, m):
    return 0 if m == 0 else (x * y) % m


def shl(x, y):
    return y << x


def shr(x, y):
    return y >> x


def _or(x, y):
    return x | y


def _not(x):
    return ~x


def add(x, y):
    return x + y


def sub(x, y):
    return x - y


def div(x, y):
    return 0 if y == 0 else x // y
