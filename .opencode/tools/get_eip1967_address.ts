import { tool } from "@opencode-ai/plugin";
import { getRpcUrl } from "./defaults";

const EIP1967_SLOTS = {
  implementation: "0x360894a13ba1a3210667c828492db98dca3e2076cc3735a920a3ca505d382bbc",
  beacon: "0xa3f0ad74e5423aebfd80d3ef4346578335a9a72aeaee59ff6cb3582b35133d50",
  admin: "0xb53127684a568b3173ae13b9f8a6016e243e63b6e8ee1178d6a717850b5d6103",
};

export default tool({
  description: "Read the implementation, beacon, or admin address from EIP-1967 proxy upgradeable contracts. Use this when working with upgradeable proxy contracts (like OpenZeppelin proxies) to find the underlying implementation or admin addresses. Requires Foundry to be installed. The default RPC URL is fetched from defaults.ts if not specified.",
  args: {
    proxy_address: tool.schema
      .string()
      .describe("The proxy contract address (e.g., 0x1234...)")
      .regex(/^0x[a-fA-F0-9]{40}$/),
    slot_type: tool.schema
      .enum(["implementation", "beacon", "admin"])
      .describe("The EIP-1967 slot type to read (implementation, beacon, or admin)"),
    chain: tool.schema
      .string()
      .optional()
      .describe("Chain name (e.g., 'mainnet', 'sepolia') or EIP-155 chain ID. Defaults to mainnet"),
    block: tool.schema
      .string()
      .optional()
      .describe("Block number to query at (defaults to latest)"),
    rpc_url: tool.schema
      .string()
      .optional()
      .describe("Custom RPC URL to use instead of the default from defaults.ts"),
  },
  async execute(args: any, context: any) {
    const cmd_parts = ["cast", "storage"];

    if (args.chain) {
      cmd_parts.push("--chain", args.chain);
    }

    if (args.block) {
      cmd_parts.push("--block", args.block);
    }

    cmd_parts.push("--rpc-url", getRpcUrl(args.chain, args.rpc_url));

    // Get the slot for the requested type
    // @ts-expect-error slot_type is validated by schema
    const slot = EIP1967_SLOTS[args.slot_type];

    // Add required arguments
    cmd_parts.push(args.proxy_address, slot);

    try {
      // @ts-expect-error Bun runtime is available
      const result = await Bun.$`${cmd_parts}`.text();
      return result.trim();
    } catch (error: any) {
      if (error.stderr) {
        throw new Error(`cast storage failed: ${error.stderr}`);
      }
      throw new Error(`cast storage failed: ${error.message || error}`);
    }
  },
});
