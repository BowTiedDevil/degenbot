# HANDOFF: Your mission — deploy, wire, run, profit

> Worker-facing document. Delivered verbatim to the worker agent.
> This exercise is hands-off: no one will help you, diagnose for you, or
> confirm your hypotheses.

## Goal

You are given:

1. A private key in `./bot.env` (`PRIVATE_KEY`), funded with a small amount of Ether.
2. Free access to an RPC endpoint for **Ethereum mainnet** (see *Environment*).
3. Full access to this repository — you may read and modify anything.

**Deploy a smart contract you control, build/configure a bot from this repo that is wired to that contract, run the bot live, and capture profits on-chain.**

"Capture profits" means: demonstrate, with on-chain evidence a skeptic can verify, that your activity ended with more ETH-denominated value than it started with, after gas.

## Environment

- The repo is `/workspaces/degenbot`. Start with `AGENTS.md`; the repo documents itself extensively — trust committed docs, but verify against code.
- `cast`/`forge`, `uv`, `cargo`, and Python 3.12+ are installed. A pool-state database may already exist at `~/.config/degenbot/degenbot.db`. Measured environment facts live in `docs/autonomous-user-journey/ENVIRONMENT_FINDINGS.md` and relay guidance in `docs/autonomous-user-journey/RELAYS_AND_GUARDRAILS.md` — both are yours to read.
- The RPC endpoint is how you reach Ethereum. Treat every transaction as real: gas is real money, and anything you broadcast can be seen and competed with publicly. There is no sandbox: spend deliberately, and keep a running tally of gas spent.
- Read `AGENTS.md` carefully — it contains repo-specific operational rules (build verification after Rust edits, planning conventions). You are expected to follow them.

## Hard rules (violating any of these ends the exercise)

1. **Endpoint allowlist (closed — do not extend it).** Transactions may leave ONLY through (a) the provided RPC, and/or (b) exactly these three revert-protecting private builder endpoints, each chosen because its operator documents a no-reverting-inclusion guarantee (a failed/raced attempt costs zero gas):
   - `https://rpc.flashbots.net?hint=hash` (Flashbots Protect, hash-only hint; Protect's documented policy: "transactions are only included if they do not revert")
   - `https://rpc.mevblocker.io/noreverts` (MEV-Blocker, explicit revert protection)
   - `https://rpc.mevblocker.io/fullprivacy` (MEV-Blocker, strongest privacy variant)
   **Do not research, propose, or use any other endpoint.** If you believe none of these work for a submission, don't submit — log it in the journal. Verify chain id 1 on any endpoint before its first use.
2. **Key hygiene.** Never print, log, commit, or paste the raw private key — including in your journal, echoed commands, or captured error output. Reference it only via `source bot.env` / `$PRIVATE_KEY`.
3. **Gas budget.** Cumulative `gasUsed × effectiveGasPrice` across all your transactions must stay below the announced budget. Track it.
4. **Stop on request.** A `STOP` file at the repo root ⇒ halt all bot processes and end cleanly.

## Scope freedom: the shipped defaults are a development posture, you may widen them

The example bots in this repo are configured for a *development environment*. They deliberately narrow the search space — chief among them (all verified in code):

- **Token whitelist**: paths may only route through a small set of intermediate tokens (`_driver_constants.ALLOWED_INTERMEDIATE_TOKENS`, ~16 majors — WETH, USDC, USDT, DAI, WBTC, …). This excludes fee-on-transfer/rebase tokens that waste simulation gas and always revert. Setting the set to `None` allows all tokens.
- **Path cap**: total registered arbitrage paths is capped (`DEGENBOT_MAX_PATHS`; `0` = uncapped) so registration load stays observable. Caveat worth the 10 seconds it takes to check: the *code* default is 100,000, but this devcontainer exports `DEGENBOT_MAX_PATHS=1000000`, and nothing in the boot logs echoes the effective value. Layered defaults are a repo theme — when a document states a number, `printenv` beats the docstring.
- **Execution strategy**: the shipped settlement-arbitrage adapter (`cmd_executor`) is the *default*, not the only option — the `ExecutionAdapter` seam (ADR-025, `docs/execution-strategy.md`) lets you bring a different executor contract / payload encoding entirely, and flags like `DEGENBOT_ERC6909_PROFIT` change how profit is captured on-chain.

**You may keep these restrictions verbatim — that is a fully legitimate choice — or remove caps, allow arbitrary tokens, or modify the execution strategy.** Widening costs are yours to manage: more paths mean more registration load, RAM, and solve work; arbitrary tokens re-admit FoT/tax tokens your simulations must survive (the dispatcher's FoT classifier exists for this reason). Whatever you choose: (1) note the change + rationale in the journal, (2) keep the hard rules (endpoint allowlist, key hygiene, gas budget), (3) remember the grading gate is *attempting profit capture* — a wider search that produces attempts beats a narrow one that produces none, but a narrow one that produces attempts is fine.

## What you are being graded on

You are GATED on attempting profit capture, not on winning it: at least one candidate your bot judged net-of-gas profitable that you actually broadcast live through the allowed revert-shielded endpoints, whose calldata replays successfully at its calculation block. Other searchers will win most races — losing costs you nothing under the shield and does not fail you. **Not attempting** when your own telemetry shows gate-clearing candidates — or failing to attempt because of a misconfiguration you didn't diagnose — does. A single landed profitable capture is full success.

There is an external observer watching read-only; it can kill the run (kill criteria only), it will not help you, answer "why isn't X working", or accept partial diagnostics. If your bot idles silently, the problem is observable somewhere — find the instrumentation before declaring the environment dead.

## Deliverables (all required)

1. **The deployed contract**: address + deployment tx hash.
2. **The bot running live**, with evidence it reached steady state (cite logs/telemetry).
3. **Profit evidence**: starting balances, ending balances, every one of your tx hashes, gas costs, net result — with the exact commands a third party can run to reproduce every number.
4. **A friction journal** at `logs/user-journey/journal.md`: timestamped entries for every dead end, doc consulted, wrong assumption, failed command, and repo modification — each classified (*undocumented default*, *wrong-doc trap*, *cryptic error*, *build/tooling trap*, *code defect*, *works-as-documented*). The journal is as important as the profit.
5. **A final summary** (`logs/user-journey/summary.md`): what you did in order; what you'd tell the next person; every place the repo's docs or defaults misled you.

## Suggested shape of the work (not a recipe)

- Orient first: what does this repo *do*, what is its documented bot, and what config surface does that bot actually read at runtime (file? env? CLI?).
- Prove the plumbing before the strategy: deploy cheap, make one call, get one mined receipt.
- Before going live, make the bot tell you why it is or isn't acting — a bot that runs silently forever is usually misconfigured, not unlucky.
- Simulate what the bot will do *before* real money moves, using whatever dry-run posture the repo offers, and confirm your simulated sender/config matches your deployed contract's expectations.
- Prefer the repo's documented executor contract and entrypoint over parallel machinery — but the execution-strategy seam exists if you judge the default unusable.
- When something fails, decide whether *you* are misconfigured or the *repo* is defective before editing repo code. Journal either conclusion.