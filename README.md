# Agent Guard: spending limits and a policy engine for AI agent wallets on NEAR

Built at **NEAR HACKATHON** (Nov 2026). Tracks: **AI Security & Defense** (`security`) and **Agent Economy** (`agents`).

**Problem.** If you give an AI agent a private key, a prompt injection or a bug can spend
everything that key controls. "Be careful" in the system prompt is not a security boundary.

**Product.** Agent Guard is a NEAR smart contract that holds the agent's money and only lets the
agent spend it inside rules a human owner sets, enforced on-chain:

| Rule | What it does | Denial reason |
| --- | --- | --- |
| Allowlist of receivers | Agent can only pay or call accounts the owner listed | `RECEIVER_NOT_ALLOWED` |
| Per-receiver rules | Plain transfers on/off; which contract methods may be called (`*` = any) | `TRANSFER_NOT_ALLOWED`, `METHOD_NOT_ALLOWED` |
| Per-transaction limit | Max NEAR per transfer or per attached deposit | `PER_TX_LIMIT_EXCEEDED` |
| Daily limit | Max NEAR per UTC day; resets at 00:00 UTC | `DAILY_LIMIT_EXCEEDED` |
| Gas cap | Max TGas the agent may attach to a call | `GAS_LIMIT_EXCEEDED` |
| Kill switch | Owner `pause()` blocks every agent action until `unpause()` | `PAUSED` |
| No self-calls | Agent can never call the guard itself (not even if the owner allowlists it) | `SELF_CALL_FORBIDDEN` |
| Balance floor | Never spends the storage reserve | `INSUFFICIENT_FUNDS` |

Denied actions **do not revert**: the transaction succeeds, returns
`{"approved": false, "reason": "...", "detail": "..."}` and emits an `action_denied` event, so every
attempt a misbehaving agent makes is recorded on-chain. If an approved action fails downstream
(e.g. the receiver contract panics), a callback refunds the amount to the agent's daily allowance
and emits `action_failed`.

**How NEAR fits in.** Three NEAR features make this a real sandbox, not a wrapper:

1. **Keyless contract account.** `deploy` creates `guard.<owner>`, funds it, deploys the wasm and
   initialises it in one transaction **without adding any access key**. Nobody, not even the
   owner, holds a key to the guard; only its code can move the funds (owner gets them back via
   `withdraw`).
2. **Function-call access keys.** The agent's runtime only gets a key that NEAR itself restricts to
   `agent_transfer` / `agent_call` on the guard, with a gas allowance. A leaked agent key cannot
   transfer NEAR from the agent account, call `unpause`, `set_policy`, or anything else.
3. **Async receipts + callbacks** for the refund-on-failure logic, and **NEP-297 events** for an
   audit trail any indexer (or nearblocks) can read.

```
 owner (human) ──set_policy / set_receiver / pause / withdraw──┐
                                                                ▼
 AI agent ──function-call key──> guard.<owner>  (no access keys, holds funds)
   agent_transfer / agent_call     │ evaluate policy ─ deny → event, return verdict
                                   └ approve → Promise(transfer | function_call) → callback
                                                         ▼
                                     allowlisted receivers (shop, wrap.testnet, APIs...)
```

## Live testnet deployment

| What | Value |
| --- | --- |
| Guard contract | [`guard.agentguard-56296b.testnet`](https://testnet.nearblocks.io/address/guard.agentguard-56296b.testnet) (0 access keys) |
| Owner | `agentguard-56296b.testnet` |
| Agent | `agentguard-56296b-agent.testnet` (signs only with a function-call key for the guard) |
| Allowlist | `agentguard-56296b-shop.testnet` (transfers), `wrap.testnet` (`storage_deposit`, `near_deposit`) |
| Policy | 0.5 NEAR per tx, 1 NEAR per day, 50 TGas max |
| State after the demo run | **paused** (kill switch on), 5 approved, 6 denied, 0 failed |

Transactions from the recorded run (`scripts/demo-testnet.sh`, 2026-11-09):

| # | Action | Result | Tx |
| --- | --- | --- | --- |
| - | Create + fund + deploy + init guard in one tx | ok | [9igdhYnG…](https://testnet.nearblocks.io/txns/9igdhYnGZVKH6Pfy2p8Gg4V7PQ4nFJUYaQWf4vc5QFZb) |
| - | Add function-call-only key to the agent | ok | [H4HRHQj1…](https://testnet.nearblocks.io/txns/H4HRHQj126zfCJ1BHdNgRyR9MbeLjyScrciZSZNwuf1T) |
| 1 | Pay shop 0.2 NEAR | APPROVED | [3RBu4ys6…](https://testnet.nearblocks.io/txns/3RBu4ys6AFpNtrNEis6KMG2XZWmaPZmVhSZpYGUprdsB) |
| 2 | Prompt injection: send 5 NEAR to attacker | DENIED `RECEIVER_NOT_ALLOWED` | [9gggGHjQ…](https://testnet.nearblocks.io/txns/9gggGHjQr5RLzd5WyGdVXNj4metPuQfUNq2jyCnyGKKX) |
| 3 | Pay shop 0.8 NEAR | DENIED `PER_TX_LIMIT_EXCEEDED` | [6F9izuwB…](https://testnet.nearblocks.io/txns/6F9izuwBSsw1Try32pNW4fEd1xqKYgZKWYiHi1v3H3Se) |
| 4 | `wrap.testnet.storage_deposit` (0.00125) | APPROVED | [Ex2NhzhK…](https://testnet.nearblocks.io/txns/Ex2NhzhK4zwEG3r9U9n4HDwbqrRQh2PijMt5CcgzPCDZ) |
| 5 | `wrap.testnet.near_deposit` (0.1) | APPROVED | [CAgjsztS…](https://testnet.nearblocks.io/txns/CAgjsztSiAuDpaKcCn28akDFPyUxMMurG8BgHpeGFm5n) |
| 6 | `wrap.testnet.ft_transfer` to attacker | DENIED `METHOD_NOT_ALLOWED` | [Hw3phM2n…](https://testnet.nearblocks.io/txns/Hw3phM2npMQoptyEkM4UBtmmiT3wYrRm76SnEskhacXB) |
| 7 | Call with 200 TGas | DENIED `GAS_LIMIT_EXCEEDED` | [G4uSsVfe…](https://testnet.nearblocks.io/txns/G4uSsVfeTxxo2XZYwHgafmRDWJoupru8K3zz2RZzFTqk) |
| 8-9 | Pay shop 0.3 NEAR twice | APPROVED | [Fuvjjggo…](https://testnet.nearblocks.io/txns/Fuvjjggo5KmT814NdFibJQjBe2Zft3VsYVGjbAc1W1VR), [4hxKHFWL…](https://testnet.nearblocks.io/txns/4hxKHFWLXP3DRBSEF4FyG6os6hEhfeaoZqnLJWdDLpx9) |
| 10 | Pay shop 0.3 NEAR (0.099 left today) | DENIED `DAILY_LIMIT_EXCEEDED` | [CrHbM3ML…](https://testnet.nearblocks.io/txns/CrHbM3MLEsx9sEK5vFP4JmmC5JPiYoFcdoqX2vs5YoyU) |
| - | Owner `pause()` | ok | [moicpS8A…](https://testnet.nearblocks.io/txns/moicpS8Ai8pveYVmJWq9tmjGppXEwsR4bDigBUXKWNr) |
| - | Agent pays while paused | DENIED `PAUSED` | [7zYeRUWn…](https://testnet.nearblocks.io/txns/7zYeRUWnFsC7xspf1ikDoJdcE4r1SsR2uGAUTBDPNgtn) |
| - | Agent calls `unpause` with its key | rejected by NEAR: `Access key does not allow method unpause` | (not included in a block) |

Reproduction evidence: the same script was re-run from a fresh `git clone` with brand-new faucet
accounts and gave the identical sequence (guard
[`guard.agentguard-1cb600.testnet`](https://testnet.nearblocks.io/address/guard.agentguard-1cb600.testnet),
final status `paused true, approved 5, denied 6, failed 0`). That run's terminal recording is
[`demo/demo-testnet.cast`](demo/demo-testnet.cast) (play with `asciinema play demo/demo-testnet.cast`).

## Try it

Requirements: Linux or macOS, Node.js >= 22.18 (runs TypeScript directly), and for the contract:
Rust via rustup (the pinned toolchain in `rust-toolchain.toml` installs itself),
[`cargo-near`](https://github.com/near/cargo-near) and, for your own testnet run,
[`near-cli-rs`](https://github.com/near/near-cli-rs). On Debian/Ubuntu the sandbox tests also need
`libssl-dev pkg-config`.

```bash
git clone https://github.com/good1uck/near-agent-guard && cd near-agent-guard
```

### A. Read the live deployment (Node only, ~1 minute, no keys)

```bash
cd sdk && npm ci
node src/cli.ts status --guard guard.agentguard-56296b.testnet
node src/cli.ts keys guard.agentguard-56296b.testnet
node src/cli.ts check-transfer attacker.testnet 1 --guard guard.agentguard-56296b.testnet
cd ..
```

You should see: `paused true`, `policy max/tx 0.5 NEAR, daily 1 NEAR, max gas 50 TGas`,
`approved 5, denied 6` (or more if someone has used it since), `0 access key(s)` for the guard, and
`WOULD DENY PAUSED  Owner kill switch is on` for the dry run.

### B. Run the tests (local, no network accounts)

```bash
# install tooling once (skip what you already have)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/near/cargo-near/releases/download/cargo-near-v0.23.0/cargo-near-installer.sh | sh
source "$HOME/.cargo/env"

cargo test                           # 23 unit tests + 1 sandbox end-to-end test
(cd sdk && npm ci && npm run typecheck && npm test)   # 3 SDK tests
```

Expected: `test result: ok. 23 passed` (unit), `test agent_guard_end_to_end ... ok` (sandbox: it builds
the wasm, starts a local NEAR node, deploys two guards and drives 11 scenarios with real
transactions, including the refund-on-downstream-failure path), and `pass 3` for the SDK.
The first run downloads crates and the sandbox binary and takes a few minutes.

### C. Full demo on testnet with your own fresh accounts (~2 minutes)

```bash
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/near/near-cli-rs/releases/latest/download/near-cli-rs-installer.sh | sh
cargo near build non-reproducible-wasm     # -> target/near/agent_guard.wasm
(cd sdk && npm ci)
bash scripts/demo-testnet.sh
```

The script creates three faucet-funded testnet accounts (`agentguard-<random>.testnet`, `-agent`,
`-shop`; keys go to `~/.near-credentials/testnet/`), deploys `guard.agentguard-<random>.testnet`,
gives the agent a function-call-only key (saved in `~/.agent-guard/testnet/`), runs the example
agent and the kill switch. You should see the same sequence as the table above: 5 `APPROVED`,
then `DENIED PAUSED`, then `error: Access key does not allow method unpause`, and a final status
with `paused true`, `approved 5, denied 6`. Each line prints a nearblocks link.
Re-running the agent against the same guard on the same UTC day hits `DAILY_LIMIT_EXCEEDED` earlier,
which is the point.

### D. Use the CLI by hand

```bash
cd sdk
export GUARD_ID=guard.<owner>.testnet
node src/cli.ts allow some-api.testnet --methods pay,subscribe --as <owner>.testnet
node src/cli.ts set-policy --max-per-tx 0.2 --daily 0.5 --as <owner>.testnet
node src/cli.ts transfer <shop>.testnet 0.1 --as <agent>.testnet --memo "invoice 42"
node src/cli.ts call wrap.testnet near_deposit --deposit 0.05 --as <agent>.testnet
node src/cli.ts pause --as <owner>.testnet
node src/cli.ts withdraw --as <owner>.testnet          # everything available back to the owner
node src/cli.ts --help
```

Keys are read from `NEAR_PRIVATE_KEY` (+ optional `NEAR_ACCOUNT_ID`), then
`~/.agent-guard/<network>/<account>.json` (agent function-call key), then the near-cli legacy
keychain `~/.near-credentials/<network>/<account>.json`. No key is ever stored in this repo.
Set `NEAR_RPC_URL` to use another RPC (default `https://test.rpc.fastnear.com`).

## Use it from your agent (TypeScript SDK)

```ts
import { AgentGuardClient, near } from "./sdk/src/index.ts";

const guard = new AgentGuardClient({ contractId: "guard.alice.testnet", signerId: "alice-agent.testnet" });
const dry = await guard.checkTransfer("shop.testnet", near("0.2"));        // free view call
if (dry.approved) {
  const r = await guard.transfer("shop.testnet", near("0.2"), "buy credits");
  console.log(r.value.reason, r.explorerUrl, r.events);                    // "OK", link, NEP-297 events
}
```

See `sdk/examples/shopping-agent.ts` for a complete agent. Its plan is scripted (it stands in for an
LLM's tool calls, including one injected malicious step) so the demo is deterministic.

## Contract API

Agent (must be the configured `agent`; anyone else panics with `Only the agent can do this`):

- `agent_transfer(receiver_id, amount, memo?) -> Verdict`
- `agent_call(receiver_id, method_name, args?: JSON string, deposit?, gas_tgas? = 30, memo?) -> Verdict`

Owner (`Only the owner can do this` otherwise): `pause`, `unpause`, `set_policy(policy)`,
`set_agent(agent | null)`, `set_receiver(receiver_id, rule)`, `remove_receiver(receiver_id)`,
`withdraw(amount?)`, `transfer_ownership(new_owner)`.

Views: `get_status`, `get_receivers`, `get_receiver(receiver_id)`,
`check_transfer(receiver_id, amount)`, `check_call(receiver_id, method_name, deposit?, gas_tgas?)`.

Anyone: `deposit()` (payable top-up; plain transfers work too).
Init: `new(owner, agent?, policy, receivers?)`, callable by the guard account or by `owner`.

Amounts are yoctoNEAR strings. `Verdict = { approved, reason, detail, action_id, remaining_today }`.
Events (`EVENT_JSON:{"standard":"agent_guard","version":"1.0.0",...}`): `action_approved`,
`action_denied`, `action_succeeded`, `action_failed`, `paused`, `unpaused`, `policy_updated`,
`agent_updated`, `receiver_set`, `receiver_removed`, `withdraw`, `deposit`, `owner_updated`.

## Repository layout

```
src/lib.rs                 contract (Rust, near-sdk 5.30)
src/tests.rs               23 unit tests (mocked VM)
tests/sandbox.rs           end-to-end test on a local NEAR sandbox node
sdk/src/client.ts          TypeScript SDK (near-api-js 7)
sdk/src/cli.ts             CLI
sdk/examples/shopping-agent.ts   example agent: allowed and blocked actions
sdk/test/sdk.test.ts       SDK unit tests
scripts/demo-testnet.sh    full reproducible testnet demo
demo/demo-testnet.cast     asciinema recording of a fresh run
```

## Limitations and next steps

- One guard per agent; the daily window is a fixed UTC day, not rolling 24h.
- Limits are in NEAR only. NEP-141 token limits (e.g. USDC), per-receiver budgets and
  NEAR Intents / Chain Signatures payouts are the next step.
- The guard is not upgradeable by design (no keys). To change code, deploy a new guard and `withdraw`.
- Not audited. Testnet only.

## Hackathon notes

- New repository; all code was written during NEAR HACKATHON (after 2026-11-01). No pre-hackathon code.
- Most of the code was written with an AI coding agent, reviewed and run by the author.
- Libraries and templates: `near-sdk` 5.30, `cargo-near` 0.23 (`cargo near new` template for the
  Cargo/toolchain setup), `near-sandbox` 0.4, `near-api` 0.8 (Rust tests), `near-api-js` 7.4,
  `near-cli-rs` for faucet accounts. External contract used in the demo: `wrap.testnet`.
- License: MIT (see `LICENSE`).
