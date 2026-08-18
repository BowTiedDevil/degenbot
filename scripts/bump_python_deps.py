#!/usr/bin/env python3
"""Rewrite pyproject.toml version requirements to the latest stable PyPI release.

The Python-side counterpart of `cargo upgrade --incompatible`: it rewrites the
version *requirements* in pyproject.toml, not just the lockfile — the step a
plain `uv lock --upgrade` cannot do, because re-resolving only advances
within the existing ranges, so `pydantic ~= 2.13` can never see 2.14 or 3.0.
The script bumps the ranges themselves (across semver majors); the
`uv lock --upgrade` + `uv sync` that follow in `just update-deps` then
re-resolve and install.

`[project] dependencies` and version-pinned entries of every
`[dependency-groups]` group are targeted. Unpinned entries (the dev group's
style) are already open-ended, so uv's re-resolve covers them — nothing to
rewrite there. Direct-URL requirements are untouched, and `<`/`<=` clauses
are respected: a requirement capped below the latest release is reported and
left alone rather than made unsatisfiable.

Only stable releases (no alpha/beta/rc/dev tail) uploaded before the
project's `[tool.uv] exclude-newer` horizon (repo setting: 14 days) are
considered, so the rewritten pins are always resolvable by uv under the very
same setting. Stdlib + tomlkit only (a degenbot production dependency, found
in the project venv by `uv run`).

Usage:
  uv run python scripts/bump_python_deps.py            # rewrite pyproject.toml + report
  uv run python scripts/bump_python_deps.py --dry-run  # report only, no writes
"""

from __future__ import annotations

import argparse
import json
import re
import sys
import urllib.error
import urllib.request
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any

import tomlkit
from tomlkit.exceptions import TOMLKitError

REPO_ROOT = Path(__file__).resolve().parent.parent
PYPI_JSON = "https://pypi.org/pypi/{name}/json"

_NAME_NORM = re.compile(r"[-_.]+")
_VERSION_PREFIX = re.compile(r"^(\d+(?:\.\d+)*)")
# Prerelease tail after a digit or separator: 8.3.0rc1, 1.0b2, 2.0.dev3 ...
_PRERELEASE = re.compile(r"(?:\d|[-._])(alpha|beta|pre|preview|dev|rc|a|b|c)\d*$", re.IGNORECASE)
_CLAUSE = re.compile(r"^(~=|==|>=|<=|!=|>|<)\s*(.+)$")
_REQ_BASE = re.compile(r"^([A-Za-z0-9][A-Za-z0-9._-]*)(\[[^\]]*\])?\s*(.*)$")
_RELATIVE = re.compile(r"^\s*(\d+)\s+(day|week|month|year)s?\s*$", re.IGNORECASE)
_UNIT_DAYS = {"day": 1, "week": 7, "month": 30, "year": 365}
_ISO = re.compile(r"^\d{4}-\d{2}-\d{2}")


def die(message: str) -> None:
    print(f"error: {message}", file=sys.stderr)
    sys.exit(1)


def version_key(value: str) -> tuple[int, ...]:
    m = _VERSION_PREFIX.match(value.strip())
    if not m:
        return (0,)
    return tuple(int(p) for p in m.group(1).split("."))


def compare_versions(a: str, b: str) -> int:
    """Compare numeric prefixes, missing components padded with 0 (8.4 == 8.4.0)."""
    ka, kb = version_key(a), version_key(b)
    width = max(len(ka), len(kb))
    ka, kb = ka + (0,) * (width - len(ka)), kb + (0,) * (width - len(kb))
    return (ka > kb) - (ka < kb)


def leading_components(value: str, count: int) -> str:
    parts = _VERSION_PREFIX.match(value.strip()).group(1).split(".")
    return ".".join(parts[:count])


def is_stable(value: str) -> bool:
    return not _PRERELEASE.search(value)


def as_utc(raw: str) -> datetime | None:
    if not raw:
        return None
    try:
        dt = datetime.fromisoformat(raw.replace("Z", "+00:00"))
    except ValueError:
        return None
    return dt if dt.tzinfo else dt.replace(tzinfo=timezone.utc)


def fetch_releases(name: str) -> list[tuple[str, str]]:
    """[(upload_time, version)] for every non-empty PyPI release of the name."""
    lookup = _NAME_NORM.sub("-", name).lower()
    request = urllib.request.Request(
        PYPI_JSON.format(name=lookup), headers={"User-Agent": "degenbot-dep-bump"}
    )
    try:
        with urllib.request.urlopen(request, timeout=30) as resp:
            payload = json.load(resp)
    except (urllib.error.HTTPError, urllib.error.URLError, TimeoutError) as exc:
        die(f"PyPI lookup for '{lookup}' failed: {exc}")
    out: list[tuple[str, str]] = []
    for version, uploads in payload["releases"].items():
        if uploads:
            out.append((max(u.get("upload_time", "") for u in uploads), version))
    return out


def pick_latest(releases: list[tuple[str, str]], cutoff: datetime | None) -> str:
    """Highest stable version uploaded before the horizon, or '' if none."""
    ordered = sorted(releases, key=lambda t: (version_key(t[1]), t[0]), reverse=True)
    for stamp, version in ordered:
        if not is_stable(version):
            continue
        if cutoff is None:
            return version
        uploaded = as_utc(stamp)
        if uploaded is None:
            continue
        if uploaded <= cutoff:
            return version
    return ""


def exclude_newer_cutoff(doc: Any) -> datetime | None:
    """The project's [tool.uv] exclude-newer horizon, or None if unset/unparseable."""
    tool = doc.get("tool")
    uv_table = tool.get("uv") if isinstance(tool, dict) else None
    raw = uv_table.get("exclude-newer") if isinstance(uv_table, dict) else None
    if raw is None:
        return None
    raw = str(raw).strip()
    m = _RELATIVE.match(raw)
    if m:
        days = int(m.group(1)) * _UNIT_DAYS[m.group(2).lower()]
        return datetime.now(timezone.utc) - timedelta(days=days)
    if _ISO.match(raw):
        dt = as_utc(raw)
        if dt is not None:
            return dt
    print(f"warning: unparseable [tool.uv] exclude-newer {raw!r}; assuming no horizon", file=sys.stderr)
    return None


def requirement_parts(req: str) -> tuple[str, str, str] | None:
    """(name, specifier_clauses, marker) or None for URL/unparseable requirements."""
    s = req.strip()
    if " @" in s:
        return None
    marker = ""
    if ";" in s:
        s, tail = s.split(";", 1)
        marker = ";" + tail
    m = _REQ_BASE.match(s.strip())
    if not m:
        return None
    return m.group(1), m.group(3).strip(), marker.strip()


def rewrite_requirement(req: str, latest: str) -> tuple[str, str]:
    """Return (new_requirement, note); new == req when nothing changed."""
    parts = requirement_parts(req)
    if parts is None:
        return req, "direct URL dependency (untouched)"
    _name, rest, marker = parts
    clauses = [c.strip() for c in rest.split(",") if c.strip()]
    if not clauses:
        return req, "no version pin; uv lock --upgrade covers it"
    new_clauses: list[str] = []
    kept: list[str] = []
    for clause in clauses:
        m = _CLAUSE.match(clause)
        if not m:
            new_clauses.append(clause)
            continue
        op, ver = m.group(1), m.group(2).strip()
        if op == "~=":
            new_clauses.append(f"~= {leading_components(latest, 2)}")
        elif op == "==" and ver.endswith(".*"):
            new_clauses.append(f"== {leading_components(latest, 2)}.*")
        elif op in ("==", ">="):
            new_clauses.append(f"{op} {latest}")
        elif op in ("<", "<="):
            cap = compare_versions(latest, ver)
            if (op == "<" and cap >= 0) or (op == "<=" and cap > 0):
                return req, f"upper bound '{clause}' is below latest {latest} (kept as-is)"
            new_clauses.append(clause)
            kept.append(clause)
        else:  # '>' / '!=' / '!!' etc. — bumping the clause would exclude the latest
            new_clauses.append(clause)
            kept.append(clause)
    note = f"clause(s) kept as-is: {', '.join(kept)}" if kept else ""
    return f"{parts[0]} {', '.join(new_clauses)}{marker}", note


def _replace_at(arr: Any, index: int, value: str) -> None:
    """Replace a tomlkit array element (item assignment may be unsupported)."""
    try:
        arr[index] = value
    except TypeError:
        arr.remove(arr[index])
        arr.insert(index, value)


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Bump pyproject.toml version requirements to the latest stable "
        "PyPI release (the cargo-upgrade analog for the Python side)."
    )
    parser.add_argument("--dry-run", action="store_true",
                        help="report the planned bumps without writing pyproject.toml")
    parser.add_argument("--pyproject", default=str(REPO_ROOT / "pyproject.toml"),
                        help="path to pyproject.toml (default: repo root)")
    args = parser.parse_args()

    path = Path(args.pyproject)
    try:
        doc = tomlkit.parse(path.read_text())
    except (OSError, TOMLKitError) as exc:
        die(f"cannot parse {path}: {exc}")

    cutoff = exclude_newer_cutoff(doc)
    if cutoff is not None:
        print(f"horizon: releases uploaded before {cutoff.strftime('%Y-%m-%d %H:%M')} UTC")

    sections: list[tuple[str, Any]] = []
    project = doc.get("project")
    if isinstance(project, dict) and "dependencies" in project:
        sections.append(("main", project["dependencies"]))
    groups = doc.get("dependency-groups")
    if isinstance(groups, dict):
        for group, deps in groups.items():
            if isinstance(deps, list):
                sections.append((str(group), deps))

    changed = 0
    replacements: list[tuple[Any, int, str]] = []
    latest_cache: dict[str, str] = {}
    for section, deps in sections:
        label = "" if section == "main" else f" [{section}]"
        for index, req in enumerate(deps):
            req = str(req)
            parts = requirement_parts(req)
            if parts is None:
                print(f"{req.strip()}: untouched (direct URL dependency)")
                continue
            name, rest, _marker = parts
            if not rest:
                print(f"{name}{label}: untouched (no version pin)")
                continue
            if name not in latest_cache:
                latest = pick_latest(fetch_releases(name), cutoff)
                if not latest:
                    die(f"no stable PyPI release of '{name}' inside the horizon; bump it by hand")
                latest_cache[name] = latest
            new_req, note = rewrite_requirement(req, latest_cache[name])
            if new_req == req:
                print(f"{name}{label}: {note or f'already at latest stable ({rest})'}")
                continue
            changed += 1
            replacements.append((deps, index, new_req))
            dry = "  [dry run]" if args.dry_run else ""
            suffix = f"  ({note})" if note else ""
            print(f"{name}{label}: {req.strip()} -> {new_req}{suffix}{dry}")

    if not changed:
        print("nothing to bump on the Python side")
        return 0
    for deps, index, new_req in replacements:
        _replace_at(deps, index, new_req)
    if args.dry_run:
        print(f"dry run: {changed} requirement(s) would change in {path.name}")
    else:
        path.write_text(tomlkit.dumps(doc))
        print(f"{changed} requirement(s) updated in {path.name}; next: uv lock --upgrade && uv sync")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
