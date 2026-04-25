"""
Solver registry for breaking circular imports between solver modules.

Both degenbot.arbitrage.optimizers.solver and degenbot.arbitrage.solver.mobius_solver
need to reference each other's solver classes. This registry allows them to register
and retrieve solver classes without creating import-time cycles.
"""


class SolverRegistry:
    _arb_solver_class: type | None = None
    _mobius_solver_class: type | None = None

    @classmethod
    def register_arb_solver(cls, solver_class: type) -> None:
        cls._arb_solver_class = solver_class

    @classmethod
    def register_mobius_solver(cls, solver_class: type) -> None:
        cls._mobius_solver_class = solver_class

    @classmethod
    def get_arb_solver(cls):  # type: ignore[name-defined]
        if cls._arb_solver_class is None:
            msg = "ArbSolver not registered"
            raise RuntimeError(msg)
        return cls._arb_solver_class()

    @classmethod
    def get_mobius_solver(cls):  # type: ignore[name-defined]
        if cls._mobius_solver_class is None:
            msg = "MobiusSolver not registered"
            raise RuntimeError(msg)
        return cls._mobius_solver_class()
