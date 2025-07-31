import importlib
import sys

import pytest


def test_lazy_loading_known_symbol_and_missing():
    # Ensure a clean import state so we can observe lazy-loading effects
    for key in list(sys.modules.keys()):
        if key == "degenbot" or key.startswith("degenbot."):
            del sys.modules[key]

    # Import the package without pulling submodules
    degenbot = importlib.import_module("degenbot")

    # At this point, only the base package should be present
    assert "degenbot" in sys.modules
    # The erc20 subpackage should not be imported yet
    assert "degenbot.erc20" not in sys.modules

    # Access a known exported symbol: ERC20Token (exported via degenbot.erc20.__all__)
    from degenbot import Erc20Token  # noqa: F401,PLC0415

    # Lazy import should have occurred for the defining submodule
    assert "degenbot.erc20" in sys.modules

    # Accessing the symbol via package attribute should succeed as well
    assert hasattr(degenbot, "Erc20Token")

    # Missing symbol should raise AttributeError with the package-level message
    missing_symbol = "DOES_NOT_EXIST"
    with pytest.raises(
        AttributeError,
        match=f"module 'degenbot' has no attribute '{missing_symbol}'",
    ):
        getattr(degenbot, missing_symbol)
