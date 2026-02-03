// Default RPC URLs for blockchain queries
// Add more chains as needed following this pattern

export const DEFAULT_RPC_URLS: Record<string, string> = {
  mainnet: "http://localhost:8545",
  // Examples for adding other chains:
  // arbitrum: "http://localhost:8546",
  // base: "http://localhost:8547",
  // optimism: "http://localhost:8548",
};

// Fallback RPC URL when no chain is specified or chain has no default
export const DEFAULT_RPC_URL = DEFAULT_RPC_URLS.mainnet;

/**
 * Get the appropriate RPC URL for a given chain.
 * Priority: 1) User-provided rpc_url, 2) Chain-specific default, 3) Fallback default
 */
export function getRpcUrl(chain?: string, rpcUrl?: string): string {
  if (rpcUrl) {
    return rpcUrl;
  }

  if (chain && chain in DEFAULT_RPC_URLS) {
    return DEFAULT_RPC_URLS[chain];
  }

  return DEFAULT_RPC_URL;
}
