"""Vulture false-positive whitelist.

Each entry references a name vulture flags as "unused" that is actually a
protocol/API contract the static analyzer can't see through. Run
`just dead-code` to regenerate the vulture report; a clean codebase exits 0
with this whitelist, so any *new* dead code stands out immediately.

Add to this file when vulture flags a name that is:
- a required parameter in a framework-signatured method (SQLAlchemy
  ``TypeDecorator``, context-manager ``__exit__``, dunder protocols),
- a parameter in a ``Protocol`` definition (part of the documented contract),
- a name used only in string-form type annotations (``cast("..."``,
  ``Annotated["Foo", ...]``) — vulture doesn't parse str-annotations,
- or any other shape that can't be removed without uglifying the public API.

Do **not** add to this file to silence real dead code — delete the code instead.
Regenerate candidate entries from a clean tree with:
    vulture src/degenbot --min-confidence 80 --make-whitelist
"""

preserve_capture  # anvil_fork.py: AnvilFork.__init__ keyword arg, API-stable call-site compat
BlockParams  # cli/aave.py: imported for string-form forward refs in cast() — vulture can't parse str-annotations
dialect  # database/models/base.py: SQLAlchemy TypeDecorator.process_bind_param signature (framework-required)
dialect  # database/models/base.py: SQLAlchemy TypeDecorator.process_result_value signature (framework-required)
exc_type  # provider/__init__.py: __exit__ context-manager protocol parameter
exc_val  # provider/__init__.py: __exit__ context-manager protocol parameter
exc_tb  # provider/__init__.py: __exit__ context-manager protocol parameter
publisher  # types/concrete.py: Subscriber.notify Protocol parameter, part of the documented pub-sub contract
