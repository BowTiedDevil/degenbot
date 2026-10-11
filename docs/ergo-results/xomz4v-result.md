# XOMZ4V result — deferred start (construction dial-free engines)

## What shipped

- `HostedSources::mint` is now SYNC and dial-free: it provisions the hub rings (per-arm `PendingTxPump::provision_on_ring`), the driver streams, the dedicated head-watch runtime (the shared `OwnedPubsubRuntime`), and stores the head clock + trigger — but starts nothing. A `started` AtomicBool is the deferred-I/O latch.
- `HostedSources::start` (NEW, exactly-once via the latch) is the engine's start boundary: launches each arm's feed pump (its first dial), the feed-telemetry sampler, connects the head-watch WS on the OWNED runtime (the second dial), registers the hub head source, and spawns the head edge.
- The feeds (`backrun_feed.rs`, `txpool_feed.rs`, `pending_tx_stream.rs`): provision/start split — the ring and views are readable at provision; only the reconnect loop launches at `PendingTxPump::start`.
- `register.rs`/`strategy.rs`: the engine start flow calls `start_hosted_sources` BEFORE the drivers (each driver's loop subscribes the hub head source the head edge registers as it starts), with the GIL released across the start so a slow WS connect never parks the interpreter.
- `boot_hosted_arms` no longer block_on's mint (mint is sync) — construction performs zero network I/O.
- Three new guards: `minting_performs_zero_connection_attempts` (a counting TCP listener — red by construction if any dial lands at mint), `starting_performs_the_deferred_dials`, `dropping_a_never_started_owner_tears_down_cleanly`.
- The chunk-B guards updated WITH REASON (they now start the hosted sources before asserting the stop path); no assertion weakened.

## PM review

- Cold diff: the acceptance shape met in full — zero dials at mint, start launches on the owned runtime, guards red-capable, drop-before-start clean, behavior-neutral for never-resumed tests (the suite stayed green untouched).
- Gates re-run by PM: degenbot-bot / degenbot-rpc / degenbot_rs green; clippy, fmt, comment-hygiene, and the config-derived-static ratchet green.
- Negative probe (PM-run): restoring the eager dial at mint (one-line `pump.start()` in mint) trips `minting_performs_zero_connection_attempts` RED with "mint must perform zero connection attempts"; restored -> green.
- Full ladder: `just dev` build clean; the full suite 3090 passed, 35 skipped, exit 0, ZERO spray signatures.

## Worker sign-off note

The dispatched worker's followUp was a stray one-character message; this result doc is the PM review record (the diff, not the story, is the evidence).
