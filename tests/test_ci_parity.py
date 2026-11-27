"""CI-parity brake (`scripts/hooks/ci-parity.sh`).

Every ci.yml job declares how it is reproduced locally in
`scripts/hooks/ci-parity-manifest.txt`, whose lines are `job|kind|notes`. The
brake fails when a job has no manifest line, when a note names a `just` recipe
that does not exist, and when a line is not exactly three fields. The manifest
carries job completeness -- the hook-less jobs and the multi-recipe jobs; a
hook's own CI coverage is declared in `scripts/gates.toml` and validated by
`tests/test_gate_registry.py` (ADR-102), so it is not restated here.

These are the gate's negative probes: each mutation is applied to a throwaway
copy of the source and must produce an observed nonzero exit. The env overrides
(`MANIFEST`, `CI`, `JF`, `TOOLS`) exist for exactly that.
"""

from __future__ import annotations

import os
import re
import shutil
import subprocess  # ruff: ignore[suspicious-subprocess-import]
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
SCRIPT = REPO_ROOT / "scripts" / "hooks" / "ci-parity.sh"
MANIFEST = REPO_ROOT / "scripts" / "hooks" / "ci-parity-manifest.txt"
CI_YML = REPO_ROOT / ".github" / "workflows" / "ci.yml"
UV_VYPER_STEP = "Install uv (executor vyper toolchain)"


def _sh() -> str:
    """Absolute path to bash (S607: never launch a bare name off PATH)."""
    exe = shutil.which("bash")
    assert exe is not None, "bash is not installed on PATH"
    return exe


def _run_ci_parity(**overrides: str) -> subprocess.CompletedProcess[str]:
    env = dict(os.environ)
    for var in ("MANIFEST", "CI", "JF", "TOOLS"):
        env.pop(var, None)
    env.update(overrides)
    return subprocess.run(  # ruff: ignore[subprocess-without-shell-equals-true]
        [_sh(), str(SCRIPT)],
        cwd=REPO_ROOT,
        env=env,
        check=False,
        capture_output=True,
        text=True,
    )


def _mutate(tmp_path: Path, source: Path, old: str, new: str) -> Path:
    """Write `source` with its first `old` replaced by `new`; return the copy."""
    text = source.read_text(encoding="utf-8")
    assert old in text, f"the mutation anchor is gone from {source}: {old!r}"
    mutated = tmp_path / source.name
    mutated.write_text(text.replace(old, new, 1), encoding="utf-8")
    return mutated


def _ci_step(source: Path, name: str) -> str:
    """`source`'s named step as its `- name` and `uses` lines, action pin included.

    Deriving the anchor from the workflow keeps a pin-only bump of the action
    from invalidating a probe: the mutation still replaces the step the
    workflow actually declares.
    """
    lines = source.read_text(encoding="utf-8").splitlines()
    for index, line in enumerate(lines[:-1]):
        if line.strip() == f"- name: {name}" and lines[index + 1].strip().startswith("uses:"):
            return f"{line}\n{lines[index + 1]}"
    msg = f"{source} declares no `- name: {name}` step with a `uses:` line"
    raise AssertionError(msg)


def _ci_line(source: Path, needle: str) -> str:
    """The first `source` line containing `needle`, verbatim (any pin included)."""
    for line in source.read_text(encoding="utf-8").splitlines():
        if needle in line:
            return line
    msg = f"{source} has no line containing {needle!r}"
    raise AssertionError(msg)


def test_parity_holds_on_the_committed_tree() -> None:
    result = _run_ci_parity()

    assert result.returncode == 0, result.stdout + result.stderr
    assert "jobs covered" in result.stdout
    assert "FAILED" not in result.stderr


def test_the_manifest_is_a_file_the_script_reads() -> None:
    script = SCRIPT.read_text(encoding="utf-8")

    assert "ci-parity-manifest.txt" in script, "the script must read the promoted manifest"
    assert MANIFEST.is_file()
    assert MANIFEST.read_text(encoding="utf-8").strip()


def test_a_legacy_four_field_manifest_line_fails_closed(tmp_path: Path) -> None:
    # A pre-migration line still carries the retired push_gates column. Folding
    # it into the notes would read that stale claim as prose, so it fails closed.
    mutated = _mutate(
        tmp_path,
        MANIFEST,
        "markdownlint|local|just lint-markdown-check",
        "markdownlint|local||just lint-markdown-check",
    )

    result = _run_ci_parity(MANIFEST=str(mutated))

    assert result.returncode == 1, result.stdout + result.stderr
    assert "markdownlint" in result.stderr
    assert "three fields" in result.stderr


def test_a_ci_job_without_a_manifest_line_fails(tmp_path: Path) -> None:

    mutated = _mutate(
        tmp_path,
        CI_YML,
        "jobs:\n",
        "jobs:\n  bogus-job:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo hi\n",
    )

    result = _run_ci_parity(CI=str(mutated))

    assert result.returncode == 1, result.stdout + result.stderr
    assert "bogus-job" in result.stderr
    assert "has no manifest line" in result.stderr


def test_a_manifest_entry_naming_a_nonexistent_recipe_fails(tmp_path: Path) -> None:
    mutated = _mutate(
        tmp_path,
        MANIFEST,
        "markdownlint|local|just lint-markdown",
        "markdownlint|local|just no-such-recipe-xyz",
    )

    result = _run_ci_parity(MANIFEST=str(mutated))

    assert result.returncode == 1, result.stdout + result.stderr
    assert "no-such-recipe-xyz" in result.stderr
    assert "does not exist" in result.stderr


def test_a_manifest_entry_naming_a_variable_fails(tmp_path: Path) -> None:
    """`just py_run` can never validate as a recipe: `py_run` is a variable."""
    mutated = _mutate(
        tmp_path,
        MANIFEST,
        "markdownlint|local|just lint-markdown-check",
        "markdownlint|local|just py_run",
    )

    result = _run_ci_parity(MANIFEST=str(mutated))

    assert result.returncode == 1, result.stdout + result.stderr
    assert "py_run" in result.stderr
    assert "does not exist" in result.stderr


def test_a_manifest_entry_naming_a_directive_fails(tmp_path: Path) -> None:
    """`just set` can never validate as a recipe: `set` is a directive."""
    mutated = _mutate(
        tmp_path,
        MANIFEST,
        "markdownlint|local|just lint-markdown-check",
        "markdownlint|local|just set",
    )

    result = _run_ci_parity(MANIFEST=str(mutated))

    assert result.returncode == 1, result.stdout + result.stderr
    assert "'set'" in result.stderr or "just set" in result.stderr
    assert "does not exist" in result.stderr


def test_a_malformed_justfile_refuses(tmp_path: Path) -> None:
    """A justfile that will not parse is a refusal, not a missing-recipe finding."""
    bad = tmp_path / "justfile"
    bad.write_text("this is : not valid\n:::\n", encoding="utf-8")

    result = _run_ci_parity(JF=str(bad))

    assert result.returncode == 2, result.stdout + result.stderr
    assert "refusing to pass" in result.stderr
    assert "does not exist" not in result.stderr


def test_a_missing_just_executable_refuses(tmp_path: Path) -> None:
    """`just` is a declared dependency of this gate: without it there is no oracle."""
    empty_bin = tmp_path / "empty-bin"
    empty_bin.mkdir()

    result = _run_ci_parity(PATH=str(empty_bin))

    assert result.returncode == 2, result.stdout + result.stderr
    assert "'just' is required" in result.stderr
    assert "refusing to pass" in result.stderr


def test_a_missing_manifest_fails_closed(tmp_path: Path) -> None:
    result = _run_ci_parity(MANIFEST=str(tmp_path / "no-such-manifest.txt"))

    assert result.returncode == 2, result.stdout + result.stderr
    assert "refusing to pass" in result.stderr
    assert "jobs covered" not in result.stdout


TOOLS = REPO_ROOT / "scripts" / "hooks" / "ci-recipe-tools.txt"


def test_the_requirement_table_provisions_every_recipe_toolchain() -> None:
    """The committed tree passes the toolchain reconciliation too."""
    result = _run_ci_parity()

    assert result.returncode == 0, result.stdout + result.stderr
    assert "recipe toolchains provisioned" in result.stdout


def test_a_job_missing_a_tool_its_recipe_needs_fails(tmp_path: Path) -> None:
    """The tier-3 job without its uv step cannot run the vyper compile."""
    mutated = _mutate(
        tmp_path,
        CI_YML,
        _ci_step(CI_YML, UV_VYPER_STEP),
        "      - name: Provision nothing\n        run: echo skipped",
    )

    result = _run_ci_parity(CI=str(mutated))

    assert result.returncode == 1, result.stdout + result.stderr
    assert "tier3-oracle" in result.stderr
    assert "verify-tier3-executor-artifact" in result.stderr
    assert "uv" in result.stderr


def test_a_wrong_install_action_tool_fails(tmp_path: Path) -> None:
    """`taiki-e/install-action` counts only when it installs nextest itself."""
    nextest = _ci_line(CI_YML, "tool: nextest@")
    mutated = _mutate(
        tmp_path,
        CI_YML,
        nextest,
        re.sub(r"nextest@\S+", "cargo-binstall", nextest),
    )

    result = _run_ci_parity(CI=str(mutated))

    assert result.returncode == 1, result.stdout + result.stderr
    assert "cargo-nextest" in result.stderr


def test_a_sha_only_action_pin_bump_keeps_the_anchor(tmp_path: Path) -> None:
    """A pin bump is not a wiring change: the toolchain probe still anchors."""
    anchor = _ci_step(CI_YML, UV_VYPER_STEP)
    fresh_sha = "deadbeef" * 5
    repinned = re.sub(r"@[0-9a-f]{40}", f"@{fresh_sha}", anchor)
    assert repinned != anchor, "the anchor must carry the pin this test bumps"
    bumped = _mutate(tmp_path, CI_YML, anchor, repinned)
    assert _ci_step(bumped, UV_VYPER_STEP) == repinned

    mutated = _mutate(
        tmp_path,
        bumped,
        _ci_step(bumped, UV_VYPER_STEP),
        "      - name: Provision nothing\n        run: echo skipped",
    )

    result = _run_ci_parity(CI=str(mutated))

    assert result.returncode == 1, result.stdout + result.stderr
    assert "tier3-oracle" in result.stderr
    assert "uv" in result.stderr


def test_a_recipe_a_job_runs_without_a_row_fails(tmp_path: Path) -> None:
    """A new `just` invocation in a job has to declare its toolchain."""
    # `just test-fork` is a real recipe, so the name-existence check passes and
    # this failure can only come from the missing requirement row.
    mutated = _mutate(tmp_path, CI_YML, "run: just check-uv-lock", "run: just test-fork")

    result = _run_ci_parity(CI=str(mutated))

    assert result.returncode == 1, result.stdout + result.stderr
    assert "test-fork" in result.stderr
    assert "no row" in result.stderr


def test_a_row_no_job_runs_fails(tmp_path: Path) -> None:
    """A requirement row that outlives its CI caller is a stale claim."""
    mutated = _mutate(
        tmp_path,
        TOOLS,
        "fmt-check-python||",
        "fmt-check-python||\norphan-recipe|cargo-nextest|",
    )

    result = _run_ci_parity(CI=str(CI_YML), TOOLS=str(mutated))

    assert result.returncode == 1, result.stdout + result.stderr
    assert "orphan-recipe" in result.stderr
    assert "no ci.yml job runs" in result.stderr


# --- CI execution coverage ---------------------------------------------------
# The pre-push tier is not a substitute for CI: `git push --no-verify` skips it
# entirely, and a gate that only ever runs on a developer machine certifies
# nothing about the merged tree. Every HOOK's CI execution is declared in
# scripts/gates.toml and validated there (ci_job/ci_entry/ci_relation, or
# ci_probe) by `just gates-check` and tests/test_gate_registry.py. What is left
# here is the non-hook recipe CI must carry.
NON_HOOK_CI_EXECUTIONS = (("consumer probes", "just check-consumer-probes"),)

RATCHET_SCRIPT = REPO_ROOT / "scripts" / "executor_release_ratchet.py"
RELEASE_BASE_STEP = "Resolve the executor release base"
ZERO_SHA = "0" * 40
GIT_IDENTITY = (
    "-c",
    "user.name=release-base",
    "-c",
    "user.email=base@example.invalid",
    "-c",
    "commit.gpgsign=false",
)


def _ci_jobs(source: Path) -> list[str]:
    """Every job id under `jobs:`, read the way ci-parity.sh's awk reads them."""
    jobs: list[str] = []
    in_jobs = False
    for line in source.read_text(encoding="utf-8").splitlines():
        if line == "jobs:":
            in_jobs = True
            continue
        if not in_jobs:
            continue
        if line and not line.startswith(" "):
            break
        match = re.fullmatch(r"  ([A-Za-z0-9_-]+):", line)
        if match:
            jobs.append(match.group(1))
    return jobs


def _ci_job(source: Path, name: str) -> str:
    """The named job's block: its key through the line before the next job key."""
    block: list[str] = []
    key = f"  {name}:"
    for line in source.read_text(encoding="utf-8").splitlines():
        if re.fullmatch(r"  [A-Za-z0-9_-]+:", line):
            if block:
                break
            if line != key:
                continue
        if block or line == key:
            block.append(line)
    assert block, f"{source} declares no job {name!r}"
    return "\n".join(block)


def _step_run_body(source: Path, name: str) -> str:
    """The run: body of the step named `name`, as shell text."""
    lines = source.read_text(encoding="utf-8").splitlines()
    start = next(
        (index for index, line in enumerate(lines) if line.strip() == f"- name: {name}"), None
    )
    assert start is not None, f"{source} declares no step named {name!r}"

    body: list[str] = []
    run_indent: int | None = None
    for line in lines[start:]:
        stripped = line.strip()
        indent = len(line) - len(line.lstrip())
        if run_indent is None:
            if stripped.startswith(("run: |", "run: >-", "run: >")):
                run_indent = indent
            continue
        if stripped and indent <= run_indent:
            break
        body.append(line[run_indent + 2 :] if len(line) > run_indent else "")
    assert body, f"the {name!r} step runs no script"
    return "\n".join(body)


def _release_base_env_name() -> str:
    """The base variable the ratchet reads, so the wiring cannot name another."""
    match = re.search(
        r'^BASE_ENV = "([A-Z0-9_]+)"$', RATCHET_SCRIPT.read_text(encoding="utf-8"), re.MULTILINE
    )
    assert match is not None, "the ratchet must declare the base variable it reads"
    return match.group(1)


def _git(repo: Path, *args: str) -> str:
    """Run git in `repo` and return its stdout, failing the test on a nonzero exit."""
    exe = shutil.which("git")
    assert exe is not None, "git is not installed on PATH"
    result = subprocess.run(  # ruff: ignore[subprocess-without-shell-equals-true]
        [exe, "-C", str(repo), *GIT_IDENTITY, *args],
        check=False,
        capture_output=True,
        text=True,
    )
    assert result.returncode == 0, f"git {' '.join(args)} failed: {result.stderr}"
    return result.stdout.strip()


def _forked_repository(tmp_path: Path) -> tuple[Path, str, str]:
    """A main branch that advanced after a feature branch forked off it.

    Returns the repository, the fork point, and the advanced base tip.
    """
    repo = tmp_path / "repository"
    repo.mkdir()
    _git(repo, "init", "-q", "-b", "main", ".")
    (repo / "release.txt").write_text("release v2", encoding="utf-8")
    _git(repo, "add", "-A")
    _git(repo, "commit", "-q", "-m", "release v2")
    fork = _git(repo, "rev-parse", "HEAD")
    _git(repo, "checkout", "-q", "-b", "feature")
    (repo / "work.txt").write_text("feature work", encoding="utf-8")
    _git(repo, "add", "-A")
    _git(repo, "commit", "-q", "-m", "feature work")
    _git(repo, "checkout", "-q", "main")
    (repo / "later.txt").write_text("a release landed after the fork", encoding="utf-8")
    _git(repo, "add", "-A")
    _git(repo, "commit", "-q", "-m", "main advanced")
    tip = _git(repo, "rev-parse", "HEAD")
    _git(repo, "update-ref", "refs/remotes/origin/main", tip)
    _git(repo, "checkout", "-q", "feature")
    return repo, fork, tip


def _run_release_base(
    repo: Path, *, pr_base: str, push_before: str, default_branch: str
) -> tuple[subprocess.CompletedProcess[str], dict[str, str]]:
    """Run the workflow's base-resolution step in `repo`, as the runner would."""
    env_file = repo / "github_env"
    env_file.write_text("", encoding="utf-8")
    env = dict(os.environ)
    env.update({
        "PR_BASE": pr_base,
        "PUSH_BEFORE": push_before,
        "DEFAULT_BRANCH": default_branch,
        "GITHUB_ENV": str(env_file),
    })
    result = subprocess.run(  # ruff: ignore[subprocess-without-shell-equals-true]
        [_sh(), "-e", "-c", _step_run_body(CI_YML, RELEASE_BASE_STEP)],
        cwd=repo,
        env=env,
        check=False,
        capture_output=True,
        text=True,
    )
    exported = {
        line.split("=", 1)[0]: line.split("=", 1)[1]
        for line in env_file.read_text(encoding="utf-8").splitlines()
        if "=" in line
    }
    return result, exported


def test_every_ci_job_is_in_the_aggregate_needs() -> None:
    """A job the aggregate does not need can fail without failing the branch."""
    jobs = _ci_jobs(CI_YML)
    assert len(jobs) > 1, "the workflow declares no jobs to aggregate"

    aggregate = _ci_job(CI_YML, "ci-green")
    match = re.search(r"^\s*needs: \[(.*)\]$", aggregate, re.MULTILINE)
    assert match is not None, "ci-green must declare a one-line needs list"
    declared = {name.strip() for name in match.group(1).split(",")}

    assert declared == set(jobs) - {"ci-green"}, (
        "ci-green's needs must name every other job: an omitted job runs "
        "unaccounted, so its failure cannot fail the aggregate"
    )


def test_the_consumer_probes_gate_has_an_explicit_ci_execution() -> None:
    """A recipe CI never runs certifies nothing about the merged tree."""
    text = CI_YML.read_text(encoding="utf-8")

    missing = [gate for gate, invocation in NON_HOOK_CI_EXECUTIONS if invocation not in text]

    assert not missing, f"ci.yml never executes: {missing}"


def test_the_python_lint_step_is_check_only() -> None:
    """CI must not run the `--fix` recipe: the runner discards its repairs."""
    text = CI_YML.read_text(encoding="utf-8")

    assert "run: just lint-python-check" in text
    assert re.search(r"run: just lint-python(?=[\s]|$)", text) is None, (
        "the fixing recipe would go green over a tree it silently repaired"
    )


def test_the_commitlint_job_lints_through_the_recipe() -> None:
    """CI lints the event's own range through `just lint-commits`.

    A runner checkout carries no upstream tracking ref, so the recipe's default
    `@{push}..HEAD` cannot resolve there: the range has to be the event's.
    """
    job = _ci_job(CI_YML, "commitlint")

    assert "just lint-commits" in job
    assert "uses: extractions/setup-just@" in job, "the recipe cannot run without just"
    assert "github.event.before" in job, "a push lints the commits being pushed"
    assert "github.event.pull_request.base.sha" in job, (
        "a PR lints against its own target branch, not a hard-coded one"
    )


def test_the_stubtest_gate_runs_after_an_extension_build() -> None:
    """stubtest introspects the INSTALLED extension; it must exist first."""
    job = _ci_job(CI_YML, "python")
    install = job.find("run: just install-extension-wheel")
    stubtest = job.find("run: just lint-stubtest")

    assert install != -1, "the python job must install the wheel it tests"
    assert stubtest != -1, "the python job must run the stubtest gate"
    assert install < stubtest, (
        "stubtest compares the stubs against the extension's real surface, so a "
        "run before the install certifies an extension the tree never built"
    )


def test_the_executor_release_ratchet_gets_an_independent_fetched_base() -> None:
    """The ratchet fails closed without a base INDEPENDENT of the commit under test.

    Supplying one is the job's contract: the gate itself refuses to compare HEAD
    with itself, so CI has to name the event's base and have that commit's
    history in the checkout.
    """
    job = _ci_job(CI_YML, "policy-gates")
    base_env = _release_base_env_name()
    exported = f"{base_env}="
    gate = job.find("run: just check-executor-release-ratchet-index")
    body = _step_run_body(CI_YML, RELEASE_BASE_STEP)

    assert gate != -1, "the policy-gates job must run the ratchet's live-tree half"
    assert "fetch-depth: 0" in job, (
        "a shallow checkout cannot resolve the base commit, so the preservation "
        "half of the ratchet could only fail closed"
    )
    assert exported in job, f"the job must hand the gate {base_env}"
    assert job.find(exported) < gate, "the base must be exported before the gate runs"
    assert "github.event.pull_request.base.sha" in job, "the PR base is the event's own"
    assert "github.event.before" in job, "the push base is the commits' own"
    assert "origin/" in job, "a force push orphans `before`; the default branch is the fallback"
    assert "git merge-base" in body, (
        "the exported base must be the fork point, not the candidate tip: a base "
        "branch that advanced after the branch forked carries release packages "
        "this commit never had"
    )
    assert body.index("git merge-base") < body.index(exported), (
        "the merge-base result is what gets exported"
    )
    assert "^{commit}" in body, "the resolved base must be verified as a commit"


def test_the_release_base_is_the_fork_point_not_the_base_tip(tmp_path: Path) -> None:
    """A base tip past the fork point would report the base's own releases missing."""
    repo, fork, tip = _forked_repository(tmp_path)
    base_env = _release_base_env_name()
    cases = {
        "pull request base": {"pr_base": tip, "push_before": ZERO_SHA, "default_branch": "main"},
        "default-branch fallback": {
            "pr_base": "",
            "push_before": ZERO_SHA,
            "default_branch": "main",
        },
        "push before": {"pr_base": "", "push_before": fork, "default_branch": "main"},
    }

    for label, resolved in cases.items():
        result, exported = _run_release_base(repo, **resolved)

        assert result.returncode == 0, f"{label}: {result.stdout}{result.stderr}"
        assert exported == {base_env: fork}, (
            f"{label}: the base must be the fork point {fork}, not the advanced tip"
            f" {tip} (exported {exported})"
        )


def test_a_release_base_without_shared_history_fails_the_step(tmp_path: Path) -> None:
    """No common ancestor means no preservation base: the step must not guess one."""
    repo, _fork, _tip = _forked_repository(tmp_path)
    _git(repo, "checkout", "-q", "--orphan", "unrelated")
    _git(repo, "commit", "-q", "--allow-empty", "-m", "unrelated root")
    unrelated = _git(repo, "rev-parse", "HEAD")
    _git(repo, "checkout", "-q", "feature")

    result, exported = _run_release_base(
        repo, pr_base="", push_before=unrelated, default_branch="main"
    )

    assert result.returncode != 0, "a base with no shared history must fail the step"
    assert exported == {}, "a rejected base must hand the gate nothing"
    assert "no history" in result.stderr


def test_a_job_without_the_llvm_toolchain_fails(tmp_path: Path) -> None:
    """A job that compiles default features needs the LLVM 23 composite action."""
    mutated = _mutate(
        tmp_path,
        CI_YML,
        "      - uses: ./.github/actions/llvm23\n\n      - name: Compile workspace at MSRV",
        "\n      - name: Compile workspace at MSRV",
    )

    result = _run_ci_parity(CI=str(mutated))

    assert result.returncode == 1, result.stdout + result.stderr
    assert "msrv" in result.stderr
    assert "llvm23" in result.stderr
