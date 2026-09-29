"""degenbot.types.rpc_types — the BlockTag taxonomy.

Pins :meth:`BlockTag.parse` (the ONE string → member conversion, with its
exact ``Unsupported block identifier: …`` ValueError contract — the provider
ladder's loud unsupported-identifier shape) and :meth:`BlockTag.to_block_number`
(tag → concrete number against the current head).
"""

from __future__ import annotations

import pytest

from degenbot.types.rpc_types import BlockTag


@pytest.mark.parametrize(
    ("raw", "expected"),
    [
        ("earliest", BlockTag.EARLIEST),
        ("latest", BlockTag.LATEST),
        ("pending", BlockTag.PENDING),
    ],
)
def test_parse_maps_the_three_canonical_tags(raw: str, expected: BlockTag) -> None:
    assert BlockTag.parse(raw) is expected


def test_parse_raises_the_exact_message_on_an_unsupported_identifier() -> None:
    # The exact contract callers pin against — NOT the raw enum error text.
    with pytest.raises(ValueError) as ei:
        BlockTag.parse("banana")
    assert str(ei.value) == "Unsupported block identifier: 'banana'"


@pytest.mark.parametrize("raw", ["LATEST", "latest ", "finalized", "safe", ""])
def test_parse_rejects_non_canonical_spellings(raw: str) -> None:
    # Case drift and the post-merge tags the taxonomy deliberately omits.
    with pytest.raises(ValueError, match="Unsupported block identifier"):
        BlockTag.parse(raw)


@pytest.mark.parametrize(
    ("tag", "expected"),
    [
        (BlockTag.LATEST, 100),
        (BlockTag.EARLIEST, 0),
        (BlockTag.PENDING, 101),
    ],
)
def test_to_block_number_resolves_against_the_head(tag: BlockTag, expected: int) -> None:
    assert tag.to_block_number(100) == expected


def test_members_compare_equal_to_their_wire_spelling() -> None:
    # StrEnum: the read boundary keeps comparing against persisted/wire strings.
    assert BlockTag.LATEST == "latest"
