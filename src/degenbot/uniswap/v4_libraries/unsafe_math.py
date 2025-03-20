from pydantic import validate_call

from degenbot.validation.evm_values import ValidatedUint256


@validate_call(validate_return=True)
def div_rounding_up(
    x: ValidatedUint256,
    y: ValidatedUint256,
) -> ValidatedUint256:
    """
    Calculates ceil(x/y)

    @dev division by 0 will return 0, and should be checked externally.
    """

    return 0 if y == 0 else x // y + int(x % y > 0)


@validate_call(validate_return=True)
def simple_mul_div(
    a: ValidatedUint256,
    b: ValidatedUint256,
    denominator: ValidatedUint256,
) -> ValidatedUint256:
    """
    Calculates floor((a*b)/denominator))

    @dev division by 0 will return 0, and should be checked externally
    """

    return 0 if denominator == 0 else (a * b) // denominator
