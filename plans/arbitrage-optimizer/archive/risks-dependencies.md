# Risks & Dependencies

## Risks & Mitigations

| Risk | Impact | Likelihood | Mitigation |
|------|--------|------------|------------|
| Float64 precision limits for extreme reserves | Medium | Low | Validated up to uint128 scale (87 bits); ratios preserve precision. Rust u256 for EVM-exact validation. |
| V3 tick crossing algebra wrong | High | Low | Validated against V3 compute_swap_step integer math. Piecewise-Möbius cross-validated with brute-force search. |
| Rust PyO3 build complexity | Medium | Medium | Stable ABI via maturin; CI builds wheels for common platforms. |
| Rust integer overflow (u256 arithmetic) | Medium | Low | Use U256/U512 types from `primitive-types` crate; extensive fuzz testing. |
| Möbius overflow for very long paths | Medium | Low | Log-domain batch implementation handles arbitrary K×M magnitudes. |
| Gas cost not modeled | High | High | Optimizer may recommend net-negative trades. Add gas to objective function (pending). |
| Tick bitmap stale data | High | Medium | Block-based cache invalidation; fallback to fresh RPC call. |
| CVXPY solver license issues | Low | Low | Use open-source solvers (CLARABEL, SCS, ECOS). CVXPY is research-only now. |
| Python GIL bottleneck for parallel | High | Certain | Use Rust (no GIL) or multiprocessing. Thread parallelism confirmed non-viable. |
| Numpy view mutation bugs | Medium | Low | Identified and fixed. Use `x = x * y` instead of `x *= y` on views. |

## Dependencies

### Required

- `scipy` — Brent optimizer, L-BFGS-B solver (already installed)
- `numpy` — Vectorized batch processing (already installed)

### Required (Rust Extension)

- `rust` toolchain — `rustc`, `cargo` (1.70+)
- `pyo3` — Python bindings for Rust (in Cargo.toml)
- `maturin` — Build and publish Rust Python extensions
- `primitive-types` — U256/U512 types for integer Möbius

### Optional (Research)

- `cvxpy` — Convex optimization library (installed)
- `clarabel` — Open-source solver (installed)
- `mosek` — Commercial solver for production (consider licensing)

### Optional (Future)

- `cupy` — GPU-accelerated NumPy (requires CUDA GPU)
- `rayon` — Rust parallelism crate (in Cargo.toml)

## Architecture Risks

### Rust/Python Boundary

| Risk | Impact | Mitigation |
|------|--------|------------|
| Serialization overhead at PyO3 boundary | Low | Use NumPy arrays for batch transfer; avoid per-element conversion |
| Rust build failures in CI | Medium | Pre-built wheels via maturin; fallback to pure Python |
| API divergence between Python and Rust implementations | Medium | Shared test suite validates both; Rust mirrors Python API |
| Memory safety for u256 arithmetic | Low | Rust's ownership model; extensive test coverage |

### Performance Risks

| Risk | Impact | Mitigation |
|------|--------|------------|
| Rust f64 vs u256 divergence | Low | Cross-validate: f64 for screening, u256 for final check |
| Batch size regression | Medium | Benchmark at multiple sizes (100, 1000, 10000); vectorization crossover at ~20-50 paths |
| Cache invalidation bugs | High | Block number tracking; unit tests for stale data detection |

## References

- [An Efficient Algorithm for Optimal Routing Through CFMMs](https://arxiv.org/abs/2304.00223) — Diamandis, Resnick, Chitra, Angeris (2023) — **Key paper for dual decomposition method**
- [CFMMRouter.jl](https://github.com/bcc-research/CFMMRouter.jl) — Reference implementation in Julia
- [CVXPY DPP Tutorial](https://www.cvxpy.org/tutorial/dpp/index.html)
- [CLARABEL Solver](https://github.com/oxfordcontrol/Clarabel.rs)
- [Uniswap V3 Whitepaper](https://uniswap.org/whitepaper-v3.pdf) — For tick-based liquidity modeling
- [Constant Function Market Makers](https://arxiv.org/abs/2103.08842) — AMM mathematical framework
- Hartigan, J. (2026). "The Geometry of Arbitrage: Generalizing Multi-Hop DEX Paths via Möbius Transformations." Medium.
- [PyO3 User Guide](https://pyo3.rs/) — Rust Python bindings
- [Maturin Documentation](https://www.maturin.rs/) — Build and publish Rust Python extensions
