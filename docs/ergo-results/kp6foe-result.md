KP6FOE CLOSE — MEVBlocker backrun bridge soak evidence (2026-09-19 02:16-02:38 UTC, session dir 20260919T021624Z-3518945)

Loop: MEVBlocker WS feed (hub ring, dropped_ring named) -> LatestOnly head source -> staged engine -> nonce-lane submission -> gap quarantine -> finality tombstones -> resolution archive (332d7daf7) -> post-hoc report (ba49997a7). All slices of both sessions exercised live (B1 hub ring, B2 head-under-hub, B3 engine channels, RouteRegistry boot snapshot B5, quarantine archive 3S7JR6).

- Boot: journal backlog of 4 frames (our own prior submissions) folded, tentatives re-entered, and ALL 4 resolved via finality tombstones at finalized=26008426 into the new archive - the restart-fold + archive path on OUR frames. Archive records complete: by-identity on all three slot_taken (self-replacement nonces 160491/160492), finalized_block, resolved_unix_ms.
- Live cycle (foreign feed tx): park at gap intake 02:21:44 (0x30ed2167, 1inch router, claimed nonce 15) -> evidence classification 02:21:50 (mined, block 26008541 + carrying hash; 6s park-to-evidence via per-head probing) -> finality tombstone 02:37:15 (finalized=26008554) -> archive line append. Full lifecycle clocked end-to-end for the FIRST time in production.
- Soak totals (21 min): 10 frames, 6 mined, 4 slot_taken all with by-identity, 0 still-pending, 0 probe failures, head watch stable, zero hub drops observed in stdout, no errors/warns beyond normal observe reasons.
- Post-hoc: quarantine_report.py exit 0 consuming journal+archive against live node; wheel rebuilt fresh under the new IFYMUI receipt contract (verify-build-fresh OK).
- Bid mode was ON (budget 1e15 wei) throughout; every frame this window was foreign-observation intake - zero of our own submissions fired (no profitable candidates in-window above budget). Gas spend: 0.

Board: 3S7JR6 done, 5ZKVH6 done, WLZNMN canceled (user). Epic KP6FOE closed on this evidence. Remaining open board items (IFYMUI done by architecture, IRUBYU done by architecture; GOTEEG gated on second strategy family demand).
