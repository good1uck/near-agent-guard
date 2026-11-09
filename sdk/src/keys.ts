// Key loading. Keys are never stored in the repo.
import { existsSync, readFileSync, mkdirSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { join, dirname } from "node:path";

/** Where `agent-guard agent-key` stores the function-call key for an agent. */
export function agentKeyPath(agentId: string, network: string): string {
  return join(homedir(), ".agent-guard", network, `${agentId}.json`);
}

export function saveAgentKey(agentId: string, network: string, privateKey: string, publicKey: string) {
  const p = agentKeyPath(agentId, network);
  mkdirSync(dirname(p), { recursive: true });
  writeFileSync(p, JSON.stringify({ account_id: agentId, public_key: publicKey, private_key: privateKey }, null, 2), { mode: 0o600 });
  return p;
}

/**
 * Resolve a private key for `accountId`, in this order:
 *  1. NEAR_PRIVATE_KEY env var (only if NEAR_ACCOUNT_ID is unset or equals accountId)
 *  2. the agent function-call key in ~/.agent-guard/<network>/<accountId>.json (unless fullAccess)
 *  3. near-cli legacy keychain ~/.near-credentials/<network>/<accountId>.json
 */
export function loadKey(accountId: string, network: string, opts: { fullAccess?: boolean } = {}): string {
  const env = process.env.NEAR_PRIVATE_KEY;
  if (env && (!process.env.NEAR_ACCOUNT_ID || process.env.NEAR_ACCOUNT_ID === accountId)) return env;
  const candidates = [
    ...(opts.fullAccess ? [] : [agentKeyPath(accountId, network)]),
    join(homedir(), ".near-credentials", network, `${accountId}.json`),
  ];
  for (const p of candidates) {
    if (existsSync(p)) {
      const j = JSON.parse(readFileSync(p, "utf8"));
      const k = j.private_key ?? j.secret_key;
      if (k) return k;
    }
  }
  throw new Error(
    `No key for ${accountId} on ${network}. Set NEAR_PRIVATE_KEY or create the account with near-cli ` +
      `(save-to-legacy-keychain) so ~/.near-credentials/${network}/${accountId}.json exists.`,
  );
}
