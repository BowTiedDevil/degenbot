"""Pipeline-side wiring of SkipGate: counter replay for memoized V4 rejects (2CBDPR).

The memo short-circuit in `PathRegistrationPipeline._consume` must keep the
summary counters byte-compatible with the first-attempt branches, so the
`[build_paths] Progress` breakdown looks identical whether a rejection is
seen live or replayed from the memo.
"""

from types import SimpleNamespace

from degenbot.runner.build_paths import PathRegistrationPipeline


def make_pipeline() -> PathRegistrationPipeline:
    ctx = SimpleNamespace(
        bot=None,
        chain_id=1,
        db=None,
        uniswap_v3_tracker=None,
        sushiswap_v3_tracker=None,
        pancakeswap_v3_tracker=None,
        weth=None,
    )
    return PathRegistrationPipeline(context=ctx, engine_registry=None)


def test_memo_replay_hook_rejection_counts_admission() -> None:
    p = make_pipeline()
    assert p._reject_v4_from_memo("v4-hook-rejected") is True
    assert p.v4_hook_rejected == 1
    assert p.v4_dynamic_fee_rejected == 0
    assert p._skip_reasons["v4-hook-rejected"] == 1


def test_memo_replay_dynamic_fee_counts_admission() -> None:
    p = make_pipeline()
    assert p._reject_v4_from_memo("v4-dynamic-fee-rejected") is True
    assert p.v4_dynamic_fee_rejected == 1
    assert p.v4_hook_rejected == 0
    assert p._skip_reasons["v4-dynamic-fee-rejected"] == 1


def test_memo_replay_plain_failure_is_not_admission() -> None:
    p = make_pipeline()
    assert p._reject_v4_from_memo("build-v4:HighFeePoolRejectedError") is False
    assert p.v4_hook_rejected == 0
    assert p.v4_dynamic_fee_rejected == 0
    assert p._skip_reasons["build-v4:HighFeePoolRejectedError"] == 1
