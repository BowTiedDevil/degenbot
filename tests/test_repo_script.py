"""Tests for the shared repository-script loader."""

from __future__ import annotations

import importlib
import sys
from typing import TYPE_CHECKING

from tests.repo_script import load_script

if TYPE_CHECKING:
    from pathlib import Path


def test_a_loaded_script_is_importable_under_its_name(tmp_path: Path) -> None:
    """The module lands in ``sys.modules``, the way a real import would."""
    script = tmp_path / "probe_module.py"
    script.write_text("VALUE = 7\n", encoding="utf-8")

    module = load_script("probe_module", script)

    assert module.VALUE == 7
    assert importlib.import_module("probe_module") is module


def test_postponed_annotations_resolve_from_the_loaded_script(tmp_path: Path) -> None:
    """A script's own dataclasses read the annotations it declared."""
    script = tmp_path / "probe_dataclass.py"
    script.write_text(
        "from __future__ import annotations\n"
        "\n"
        "from dataclasses import KW_ONLY, dataclass\n"
        "\n"
        "\n"
        "@dataclass(frozen=True)\n"
        "class Probe:\n"
        "    first: int\n"
        "    _: KW_ONLY\n"
        "    second: str = 'tail'\n",
        encoding="utf-8",
    )

    module = load_script("probe_dataclass", script)

    assert module.Probe(1).second == "tail"


def test_a_sibling_import_resolves_during_the_load(tmp_path: Path) -> None:
    """Script mode has the script's directory on the path; the load matches it."""
    (tmp_path / "probe_sibling.py").write_text("VALUE = 3\n", encoding="utf-8")
    script = tmp_path / "probe_importer.py"
    script.write_text(
        "from probe_sibling import VALUE\n\nDOUBLED = VALUE * 2\n",
        encoding="utf-8",
    )

    module = load_script("probe_importer", script)

    assert module.DOUBLED == 6
    assert str(tmp_path) not in sys.path
