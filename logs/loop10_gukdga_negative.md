## GUKDGA — done (measured negative; reverted, zero diff)

Conservative approximate-key cascade: implemented green-first (containment test proves every certified drop is exact-drop), then measured and reverted.

gate_bench per-path splits before/after:
- pid 7036: s1 31→13us but hull 100→162us, gate 288→326us.
- pid 3692: s1 53→20us but hull 90→312us, gate 353→575us.

The cascade certifies fewer near-tied dominator clusters (their margins are wei-scale); the extra survivors flood the exact hull (each survivor reduced with wide division). A hybrid cleaning pass over survivors cannot repair it: cascade-dropped dominators are unavailable to the cleaning sweep, so survivors stay contaminated.

Conclusion: any stage-1 replacement must resolve near-tie domination EXACTLY. The exact endpoint sweep stays. Next: exact wavefront hull (IQ7DN6), which never reduces non-hull lines.
