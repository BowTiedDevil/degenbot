# Hull-edge seed evaluation (no-op verdict)

Evaluated seeding the active-set walk's first-piece right-edge bisection
(`piece_window_right_edge_seeded`, cl/crossings.rs) from the composed
bound hull's first growth-rate-takeover (bound slope crossing 1) to cut
grow-loop probes.

## Outcome: not shipped

The mechanism is sound (byte-identical goldens; the seed is an advisory
`hi` feeding the same validated grow loop), but it misses production:

- The walk's event solver fully supersedes the seeded bisection at the
  production-default stance (`event_solver_fallbacks` = 0 on all measured
  corpora: `heavy_cl_solve_captures`, `live_capture_loop13/17`,
  synthetic `cl_corpus`).
- Where the legacy stance runs, the measured win is small: −39/12 path
  probes per block (heavy corpus) against millions of probes spent
  elsewhere in the same corpora.
- The hull's first takeover frequently sits at the origin (surrogate
  peak at x = 0 for saturated paths), and where it exists it tends to
  overshoot the actual piece edge, so only a handful of probes move.

Maintenance cost (+174/−8 lines, four files) is not justified for a
legacy-branch-only, low-double-digit probe saving. Revert, and keep this
evaluation as the record for future ideas that would seed walk bisections
from envelope geometry: measure under the production stance first.

Rejected landing: the seed computation now lives only in git history of
the working tree that produced the measurements; no residual machinery.
