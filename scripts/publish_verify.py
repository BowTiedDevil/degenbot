#!/usr/bin/env python3
"""The publish-verify overlay: a non-uploading `cargo publish --dry-run` that resolves.

# live-invariant gate (ADR-087 D2): drives the publish-blocked census and the
# live workspace manifests; not tripwire precedent.


The problem
-----------

`cargo publish` rewrites every path dependency at packaging: the source key is
stripped and what remains is a version requirement that must resolve on
crates.io. Under ADR-009 lockstep versioning, a fresh release bumps every
member to a version crates.io has never seen, so the plain dry run dies on the
first member that a later member requires -- and `--no-verify` changes
nothing, because dependency resolution happens before any verify build. The
real `cargo publish --workspace` survives the same shape only because it
uploads members in dependency order, so each sibling is index-visible by the
time its dependents are packaged; a non-uploading verification cannot use that
ordering.


The mechanism
-------------

For the duration of one run this script replaces the crates.io source with a
temporary overlay registry (cargo source replacement, `--registry crates-io`
so cargo accepts the replaced source) holding exactly what the real upload
round will have staged by the time each member is reached:

1. every crate the workspace lock pins to crates.io, mirrored verbatim from
   the real crates.io sparse index -- the overlay can never satisfy a
   requirement the real registry cannot, and a mirror fetch that comes up
   empty fails the run loudly instead of certifying a partial oracle. A pin
   locked to a path, a git repository, or an alternate registry fails the run
   loudly too: resolving it by name would serve a same-named stranger;
2. every publishable member the publish-blocked census does not withhold,
   packed with `cargo package --locked` in dependency order and indexed from
   its own packaged manifest, with the entry checksum taken from the packed
   `.crate` -- the exact bytes a real upload would carry. A member whose
   `publish` allowlist names a non-crates.io registry fails loud, and caller
   flags that would retarget the run (`--registry`, `--index`, `--token`,
   `--config`) are refused.

Packing a member resolves its rewritten manifest, dev-dependencies included,
so a member cycle (a dev-dependency edge closing a normal-dependency cycle)
cannot be packed in any strict order. Members inside such a cycle get a
provisional entry -- name and version only, enough for a requirement to match,
never used for a build -- before the first of them packs, and every entry is
rewritten from the packaged manifests once packing completes, before the
publish run starts against a fresh index port (a rewritten entry behind an
unchanged mtime second could otherwise be served as a stale 304).

Members the census withholds (genuinely unresolvable external/git shapes) are
excluded from the run by name and keep their file-inclusion coverage through
`cargo package --list`, exactly as before. Everything else is packed and
indexed; by default every such member is also verified (actually built, unless
`--no-verify` is forwarded) against the overlay, and cargo aborts before any
upload because the command stays a `--dry-run`. No token is read or required.

The run separates two member sets. The RESOLUTION set is every publishable
non-withheld member: each is packed and indexed from its own packaged form, so
any member's rewritten manifest resolves against every sibling at the lockstep
version -- which is what crates.io actually holds when a sibling is live. The
VERIFICATION set is the members that get their own publish round. The release
pre-publish step forwards cargo's own `--exclude <name>` flags for members the
live-set audit (`crates_io_pending.sh --classify`) reports already live at this
version; an exclusion moves a member out of the verification set only -- it
stays in the resolution set, because an already-live member cannot be
re-uploaded and re-verifying it is precisely what makes a resumed re-run
expensive. A fresh tag forwards nothing, so every pending member verifies, and
the release step exits before any packaging when the audit reports nothing
pending. The flags cannot be forwarded to the rounds themselves: each round is
one member (`cargo publish -p name`), where cargo refuses `--exclude` outright
(`--exclude can only be used together with --workspace`), so the script
consumes them and everything else (`--no-verify`) keeps forwarding. An
`--exclude` naming a non-member fails loud before any packing, mirroring
cargo's own package-selection error; census withholding is independent of and
unchanged by exclusions.

The overlay lives under `rust/target/` in a fresh per-run directory, so two
runs never delete each other's served files; the mirror of external index
entries is cached under `rust/target/` keyed by the lockfile hash. Every cargo
invocation of a run executes with `CARGO_HOME` pointed at the run's own
directory inside its overlay: the loopback-registry caches and staging state
cargo writes during packing and the dry-run rounds (`127.0.0.1-*` residue)
land there and are removed with the overlay, leaving the caller's real Cargo
home byte-for-byte unchanged. Runs sharing one workspace serialize their
cargo phases through an interprocess lock (`rust/target/publish-verify.lock`,
an `flock` held from the first cargo resolution to the last publish round):
`rust/target/package` staging and the lockfile cannot interleave between two
same-tree runs, while the lock path is derived from the tree root, so runs in
different workspaces never wait on each other. A second same-tree run waits
a bounded time (`PUBLISH_VERIFY_LOCK_WAIT_SECONDS`, default 1800 seconds) and
then fails loud with an error naming the lock file and the recorded holder;
the lock file itself is never deleted, because removing an `flock`'d file
would let a waiter acquire against an inode the holder no longer holds. The
sparse index itself must be served over HTTP (cargo rejects `file://` sparse
indexes), so a loopback-only server lives for the duration of the run, bound
to an ephemeral port: no fixed port can be pre-bound by something else, and
concurrent runs cannot collide. Mirror fetches carry an explicit timeout,
bounded retries with backoff, and a failure naming the crate and URL; in the
`--no-verify` form only index metadata is mirrored, because cargo resolves a
no-verify run from the index alone.

Exit codes: 0 covered, 1 census findings / packaging / resolution failure,
2 census refused to certify.
"""

from __future__ import annotations

import argparse
import contextlib
import fcntl
import hashlib
import json
import math
import os
import shutil
import subprocess
import sys
import tarfile
import threading
import time
import tomllib
import urllib.error
import urllib.request
import uuid
from concurrent.futures import ThreadPoolExecutor
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import TYPE_CHECKING, Any, NoReturn
from urllib.parse import quote

if TYPE_CHECKING:
    from collections.abc import Generator
    from typing import BinaryIO

REPO_ROOT = Path(__file__).resolve().parents[1]
MANIFEST_REL = "rust/Cargo.toml"
LOCK_REL = "rust/Cargo.lock"
CENSUS_GATE = Path(__file__).resolve().parent / "publish_blocked.py"
OVERLAY_REL = "rust/target/publish-verify-overlay"
MIRROR_REL = "rust/target/publish-verify-mirror"
# Same-workspace mutual exclusion: runs in one tree share rust/target/package
# staging and the lockfile, so the phases that touch them serialize through an
# flock on this file. The path is derived from the tree root, so runs in
# different workspaces take different locks and never wait on each other. The
# file itself is never deleted: removing an flock'd file would let a waiter
# acquire against an inode the holder no longer holds.
RUN_LOCK_REL = "rust/target/publish-verify.lock"
LOCK_POLL_SECONDS = 0.1
DEFAULT_LOCK_WAIT_SECONDS = 1800.0
LOCK_WAIT_ENV = "PUBLISH_VERIFY_LOCK_WAIT_SECONDS"
SOURCE_NAME = "pubverify"
CRATES_IO_INDEX = "https://index.crates.io"

# Bounds for every external fetch: a slow or dead crates.io endpoint fails
# the run inside its own budget with a message naming the crate and URL,
# instead of hanging on the default socket timeout.
FETCH_TIMEOUT_SECONDS = 30.0
FETCH_ATTEMPTS = 3
FETCH_BACKOFF_SECONDS = 2.0
# Status codes at or above this floor are transient server-side answers and
# retry; anything below is a definitive 4xx about what the endpoint serves.
HTTP_SERVER_ERROR_FLOOR = 500
# The lockfile spellings of the one registry the overlay can ever serve. The
# canonical `registry+` form is what cargo records for crates.io; the `sparse+`
# spelling is accepted for protocol-explicit pins. Anything else fails loud.
CRATES_IO_SOURCES = frozenset({
    "registry+https://github.com/rust-lang/crates.io-index",
    "sparse+https://index.crates.io/",
})
CRATES_IO_DL = "https://crates.io/api/v1/crates"

# The dynamic JSON payloads this oracle consumes and writes (cargo metadata
# package objects, crates.io sparse-index entries): each field is read once
# and passed on whole, so the payloads stay Any-typed instead of
# re-declaring cargo's JSON schema at every hop.
Json = dict[str, Any]

# crates.io's sparse-index path buckets (the documented layout): a one- or
# two-character name lives under a directory named for its length, a
# three-character name under its first letter, and everything else under the
# first four characters split 2/2.
INDEX_LEN_ONE = 1
INDEX_LEN_TWO = 2
INDEX_LEN_THREE = 3


# Caller-forwarded flags that would retarget the oracle: the run exists to
# certify resolution against the crates.io overlay it builds, so a caller
# cannot point it at another registry, a foreign index, a token, or an
# overriding config. Both `--x value` and `--x=value` are refused.
RETARGETING_FLAGS = frozenset({"--registry", "--index", "--token", "--config"})


def reject_retargeting(extra: list[str]) -> None:
    """Refuse every forwarded flag that would point the run away from crates.io."""
    for argument in extra:
        flag = argument.split("=", 1)[0]
        if flag in RETARGETING_FLAGS:
            fail(f"{flag} would retarget the overlay and cannot be forwarded")


def split_exclusions(extra: list[str]) -> tuple[list[str], list[str]]:
    """Split the caller's forwarded flags into exclusions and the rest.

    `--exclude <name>` and `--exclude=<name>` are cargo's own workspace
    publish spellings, and release.yml's pre-publish step forwards exactly the
    flags its upload rounds carry. They cannot ride through to the rounds:
    each round is one member (`cargo publish -p name`), where cargo refuses
    `--exclude` outright, and plain forwarding would re-verify live members.
    So this run consumes them -- everything else (`--no-verify`) keeps
    forwarding -- and an excluded name keeps only its resolution-set
    membership (see the module docstring). The returned names are validated
    against the workspace before any packing; a value with no name fails here.

    Returns:
        The excluded member names, and the flags that keep forwarding.

    """
    excluded: list[str] = []
    rest: list[str] = []
    index = 0
    while index < len(extra):
        argument = extra[index]
        if argument == "--exclude":
            if index + 1 >= len(extra):
                fail("--exclude needs a member name")
            excluded.append(extra[index + 1])
            index += 2
        elif argument.startswith("--exclude="):
            excluded.append(argument.split("=", 1)[1])
            index += 1
        else:
            rest.append(argument)
            index += 1
    return excluded, rest


def fail(message: str) -> NoReturn:
    """Report the run's failure on stderr and exit nonzero.

    Raises:
        SystemExit: Always, with exit code 1.

    """
    print(f"publish-verify: {message}", file=sys.stderr)
    raise SystemExit(1)


def run(
    cmd: list[str],
    cwd: Path | None = None,
    *,
    capture: bool = False,
    env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    """Run one external command and return its completed process.

    A nonzero exit never raises here: the oracle reads `returncode` itself,
    because a failing cargo invocation is oracle output, not a crash.

    Returns:
        The completed process with the caller's capture/text choices.

    """
    return subprocess.run(cmd, cwd=cwd, check=False, capture_output=capture, text=True, env=env)


def cargo_environment(cargo_home: Path) -> dict[str, str]:
    """Build the environment for every cargo invocation of one run.

    CARGO_HOME points at the run's own directory inside its per-run overlay:
    the loopback-registry caches and staging state cargo writes during packing
    and the dry-run rounds (`127.0.0.1-*` residue) land there and are removed
    with the overlay, instead of accumulating in the caller's real Cargo home.

    Returns:
        The caller's environment with CARGO_HOME redirected to the run's own
        directory.

    """
    return {**os.environ, "CARGO_HOME": str(cargo_home)}


def lock_wait_seconds() -> float:
    """Resolve the bounded wait for a same-workspace lock, caller-overridable.

    Defaults to a generous bound (a verified run holds the lock for tens of
    minutes); a caller who prefers failing fast sets the environment variable.

    Returns:
        The wait in seconds: the default, or the `LOCK_WAIT_ENV` override,
        which fails loud instead of falling back when it is non-numeric,
        non-finite (NaN or an infinity would make the deadline comparison
        permanently false or the wait unbounded), or negative.

    """
    raw = os.environ.get(LOCK_WAIT_ENV)
    if raw is None:
        return DEFAULT_LOCK_WAIT_SECONDS
    try:
        value = float(raw)
    except ValueError:
        fail(f"{LOCK_WAIT_ENV}={raw!r} is not a number of seconds")
    if not math.isfinite(value):
        fail(f"{LOCK_WAIT_ENV}={raw!r} is not a finite number of seconds")
    if value < 0:
        fail(f"{LOCK_WAIT_ENV}={raw!r} is negative")
    return value


def _holder_description(lock_path: Path) -> str:
    """Describe the best-effort identity of the run holding the lock.

    A record missing either field (a holder that died between truncating and
    writing, a torn read) degrades the message, never the exclusion.

    Returns:
        A short human-readable holder description, or a fixed placeholder
        when the record cannot be read.

    """
    try:
        holder = json.loads(lock_path.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return "holder not recorded"
    parts = []
    if isinstance(holder, dict):
        if "pid" in holder:
            parts.append(f"pid {holder['pid']}")
        if "started" in holder:
            parts.append(f"started {holder['started']}")
    return " ".join(parts) if parts else "holder not recorded"


@contextlib.contextmanager
def run_lock(root: Path, wait_seconds: float) -> Generator[None]:
    """Hold the same-workspace publish-verify lock for the enclosed phase.

    The lock is an `flock` on a file derived from the tree root, so only runs
    sharing this workspace serialize. Acquisition polls a non-blocking try
    until `wait_seconds` elapse, then fails loud with an error naming the
    lock file and the recorded holder. The holder records its pid and start
    time into the file (best effort -- a torn read only degrades the error
    message, never the exclusion, which is the flock itself). The lock file
    is never deleted; release is the unlock and close alone.
    """
    lock_path = root / RUN_LOCK_REL
    lock_path.parent.mkdir(parents=True, exist_ok=True)
    fd = os.open(lock_path, os.O_RDWR | os.O_CREAT | os.O_CLOEXEC, 0o644)
    try:
        deadline = time.monotonic() + max(wait_seconds, 0.0)
        while True:
            try:
                fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
                break
            except OSError:
                if time.monotonic() >= deadline:
                    fail(
                        f"another publish-verify run holds the same-workspace lock "
                        f"{lock_path} ({_holder_description(lock_path)}); waited "
                        f"{wait_seconds:g}s. Re-run when the other run finishes, "
                        f"or raise {LOCK_WAIT_ENV}."
                    )
                time.sleep(LOCK_POLL_SECONDS)
        try:
            record = json.dumps({
                "pid": os.getpid(),
                "started": time.strftime("%Y-%m-%dT%H:%M:%S"),
            }).encode("utf-8")
            os.ftruncate(fd, 0)
            os.pwrite(fd, record, 0)
        except OSError:
            pass  # exclusion is the flock; an unwritable record only blurs the error
        try:
            yield
        finally:
            fcntl.flock(fd, fcntl.LOCK_UN)
    finally:
        os.close(fd)


def census(root: Path, *args: str) -> subprocess.CompletedProcess[str]:
    """Run the publish-blocked census gate against the same tree root.

    Returns:
        The completed census process; the caller reads its streams.

    """
    return run([sys.executable, str(CENSUS_GATE), "--root", str(root), *args], capture=True)


def index_rel(name: str) -> str:
    """Compute the crates.io sparse-index path for a crate name.

    Returns:
        The path under the index root, following crates.io's documented
        bucketing (see the `INDEX_LEN_*` constants).

    """
    if len(name) == INDEX_LEN_ONE:
        return f"3/{name}"
    if len(name) == INDEX_LEN_TWO:
        return f"2/{name}"
    if len(name) == INDEX_LEN_THREE:
        return f"3/{name[0]}/{name}"
    return f"{name[:2]}/{name[2:4]}/{name}"


def requirement(version: str) -> str:
    """Spell the index `req` the way cargo itself sends for a bare version.

    Returns:
        A caret pin for a bare version, otherwise the stripped input
        unchanged.

    """
    stripped = version.strip()
    if stripped and stripped[0].isdigit():
        return f"^{stripped}"
    return stripped


def entry_from_packaged_manifest(manifest: str, cksum: str) -> Json:
    """Derive the sparse-index entry for a member from its packaged manifest.

    The packaged manifest is the exact form a real upload would carry, so the
    entry mirrors it: a dependency renamed through `package = ...` keeps
    Cargo's index convention -- the alias in the dependency's `name` field and
    the original package name in its `package` field (the documented index
    spelling of a rename; `explicit_name_in_toml` is the Publish API's field,
    not the index's) -- so a member compiled from this entry is linked under
    the alias its code imports. Target-scoped dependencies keep their cfg
    string, and the features map is the manifest's own `[features]` table
    (the `dep:` spellings inside it are what makes the entry a v2 entry).

    Returns:
        The index entry: name, version, dependencies (with the rename
        convention above), the packed checksum, and the split features map.

    """
    doc = tomllib.loads(manifest)
    deps: list[Json] = []

    def collect(table: dict[str, str | Json], kind: str, target: str | None = None) -> None:
        for key, value in table.items():
            if isinstance(value, str):
                spec: Json = {"version": value}
            else:
                spec = value
            version = spec.get("version")
            if version is None:
                continue
            dep = {
                "name": key,
                "req": requirement(version),
                "features": list(spec.get("features", [])),
                "optional": bool(spec.get("optional", False)),
                "default_features": bool(spec.get("default-features", True)),
                "target": target,
                "kind": kind,
            }
            package = spec.get("package")
            if package is not None and package != key:
                dep["package"] = package
            deps.append(dep)

    for section, kind in (
        ("dependencies", "normal"),
        ("build-dependencies", "build"),
        ("dev-dependencies", "dev"),
    ):
        if section in doc:
            collect(doc[section], kind)
    for target, scoped in doc.get("target", {}).items():
        for section, kind in (
            ("dependencies", "normal"),
            ("build-dependencies", "build"),
            ("dev-dependencies", "dev"),
        ):
            if section in scoped:
                collect(scoped[section], kind, target=target)

    # The index splits the `[features]` table: entries whose definition uses
    # v2 syntax (`dep:` or a weak `?/` reference) go into `features2`, the
    # rest stay in `features`. A `dep:`-bearing value left in `features`
    # makes cargo reject the whole entry as invalid. The references keep the
    # manifest's own spellings: a `dep:` or `?/` name is the dependency key
    # the member's Cargo.toml writes, which for a renamed dependency (`bn =
    # { package = "substrate-bn" }`) is the alias the entry lists as the
    # dependency's `name` -- cargo validates the reference against that name,
    # not the real package name.
    features: dict[str, list[str]] = {}
    features2: dict[str, list[str]] = {}
    for name, feature_values in doc.get("features", {}).items():
        values = list(feature_values)
        if any("dep:" in value or "?/" in value for value in values):
            features2[name] = values
        else:
            features[name] = values
    entry: Json = {
        "name": doc["package"]["name"],
        "vers": doc["package"]["version"],
        "deps": deps,
        "cksum": cksum,
        "features": features,
        "yanked": False,
    }
    if features2:
        entry["features2"] = features2
        entry["v"] = 2
    return entry


def _source_problem(name: str, version: str, source: str | None) -> str:
    if source is None:
        return (
            f"{name} {version} is locked to a path source, not crates.io -- "
            "the overlay mirrors crates.io pins only"
        )
    return (
        f"{name} {version} is locked to {source}, not crates.io -- "
        "the overlay mirrors crates.io pins only"
    )


def lock_externals(
    lock_path: Path, member_names: set[str]
) -> tuple[dict[str, set[str]], list[str]]:
    """Collect the crates.io packages the lockfile pins, keyed by crate name.

    The overlay mirrors what crates.io serves, so only a pin whose locked
    source is crates.io is ever mirrored. A non-member locked to a path, a git
    repository, or an alternate registry must fail the run loudly instead:
    mirroring it would resolve the pin by name against crates.io and serve
    whatever same-named stranger the registry hosts.

    Returns:
        The externals -- versions to mirror keyed by crate name -- and the
        problems: one message per pin whose source the overlay cannot serve.

    """
    doc = tomllib.loads(lock_path.read_text(encoding="utf-8"))
    externals: dict[str, set[str]] = {}
    problems: list[str] = []
    for package in doc.get("package", []):
        name = package["name"]
        if name in member_names:
            continue
        source = package.get("source")
        if source in CRATES_IO_SOURCES:
            externals.setdefault(name, set()).add(package["version"])
        else:
            problems.append(_source_problem(name, package["version"], source))
    return externals, problems


def fetch_bytes(
    url: str,
    *,
    timeout: float = FETCH_TIMEOUT_SECONDS,
    attempts: int = FETCH_ATTEMPTS,
    backoff: float = FETCH_BACKOFF_SECONDS,
) -> bytes:
    """Fetch a URL with an explicit timeout and bounded retries with backoff.

    Only transient failures retry -- connection, DNS, timeout, and 5xx
    statuses; a 4xx is a definitive answer about what the endpoint serves
    and fails immediately. Exhausted retries raise with the URL and the
    attempt count, so the caller's failure names what to check.

    Returns:
        The response body.

    Raises:
        urllib.error.HTTPError: A 4xx answer, raised unretouched.
        RuntimeError: The endpoint stayed unreachable for `attempts` tries.

    """
    last_error: Exception | None = None
    for attempt in range(1, attempts + 1):
        try:
            with urllib.request.urlopen(url, timeout=timeout) as response:
                body: bytes = response.read()
                return body
        except urllib.error.HTTPError as exc:
            exc.close()
            if exc.code < HTTP_SERVER_ERROR_FLOOR:
                raise
            last_error = exc
        except OSError as exc:
            last_error = exc
        if attempt < attempts:
            time.sleep(backoff * attempt)
    message = f"{url} unreachable after {attempts} attempts: {last_error}"
    raise RuntimeError(message)


def atomic_write(path: Path, data: bytes) -> None:
    """Write `data` to `path` so a concurrent reader never sees a partial file."""
    staging = path.parent / f"{path.name}.{uuid.uuid4().hex}.tmp"
    staging.write_bytes(data)
    staging.replace(path)


def mirror_external(
    name: str,
    versions: set[str],
    lock_hash: str,
    mirror: Path,
    overlay: Path,
    *,
    no_verify: bool,
    index_base: str = CRATES_IO_INDEX,
    dl_base: str = CRATES_IO_DL,
    timeout: float = FETCH_TIMEOUT_SECONDS,
    attempts: int = FETCH_ATTEMPTS,
    backoff: float = FETCH_BACKOFF_SECONDS,
) -> str:
    """Mirror one external crate's index entry, plus its `.crate` when verified.

    Index responses
    are cached under `mirror` keyed by the lockfile hash, so an unchanged lock
    re-fetches nothing; the cache write is atomic, so two runs sharing a
    cache never serve each other half a response. Fetches carry an explicit
    timeout and bounded retries with backoff, and a failure names the crate,
    the URL, and the attempts spent.

    Returns:
        An error message, or the empty string on success.

    In the `--no-verify` form only index metadata is mirrored: cargo resolves
    a no-verify run from the index alone (an index-only overlay serves a
    `--no-verify` publish/package with no `/dl` request, while the verified
    form dies on the missing download), so the run touches the network only
    for the metadata resolution needs.

    """
    cache = mirror / lock_hash
    raw_path = cache / "index" / f"{name}.lines"
    if not raw_path.is_file():
        raw_path.parent.mkdir(parents=True, exist_ok=True)
        url = f"{index_base}/{index_rel(name)}"
        try:
            raw = fetch_bytes(url, timeout=timeout, attempts=attempts, backoff=backoff)
        except (RuntimeError, urllib.error.HTTPError) as exc:
            return f"crates.io index fetch failed for {name} ({url}): {exc}"
        atomic_write(raw_path, raw)
    wanted = [line for line in raw_path.read_text(encoding="utf-8").splitlines() if line]
    matched = [line for line in wanted if json.loads(line)["vers"] in versions]
    missing = versions - {json.loads(line)["vers"] for line in matched}
    if missing:
        return (
            f"crates.io holds none of {', '.join(sorted(missing))} "
            f"for {name} -- the lockfile pins an unresolvable dependency"
        )
    dest = overlay / "index" / index_rel(name)
    dest.parent.mkdir(parents=True, exist_ok=True)
    dest.write_text("".join(f"{line}\n" for line in matched), encoding="utf-8")
    if no_verify:
        return ""
    for version in versions:
        crate_file = None
        for candidate in sorted(
            (Path.home() / ".cargo" / "registry" / "cache").glob(f"*/{name}-{version}.crate")
        ):
            crate_file = candidate
            break
        if crate_file is None:
            crate_file = cache / "dl" / f"{name}-{version}.crate"
            if not crate_file.is_file():
                crate_file.parent.mkdir(parents=True, exist_ok=True)
                url = f"{dl_base}/{name}/{version}/download"
                try:
                    atomic_write(
                        crate_file,
                        fetch_bytes(url, timeout=timeout, attempts=attempts, backoff=backoff),
                    )
                except (RuntimeError, urllib.error.HTTPError) as exc:
                    crate_file.unlink(missing_ok=True)
                    return f"crates.io download failed for {name}-{version} ({url}): {exc}"
        link = overlay / "dl" / name / version / "download"
        link.parent.mkdir(parents=True, exist_ok=True)
        link.unlink(missing_ok=True)
        link.symlink_to(crate_file)
    return ""


def _quiet_handler(overlay: Path) -> type:
    class Handler(SimpleHTTPRequestHandler):
        def __init__(self, *args: Any, **kwargs: Any) -> None:
            super().__init__(*args, directory=str(overlay), **kwargs)

        # The stdlib protocol names the parameter `format`; the name stays so
        # the override remains call-compatible (a renamed parameter is an
        # invalid override), and the shadowing is the seam's, not ours.
        def log_message(self, format: str, *args: object) -> None:  # ruff: ignore[builtin-argument-shadowing]
            pass

        def send_head(self) -> BinaryIO | None:
            # Never answer a conditional request with 304: the overlay
            # entries are rewritten between packing and publishing, and
            # python's mtime comparison has second granularity, so a
            # same-second rewrite would otherwise let cargo keep resolving
            # against the previous bytes it cached for this URL.
            del self.headers["If-Modified-Since"]
            del self.headers["If-None-Match"]
            return super().send_head()

    return Handler


def overlay_run_dir(root: Path) -> Path:
    """Create a fresh overlay directory for one run, under the ignored target tree.

    Two runs never share one: a shared directory forces whoever starts second
    to delete the files the first is serving mid-run. Each run removes only
    its own directory when it finishes.

    Returns:
        The run's own directory path.

    """
    return root / OVERLAY_REL / f"run-{uuid.uuid4().hex}"


def serve_index(overlay: Path) -> tuple[ThreadingHTTPServer, int]:
    """Serve the overlay over loopback on an ephemeral port.

    cargo rejects `file://` sparse indexes. Binding port 0 lets the OS pick
    an unbound port at bind time, so no fixed port can be pre-bound by
    something else ahead of the run and no two concurrent runs can collide;
    the returned port is the one actually bound, read back after bind. The
    server runs on a daemon thread: it exists only for the run's own cargo
    invocations and dies with the process.

    Returns:
        The bound server and the port it actually bound.

    """
    server = ThreadingHTTPServer(("127.0.0.1", 0), _quiet_handler(overlay))
    server.daemon_threads = True
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server, server.server_address[1]


def packing_plan(packages: dict[str, Json], packed: set[str]) -> tuple[list[str], set[str]]:
    """Order the packed members by dependency, and name the cyclic members.

    Packing member M resolves M's rewritten manifest, dev-dependencies
    included, so every member M requires must already be indexed. A
    dev-dependency edge can close a normal-dependency cycle (db dev-requires
    pools while uniswap requires db), and no strict order exists there: the
    returned `cyclic` set names the members of multi-member cycles, which get
    provisional entries before the first of them packs. Components order by
    Tarjan, members alphabetically inside a component.

    Returns:
        The packing order -- dependencies before dependents, cycle members
        contiguous -- and the cyclic member set.

    """
    edges = {
        name: [dep for dep in (d["name"] for d in pkg["dependencies"]) if dep in packed]
        for name, pkg in packages.items()
        if name in packed
    }
    index: dict[str, int] = {}
    low: dict[str, int] = {}
    on_stack: set[str] = set()
    stack: list[str] = []
    sccs: list[list[str]] = []
    counter = [0]

    def strongconnect(node: str) -> None:
        index[node] = low[node] = counter[0]
        counter[0] += 1
        stack.append(node)
        on_stack.add(node)
        for dep in edges.get(node, ()):
            if dep not in index:
                strongconnect(dep)
                low[node] = min(low[node], low[dep])
            elif dep in on_stack:
                low[node] = min(low[node], index[dep])
        if low[node] == index[node]:
            component = []
            while True:
                member = stack.pop()
                on_stack.discard(member)
                component.append(member)
                if member == node:
                    break
            sccs.append(sorted(component))

    for name in sorted(edges):
        if name not in index:
            strongconnect(name)

    cyclic = {name for scc in sccs if len(scc) > 1 for name in scc}
    component_of = {name: i for i, scc in enumerate(sccs) for name in scc}
    component_edges = {
        i: {component_of[dep] for dep in edges[scc[0]] if component_of[dep] != i}
        for i, scc in enumerate(sccs)
    }
    for i, scc in enumerate(sccs):
        component_edges[i] = {
            component_of[dep] for name in scc for dep in edges[name] if component_of[dep] != i
        }
    ready = [i for i, deps in component_edges.items() if not deps]
    placed: set[int] = set()
    plan: list[str] = []
    while ready:
        component = min(ready)
        ready.remove(component)
        placed.add(component)
        plan.extend(sccs[component])
        for other, deps in component_edges.items():
            if other not in placed and component in deps:
                deps.discard(component)
                if not deps:
                    ready.append(other)
    if len(plan) != len(edges):
        fail("dependency graph between member cycles did not order")
    return plan, cyclic


def provisional_entry(name: str, version: str) -> Json:
    """Build an entry that only has to let a requirement match during packing.

    A packing run never builds, so the missing dependency closure inside a
    cycle costs nothing; the faithful entry replaces this one before the
    publish run resolves anything through it.

    Returns:
        The provisional index entry: name and version only, with a null
        checksum and no dependencies.

    """
    return {
        "name": name,
        "vers": version,
        "deps": [],
        "cksum": "0" * 64,
        "features": {},
        "yanked": False,
    }


def write_entry(overlay: Path, entry: Json) -> None:
    """Write one index entry into the overlay at its sparse-index path."""
    path = overlay / "index" / index_rel(entry["name"])
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(entry) + "\n", encoding="utf-8")


def packed_crate(rust: Path, name: str, version: str) -> Path:
    """Return the packed `.crate` for a member, failing loud if cargo made none.

    Returns:
        The `.crate` path cargo's packaging left in the staging tree.

    """
    crate = rust / "target" / "package" / f"{name}-{version}.crate"
    if not crate.is_file():
        fail(f"cargo package produced no .crate for {name}")
    return crate


def discard_stale_package(rust: Path, name: str, version: str) -> None:
    """Remove a previous pack's artifacts for this member.

    A packed `.crate` embeds the lockfile of the resolution that produced it;
    when an overlay entry has since changed (a packing cycle's provisional
    checksums becoming faithful), reusing the old artifact would make cargo
    reject the pack against its own stale embedded lock.
    """
    package_dir = rust / "target" / "package"
    (package_dir / f"{name}-{version}.crate").unlink(missing_ok=True)
    shutil.rmtree(package_dir / f"{name}-{version}", ignore_errors=True)
    # cargo stages workspace members it has already packaged into a persistent
    # local registry here; entries staged against an earlier overlay state
    # (a packing cycle's provisional checksums) would otherwise outlive the
    # overlay entries they were read from and fail the next pack's resolution.
    shutil.rmtree(package_dir / "tmp-registry", ignore_errors=True)


def packaged_manifest(crate: Path, name: str, version: str) -> str:
    """Read the packaged Cargo.toml out of a member's `.crate` archive.

    Returns:
        The rewritten manifest text exactly as an upload would carry it.

    """
    with tarfile.open(crate) as archive:
        member = archive.extractfile(f"{name}-{version}/Cargo.toml")
        if member is None:
            fail(f"packed crate for {name} has no manifest")
        return member.read().decode("utf-8")


def main(argv: list[str]) -> int:
    """Parse arguments, gate on the census, and run the verification.

    Returns:
        The process exit code: 0 on success, otherwise the first failing
        phase's code.

    """
    parser = argparse.ArgumentParser(description="The publish-verify overlay oracle.")
    parser.add_argument("--root", type=Path, default=REPO_ROOT, help="tree root")
    parser.add_argument(
        "--keep-overlay",
        action="store_true",
        help="keep the per-run overlay directory after the run (for inspection)",
    )
    args, extra = parser.parse_known_args(argv)
    reject_retargeting(extra)
    exclusions, extra = split_exclusions(extra)
    root = args.root.resolve()
    rust = root / "rust"

    checked = census(root, "--check")
    sys.stdout.write(checked.stdout)
    sys.stderr.write(checked.stderr)
    if checked.returncode != 0:
        return checked.returncode
    blocked = census(root, "--blocked")
    if blocked.returncode != 0:
        sys.stderr.write(blocked.stderr)
        return blocked.returncode
    withheld = sorted(blocked.stdout.split())

    overlay = overlay_run_dir(root)
    mirror = root / MIRROR_REL
    try:
        with run_lock(root, lock_wait_seconds()):
            cargo_env = cargo_environment(overlay / "cargo-home")
            (overlay / "index").mkdir(parents=True, exist_ok=True)
            (overlay / "dl").mkdir(parents=True, exist_ok=True)
            # The workspace metadata is the first cargo resolution of the run,
            # already under the same-workspace lock.
            meta = run(
                [
                    "cargo",
                    "metadata",
                    "--no-deps",
                    "--format-version",
                    "1",
                    "--manifest-path",
                    str(rust / "Cargo.toml"),
                ],
                cwd=rust,
                capture=True,
                env=cargo_env,
            )
            if meta.returncode != 0:
                sys.stderr.write(meta.stderr)
                return meta.returncode
            packages: dict[str, Json] = {p["name"]: p for p in json.loads(meta.stdout)["packages"]}
            # Validate the exclusions the moment the workspace membership is
            # known: an unknown name must fail loud before any packing, not
            # surface as cargo's own selection error after a partial run.
            unknown = sorted(set(exclusions) - set(packages))
            if unknown:
                fail("--exclude names no workspace member: " + ", ".join(unknown))
            return _verify(
                root,
                rust,
                overlay,
                mirror,
                packages,
                withheld,
                extra,
                cargo_env,
                exclusions=set(exclusions),
            )
    finally:
        if not args.keep_overlay:
            shutil.rmtree(overlay, ignore_errors=True)


def _verify(
    root: Path,
    rust: Path,
    overlay: Path,
    mirror: Path,
    packages: dict[str, Json],
    withheld: list[str],
    extra: list[str],
    cargo_env: dict[str, str],
    *,
    exclusions: set[str],
) -> int:
    """Build the overlay, pack the covered members, publish each against it.

    Runs under the same-workspace lock (`main` holds it across the metadata,
    packing, and publish phases), and every cargo invocation carries the
    run's own CARGO_HOME. `exclusions` are members the caller removed from
    the verification set: still packed and indexed (resolution set), never
    given a publish round of their own.

    Returns:
        The process exit code: 0 when every verification round passed,
        otherwise the failing round's code.

    """
    no_verify = "--no-verify" in extra
    # The sparse-index config: cargo downloads `.crate` files straight off the
    # overlay's own dl tree, so the verified build needs no network at all.
    dl_url = "file://" + quote(str(overlay / "dl"))
    (overlay / "index" / "config.json").write_text(
        json.dumps({"dl": f"{dl_url}/{{crate}}/{{version}}/download"}), encoding="utf-8"
    )

    # The mirror is keyed by the lockfile the workspace resolves to; a tree
    # without one (fresh checkouts of throwaway workspaces) gets it here, under
    # the same-workspace lock so two runs cannot race the shared lockfile.
    if not (root / LOCK_REL).is_file():
        lockfile = run(
            ["cargo", "metadata", "--format-version", "1"],
            cwd=rust,
            capture=True,
            env=cargo_env,
        )
        if lockfile.returncode != 0:
            sys.stderr.write(lockfile.stderr)
            return lockfile.returncode
    lock_hash = hashlib.sha256((root / LOCK_REL).read_bytes()).hexdigest()
    externals, source_problems = lock_externals(root / LOCK_REL, set(packages))
    for problem in source_problems:
        fail(problem)
    if externals:
        errors: list[str] = []
        with ThreadPoolExecutor(max_workers=16) as pool:
            errors.extend(
                message
                for message in pool.map(
                    lambda item: mirror_external(
                        item[0], item[1], lock_hash, mirror, overlay, no_verify=no_verify
                    ),
                    sorted(externals.items()),
                )
                if message
            )
        if errors:
            for message in errors:
                fail(message)

    server, port = serve_index(overlay)
    try:
        config_flags = [
            f'source.crates-io.replace-with="{SOURCE_NAME}"',
            f'source.{SOURCE_NAME}.registry="sparse+http://127.0.0.1:{port}/index/"',
        ]

        for name in withheld:
            listing = run(
                ["cargo", "package", "--list", "--allow-dirty", "-p", name],
                cwd=rust,
                capture=True,
                env=cargo_env,
            )
            files = len(listing.stdout.splitlines()) if listing.returncode == 0 else 0
            print(f"publish-blocked {name}: {files} files would package")

        # `publish = false` members are out of the census's scope and cargo
        # refuses to publish them; they are not part of the covered set. An
        # allowlist naming any other registry is not a crates.io publish
        # shape: this run certifies the crates.io form only, so it fails loud
        # instead of silently verifying the member against the overlay.
        packed = []
        for name in sorted(packages):
            if name in withheld:
                continue
            allowlist = packages[name].get("publish")
            strangers = [registry for registry in (allowlist or []) if registry != "crates-io"]
            if strangers:
                fail(
                    f"{name} publishes to {', '.join(strangers)} -- "
                    "the overlay certifies the crates.io publish form only"
                )
            if allowlist != []:
                packed.append(name)
        plan, cyclic = packing_plan(packages, set(packed))
        for name in sorted(cyclic):
            write_entry(overlay, provisional_entry(name, packages[name]["version"]))
        for name in plan:
            pkg = packages[name]
            version = pkg["version"]
            discard_stale_package(rust, name, version)
            packaging = run(
                [
                    "cargo",
                    "package",
                    "--no-verify",
                    "--allow-dirty",
                    "--locked",
                    "-p",
                    name,
                    "--registry",
                    "crates-io",
                    "--config",
                    config_flags[0],
                    "--config",
                    config_flags[1],
                ],
                cwd=rust,
                capture=True,
                env=cargo_env,
            )
            if packaging.returncode != 0:
                sys.stderr.write(packaging.stdout)
                sys.stderr.write(packaging.stderr)
                fail(f"cannot package {name}: the published form does not resolve")
            crate = packed_crate(rust, name, version)
            download = overlay / "dl" / name / version / "download"
            download.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(crate, download)
            with tarfile.open(crate) as archive:
                files = sum(1 for _ in archive)
            entry = entry_from_packaged_manifest(
                packaged_manifest(crate, name, version),
                hashlib.sha256(crate.read_bytes()).hexdigest(),
            )
            write_entry(overlay, entry)
            print(f"publish-verify covered {name} v{version}: {files} files packaged")

        # A packing cycle's members were only provisionally indexed (their
        # cycle order left nothing to index them from); their packaged
        # artifacts now exist, so their entries carry the exact deps and
        # checksums a real upload would.
        for name in sorted(cyclic):
            version = packages[name]["version"]
            crate = packed_crate(rust, name, version)
            entry = entry_from_packaged_manifest(
                packaged_manifest(crate, name, version),
                hashlib.sha256(crate.read_bytes()).hexdigest(),
            )
            write_entry(overlay, entry)
        server.shutdown()
        server.server_close()

        # The verification runs as one publish round per member against the
        # finished overlay, rather than a single `--workspace` round. cargo's
        # workspace round stages every packaged member into a persistent
        # local registry (rust/target/package/tmp-registry) and resolves the
        # remaining members partly from it; a packing cycle makes the staged
        # bytes and the overlay entries diverge (each side embeds the other's
        # checksums), which cargo rejects as a checksum changed between lock
        # files. One member per round, with the staging registry discarded in
        # between, resolves every member purely against the overlay -- the
        # same rewritten manifests and verify builds a real upload round
        # checks, without the registry writes.
        publish_server, publish_port = serve_index(overlay)
        try:
            publish_flags = [
                f'source.crates-io.replace-with="{SOURCE_NAME}"',
                f'source.{SOURCE_NAME}.registry="sparse+http://127.0.0.1:{publish_port}/index/"',
            ]
            for name in plan:
                if name in exclusions:
                    version = packages[name]["version"]
                    print(
                        f"publish-verify resolution-only {name} v{version}: "
                        "--exclude keeps it in the overlay, no verify round"
                    )
                    continue
                version = packages[name]["version"]
                discard_stale_package(rust, name, version)
                publish = run(
                    [
                        "cargo",
                        "publish",
                        "--registry",
                        "crates-io",
                        "--locked",
                        "--dry-run",
                        "--allow-dirty",
                        "-p",
                        name,
                        *[flag for value in publish_flags for flag in ("--config", value)],
                        *extra,
                    ],
                    cwd=rust,
                    capture=True,
                    env=cargo_env,
                )
                sys.stdout.write(publish.stdout)
                sys.stderr.write(publish.stderr)
                if publish.returncode != 0:
                    return publish.returncode
            return 0
        finally:
            publish_server.shutdown()
            publish_server.server_close()
    finally:
        server.shutdown()
        server.server_close()


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
