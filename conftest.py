"""
Sybil-powered doctest for README.md.

Validates that Python code blocks in the project README remain importable
and syntactically valid after refactors.

This conftest lives at the project root so Sybil can discover README.md
relative to its path. It is picked up because pyproject.toml includes
"README.md" in testpaths.
"""

from sybil import Sybil
from sybil.parsers.markdown.clear import ClearNamespaceParser
from sybil.parsers.markdown.codeblock import PythonCodeBlockParser
from sybil.parsers.markdown.skip import SkipParser

pytest_collect_file = Sybil(
    parsers=[PythonCodeBlockParser(), SkipParser(), ClearNamespaceParser()],
    path=".",
    patterns=["README.md"],
    excludes=["*/README.md"],
).pytest()
