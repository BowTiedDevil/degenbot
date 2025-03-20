from pydantic import validate_call

from degenbot.validation.evm_values import ValidatedUint256, ValidatedUint256NonZero


@validate_call(validate_return=True)
def mulmod(
    x: ValidatedUint256,
    y: ValidatedUint256,
    k: ValidatedUint256NonZero,
) -> ValidatedUint256:
    """
    Returns (x*y)%k, as implemented by Yul.

    ref: https://docs.soliditylang.org/en/latest/yul.html
    """

    return 0 if k == 0 else (x * y) % k
