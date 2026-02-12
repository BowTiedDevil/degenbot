import { existsSync, readFileSync } from "fs"
import { join } from "path"

interface ChainInfo {
  name: string
  chain: string
  chainId: number
  rpc: string[]
  nativeCurrency: {
    name: string
    symbol: string
    decimals: number
  }
}

// Chain alias mapping - maps various names to canonical chain identifiers
const CHAIN_ALIASES: { [key: string]: string } = {
  // Ethereum
  "ethereum": "ethereum",
  "mainnet": "ethereum",
  "eth": "ethereum",
  "1": "ethereum",
  // Arbitrum
  "arbitrum": "arbitrum",
  "arb": "arbitrum",
  "arbitrum-one": "arbitrum",
  "42161": "arbitrum",
  // Optimism
  "optimism": "optimism",
  "op": "optimism",
  "10": "optimism",
  // Base
  "base": "base",
  "8453": "base",
}

const DEFAULT_CONFIG_PATH = ".opencode/rpc-config.json"

function getCanonicalChainName(chain: string): string {
  const normalized = chain.toLowerCase().trim()
  return CHAIN_ALIASES[normalized] || normalized
}

function loadProjectConfig(worktree: string): Record<string, string | string[]> | null {
  const configPath = join(worktree, DEFAULT_CONFIG_PATH)

  if (!existsSync(configPath)) {
    return null
  }

  try {
    const content = readFileSync(configPath, "utf-8")
    const parsed = JSON.parse(content) as Record<string, unknown>

    // Validate structure
    for (const [key, value] of Object.entries(parsed)) {
      if (typeof value !== "string" && !Array.isArray(value)) {
        return null
      }
      if (Array.isArray(value) && !value.every((v) => typeof v === "string")) {
        return null
      }
    }

    return parsed as Record<string, string | string[]>
  } catch {
    return null
  }
}

function findInConfig(
  config: Record<string, string | string[]>,
  chain: string
): string | string[] | null {
  const canonicalName = getCanonicalChainName(chain)

  // Direct lookup
  if (config[chain]) {
    return config[chain]
  }

  // Try canonical name
  if (config[canonicalName]) {
    return config[canonicalName]
  }

  // Try numeric chain ID if applicable
  const chainId = parseInt(chain)
  if (!isNaN(chainId)) {
    const idKey = Object.keys(config).find((key) => parseInt(key) === chainId)
    if (idKey) {
      return config[idKey]
    }
  }

  return null
}

export async function resolveRpcUrl(
  chain: string,
  worktree: string
): Promise<string> {
  const chainIdentifier = chain.toLowerCase().trim()

  // Check project config first
  const projectConfig = loadProjectConfig(worktree)

  if (projectConfig) {
    const configRpc = findInConfig(projectConfig, chainIdentifier)
    if (configRpc) {
      const rpcUrls = Array.isArray(configRpc) ? configRpc : [configRpc]
      if (rpcUrls.length > 0) {
        return rpcUrls[0]
      }
    }
  }

  // Fall back to fetching from chainid.network
  const response = await fetch("https://chainid.network/chains.json")
  if (!response.ok) {
    throw new Error(`Failed to fetch chain data from chainid.network (status ${response.status})`)
  }

  const chains: ChainInfo[] = await response.json()

  const chainId = parseInt(chainIdentifier)
  const matchedChain = chains.find((c) => {
    if (!isNaN(chainId) && c.chainId === chainId) {
      return true
    }
    return (
      c.name.toLowerCase() === chainIdentifier ||
      c.chain.toLowerCase() === chainIdentifier
    )
  })

  if (!matchedChain) {
    throw new Error(`No chain found for identifier "${chain}"`)
  }

  // Filter out placeholder URLs
  const validRpcs = matchedChain.rpc.filter(
    (url) => !url.includes("${") && !url.includes("API_KEY")
  )

  if (validRpcs.length === 0) {
    throw new Error(`No public RPC URLs available for ${matchedChain.name} (Chain ID: ${matchedChain.chainId})`)
  }

  return validRpcs[0]
}
