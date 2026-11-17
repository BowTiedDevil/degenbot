"""Generate Solidity interfaces from the pinned-vyper artifacts.

Vyper's ABI is the authority here: the generated interfaces are what the tests
call, so an ABI change is a compile error in this lane rather than a silently
stale hand-written declaration.
"""

import json
import re
from pathlib import Path

HERE = Path(__file__).resolve().parent
ARTIFACTS = HERE / "artifacts"
OUT = HERE / "src" / "generated"

# Content-keyed struct names, with readable overrides for the tuple shapes that
# the tests construct by hand.
NAMED = {
    "(address,address,uint24,int24,address)": "PoolKey",
    "(bool,int256,uint160)": "SwapParams",
    "(int24,int24,int256,bytes32)": "ModifyLiquidityParams",
    "(uint256,uint256)": "ExecuteResult",
}


def sanitize(type_str: str) -> str:
    return re.sub(r"[^A-Za-z0-9]+", "_", type_str).strip("_")


def main() -> None:
    OUT.mkdir(parents=True, exist_ok=True)
    structs: dict[str, str] = {}
    bodies: dict[str, list[str]] = {}

    def struct_for(type_str: str, components) -> str:
        if type_str in structs:
            return structs[type_str]
        name = NAMED.get(type_str, "T_" + sanitize(type_str))
        structs[type_str] = name
        fields = []
        for i, comp in enumerate(components):
            ctype = comp["type"]
            fname = comp.get("name") or f"f{i}"
            if ctype.startswith("("):
                inner = struct_for(ctype, comp["components"])
                fields.append(f"        {inner} {fname};")
            else:
                fields.append(f"        {ctype} {fname};")
        # placeholder written once children are known; re-emit at the end
        bodies[name] = fields
        return name

    def tuple_key(components) -> str:
        return "(" + ",".join(c["type"] for c in components) + ")"

    def solidity_type(entry) -> str:
        t = entry["type"]
        if t.startswith("tuple"):
            name = struct_for(tuple_key(entry["components"]), entry["components"])
            suffix = t[len("tuple") :]
            if suffix:  # tuple[] / tuple[2]
                return name + suffix
            return name
        return t

    def is_ref(type_str: str) -> bool:
        return (
            type_str in ("bytes", "string")
            or type_str.endswith("]")
            or not type_str.startswith(("uint", "int", "address", "bool", "bytes", "string"))
        )

    def param_list(entries, data_location: str, prefix: str = "a") -> str:
        parts = []
        for i, e in enumerate(entries):
            t = solidity_type(e)
            loc = f" {data_location}" if is_ref(t) else ""
            parts.append(f"{t}{loc} {e.get('name') or f'{prefix}{i}'}")
        return ", ".join(parts)

    paths = sorted(ARTIFACTS.glob("*.json")) + sorted((ARTIFACTS / "real").glob("*.json"))
    for path in paths:
        if path.name == "manifest.json":
            continue
        art = json.loads(path.read_text())
        name = art.get("contractName")
        if not name:
            continue  # manifest / stamp files are not contract artifacts
        lines = []
        for entry in art["abi"]:
            kind = entry["type"]
            if kind == "event":
                inputs = ", ".join(
                    f"{solidity_type(i)}"
                    f"{' indexed' if i.get('indexed') else ''}"
                    f" {i.get('name') or f'a{n}'}"
                    for n, i in enumerate(entry["inputs"])
                )
                lines.append(f"    event {entry['name']}({inputs});")
        for entry in art["abi"]:
            kind = entry["type"]
            if kind != "function":
                continue
            mutability = entry.get("stateMutability", "nonpayable")
            mut = "" if mutability == "nonpayable" else mutability
            params = param_list(entry["inputs"], "calldata")
            outs = param_list(entry["outputs"], "memory", prefix="r")
            ret = f" returns ({outs})" if outs else ""
            suffix = "" if not mut else f" {mut}"
            lines.append(f"    function {entry['name']}({params}) external{suffix}{ret};")
        bodies_repr = "\n".join(lines)
        (OUT / f"I{name}.sol").write_text(
            "// SPDX-License-Identifier: MIT\n"
            f"// Generated from artifacts/{name}.json by gen_interfaces.py — do not edit.\n"
            "pragma solidity ^0.8.28;\n\n"
            "import {PoolKey, SwapParams, ModifyLiquidityParams, ExecuteResult} "
            'from "./Types.sol";\n\n'
            f"interface I{name} {{\n{bodies_repr}\n}}\n"
        )
        print(f"I{name}.sol: {len(lines)} members")

    struct_text = "\n\n".join(
        f"struct {name} {{\n" + "\n".join(f.replace("        ", "    ") for f in fields) + "\n}"
        for name, fields in bodies.items()
    )
    (OUT / "Types.sol").write_text(
        "// SPDX-License-Identifier: MIT\n"
        "// Generated from the artifacts' tuple shapes by gen_interfaces.py — do not edit.\n"
        "pragma solidity ^0.8.28;\n\n"
        f"{struct_text}\n"
    )
    print("structs:", ", ".join(bodies))


if __name__ == "__main__":
    main()
