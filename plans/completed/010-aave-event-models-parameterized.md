# Plan 010: Parameterize Aave Event Model Taxonomy

## Overview

Replace the 18 Pydantic event model classes with a single
`EnrichedScaledTokenEvent` parameterized by a `TokenEventDescriptor` frozen
dataclass. The descriptor encodes `(category, direction, accrual_type)`,
eliminating the need for 18 nearly-identical classes.

## Files Involved

- **Existing:**
  - `src/degenbot/aave/events.py` — `ScaledTokenEventType` enum (18 members)
  - `src/degenbot/aave/models.py` — 18 `Enriched*Event` classes + base classes
  - `src/degenbot/aave/enrichment.py` — `class_map` with 18 entries
- **Rewrite:**
  - `src/degenbot/aave/models.py` — collapse 18 classes to 1 + descriptor
- **Updates:**
  - `src/degenbot/aave/enrichment.py` — replace `class_map` with descriptor-based build
  - `src/degenbot/aave/events.py` — keep enum but add helper to derive descriptor

## Problem

`ScaledTokenEventType` has 18 members mapping to 18 Pydantic classes. The
taxonomy is structured along three orthogonal axes:

| Axis | Values |
|------|--------|
| Token category | COLLATERAL, DEBT, GHO_DEBT |
| Direction | MINT, BURN, TRANSFER |
| Accrual type | OPERATION (standard), INTEREST |

Most classes are structurally identical:
- `EnrichedCollateralMintEvent` vs `EnrichedDebtMintEvent` — same fields,
  same `IndexScaledEvent` base, different `event_type` default
- `EnrichedCollateralInterestMintEvent` — same fields again, just a different
  `event_type`

`_create_enriched_event` in enrichment has a `class_map` with 18 entries and
an `interest_event_type_map` with 6 entries. Adding a new event type requires
editing 4 files.

The taxonomy duplicates information. The `event_type` field already encodes
category + direction + accrual type. The class hierarchy just enforces
defaults for that field.

## Target State

### `events.py` — descriptor derivation

```python
from enum import Enum, auto
from dataclasses import dataclass


class TokenCategory(Enum):
    COLLATERAL = auto()
    DEBT = auto()
    GHO_DEBT = auto()


class TokenDirection(Enum):
    MINT = auto()
    BURN = auto()
    TRANSFER = auto()


class AccrualType(Enum):
    OPERATION = auto()
    INTEREST = auto()


@dataclass(frozen=True, slots=True)
class TokenEventDescriptor:
    """Fully describes an event type along three orthogonal axes.

    The descriptor is the single source of truth for:
    - Which fields are relevant
    - Which validation rules apply
    - Which TokenMath method to use for validation
    """

    category: TokenCategory
    direction: TokenDirection
    accrual_type: AccrualType

    @property
    def is_collateral(self) -> bool:
        return self.category == TokenCategory.COLLATERAL

    @property
    def is_debt(self) -> bool:
        return self.category in (TokenCategory.DEBT, TokenCategory.GHO_DEBT)

    @property
    def is_gho(self) -> bool:
        return self.category == TokenCategory.GHO_DEBT

    @property
    def is_mint(self) -> bool:
        return self.direction == TokenDirection.MINT

    @property
    def uses_index(self) -> bool:
        return self.direction in (TokenDirection.MINT, TokenDirection.BURN)

    @property
    def validation_method(self) -> str:
        """Map to TokenMath method name for model validation."""
        if self.accrual_type == AccrualType.INTEREST:
            return ""  # Interest: no TokenMath validation
        prefix = "collateral" if self.is_collateral else "debt"
        dir_ = "mint" if self.is_mint else "burn"
        return f"get_{prefix}_{dir_}_scaled_amount"
```

### `events.py` — enum to descriptor mapping (single source of truth)

```python
_SCALED_TOKEN_EVENT_DESCRIPTORS: dict[ScaledTokenEventType, TokenEventDescriptor] = {
    ScaledTokenEventType.COLLATERAL_MINT: TokenEventDescriptor(COLLATERAL, MINT, OPERATION),
    ScaledTokenEventType.DEBT_MINT: TokenEventDescriptor(DEBT, MINT, OPERATION),
    # ... etc for all 18 types
}


def get_descriptor(event_type: ScaledTokenEventType) -> TokenEventDescriptor:
    return _SCALED_TOKEN_EVENT_DESCRIPTORS[event_type]
```

### `models.py` — single parameterized class

```python
class EnrichedScaledTokenEvent(BaseModel):
    """Unified enriched event model.

    Previously split into 18 classes. Now parameterized by descriptor
    which controls validation, defaults, and field presence.
    """

    model_config = {"frozen": True}

    # Core identity
    event: LogReceiptField
    descriptor: TokenEventDescriptor  # replaces event_type as source of truth
    user_address: ChecksumAddress

    # Amount fields
    raw_amount: int
    scaled_amount: int | None

    # Context
    pool_revision: int
    token_revision: int
    token_address: ChecksumAddress
    underlying_asset: ChecksumAddress

    # Index-scaled fields (present when descriptor.uses_index)
    index: int | None = None
    balance_increase: int | None = None

    # Transfer fields (present when direction == TRANSFER)
    from_address: ChecksumAddress | None = None
    to_address: ChecksumAddress | None = None

    # GHO fields (present when category == GHO_DEBT)
    discount_percent: int | None = None
    discount_scaled: int | None = None

    # Mint/burn specific fields
    caller_address: ChecksumAddress | None = None
    target_address: ChecksumAddress | None = None

    @property
    def event_type(self) -> ScaledTokenEventType:
        """Derived from descriptor for backward compatibility."""
        return _EVENT_TYPE_FROM_DESCRIPTOR[self.descriptor]

    @property
    def is_collateral(self) -> bool:
        return self.descriptor.is_collateral

    @property
    def is_debt(self) -> bool:
        return self.descriptor.is_debt

    # ... other properties delegate to descriptor

    @model_validator(mode="after")
    def validate_scaled_amount(self) -> "EnrichedScaledTokenEvent":
        """Validate using descriptor-based rules."""
        if self.scaled_amount is None:
            return self
        if self.descriptor.accrual_type == AccrualType.INTEREST:
            return self  # No TokenMath for interest

        method_name = self.descriptor.validation_method
        if not method_name:
            return self

        token_math = TokenMathFactory.get_token_math_for_token_revision(self.token_revision)
        method = getattr(token_math, method_name)
        expected = method(self.raw_amount, self.index)

        if self.scaled_amount != expected:
            raise ScaledAmountValidationError(...)

        return self

    @model_validator(mode="after")
    def validate_gho_fields(self) -> "EnrichedScaledTokenEvent":
        """GHO events require discount fields."""
        if self.descriptor.is_gho:
            if self.discount_percent is None or not (0 <= self.discount_percent <= 10000):
                raise ValueError("GHO events require discount_percent 0-10000")
        return self
```

### `enrichment.py` — descriptor-based build

```python
# OLD: 18-entry class_map
class_map = {
    ScaledTokenEventType.COLLATERAL_MINT: EnrichedCollateralMintEvent,
    ScaledTokenEventType.COLLATERAL_BURN: EnrichedCollateralBurnEvent,
    # ... 16 more
}


# NEW: descriptor-based builder
def build_enriched_event(
    event: ScaledTokenEvent,
    operation: Operation,
    raw_amount: int,
    scaled_amount: int | None,
    token_revision: int,
    token_address: ChecksumAddress,
    underlying_asset: ChecksumAddress,
) -> EnrichedScaledTokenEvent:
    descriptor = get_descriptor(event.event_type)

    # Determine actual descriptor (interest accrual override)
    if operation.operation_type == OperationType.INTEREST_ACCRUAL:
        descriptor = TokenEventDescriptor(
            category=descriptor.category,
            direction=descriptor.direction,
            accrual_type=AccrualType.INTEREST,
        )

    kwargs = {
        "event": event.event,
        "descriptor": descriptor,
        "user_address": event.user_address,
        "raw_amount": raw_amount,
        "scaled_amount": scaled_amount,
        # ...
    }

    # Add direction-specific fields
    if descriptor.direction == TokenDirection.MINT:
        kwargs["caller_address"] = event.caller_address
    elif descriptor.direction == TokenDirection.BURN:
        kwargs["from_address"] = event.from_address or event.user_address
        kwargs["target_address"] = event.target_address
    elif descriptor.direction == TokenDirection.TRANSFER:
        kwargs["from_address"] = event.from_address or event.user_address
        kwargs["to_address"] = event.target_address or event.user_address

    # Add GHO fields
    if descriptor.is_gho:
        kwargs["discount_percent"] = getattr(event, "discount_percent", 0)
        kwargs["discount_scaled"] = getattr(event, "discount_scaled", 0)

    # Add index fields
    if descriptor.uses_index:
        kwargs["index"] = event.index
        kwargs["balance_increase"] = event.balance_increase

    return EnrichedScaledTokenEvent(**kwargs)
```

## Migration Steps

1. **Create `TokenEventDescriptor` and descriptor mapping** in `events.py`.
2. **Rewrite `models.py`**:
   - Replace `BaseEnrichedScaledTokenEvent`, `IndexScaledEvent`, `TransferEvent`,
     `InterestAccrualEvent` and all 18 concrete classes with single
     `EnrichedScaledTokenEvent`.
   - Preserve all validation logic but route through descriptor.
   - Keep `is_collateral`, `is_debt`, `is_burn`, `is_mint` as properties.
3. **Rewrite `enrichment.py`** `_create_enriched_event` to use descriptor builder.
4. **Add `event_type` property** on unified model for backward compatibility.
5. **Search/replace** all type-specific references in codebase (e.g.,
   `isinstance(event, EnrichedCollateralMintEvent)` → `event.descriptor.category == COLLATERAL`).
6. **Run all tests.**

## Test Strategy

**Red phase:** Before collapsing classes, write tests that exercise each
unique combination of (category, direction, accrual_type) through the unified
model. These become the contract.

```python
DESCRIPTOR_COMBINATIONS = [
    (COLLATERAL, MINT, OPERATION),
    (COLLATERAL, BURN, OPERATION),
    (COLLATERAL, TRANSFER, OPERATION),
    (DEBT, MINT, OPERATION),
    (DEBT, BURN, OPERATION),
    (DEBT, TRANSFER, OPERATION),
    (GHO_DEBT, MINT, OPERATION),
    (GHO_DEBT, BURN, OPERATION),
    (GHO_DEBT, TRANSFER, OPERATION),
    (COLLATERAL, MINT, INTEREST),
    (DEBT, BURN, INTEREST),
    (GHO_DEBT, MINT, INTEREST),
]


@pytest.mark.parametrize("descriptor", DESCRIPTOR_COMBINATIONS)
def test_event_builds_with_descriptor(descriptor):
    event = EnrichedScaledTokenEvent(
        descriptor=descriptor,
        event=FAKE_LOG_RECEIPT,
        user_address=FAKE_ADDRESS,
        raw_amount=1000,
        scaled_amount=500,
        pool_revision=5,
        token_revision=5,
        token_address=FAKE_ADDRESS,
        underlying_asset=FAKE_ADDRESS,
        # Optional fields validated conditionally by descriptor
    )
    assert event.descriptor == descriptor
```

**Green phase:**

| Test module | Coverage |
|-------------|----------|
| `tests/aave/test_models_descriptor.py` | All 18 combinations build and validate |
| `tests/aave/test_models_gho.py` | GHO fields required only for GHO descriptors |
| `tests/aave/test_models_validation.py` | TokenMath validation: correct method called per descriptor |

**Regression:** All existing model tests pass. Enrichment tests pass.
Position analysis tests pass (they consume enriched events).

## Risks

| Risk | Mitigation |
|------|------------|
| Pydantic discriminated unions used elsewhere (`EnrichedScaledTokenEvent` union type) | Replace union with single type; add `descriptor.category` checks where union was used for dispatch |
| `isinstance()` checks on old classes in CLI or other modules | `grep -r "Enriched.*Event" src/` to find all references; convert to descriptor checks |
| Model serialization (to/from JSON) changes | Add custom serializer/deserializer if needed; test roundtrip |
| `event_type` enum used as dict keys | Add bidirectional mapping: `descriptor → event_type` and `event_type → descriptor` |

## Rollback

Keep old `models.py` as `models_legacy.py` during migration. Update imports
to use new model. If tests fail, revert imports.

## Completion Summary

**All 18 Pydantic classes replaced by a single `EnrichedScaledTokenEvent` class.**

**Design decision:** Instead of the `TokenEventDescriptor` dataclass proposed in the plan,
the implementation uses the existing `ScaledTokenEventType` enum directly. Properties
(`is_collateral`, `is_debt`, `is_mint`, `is_burn`, `is_transfer`, `is_gho`, `is_interest`)
derive from module-level sets keyed by the enum. This is simpler and more Pythonic
than a separate descriptor dataclass — the enum already encodes all three axes
(category, direction, accrual type).

**Files changed:**
- `src/degenbot/aave/models.py` — 513 → 353 lines. Deleted 18 concrete classes, 3 base
  classes (BaseEnrichedScaledTokenEvent, IndexScaledEvent, TransferEvent, InterestAccrualEvent),
  and the 15-member type union. Created single `EnrichedScaledTokenEvent` with all optional
  fields and `model_validator` routing TokenMath validation by event_type.
- `src/degenbot/aave/enrichment/context.py` — Removed 15 class imports and two `class_map`
  dicts. `build_enriched_event()` now directly instantiates `EnrichedScaledTokenEvent(**kwargs)`.
- 12 handler test files — Replaced `isinstance(result, SpecificClass)` with
  `result.event_type == ScaledTokenEventType.X`, removed `class_map` lookup patterns.
- `tests/aave/test_models_unified.py` — New test file: 48 tests covering construction,
  validation routing, GHO discount validation, immutability, and parametrized property
  derivation for all 15 event types.

**Test results:** 306 Aave tests pass (258 existing + 48 new).

**Definition of Done:**

- [x] 18 Pydantic classes replaced by 1 (`EnrichedScaledTokenEvent`)
- [x] ~~`TokenEventDescriptor` fully describes all 18 event types~~ — Not needed;
      `ScaledTokenEventType` enum + module-level property sets is simpler
- [x] ~~Bidirectional mapping: `ScaledTokenEventType ↔ TokenEventDescriptor`~~ —
      Not needed; event_type IS the source of truth
- [x] `event_type` field provides full classification (replaces `event_type` property)
- [x] All `isinstance(event, OldClass)` replaced with event_type/property checks
- [x] All model tests pass
- [x] All enrichment tests pass
- [x] All downstream tests (position analysis, CLI) pass
- [ ] `models.py` < 200 lines — 353 lines (includes validation logic that was previously
      split across 3 base classes; the class definitions themselves are gone)
