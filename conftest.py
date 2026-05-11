"""
Sybil-powered doctest for README.md.

Validates that Python code blocks in the project README remain importable
and syntactically valid after refactors.

NOTE: This conftest lives at the project root so Sybil can discover README.md
via its path='.' pattern. It is intentionally not in tests/ because Sybil's
path matching is relative to the conftest location.
"""

from sybil import Sybil
from sybil.parsers.markdown.codeblock import PythonCodeBlockParser

pytest_collect_file = Sybil(
    parsers=[PythonCodeBlockParser()],
    path=".",
    pattern="README.md",
).pytest()
