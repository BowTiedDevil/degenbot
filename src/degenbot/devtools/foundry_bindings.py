from __future__ import annotations

import argparse
import hashlib
import subprocess
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from collections.abc import Mapping, Sequence

DEFAULT_CRATE_NAME = "degenbot_contract_bindings"
DEFAULT_ALLOY_VERSION = "2.0.5"
DEFAULT_SELECTED_CONTRACTS = (
    # The standalone monolith executors (Executor / AtomicExecutor /
    # LiquidationExecutor) were archived at P8 (EIP-2535 executor diamond is the
    # sole on-chain executor); their Foundry artifacts no longer exist. Bind the
    # canonical `IExecutor` interface instead — same strategy entry points
    # (executeNativeArb / matchInternal / composeFourLeg / triggerCoWFlashLoanRouter
    # / transferToSettlement), implemented by the diamond facets, kept current
    # (12-field ADR-029 MatchParams/ComposeParams).
    "IExecutor",
    "IFlashLoanRouter",
    "IFlashLoanReceiver",
    "IERC3156FlashBorrower",
    "IMorphoFlashLoanCallback",
    "IUniswapV3FlashCallback",
    "IReactorCallback",
    "IUniswapV4Hook",
    "MevPaymasterV9",
    "BaseMevPaymaster",
    "MevSafe",
    "MevBotDelegate",
    "StrategyLedger",
    "PermissionToken",
    "PathFinder",
    "IPathFinder",
    "MultiHopCaller",
    "RouterRegistry",
    "LpTransferLib",
    "TokenStandardIds",
    "TransientStorage",
)
IGNORED_DIRECTORY_NAMES = frozenset({".foundry", ".git", "cache", "out", "target"})


@dataclass(frozen=True)
class BindingWorkflowConfig:
    """Configuration for generating Rust bindings from the parent Solidity workspace."""

    contracts_root: Path
    bindings_path: Path
    crate_name: str = DEFAULT_CRATE_NAME
    alloy_version: str = DEFAULT_ALLOY_VERSION
    selected_contracts: tuple[str, ...] = DEFAULT_SELECTED_CONTRACTS

    def forge_build_command(
        self, *, out_path: Path | None = None, cache_path: Path | None = None
    ) -> list[str]:
        command = [
            "forge",
            "build",
            "--root",
            str(self.contracts_root),
            "src",
            "--skip",
            "test",
            "--skip",
            "script",
        ]
        if out_path is not None:
            command.extend(("--out", str(out_path)))
        if cache_path is not None:
            command.extend(("--cache-path", str(cache_path)))
        return command

    def forge_bind_command(
        self,
        *,
        overwrite: bool,
        out_path: Path | None = None,
        cache_path: Path | None = None,
    ) -> list[str]:
        command = [
            "forge",
            "bind",
            "--bindings-path",
            str(self.bindings_path),
            "--root",
            str(self.contracts_root),
            "--crate-name",
            self.crate_name,
            "--skip-build",
            "--alloy-version",
            self.alloy_version,
        ]
        if out_path is not None:
            command.extend(("--out", str(out_path)))
        if cache_path is not None:
            command.extend(("--cache-path", str(cache_path)))
        for contract_name in self.selected_contracts:
            command.extend(("--select", contract_name))
        if overwrite:
            command.append("--overwrite")
        return command


def default_repo_root() -> Path:
    return Path(__file__).resolve().parents[3]


def default_contracts_root(repo_root: Path) -> Path:
    return repo_root.parent.parent / "contracts"


def default_bindings_path(repo_root: Path) -> Path:
    return repo_root / "rust/crates/contract_bindings"


def validate_contracts_root(contracts_root: Path) -> Path:
    if not (contracts_root / "foundry.toml").is_file():
        msg = f"contracts root must contain foundry.toml: {contracts_root}"
        raise ValueError(msg)
    if not (contracts_root / "src").is_dir():
        msg = f"contracts root must contain src/: {contracts_root}"
        raise ValueError(msg)
    return contracts_root


def directories_match(left: Path, right: Path) -> bool:
    return _directory_digest(left) == _directory_digest(right)


def _directory_digest(root: Path) -> Mapping[str, str]:
    if not root.is_dir():
        return {}

    result: dict[str, str] = {}
    for file_path in sorted(path for path in root.rglob("*") if path.is_file()):
        relative = file_path.relative_to(root)
        if any(part in IGNORED_DIRECTORY_NAMES for part in relative.parts[:-1]):
            continue
        result[relative.as_posix()] = hashlib.sha256(file_path.read_bytes()).hexdigest()
    return result


def generate_bindings(config: BindingWorkflowConfig, *, dry_run: bool) -> int:
    validate_contracts_root(config.contracts_root)

    with tempfile.TemporaryDirectory(prefix="degenbot-contract-artifacts-") as temp_dir:
        artifact_root = Path(temp_dir)
        out_path = artifact_root / "out"
        cache_path = artifact_root / "cache"
        commands = [
            config.forge_build_command(out_path=out_path, cache_path=cache_path),
            config.forge_bind_command(
                overwrite=True,
                out_path=out_path,
                cache_path=cache_path,
            ),
        ]
        if dry_run:
            _print_commands(commands)
            return 0

        config.bindings_path.parent.mkdir(parents=True, exist_ok=True)
        status = _run_commands(commands)
        if status != 0:
            return status

        normalize_generated_crate_manifest(
            config.bindings_path,
            alloy_version=config.alloy_version,
        )
        inject_selected_binding_count(config.bindings_path)
        return 0


def check_bindings(config: BindingWorkflowConfig, *, dry_run: bool) -> int:
    validate_contracts_root(config.contracts_root)

    with tempfile.TemporaryDirectory(prefix="degenbot-contract-bindings-") as temp_dir:
        temp_root = Path(temp_dir)
        out_path = temp_root / "out"
        cache_path = temp_root / "cache"
        generated_path = temp_root / "contract_bindings"
        temp_config = BindingWorkflowConfig(
            contracts_root=config.contracts_root,
            bindings_path=generated_path,
            crate_name=config.crate_name,
            alloy_version=config.alloy_version,
            selected_contracts=config.selected_contracts,
        )
        commands = [
            temp_config.forge_build_command(out_path=out_path, cache_path=cache_path),
            temp_config.forge_bind_command(
                overwrite=True,
                out_path=out_path,
                cache_path=cache_path,
            ),
        ]
        if dry_run:
            _print_commands(commands)
            return 0

        status = _run_commands(commands)
        if status != 0:
            return status
        normalize_generated_crate_manifest(
            generated_path,
            alloy_version=config.alloy_version,
        )
        inject_selected_binding_count(generated_path)

        if directories_match(config.bindings_path, generated_path):
            return 0

    sys.stderr.write(
        "generated contract bindings are missing or stale; "
        "run `just gen-contract-bindings` and commit the generated crate\n",
    )
    return 1


def normalize_generated_crate_manifest(bindings_path: Path, *, alloy_version: str) -> None:
    """
    Keep generated bindings on the narrow Alloy feature surface degenbot needs.
    """

    manifest_path = bindings_path / "Cargo.toml"
    manifest_text = manifest_path.read_text(encoding="utf-8")
    generated_dependency = (
        f'alloy = {{ version = "{alloy_version}", features = ["sol-types", "contract"] }}'
    )
    normalized_dependency = (
        f'alloy = {{ version = "{alloy_version}", default-features = false, '
        'features = ["contract", "sol-types"] }'
    )
    if generated_dependency in manifest_text:
        manifest_path.write_text(
            manifest_text.replace(generated_dependency, normalized_dependency),
            encoding="utf-8",
        )
        return

    if normalized_dependency in manifest_text:
        return

    msg = f"generated Cargo.toml does not contain expected Alloy dependency: {manifest_path}"
    raise RuntimeError(msg)


def inject_selected_binding_count(bindings_path: Path) -> None:
    """
    Append a `SELECTED_CONTRACT_BINDING_COUNT` constant to the generated
    `lib.rs`.

    `forge bind --overwrite` regenerates `lib.rs` from scratch on every run, so
    this tripwire (used by `rust/tests/contract_bindings.rs` to catch the
    curated binding set silently shrinking) must be re-injected after each
    bind. The value is the number of generated `pub mod` modules — deterministic
    from the generated file, so the `check` and `generate` paths agree.
    """

    lib_path = bindings_path / "src" / "lib.rs"
    lib_text = lib_path.read_text(encoding="utf-8")
    module_count = sum(1 for line in lib_text.splitlines() if line.startswith("pub mod "))
    constant_block = (
        "\n"
        "/// Number of generated binding modules. Tripwire for the curated\n"
        "/// `DEFAULT_SELECTED_CONTRACTS` set in\n"
        "/// `degenbot.devtools.foundry_bindings` silently shrinking; injected\n"
        "/// after `forge bind` (not produced by `forge bind` itself).\n"
        f"pub const SELECTED_CONTRACT_BINDING_COUNT: usize = {module_count};\n"
    )
    lib_path.write_text(lib_text + constant_block, encoding="utf-8")


def _run_commands(commands: Sequence[Sequence[str]]) -> int:
    for command in commands:
        completed = subprocess.run(command, check=False)  # noqa: S603
        if completed.returncode != 0:
            return completed.returncode
    return 0


def _print_commands(commands: Sequence[Sequence[str]]) -> None:
    for command in commands:
        sys.stdout.write(f"{' '.join(command)}\n")


def _build_arg_parser() -> argparse.ArgumentParser:
    repo_root = default_repo_root()
    parser = argparse.ArgumentParser(
        description="Generate or check Foundry Rust contract bindings for degenbot.",
    )
    parser.add_argument("command", choices=("check", "generate"))
    parser.add_argument(
        "--contracts-root",
        type=Path,
        default=default_contracts_root(repo_root),
        help="Foundry contracts workspace to build.",
    )
    parser.add_argument(
        "--bindings-path",
        type=Path,
        default=default_bindings_path(repo_root),
        help="Generated Rust bindings crate path.",
    )
    parser.add_argument(
        "--crate-name",
        default=DEFAULT_CRATE_NAME,
        help="Rust crate name passed to forge bind.",
    )
    parser.add_argument(
        "--alloy-version",
        default=DEFAULT_ALLOY_VERSION,
        help="Alloy crate version written into generated bindings.",
    )
    parser.add_argument(
        "--select",
        action="append",
        dest="selected_contracts",
        help="Contract name filter passed to forge bind; repeat to override defaults.",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Print forge commands without executing them.",
    )
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = _build_arg_parser().parse_args(argv)
    config = BindingWorkflowConfig(
        contracts_root=args.contracts_root,
        bindings_path=args.bindings_path,
        crate_name=args.crate_name,
        alloy_version=args.alloy_version,
        selected_contracts=tuple(args.selected_contracts or DEFAULT_SELECTED_CONTRACTS),
    )

    try:
        if args.command == "generate":
            return generate_bindings(config, dry_run=args.dry_run)
        return check_bindings(config, dry_run=args.dry_run)
    except ValueError as exc:
        sys.stderr.write(f"{exc}\n")
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
