from typing import Any, Dict, Tuple


class MultiLiquidityPool:
    def __init__(self, *args: Tuple[Any], **kwargs: Dict[Any, Any]) -> None:  # pragma: no cover
        raise DeprecationWarning("This class has been deprecated in favor of UniswapLpCycle.")
