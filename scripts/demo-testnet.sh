#!/usr/bin/env bash
# End-to-end testnet demo: creates fresh testnet accounts from the faucet, deploys a
# guard, issues the agent a function-call-only key, runs the example agent, then
# shows the owner kill switch. Needs: near (near-cli-rs), node >= 22.18, built wasm.
set -euo pipefail
cd "$(dirname "$0")/.."

NAME=${NAME:-agentguard-$(openssl rand -hex 3)}
OWNER=${OWNER:-$NAME.testnet}
AGENT=${AGENT:-$NAME-agent.testnet}
SHOP=${SHOP:-$NAME-shop.testnet}
GUARD=guard.$OWNER
CLI="node sdk/src/cli.ts --network testnet"

step() { printf '\n\033[1;36m== %s\033[0m\n' "$*"; }

[ -f target/near/agent_guard.wasm ] || { echo "build first: cargo near build non-reproducible-wasm"; exit 1; }
[ -d sdk/node_modules ] || (cd sdk && npm ci)

create() { # faucet-funded testnet account, key saved to ~/.near-credentials/testnet/
  near account view-account-summary "$1" network-config testnet now </dev/null >/dev/null 2>&1 && { echo "$1 exists"; return; }
  near account create-account sponsor-by-faucet-service "$1" autogenerate-new-keypair --signature-scheme ed25519 \
    save-to-legacy-keychain network-config testnet create </dev/null 2>&1 | grep -E "created successfully|Transaction ID|rror" || true
}

step "1. Create testnet accounts (faucet): owner, agent, shop"
create "$OWNER"; create "$AGENT"; create "$SHOP"
sleep 3

step "2. Owner deploys the guard: create + fund 4 NEAR + deploy + init in ONE tx, no access keys"
$CLI deploy --owner "$OWNER" --agent "$AGENT" --fund 4 \
  --max-per-tx 0.5 --daily 1 --max-gas 50 \
  --allow "$SHOP:transfer" \
  --allow "wrap.testnet:storage_deposit,near_deposit"
export GUARD_ID=$GUARD
$CLI keys "$GUARD"

step "3. Agent gets a function-call-only key (can call only agent_transfer/agent_call on the guard)"
$CLI agent-key --agent "$AGENT" --guard "$GUARD" --allowance 0.5
$CLI keys "$AGENT"

step "4. Guard status"
$CLI status --guard "$GUARD"

step "5. Run the example agent (allowed and blocked actions)"
GUARD_ID=$GUARD AGENT_ID=$AGENT SHOP_ID=$SHOP node sdk/examples/shopping-agent.ts

step "6. Owner hits the kill switch"
$CLI pause --guard "$GUARD" --as "$OWNER"

step "7. Agent tries to pay while paused"
$CLI transfer "$SHOP" 0.01 --guard "$GUARD" --as "$AGENT" --memo "after kill switch"

step "8. Agent tries to lift the kill switch with its key (NEAR rejects: key not allowed for 'unpause')"
$CLI unpause --guard "$GUARD" --as "$AGENT" || true

step "9. Final status"
$CLI status --guard "$GUARD"

cat <<EOT

Accounts: OWNER=$OWNER AGENT=$AGENT SHOP=$SHOP GUARD=$GUARD
Explorer: https://testnet.nearblocks.io/address/$GUARD
EOT
