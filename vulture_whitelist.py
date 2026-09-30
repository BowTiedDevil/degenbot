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


dialect  # database/models/base.py: SQLAlchemy TypeDecorator.process_bind_param signature (framework-required)
exc_type  # provider/__init__.py + provider/sync.py: __exit__ context-manager protocol parameter
exc_val  # provider/__init__.py + provider/sync.py: __exit__ context-manager protocol parameter
exc_tb  # provider/__init__.py + provider/sync.py: __exit__ context-manager protocol parameter

# uniswap/v4_liquidity_pool.py: ``Hooks`` is a faithful mirror of v4-core
# Hooks.sol's caller bitmap. Members Python never reads still document the
# protocol contract; dropping half the enum is worse than keeping it.
BEFORE_INITIALIZE
AFTER_INITIALIZE
BEFORE_ADD_LIQUIDITY
AFTER_ADD_LIQUIDITY
BEFORE_REMOVE_LIQUIDITY
AFTER_REMOVE_LIQUIDITY
BEFORE_DONATE
AFTER_DONATE
AFTER_ADD_LIQUIDITY_RETURNS_DELTA
AFTER_REMOVE_LIQUIDITY_RETURNS_DELTA
