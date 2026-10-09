C3.2 (I5OO6J): gate stats now ride SolveOutcome.gate_stats.
Deleted the gate stats TLS handshake (gate_tls / reset_gate_stats / take_last_gate_stats) from degenbot-solvers::profit_envelope; the gate accumulates a local GateStats and returns it (path_profit_bound_with_floor -> (Envelope, GateStats); new path_profit_bound_with_stats for offline harnesses). lane_walk reads outcome.gate_stats into the gate_total merge + K-slowest gate split.
Gates: cargo test -p degenbot-solvers PASS; cargo test -p degenbot-bot PASS.
