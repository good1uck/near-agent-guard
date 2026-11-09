// AgentGuardClient: a thin, typed wrapper around the agent-guard contract.
import {
  Account,
  JsonRpcProvider,
  KeyPair,
  actions,
  getTransactionLastResult,
  nearToYocto,
  teraToGas,
  yoctoToNear,
} from "near-api-js";
import type { KeyPairString } from "near-api-js";
import { loadKey } from "./keys.ts";

export type Network = "testnet" | "mainnet" | "sandbox";

export interface Policy {
  max_per_tx: string; // yoctoNEAR
  daily_limit: string; // yoctoNEAR
  max_gas_tgas: number;
}
export interface ReceiverRule {
  allow_transfer: boolean;
  methods: string[]; // ["*"] = any method
}
export interface Verdict {
  approved: boolean;
  reason: string;
  detail: string;
  action_id: string | null;
  remaining_today: string;
}
export interface Status {
  owner: string;
  agent: string | null;
  paused: boolean;
  policy: Policy;
  balance: string;
  available: string;
  spent_today: string;
  remaining_today: string;
  window_resets_at: string;
  approved_count: string;
  denied_count: string;
  failed_count: string;
}
export interface GuardEvent {
  event: string;
  data: Record<string, unknown>;
}
export interface TxResult<T> {
  value: T;
  txHash: string;
  explorerUrl: string;
  events: GuardEvent[];
}

export const RPC: Record<Network, string> = {
  testnet: "https://test.rpc.fastnear.com",
  mainnet: "https://free.rpc.fastnear.com",
  sandbox: "http://127.0.0.1:3030",
};
export const EXPLORER: Record<Network, string> = {
  testnet: "https://testnet.nearblocks.io/txns/",
  mainnet: "https://nearblocks.io/txns/",
  sandbox: "local:",
};

export const near = (n: string | number): string => nearToYocto(String(n) as any).toString();
export const fmt = (yocto: string | bigint): string => `${yoctoToNear(BigInt(yocto) as any, 4)} NEAR`;

export function provider(network: Network): JsonRpcProvider {
  return new JsonRpcProvider({ url: process.env.NEAR_RPC_URL || RPC[network] });
}

/** Parse NEP-297 `EVENT_JSON:` logs emitted by the guard from every receipt of a tx. */
export function parseEvents(tx: any): GuardEvent[] {
  const out: GuardEvent[] = [];
  for (const r of tx?.receipts_outcome ?? []) {
    for (const log of r.outcome?.logs ?? []) {
      if (!log.startsWith("EVENT_JSON:")) continue;
      try {
        const e = JSON.parse(log.slice("EVENT_JSON:".length));
        if (e.standard === "agent_guard") out.push({ event: e.event, data: e.data?.[0] ?? {} });
      } catch {
        /* ignore malformed logs */
      }
    }
  }
  return out;
}

/** Parse a receiver spec `account[:transfer][:method1,method2|*]` into a rule. */
export function parseAllow(spec: string): [string, ReceiverRule] {
  const [id, ...parts] = spec.split(":");
  if (!id) throw new Error(`bad --allow spec: ${spec}`);
  const rule: ReceiverRule = { allow_transfer: false, methods: [] };
  for (const p of parts) {
    if (p === "transfer") rule.allow_transfer = true;
    else if (p) rule.methods = p.split(",");
  }
  return [id, rule];
}

/** Raw access key list of an account (JSON-RPC `view_access_key_list`). */
export async function accessKeys(accountId: string, network: Network): Promise<any[]> {
  const res = await fetch(process.env.NEAR_RPC_URL || RPC[network], {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      jsonrpc: "2.0",
      id: 1,
      method: "query",
      params: { request_type: "view_access_key_list", finality: "optimistic", account_id: accountId },
    }),
  });
  const j: any = await res.json();
  if (j.error) throw new Error(JSON.stringify(j.error));
  return j.result.keys;
}

export class AgentGuardClient {
  readonly contractId: string;
  readonly network: Network;
  readonly provider: JsonRpcProvider;
  private account?: Account;

  /** `signerId` signs transactions; its key comes from NEAR_PRIVATE_KEY,
   *  AGENT_GUARD_KEY_FILE, or ~/.near-credentials/<network>/<signerId>.json. */
  constructor(opts: { contractId: string; network?: Network; signerId?: string; privateKey?: string }) {
    this.contractId = opts.contractId;
    this.network = opts.network ?? "testnet";
    this.provider = provider(this.network);
    if (opts.signerId) {
      const key = (opts.privateKey ?? loadKey(opts.signerId, this.network)) as KeyPairString;
      this.account = new Account(opts.signerId, this.provider, key);
    }
  }

  get signerId(): string | undefined {
    return this.account?.accountId;
  }

  // ---------------- views ----------------
  private view<T>(method: string, args: Record<string, unknown> = {}): Promise<T> {
    return this.provider.callFunction({ contractId: this.contractId, method, args }) as Promise<T>;
  }
  status(): Promise<Status> {
    return this.view("get_status");
  }
  receivers(): Promise<[string, ReceiverRule][]> {
    return this.view("get_receivers");
  }
  checkTransfer(receiverId: string, amountYocto: string): Promise<Verdict> {
    return this.view("check_transfer", { receiver_id: receiverId, amount: amountYocto });
  }
  checkCall(receiverId: string, methodName: string, depositYocto = "0", gasTgas = 30): Promise<Verdict> {
    return this.view("check_call", {
      receiver_id: receiverId,
      method_name: methodName,
      deposit: depositYocto,
      gas_tgas: gasTgas,
    });
  }

  // ---------------- writes ----------------
  private async tx<T>(methodName: string, args: Record<string, unknown>, gasTgas = 150): Promise<TxResult<T>> {
    if (!this.account) throw new Error("No signer configured (pass signerId)");
    const res: any = await this.account.signAndSendTransaction({
      receiverId: this.contractId,
      actions: [actions.functionCall(methodName, args, teraToGas(String(gasTgas) as any), 0n)],
      waitUntil: "FINAL",
    });
    const txHash: string = res.transaction?.hash ?? res.transaction_outcome?.id;
    return {
      value: getTransactionLastResult(res) as T,
      txHash,
      explorerUrl: EXPLORER[this.network] + txHash,
      events: parseEvents(res),
    };
  }

  /** Agent: send NEAR through the guard. Denials resolve normally with approved=false. */
  transfer(receiverId: string, amountYocto: string, memo?: string) {
    return this.tx<Verdict>("agent_transfer", { receiver_id: receiverId, amount: amountYocto, memo });
  }
  /** Agent: call a contract method through the guard. */
  call(receiverId: string, methodName: string, opts: { args?: object; depositYocto?: string; gasTgas?: number; memo?: string } = {}) {
    return this.tx<Verdict>("agent_call", {
      receiver_id: receiverId,
      method_name: methodName,
      args: JSON.stringify(opts.args ?? {}),
      deposit: opts.depositYocto ?? "0",
      gas_tgas: opts.gasTgas ?? 30,
      memo: opts.memo,
    });
  }

  // owner controls
  pause() { return this.tx<null>("pause", {}); }
  unpause() { return this.tx<null>("unpause", {}); }
  setPolicy(policy: Policy) { return this.tx<null>("set_policy", { policy }); }
  setAgent(agent: string | null) { return this.tx<null>("set_agent", { agent }); }
  setReceiver(receiverId: string, rule: ReceiverRule) { return this.tx<null>("set_receiver", { receiver_id: receiverId, rule }); }
  removeReceiver(receiverId: string) { return this.tx<null>("remove_receiver", { receiver_id: receiverId }); }
  withdraw(amountYocto?: string) { return this.tx<null>("withdraw", { amount: amountYocto ?? null }); }

  /**
   * Owner: create `<prefix>.<owner>` as a guard in ONE transaction:
   * create account + fund + deploy wasm + init. No access key is added, so the
   * guard account has no keys: only the contract code can ever move its funds.
   */
  static async deploy(opts: {
    ownerId: string;
    network?: Network;
    prefix?: string;
    wasm: Uint8Array;
    fundNear: string;
    agentId: string | null;
    policy: Policy;
    receivers: [string, ReceiverRule][];
  }): Promise<{ contractId: string; txHash: string; explorerUrl: string }> {
    const network = opts.network ?? "testnet";
    const p = provider(network);
    const owner = new Account(opts.ownerId, p, loadKey(opts.ownerId, network) as KeyPairString);
    const contractId = `${opts.prefix ?? "guard"}.${opts.ownerId}`;
    const res: any = await owner.signAndSendTransaction({
      receiverId: contractId,
      actions: [
        actions.createAccount(),
        actions.transfer(BigInt(near(opts.fundNear))),
        actions.deployContract(opts.wasm),
        actions.functionCall(
          "new",
          { owner: opts.ownerId, agent: opts.agentId, policy: opts.policy, receivers: opts.receivers },
          teraToGas("100"),
          0n,
        ),
      ],
      waitUntil: "FINAL",
    });
    const txHash = res.transaction?.hash ?? res.transaction_outcome?.id;
    return { contractId, txHash, explorerUrl: EXPLORER[network] + txHash };
  }

  /**
   * Agent account: add a function-call-only access key that can call nothing
   * but `agent_transfer` / `agent_call` on the guard, with a gas allowance.
   * The agent runtime only ever gets this key, never a full-access key.
   */
  static async issueAgentKey(opts: {
    agentId: string;
    contractId: string;
    network?: Network;
    allowanceNear?: string;
  }): Promise<{ privateKey: string; publicKey: string; txHash: string; explorerUrl: string }> {
    const network = opts.network ?? "testnet";
    const p = provider(network);
    const agent = new Account(opts.agentId, p, loadKey(opts.agentId, network, { fullAccess: true }) as KeyPairString);
    const kp = KeyPair.fromRandom("ed25519");
    const res: any = await agent.addFunctionCallAccessKey({
      publicKey: kp.getPublicKey(),
      contractId: opts.contractId,
      methodNames: ["agent_transfer", "agent_call"],
      allowance: BigInt(near(opts.allowanceNear ?? "0.5")),
    });
    const txHash = res.transaction?.hash ?? res.transaction_outcome?.id;
    return {
      privateKey: kp.toString(),
      publicKey: kp.getPublicKey().toString(),
      txHash,
      explorerUrl: EXPLORER[network] + txHash,
    };
  }
}
