"""Package version derived from ``importlib.metadata``."""
from importlib.metadata import version

__version__: str = version(__package__ or "degenbot")
