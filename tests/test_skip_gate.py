"""Session-scoped fatal-skip memo + skip-log dedupe for build_paths (2CBDPR).

``SkipGate`` keeps the per-block path-registration loop from re-building and
re-logging pools whose verdict can never change (typed admission rejections,
discovery data mismatches, duplicate registrations), and rate-limits log lines
for everything else.
"""

from degenbot.runner.skip_gate import SkipGate


def make_gate(cooldown: float = 60.0) -> tuple[dict[str, float], SkipGate]:
    clock: dict[str, float] = {"t": 1000.0}

    gate = SkipGate(cooldown_seconds=cooldown, now=lambda: clock["t"])
    return clock, gate


def test_first_note_logs() -> None:
    _clock, gate = make_gate()
    assert gate.note("v4", "0xaa", "v4-high-fee", fatal=True) is True


def test_repeats_within_cooldown_are_suppressed() -> None:
    clock, gate = make_gate()
    assert gate.note("v4", "0xaa", "v4-high-fee", fatal=True) is True
    for _ in range(5):
        assert gate.note("v4", "0xaa", "v4-high-fee", fatal=True) is False
    clock["t"] += 30.0
    assert gate.note("v4", "0xaa", "v4-high-fee", fatal=True) is False


def test_logs_again_after_cooldown_elapses() -> None:
    clock, gate = make_gate()
    assert gate.note("v4", "0xaa", "build-v4:ConnectionError") is True
    assert gate.note("v4", "0xaa", "build-v4:ConnectionError") is False
    clock["t"] += 61.0
    assert gate.note("v4", "0xaa", "build-v4:ConnectionError") is True


def test_fatal_memo_persists_across_cooldowns_and_reports_tag() -> None:
    clock, gate = make_gate()
    assert gate.note("v4", "0xaa", "v4-high-fee", fatal=True) is True
    clock["t"] += 61.0
    assert gate.fatal_tag("v4", "0xaa") == "v4-high-fee"
    # A later note still logs after the cooldown and must not disturb the memo.
    assert gate.note("v4", "0xaa", "v4-high-fee", fatal=True) is True
    assert gate.fatal_tag("v4", "0xaa") == "v4-high-fee"


def test_non_fatal_does_not_memo() -> None:
    _clock, gate = make_gate()
    assert gate.note("v4", "0xaa", "build-v4:ConnectionError") is True
    assert gate.fatal_tag("v4", "0xaa") is None


def test_non_fatal_note_upgrades_to_fatal_with_new_tag() -> None:
    clock, gate = make_gate()
    assert gate.note("v4", "0xaa", "build-v4:ConnectionError") is True
    clock["t"] += 61.0
    assert gate.note("v4", "0xaa", "v4-high-fee", fatal=True) is True
    assert gate.fatal_tag("v4", "0xaa") == "v4-high-fee"


def test_distinct_keys_are_independent() -> None:
    _clock, gate = make_gate()
    assert gate.note("v4", "0xaa", "v4-high-fee", fatal=True) is True
    assert gate.note("v4", "0xbb", "v4-high-fee", fatal=True) is True
    assert gate.note("v2", "0xaa", "build-v2:PoolAlreadyRegisteredError", fatal=True) is True
    assert gate.fatal_tag("v4", "0xaa") == "v4-high-fee"
    assert gate.fatal_tag("v4", "0xbb") == "v4-high-fee"
    assert gate.fatal_tag("v2", "0xaa") == "build-v2:PoolAlreadyRegisteredError"


def test_default_clock_is_monotonic_time() -> None:
    gate = SkipGate()
    assert gate.note("v4", "0xaa", "tag") is True
    assert gate.note("v4", "0xaa", "tag") is False
