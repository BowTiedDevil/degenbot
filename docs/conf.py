from __future__ import annotations

import tomllib
from pathlib import Path

# -- Project information -----------------------------------------------------

_repo_root = Path(__file__).resolve().parents[1]
try:
    _proj = tomllib.loads((_repo_root / "pyproject.toml").read_text())
    project = _proj["project"]["name"]
    author = ", ".join(_proj["project"]["authors"].get(i, {}).get("name", "") for i in range(3)) or "degenbot contributors"
except Exception:
    project, author = "degenbot", "degenbot contributors"

copyright = "2025, degenbot contributors"
version = "0.6"
release = version

# -- General configuration ---------------------------------------------------

extensions = [
    "myst_parser",
    "sphinx_copybutton",
    "sphinx.ext.intersphinx",
    "sphinx.ext.viewcode",
]

# Existing docs/ is plain markdown; MyST renders it as-is.
myst_heading_anchors = 3
myst_all_links_external = False
suppress_warnings = [
    "myst.header",               # existing files have h2/h3 skips
    "misc.highlighting_failure", # pre-tool 'mermaid' fences + pygments lacking a lexer for the 'solidity' snippets in docs/aave
]

# The Rust core is documented on docs.rs, not here (degenbot_rs is
# `publish = false`, so it never reaches docs.rs). Cross-link by URL as needed.
intersphinx_mapping = {
    "python": ("https://docs.python.org/3.12", None),
}

templates_path = ["_templates"]
exclude_patterns = [
    "_build", "Thumbs.db", ".DS_Store",
    "handoffs/*", "tasks/*", "gate-viz/*", "grafana/*",
    # untitled ergo-task artifacts; releases/ carries the site copy
    "release-notes/*",
]

# -- Options for HTML output -------------------------------------------------

html_theme = "furo"
html_static_path = ["_static"]
html_title = f"{project} documentation"

# Python API reference is generated from the PyO3 layer's stubs/docstrings;
# until that lands,see docs/index.md for the autodoc follow-up note.
