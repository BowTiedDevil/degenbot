from abc import ABC, abstractmethod
from collections.abc import Sequence

from degenbot.arbitrage.solver.types import HopState, MobiusSolveResult


class SolverProtocol(ABC):
    """
    Abstract interface for arbitrage solvers.

    Implementations receive a sequence of HopState objects and return
    a MobiusSolveResult. The supports() method allows callers to
    check compatibility before calling solve().
    """

    @abstractmethod
    def solve(
        self,
        hops: Sequence[HopState],
        max_input: int | None = None,
    ) -> MobiusSolveResult: ...

    @abstractmethod
    def supports(self, hops: Sequence[HopState]) -> bool: ...
