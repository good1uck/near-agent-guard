// Offline unit tests for SDK helpers: `npm test` (no network needed).
import { test } from "node:test";
import assert from "node:assert/strict";
import { parseAllow, parseEvents, near, fmt } from "../src/client.ts";

test("parseAllow parses transfer flag and method lists", () => {
  assert.deepEqual(parseAllow("shop.testnet:transfer"), ["shop.testnet", { allow_transfer: true, methods: [] }]);
  assert.deepEqual(parseAllow("wrap.testnet:storage_deposit,near_deposit"), [
    "wrap.testnet",
    { allow_transfer: false, methods: ["storage_deposit", "near_deposit"] },
  ]);
  assert.deepEqual(parseAllow("api.testnet:transfer:*"), ["api.testnet", { allow_transfer: true, methods: ["*"] }]);
  assert.throws(() => parseAllow(":transfer"));
});

test("parseEvents keeps only agent_guard NEP-297 events", () => {
  const tx = {
    receipts_outcome: [
      { outcome: { logs: ['EVENT_JSON:{"standard":"agent_guard","version":"1.0.0","event":"action_denied","data":[{"reason":"PAUSED"}]}'] } },
      { outcome: { logs: ["plain log", 'EVENT_JSON:{"standard":"nep141","event":"ft_transfer","data":[]}'] } },
      { outcome: { logs: ['EVENT_JSON:{"standard":"agent_guard","version":"1.0.0","event":"action_succeeded","data":[{"action_id":"1"}]}'] } },
    ],
  };
  assert.deepEqual(parseEvents(tx), [
    { event: "action_denied", data: { reason: "PAUSED" } },
    { event: "action_succeeded", data: { action_id: "1" } },
  ]);
});

test("near/fmt unit conversions", () => {
  assert.equal(near("1"), "1000000000000000000000000");
  assert.equal(near("0.00125"), "1250000000000000000000");
  assert.equal(fmt("500000000000000000000000"), "0.5 NEAR");
});
