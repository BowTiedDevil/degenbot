def gt(x, y):
    return 1 if x > y else 0


def mod(x, y):
    return 0 if y == 0 else x % y


def mul(x, y):
    return x * y


def shl(x, y):
    return y << x


def shr(x, y):
    return y >> x


def or_(x, y):
    return x | y


def add(x, y):
    return x + y


def div(x, y):
    return 0 if y == 0 else x // y
