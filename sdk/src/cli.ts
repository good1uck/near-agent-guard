#!/usr/bin/env node
// agent-guard CLI. Run: node src/cli.ts <command> [--flags]   (Node >= 22.18)
import { readFileSync } from "node:fs";
import { parseArgs } from "node:util";
import { AgentGuardClient, fmt, near, parseAllow, accessKeys } from "./client.ts";
import type { Network, ReceiverRule } from "./client.ts";
import { saveAgentKey } from "./keys.ts";

const HELP = `agent-guard <command> [options]

Global options:
  --network testnet|mainnet|sandbox   (default: testnet, or $NEAR_NETWORK)
  --guard <contract id>               guard contract (or $GUARD_ID)
  --as <account id>                   signer (owner or agent)

Owner commands:
  deploy --owner <id> --agent <id> [--prefix guard] [--fund 3] [--wasm path]
         [--max-per-tx 0.5] [--daily 1] [--max-gas 50] [--allow <id>[:transfer][:m1,m2|*]]...
  allow <receiver> [--transfer] [--methods m1,m2|*]
  disallow <receiver>
  set-policy --max-per-tx <NEAR> --daily <NEAR> [--max-gas 50]
  set-agent <id|none>
  pause | unpause          (kill switch)
  withdraw [NEAR]          (default: all available)

Agent commands:
  agent-key --agent <id> [--allowance 0.5]   add a function-call-only key for the guard
  transfer <receiver> <NEAR> [--memo text]
  call <receiver> <method> [--args JSON] [--deposit NEAR] [--gas TGas] [--memo text]

Read-only:
  status | receivers
  keys <account>           list access keys (a guard should have none)
  check-transfer <receiver> <NEAR>
  check-call <receiver> <method> [--deposit NEAR] [--gas TGas]
`;

const { values: o, positionals } = parseArgs({
  allowPositionals: true,
  options: {
    network: { type: "string" },
    guard: { type: "string" },
    as: { type: "string" },
    owner: { type: "string" },
    agent: { type: "string" },
    prefix: { type: "string" },
    fund: { type: "string" },
    wasm: { type: "string" },
    "max-per-tx": { type: "string" },
    daily: { type: "string" },
    "max-gas": { type: "string" },
    allow: { type: "string", multiple: true },
    transfer: { type: "boolean" },
    methods: { type: "string" },
    allowance: { type: "string" },
    memo: { type: "string" },
    args: { type: "string" },
    deposit: { type: "string" },
    gas: { type: "string" },
    json: { type: "boolean" },
    help: { type: "boolean", short: "h" },
  },
});

const [cmd, ...rest] = positionals;
const network = (o.network ?? process.env.NEAR_NETWORK ?? "testnet") as Network;
const guardId = o.guard ?? process.env.GUARD_ID;

function need<T>(v: T | undefined, what: string): T {
  if (v === undefined || v === "") {
    console.error(`missing ${what}\n\n${HELP}`);
    process.exit(2);
  }
  return v;
}
function client(signer?: string) {
  return new AgentGuardClient({ contractId: need(guardId, "--guard"), network, signerId: signer });
}
function printVerdict(r: { value: any; explorerUrl: string; events: any[] }) {
  const v = r.value;
  if (o.json) return console.log(JSON.stringify(r, null, 2));
  if (v && typeof v === "object" && "approved" in v) {
    console.log(`${v.approved ? "APPROVED" : "DENIED  "} ${v.reason}  ${v.detail}`);
    console.log(`  action #${v.action_id}  remaining today: ${fmt(v.remaining_today)}`);
  } else console.log("OK");
  for (const e of r.events) console.log(`  event ${e.event}`);
  console.log(`  tx: ${r.explorerUrl}`);
}

async function main() {
  if (!cmd || o.help) return console.log(HELP);
  switch (cmd) {
    case "deploy": {
      const owner = need(o.owner ?? o.as, "--owner");
      const wasm = readFileSync(o.wasm ?? new URL("../../target/near/agent_guard.wasm", import.meta.url));
      const r = await AgentGuardClient.deploy({
        ownerId: owner,
        network,
        prefix: o.prefix,
        wasm,
        fundNear: o.fund ?? "3",
        agentId: o.agent ?? null,
        policy: {
          max_per_tx: near(o["max-per-tx"] ?? "0.5"),
          daily_limit: near(o.daily ?? "1"),
          max_gas_tgas: Number(o["max-gas"] ?? 50),
        },
        receivers: (o.allow ?? []).map(parseAllow),
      });
      console.log(`guard deployed: ${r.contractId}\n  tx: ${r.explorerUrl}`);
      console.log(`export GUARD_ID=${r.contractId}`);
      return;
    }
    case "agent-key": {
      const agent = need(o.agent ?? o.as, "--agent");
      const r = await AgentGuardClient.issueAgentKey({
        agentId: agent,
        contractId: need(guardId, "--guard"),
        network,
        allowanceNear: o.allowance,
      });
      const path = saveAgentKey(agent, network, r.privateKey, r.publicKey);
      console.log(`function-call key ${r.publicKey} added to ${agent}`);
      console.log(`  can call only: ${guardId} :: agent_transfer, agent_call`);
      console.log(`  saved to ${path} (the agent uses this key, never its full-access key)`);
      console.log(`  tx: ${r.explorerUrl}`);
      return;
    }
    case "status": {
      const s = await client().status();
      if (o.json) return console.log(JSON.stringify(s, null, 2));
      console.log(`guard        ${guardId} (${network})`);
      console.log(`owner        ${s.owner}`);
      console.log(`agent        ${s.agent ?? "(none)"}`);
      console.log(`paused       ${s.paused}`);
      console.log(`balance      ${fmt(s.balance)}  (available ${fmt(s.available)})`);
      console.log(`policy       max/tx ${fmt(s.policy.max_per_tx)}, daily ${fmt(s.policy.daily_limit)}, max gas ${s.policy.max_gas_tgas} TGas`);
      console.log(`today        spent ${fmt(s.spent_today)}, remaining ${fmt(s.remaining_today)}`);
      console.log(`actions      approved ${s.approved_count}, denied ${s.denied_count}, failed ${s.failed_count}`);
      return;
    }
    case "keys": {
      const acct = need(rest[0] ?? guardId, "account");
      const keys = await accessKeys(acct, network);
      console.log(`${acct}: ${keys.length} access key(s)`);
      for (const k of keys) {
        const p = k.access_key.permission;
        console.log(`  ${k.public_key}  ${p === "FullAccess" ? "FullAccess" : `FunctionCall ${p.FunctionCall.receiver_id} [${p.FunctionCall.method_names.join(", ")}]`}`);
      }
      return;
    }
    case "receivers": {
      const rs = await client().receivers();
      for (const [id, r] of rs)
        console.log(`${id}  transfer=${r.allow_transfer}  methods=${r.methods.length ? r.methods.join(",") : "(none)"}`);
      return;
    }
    case "check-transfer": {
      const v = await client().checkTransfer(need(rest[0], "receiver"), near(need(rest[1], "amount")));
      return console.log(`${v.approved ? "WOULD APPROVE" : "WOULD DENY"} ${v.reason}  ${v.detail}`);
    }
    case "check-call": {
      const v = await client().checkCall(
        need(rest[0], "receiver"),
        need(rest[1], "method"),
        near(o.deposit ?? "0"),
        Number(o.gas ?? 30),
      );
      return console.log(`${v.approved ? "WOULD APPROVE" : "WOULD DENY"} ${v.reason}  ${v.detail}`);
    }
    case "transfer":
      return printVerdict(
        await client(need(o.as, "--as")).transfer(need(rest[0], "receiver"), near(need(rest[1], "amount")), o.memo),
      );
    case "call":
      return printVerdict(
        await client(need(o.as, "--as")).call(need(rest[0], "receiver"), need(rest[1], "method"), {
          args: o.args ? JSON.parse(o.args) : {},
          depositYocto: near(o.deposit ?? "0"),
          gasTgas: Number(o.gas ?? 30),
          memo: o.memo,
        }),
      );
    case "allow": {
      const rule: ReceiverRule = {
        allow_transfer: !!o.transfer,
        methods: o.methods ? o.methods.split(",") : [],
      };
      return printVerdict(await client(need(o.as, "--as")).setReceiver(need(rest[0], "receiver"), rule));
    }
    case "disallow":
      return printVerdict(await client(need(o.as, "--as")).removeReceiver(need(rest[0], "receiver")));
    case "set-policy":
      return printVerdict(
        await client(need(o.as, "--as")).setPolicy({
          max_per_tx: near(need(o["max-per-tx"], "--max-per-tx")),
          daily_limit: near(need(o.daily, "--daily")),
          max_gas_tgas: Number(o["max-gas"] ?? 50),
        }),
      );
    case "set-agent": {
      const a = need(rest[0], "agent id or none");
      return printVerdict(await client(need(o.as, "--as")).setAgent(a === "none" ? null : a));
    }
    case "pause":
      return printVerdict(await client(need(o.as, "--as")).pause());
    case "unpause":
      return printVerdict(await client(need(o.as, "--as")).unpause());
    case "withdraw":
      return printVerdict(await client(need(o.as, "--as")).withdraw(rest[0] ? near(rest[0]) : undefined));
    default:
      console.error(`unknown command: ${cmd}\n\n${HELP}`);
      process.exit(2);
  }
}

main().catch((e) => {
  console.error(`error: ${e?.message ?? e}`);
  process.exit(1);
});
