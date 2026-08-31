## PVOPYP — done: early-select tangent derivation on fat crossing tables

Measurement-first: new GATE_POSTPRUNE_REDUCE_NS / GATE_SAMPLE_NS splits + gate_bench drv/rdu/smpl columns showed the fat-family gate time was neither product nor prune: **derive** owned 60% (3.1ms of 5.1ms on [2500,4000,2500]; 45% on [1200,3000,1500]).

Fix: tangent lines get sampled to 32+last anyway; pre-select the sampled keep-indices and derive only those on tables with >32 keepable ranges. Same membership rule as the sampling cap → byte-identical sampled sets on every normal table.

A/B (gate_bench med):
| family | gate before | after | Δ |
|---|---|---|---|
| [2500,4000,2500] | 5129us | 3642us | −29% |
| [1200,3000,1500] | 3806us | 3017us | −21% |
| [1,390,3] | 413us | 317us | −23% |
| heavy corpus shapes | unchanged | unchanged | 0 |

Parity: heavy_cl replay floor lines byte-identical (420/420/465 skips, false skips 0), golden 692/701, deterministic 701/701; mixed replay divergent=0; lib 11/11 test files green.

Remaining derive floor on the synth bench = the owned crossings build (seq.crossings()); live paths pass caller-built crossing tables so live derive is already cheaper.
