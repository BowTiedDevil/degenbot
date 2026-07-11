"""Constants and enums for Aave V3 CLI processing.

Slimmed by the §4.2 writer retirement (CZM7TI): ``UserOperation``,
``LIQUIDATION_OPERATION_TYPES``, and ``GHO_DISCOUNT_DEPRECATION_REVISION``
were used only by the deleted Python writer pipeline (event handlers,
operations parser, token/transaction processors, db-verification invariants)
— the Rust core owns those dispatch paths now. What remains is the display
limit used by the ``aave position risk`` read-back command.
"""

# Display limit for position risk analysis output
POSITION_RISK_DISPLAY_LIMIT = 20
