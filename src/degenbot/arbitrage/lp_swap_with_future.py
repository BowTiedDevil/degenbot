from typing import Any, Dict, Tuple

from ..baseclasses import BaseArbitrage


class LpSwapWithFuture(BaseArbitrage):  # pragma: no cover
    def __init__(self, *args: Tuple[Any], **kwargs: Dict[Any, Any]) -> None:
        raise DeprecationWarning("This class has been deprecated in favor of UniswapLpCycle.")
