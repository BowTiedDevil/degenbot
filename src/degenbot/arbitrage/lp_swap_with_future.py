from ..baseclasses import ArbitrageHelper


class LpSwapWithFuture(ArbitrageHelper):  # pragma: no cover
    def __init__(self, *args, **kwargs):
        raise DeprecationWarning("This class has been deprecated in favor of UniswapLpCycle.")
