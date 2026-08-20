5D3YVK — completed 2026-08-20 (commit 89390c222).
Tier-3 CL parity oracles relocated from degenbot-pools/tests to degenbot-simulation/tests,
deleting the publish-blocking dev-dep edge (pools -> simulation -> bot -> pools).
Gates: 3/3 relocated binaries build; 21/21 relocated tests green (v3 9, v4 9, pancake 3)
on committed tier3-oracle artifacts; degenbot-pools 230/230 green edge-free;
check-no-pyo3-in-cores green. Exception wording retired in MYYV2X/S7R5K4/TGO5ZY.
Policy (user): never publish with --no-verify.
