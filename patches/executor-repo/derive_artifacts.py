import json, os, sys
combined_path, out = sys.argv[1], sys.argv[2]
os.makedirs(out, exist_ok=True)
d = json.load(open(combined_path))
key = [k for k in d if "cmd_executor.vy" in k][0]
c = d[key]
strip0x = lambda s: s[2:] if s.startswith("0x") else s
creation = strip0x(c["bytecode"]).strip()
runtime = strip0x(c["bytecode_runtime"]).strip()
open(f"{out}/cmd_executor.creation.hex","w").write(creation)
open(f"{out}/cmd_executor.runtime.hex","w").write(runtime)
open(f"{out}/cmd_executor.abi.json","w").write(json.dumps(c["abi"], indent=0))
open(f"{out}/cmd_executor.method_identifiers.json","w").write(json.dumps(c["method_identifiers"], indent=0))
open(f"{out}/cmd_executor.error_map.json","w").write(json.dumps(c["source_map_runtime"].get("error_map",{}), indent=0))
open(f"{out}/cmd_executor.immutables.json","w").write(json.dumps(c["layout"]["code_layout"], indent=0))
print(f"creation {len(creation)//2}B; runtime {len(runtime)//2}B; vyper {d.get('version')}")
