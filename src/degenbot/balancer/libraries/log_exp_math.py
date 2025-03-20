from degenbot.exceptions import EVMRevertError

ONE_18 = 1 * 10**18

# Internally, intermediate values are computed with higher precision as 20 decimal fixed point
# numbers, and in the case of ln36, 36 decimals.
ONE_20 = 1 * 10**20
ONE_36 = 1 * 10**36


# The domain of natural exponentiation is bound by the word size and number of decimals used.
#
# Because internally the result will be stored using 20 decimals, the largest possible result is
# (2^255 - 1) / 10^20, which makes the largest exponent
# ln((2^255 - 1) / 10^20) = 130.700829182905140221.
# The smallest possible result is 10^(-18), which makes largest negative argument
# ln(10^(-18)) = -41.446531673892822312.
# We use 130.0 and -41.0 to have some safety margin.
MAX_NATURAL_EXPONENT = 130 * 10**18
MIN_NATURAL_EXPONENT = -41 * 10**18


# Bounds for ln_36's argument. Both ln(0.9) and ln(1.1) can be represented with 36 decimal places in
# a fixed point 256 bit integer.
LN_36_LOWER_BOUND = ONE_18 - 1 * 10**17
LN_36_UPPER_BOUND = ONE_18 + 1 * 10**17
MILD_EXPONENT_BOUND = 2**254 // ONE_20

# 18 decimal constants
x0 = 128000000000000000000  # 2^7
a0 = 38877084059945950922200000000000000000000000000000000000  # e^(x0) (no decimals)
x1 = 64000000000000000000  # 2^6
a1 = 6235149080811616882910000000  # e^(x1) (no decimals)

# 20 decimal constants
x2 = 3200000000000000000000  # 2^5
a2 = 7896296018268069516100000000000000  # e^(x2)
x3 = 1600000000000000000000  # 2^4
a3 = 888611052050787263676000000  # e^(x3)
x4 = 800000000000000000000  # 2^3
a4 = 298095798704172827474000  # e^(x4)
x5 = 400000000000000000000  # 2^2
a5 = 5459815003314423907810  # e^(x5)
x6 = 200000000000000000000  # 2^1
a6 = 738905609893065022723  # e^(x6)
x7 = 100000000000000000000  # 2^0
a7 = 271828182845904523536  # e^(x7)
x8 = 50000000000000000000  # 2^-1
a8 = 164872127070012814685  # e^(x8)
x9 = 25000000000000000000  # 2^-2
a9 = 128402541668774148407  # e^(x9)
x10 = 12500000000000000000  # 2^-3
a10 = 113314845306682631683  # e^(x10)
x11 = 6250000000000000000  # 2^-4
a11 = 106449445891785942956  # e^(x11)


def pow(x: int, y: int) -> int:  # noqa: A001
    if y == 0:
        # We solve the 0^0 indetermination by making it equal one.
        return ONE_18

    if x == 0:
        return 0

    # Instead of computing x^y directly, we instead rely on the properties of logarithms and
    # exponentiation to arrive at that result. In particular, exp(ln(x)) = x, and
    # ln(x^y) = y * ln(x). This means x^y = exp(y * ln(x)).

    # The ln function takes a signed value, so we need to make sure x fits in the signed 256 bit
    # range.
    if x >= 2**255:
        raise EVMRevertError(error="X_OUT_OF_BOUNDS")
    x_int256 = x

    # We will compute y * ln(x) in a single step. Depending on the value of x, we can either use ln
    # or ln_36. In both cases, we leave the division by ONE_18 (due to fixed point multiplication)
    # to the end.

    # This prevents y * ln(x) from overflowing, and at the same time guarantees y fits in the signed
    # 256 bit range.
    if y >= MILD_EXPONENT_BOUND:
        raise EVMRevertError(error="Y_OUT_OF_BOUNDS")
    y_int256 = y

    if LN_36_LOWER_BOUND < x_int256 < LN_36_UPPER_BOUND:
        ln_36_x = _ln_36(x_int256)

        # ln_36_x has 36 decimal places, so multiplying by y_int256 isn't as straightforward, since
        # we can't just bring y_int256 to 36 decimal places, as it might overflow. Instead, we
        # perform two 18 decimal multiplications and add the results: one with the first 18 decimals
        # of ln_36_x, and one with the (downscaled) last 18 decimals.
        logx_times_y = (ln_36_x // ONE_18) * y_int256 + ((ln_36_x % ONE_18) * y_int256) // ONE_18
    else:
        logx_times_y = _ln(x_int256) * y_int256

    logx_times_y //= ONE_18

    # Finally, we compute exp(y * ln(x)) to arrive at x^y
    if not (MIN_NATURAL_EXPONENT <= logx_times_y <= MAX_NATURAL_EXPONENT):
        raise EVMRevertError(error="PRODUCT_OUT_OF_BOUNDS")

    return exp(logx_times_y)


def exp(x: int) -> int:
    if not (MIN_NATURAL_EXPONENT <= x <= MAX_NATURAL_EXPONENT):
        raise EVMRevertError(error="Invalid exponent")

    if x < 0:
        # We only handle positive exponents: e^(-x) is computed as 1 / e^x. We can safely make x
        # positive since it fits in the signed 256 bit range (as it is larger than
        # MIN_NATURAL_EXPONENT). Fixed point division requires multiplying by ONE_18.
        return (ONE_18 * ONE_18) // exp(-x)

    # First, we use the fact that e^(x+y) = e^x * e^y to decompose x into a sum of powers of two,
    # which we call x_n, where x_n == 2^(7 - n), and e^x_n = a_n has been precomputed. We choose the
    # first x_n, x0, to equal 2^7 because all larger powers are larger than MAX_NATURAL_EXPONENT,
    # and therefore not present in the decomposition.
    # At the end of this process we will have the product of all e^x_n = a_n that apply, and the
    # remainder of this decomposition, which will be lower than the smallest x_n.
    # exp(x) = k_0 * a_0 * k_1 * a_1 * ... + k_n * a_n * exp(remainder), where each k_n equals
    # either 0 or 1.
    # We mutate x by subtracting x_n, making it the remainder of the decomposition.

    # The first two a_n (e^(2^7) and e^(2^6)) are too large if stored as 18 decimal numbers, and
    # could cause intermediate overflows. Instead we store them as plain integers, with 0 decimals.
    # Additionally, x0 + x1 is larger than MAX_NATURAL_EXPONENT, which means they will not both be
    # present in the decomposition.

    # For each x_n, we test if that term is present in the decomposition (if x is larger than it),
    # and if so deduct it and compute the accumulated product.

    first_an: int
    if x >= x0:
        x -= x0
        first_an = a0
    elif x >= x1:
        x -= x1
        first_an = a1
    else:
        first_an = 1  # One with no decimal places

    # We now transform x into a 20 decimal fixed point number, to have enhanced precision when
    # computing the smaller terms.
    x *= 100

    # `product` is the accumulated product of all a_n (except a0 and a1), which starts at 20 decimal
    # fixed point one. Recall that fixed point multiplication requires dividing by ONE_20.
    product = ONE_20

    if x >= x2:
        x -= x2
        product = (product * a2) // ONE_20
    if x >= x3:
        x -= x3
        product = (product * a3) // ONE_20
    if x >= x4:
        x -= x4
        product = (product * a4) // ONE_20
    if x >= x5:
        x -= x5
        product = (product * a5) // ONE_20
    if x >= x6:
        x -= x6
        product = (product * a6) // ONE_20
    if x >= x7:
        x -= x7
        product = (product * a7) // ONE_20
    if x >= x8:
        x -= x8
        product = (product * a8) // ONE_20
    if x >= x9:
        x -= x9
        product = (product * a9) // ONE_20

    # x10 and x11 are unnecessary here since we have high enough precision already.

    # Now we need to compute e^x, where x is small (in particular, it is smaller than x9). We use
    # the Taylor series expansion for e^x: 1 + x + (x^2 / 2!) + (x^3 / 3!) + ... + (x^n / n!).

    series_sum = ONE_20  # The initial one in the sum, with 20 decimal places.
    term: int  # Each term in the sum, where the nth term is (x^n / n!).

    # The first term is simply x.
    term = x
    series_sum += term

    # Each term (x^n / n!) equals the previous one times x, divided by n. Since x is a fixed point
    # number, multiplying by it requires dividing by ONE_20, but dividing by the non-fixed point n
    # values does not.

    term = ((term * x) // ONE_20) // 2
    series_sum += term

    term = ((term * x) // ONE_20) // 3
    series_sum += term

    term = ((term * x) // ONE_20) // 4
    series_sum += term

    term = ((term * x) // ONE_20) // 5
    series_sum += term

    term = ((term * x) // ONE_20) // 6
    series_sum += term

    term = ((term * x) // ONE_20) // 7
    series_sum += term

    term = ((term * x) // ONE_20) // 8
    series_sum += term

    term = ((term * x) // ONE_20) // 9
    series_sum += term

    term = ((term * x) // ONE_20) // 10
    series_sum += term

    term = ((term * x) // ONE_20) // 11
    series_sum += term

    term = ((term * x) // ONE_20) // 12
    series_sum += term

    # 12 Taylor terms are sufficient for 18 decimal precision.

    # We now have the first a_n (with no decimals), and the product of all other a_n present, and
    # the Taylor approximation of the exponentiation of the remainder (both with 20 decimals). All
    # that remains is to multiply all three (one 20 decimal fixed point multiplication, dividing by
    # ONE_20, and one integer multiplication), and then drop two digits to return an 18 decimal
    # value.

    return (((product * series_sum) // ONE_20) * first_an) // 100


# @dev Logarithm (log(arg, base), with signed 18 decimal fixed point base and argument.
def log(arg: int, base: int) -> int:
    # This performs a simple base change: log(arg, base) = ln(arg) / ln(base).

    # Both logBase and logArg are computed as 36 decimal fixed point numbers, either by using ln_36,
    # or by upscaling.

    log_base = _ln_36(base) if LN_36_LOWER_BOUND < base < LN_36_UPPER_BOUND else _ln(base) * ONE_18
    log_arg = _ln_36(arg) if LN_36_LOWER_BOUND < arg < LN_36_UPPER_BOUND else _ln(arg) * ONE_18

    # When dividing, we multiply by ONE_18 to arrive at a result with 18 decimal places
    return (log_arg * ONE_18) // log_base


def ln(a: int) -> int:
    # The real natural logarithm is not defined for negative numbers or zero.
    if a <= 0:
        raise EVMRevertError(error="OUT_OF_BOUNDS")

    if LN_36_LOWER_BOUND < a < LN_36_UPPER_BOUND:
        return _ln_36(a) // ONE_18

    return _ln(a)


def _ln(a: int) -> int:
    if a < ONE_18:
        # Since ln(a^k) = k * ln(a), we can compute ln(a) as ln(a) = ln((1/a)^(-1)) = - ln((1/a)).
        # If a is less than one, 1/a will be greater than one, and this if statement will not be
        # entered in the recursive call. Fixed point division requires multiplying by ONE_18.
        return -_ln((ONE_18 * ONE_18) // a)

    # First, we use the fact that ln^(a * b) = ln(a) + ln(b) to decompose ln(a) into a sum of powers
    # of two, which we call x_n, where x_n == 2^(7 - n), which are the natural logarithm of
    # precomputed quantities a_n (that is, ln(a_n) = x_n). We choose the first x_n, x0, to equal 2^7
    # because the exponential of all larger powers cannot be represented as 18 fixed point decimal
    # numbers in 256 bits, and are therefore larger than a. At the end of this process we will have
    # the sum of all x_n = ln(a_n) that apply, and the remainder of this decomposition, which will
    # be lower than the smallest a_n.
    # ln(a) = k_0 * x_0 + k_1 * x_1 + ... + k_n * x_n + ln(remainder), where each k_n equals either
    # 0 or 1. We mutate a by subtracting a_n, making it the remainder of the decomposition.

    # For reasons related to how `exp` works, the first two a_n (e^(2^7) and e^(2^6)) are not stored
    # as fixed point numbers with 18 decimals, but instead as plain integers with 0 decimals, so we
    # need to multiply them by ONE_18 to convert them to fixed point. For each a_n, we test if that
    # term is present in the decomposition (if a is larger than it), and if so divide by it and
    # compute the accumulated sum.

    _sum = 0
    if a >= a0 * ONE_18:
        a //= a0  # Integer, not fixed point division
        _sum += x0

    if a >= a1 * ONE_18:
        a //= a1  # Integer, not fixed point division
        _sum += x1

    # All other a_n and x_n are stored as 20 digit fixed point numbers, so we convert the sum and a
    # to this format.
    _sum *= 100
    a *= 100

    # Because further a_n are  20 digit fixed point numbers, we multiply by ONE_20 when dividing by
    # them.

    if a >= a2:
        a = (a * ONE_20) // a2
        _sum += x2
    if a >= a3:
        a = (a * ONE_20) // a3
        _sum += x3
    if a >= a4:
        a = (a * ONE_20) // a4
        _sum += x4
    if a >= a5:
        a = (a * ONE_20) // a5
        _sum += x5
    if a >= a6:
        a = (a * ONE_20) // a6
        _sum += x6
    if a >= a7:
        a = (a * ONE_20) // a7
        _sum += x7
    if a >= a8:
        a = (a * ONE_20) // a8
        _sum += x8
    if a >= a9:
        a = (a * ONE_20) // a9
        _sum += x9
    if a >= a10:
        a = (a * ONE_20) // a10
        _sum += x10
    if a >= a11:
        a = (a * ONE_20) // a11
        _sum += x11

    # a is now a small number (smaller than a_11, which roughly equals 1.06). This means we can use
    # a Taylor series that converges rapidly for values of `a` close to one - the same one used in
    # ln_36.
    # Let z = (a - 1) / (a + 1).
    # ln(a) = 2 * (z + z^3 / 3 + z^5 / 5 + z^7 / 7 + ... + z^(2 * n + 1) / (2 * n + 1))

    # Recall that 20 digit fixed point division requires multiplying by ONE_20, and multiplication
    # requires division by ONE_20.
    z = ((a - ONE_20) * ONE_20) // (a + ONE_20)
    z_squared = (z * z) // ONE_20

    # num is the numerator of the series: the z^(2 * n + 1) term
    num = z

    # seriesSum holds the accumulated sum of each term in the series, starting with the initial z
    series_sum = num

    # In each step, the numerator is multiplied by z^2
    num = (num * z_squared) // ONE_20
    series_sum += num // 3

    num = (num * z_squared) // ONE_20
    series_sum += num // 5

    num = (num * z_squared) // ONE_20
    series_sum += num // 7

    num = (num * z_squared) // ONE_20
    series_sum += num // 9

    num = (num * z_squared) // ONE_20
    series_sum += num // 11

    # 6 Taylor terms are sufficient for 36 decimal precision.

    # Finally, we multiply by 2 (non fixed point) to compute ln(remainder)
    series_sum *= 2

    # We now have the sum of all x_n present, and the Taylor approximation of the logarithm of the
    # remainder (both with 20 decimals). All that remains is to sum these two, and then drop two
    # digits to return a 18 decimal value.

    return (_sum + series_sum) // 100


def _ln_36(x: int) -> int:
    # Since ln(1) = 0, a value of x close to one will yield a very small result, which makes using
    # 36 digits worthwhile.

    # First, we transform x to a 36 digit fixed point value.
    x *= ONE_18

    # We will use the following Taylor expansion, which converges very rapidly.
    # Let z = (x - 1) / (x + 1).
    # ln(x) = 2 * (z + z^3 / 3 + z^5 / 5 + z^7 / 7 + ... + z^(2 * n + 1) / (2 * n + 1))

    # Recall that 36 digit fixed point division requires multiplying by ONE_36, and multiplication
    # requires division by ONE_36.
    z = ((x - ONE_36) * ONE_36) // (x + ONE_36)
    z_squared = (z * z) // ONE_36

    # num is the numerator of the series: the z^(2 * n + 1) term
    num = z

    # seriesSum holds the accumulated sum of each term in the series, starting with the initial z
    series_sum = num

    # In each step, the numerator is multiplied by z^2
    num = (num * z_squared) // ONE_36
    series_sum += num // 3

    num = (num * z_squared) // ONE_36
    series_sum += num // 5

    num = (num * z_squared) // ONE_36
    series_sum += num // 7

    num = (num * z_squared) // ONE_36
    series_sum += num // 9

    num = (num * z_squared) // ONE_36
    series_sum += num // 11

    num = (num * z_squared) // ONE_36
    series_sum += num // 13

    num = (num * z_squared) // ONE_36
    series_sum += num // 15

    # 8 Taylor terms are sufficient for 36 decimal precision.

    # All that remains is multiplying by 2 (non fixed point).
    return series_sum * 2
