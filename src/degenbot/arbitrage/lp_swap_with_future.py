from ..baseclasses import ArbitrageHelper


class LpSwapWithFuture(ArbitrageHelper):
    def __init__(self, *args, **kwargs):
        raise DeprecationWarning(
            "This class has been deprecated, please transition to UniswapLpCycle."
        )
