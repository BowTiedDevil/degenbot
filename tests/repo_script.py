"""Load a repository script as a module the way running it would.

The scripts under ``scripts/`` are not an importable package, so tests reach
them through a path spec. Two things make such a load behave like the script's
own execution: the module is registered under its name, because postponed
annotations resolve through ``sys.modules`` and ``dataclasses`` reads a module's
namespace from there, and the script's directory is on ``sys.path`` for the
load, because a script run by path gets it implicitly and its sibling imports
rely on it.
"""

from __future__ import annotations

import importlib.util
import sys
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from pathlib import Path
    from types import ModuleType


def load_script(name: str, path: Path) -> ModuleType:
    """Import ``path`` as the module ``name``, without running its entry point."""
    spec = importlib.util.spec_from_file_location(name, path)
    if spec is None or spec.loader is None:
        message = f"cannot load script: {path}"
        raise RuntimeError(message)
    module = importlib.util.module_from_spec(spec)
    scripts_dir = str(path.parent)
    added = scripts_dir not in sys.path
    if added:
        sys.path.insert(0, scripts_dir)
    sys.modules[name] = module
    try:
        spec.loader.exec_module(module)
    except BaseException:
        del sys.modules[name]
        raise
    finally:
        if added:
            sys.path.remove(scripts_dir)
    return module
