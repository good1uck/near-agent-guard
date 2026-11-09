//! # Agent Guard
//!
//! A NEAR smart contract that holds funds on behalf of an AI agent and only lets
//! the agent move them inside a policy set by a human owner:
//!
//! * per-transaction limit and a 24h (UTC day) spending limit,
//! * an allowlist of receivers, each with its own rules
//!   (plain transfers allowed or not, which contract methods may be called),
//! * a gas cap for function calls,
//! * an owner kill switch (`pause`) and emergency withdraw,
//! * an on-chain audit trail: every approved, denied and failed action is
//!   emitted as a NEP-297 event (`EVENT_JSON:` log) and counted in state.
//!
//! Denied actions do NOT panic. They return `{ "approved": false, "reason": .. }`
//! and are logged, so every attempt a misbehaving agent makes stays visible on-chain.

use near_sdk::json_types::U64;
use near_sdk::serde_json::{self, json};
use near_sdk::store::IterableMap;
use near_sdk::{
    AccountId, BorshStorageKey, Gas, NearToken, PanicOnDefault, Promise, PromiseError, env, near,
    require,
};

pub const EVENT_STANDARD: &str = "agent_guard";
pub const EVENT_VERSION: &str = "1.0.0";
pub const DAY_NS: u64 = 86_400 * 1_000_000_000;
/// Gas reserved for the result callback that refunds the spending window on failure.
pub const CALLBACK_GAS: Gas = Gas::from_tgas(10);
/// Kept aside on top of the storage cost so the contract never bricks itself.
pub const BALANCE_BUFFER: NearToken = NearToken::from_millinear(50);
/// Wildcard entry for `ReceiverRule::methods`: any method may be called.
pub const ANY_METHOD: &str = "*";

#[near]
#[derive(BorshStorageKey)]
enum StorageKey {
    Receivers,
}

/// Limits the owner sets for the agent.
#[near(serializers = [json, borsh])]
#[derive(Clone, Debug, PartialEq)]
pub struct Policy {
    /// Maximum value (transfer amount or attached deposit) of a single action.
    pub max_per_tx: NearToken,
    /// Maximum total value per UTC day (block timestamp / 24h).
    pub daily_limit: NearToken,
    /// Maximum gas, in TGas, the agent may attach to a single function call.
    pub max_gas_tgas: u64,
}

/// What the agent may do with one receiver.
#[near(serializers = [json, borsh])]
#[derive(Clone, Debug, PartialEq)]
pub struct ReceiverRule {
    /// Plain NEAR transfers to this receiver are allowed.
    pub allow_transfer: bool,
    /// Methods the agent may call on this receiver. `["*"]` allows any method,
    /// `[]` allows none.
    pub methods: Vec<String>,
}

/// Result of an agent action, or of a dry-run check.
#[near(serializers = [json])]
#[derive(Clone, Debug, PartialEq)]
pub struct Verdict {
    pub approved: bool,
    /// Machine-readable reason code when denied, `"OK"` when approved.
    pub reason: String,
    /// Human-readable detail.
    pub detail: String,
    /// Id of the action in the audit trail (only set by real actions, not dry runs).
    pub action_id: Option<U64>,
    /// How much the agent can still spend today after this action.
    pub remaining_today: NearToken,
}

#[near(serializers = [json])]
pub struct Status {
    pub owner: AccountId,
    pub agent: Option<AccountId>,
    pub paused: bool,
    pub policy: Policy,
    pub balance: NearToken,
    pub available: NearToken,
    pub spent_today: NearToken,
    pub remaining_today: NearToken,
    /// Nanosecond timestamp at which the daily window resets.
    pub window_resets_at: U64,
    pub approved_count: U64,
    pub denied_count: U64,
    pub failed_count: U64,
}

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct AgentGuard {
    owner: AccountId,
    agent: Option<AccountId>,
    paused: bool,
    policy: Policy,
    receivers: IterableMap<AccountId, ReceiverRule>,
    window_day: u64,
    spent_in_window: NearToken,
    next_action_id: u64,
    approved_count: u64,
    denied_count: u64,
    failed_count: u64,
}

/// What the agent is trying to do; used by both real actions and dry runs.
enum Action<'a> {
    Transfer {
        receiver_id: &'a AccountId,
        amount: NearToken,
    },
    Call {
        receiver_id: &'a AccountId,
        method_name: &'a str,
        deposit: NearToken,
        gas_tgas: u64,
    },
}

impl Action<'_> {
    fn receiver(&self) -> &AccountId {
        match self {
            Action::Transfer { receiver_id, .. } | Action::Call { receiver_id, .. } => receiver_id,
        }
    }
    fn value(&self) -> NearToken {
        match self {
            Action::Transfer { amount, .. } => *amount,
            Action::Call { deposit, .. } => *deposit,
        }
    }
    fn kind(&self) -> &'static str {
        match self {
            Action::Transfer { .. } => "transfer",
            Action::Call { .. } => "function_call",
        }
    }
    fn method(&self) -> Option<&str> {
        match self {
            Action::Transfer { .. } => None,
            Action::Call { method_name, .. } => Some(method_name),
        }
    }
}

fn emit(event: &str, data: serde_json::Value) {
    env::log_str(&format!(
        "EVENT_JSON:{}",
        json!({"standard": EVENT_STANDARD, "version": EVENT_VERSION, "event": event, "data": [data]})
    ));
}

#[near]
impl AgentGuard {
    /// Initialise the guard. Callable by the contract account itself or by the
    /// `owner` being set, so it can be batched into the transaction that creates
    /// the guard account (create + fund + deploy + init, with no access key added:
    /// the guard account then has no keys at all and only this code can move funds).
    #[init]
    pub fn new(
        owner: AccountId,
        agent: Option<AccountId>,
        policy: Policy,
        receivers: Option<Vec<(AccountId, ReceiverRule)>>,
    ) -> Self {
        let caller = env::predecessor_account_id();
        require!(
            caller == env::current_account_id() || caller == owner,
            "Only the guard account or the owner can initialise"
        );
        Self::assert_policy(&policy);
        let mut this = Self {
            owner,
            agent,
            paused: false,
            policy,
            receivers: IterableMap::new(StorageKey::Receivers),
            window_day: env::block_timestamp() / DAY_NS,
            spent_in_window: NearToken::from_yoctonear(0),
            next_action_id: 0,
            approved_count: 0,
            denied_count: 0,
            failed_count: 0,
        };
        for (id, rule) in receivers.unwrap_or_default() {
            this.insert_receiver(id, rule);
        }
        this
    }

    // ------------------------------------------------------------------
    // Funding
    // ------------------------------------------------------------------

    /// Anyone can top up the guard. Plain transfers to the account work too.
    #[payable]
    pub fn deposit(&mut self) {
        emit(
            "deposit",
            json!({"from": env::predecessor_account_id(), "amount": env::attached_deposit()}),
        );
    }

    // ------------------------------------------------------------------
    // Agent actions
    // ------------------------------------------------------------------

    /// Agent: send `amount` yoctoNEAR to `receiver_id` if the policy allows it.
    pub fn agent_transfer(
        &mut self,
        receiver_id: AccountId,
        amount: NearToken,
        memo: Option<String>,
    ) -> Verdict {
        self.assert_agent();
        let action = Action::Transfer {
            receiver_id: &receiver_id,
            amount,
        };
        match self.record(&action, memo) {
            Ok((id, verdict)) => {
                Promise::new(receiver_id.clone())
                    .transfer(amount)
                    .then(
                        Self::ext(env::current_account_id())
                            .with_static_gas(CALLBACK_GAS)
                            .on_action_result(U64(id), amount, U64(self.window_day)),
                    )
                    .detach();
                verdict
            }
            Err(v) => v,
        }
    }

    /// Agent: call `method_name` on `receiver_id` with JSON `args`, attaching
    /// `deposit` and `gas_tgas` TGas, if the policy allows it.
    pub fn agent_call(
        &mut self,
        receiver_id: AccountId,
        method_name: String,
        args: Option<String>,
        deposit: Option<NearToken>,
        gas_tgas: Option<u64>,
        memo: Option<String>,
    ) -> Verdict {
        self.assert_agent();
        let deposit = deposit.unwrap_or(NearToken::from_yoctonear(0));
        let gas_tgas = gas_tgas.unwrap_or(30);
        let action = Action::Call {
            receiver_id: &receiver_id,
            method_name: &method_name,
            deposit,
            gas_tgas,
        };
        match self.record(&action, memo) {
            Ok((id, verdict)) => {
                Promise::new(receiver_id.clone())
                    .function_call(
                        method_name.clone(),
                        args.unwrap_or_else(|| "{}".to_string()).into_bytes(),
                        deposit,
                        Gas::from_tgas(gas_tgas),
                    )
                    .then(
                        Self::ext(env::current_account_id())
                            .with_static_gas(CALLBACK_GAS)
                            .on_action_result(U64(id), deposit, U64(self.window_day)),
                    )
                    .detach();
                verdict
            }
            Err(v) => v,
        }
    }

    /// Callback: if the outgoing action failed, the funds came back to the
    /// guard, so give the agent its daily allowance back and log it.
    #[private]
    pub fn on_action_result(&mut self, action_id: U64, value: NearToken, window_day: U64) -> bool {
        // Any return value (even one too long to read) means success.
        let ok = !matches!(env::promise_result_checked(0, 0), Err(PromiseError::Failed));
        if ok {
            emit("action_succeeded", json!({"action_id": action_id}));
        } else {
            self.failed_count += 1;
            if window_day.0 == self.window_day {
                self.spent_in_window = self.spent_in_window.saturating_sub(value);
            }
            emit(
                "action_failed",
                json!({"action_id": action_id, "refunded_to_allowance": value}),
            );
        }
        ok
    }

    // ------------------------------------------------------------------
    // Dry runs (view): let an agent or SDK check before spending gas
    // ------------------------------------------------------------------

    pub fn check_transfer(&self, receiver_id: AccountId, amount: NearToken) -> Verdict {
        let action = Action::Transfer {
            receiver_id: &receiver_id,
            amount,
        };
        self.evaluate(&action, self.spent_today())
    }

    pub fn check_call(
        &self,
        receiver_id: AccountId,
        method_name: String,
        deposit: Option<NearToken>,
        gas_tgas: Option<u64>,
    ) -> Verdict {
        let action = Action::Call {
            receiver_id: &receiver_id,
            method_name: &method_name,
            deposit: deposit.unwrap_or(NearToken::from_yoctonear(0)),
            gas_tgas: gas_tgas.unwrap_or(30),
        };
        self.evaluate(&action, self.spent_today())
    }

    // ------------------------------------------------------------------
    // Owner controls
    // ------------------------------------------------------------------

    /// Kill switch: the agent can do nothing until `unpause`.
    pub fn pause(&mut self) {
        self.assert_owner();
        self.paused = true;
        emit("paused", json!({"by": env::predecessor_account_id()}));
    }

    pub fn unpause(&mut self) {
        self.assert_owner();
        self.paused = false;
        emit("unpaused", json!({"by": env::predecessor_account_id()}));
    }

    pub fn set_policy(&mut self, policy: Policy) {
        self.assert_owner();
        Self::assert_policy(&policy);
        self.policy = policy.clone();
        emit("policy_updated", json!({"policy": policy}));
    }

    /// Rotate (Some) or revoke (None) the agent account.
    pub fn set_agent(&mut self, agent: Option<AccountId>) {
        self.assert_owner();
        self.agent = agent.clone();
        emit("agent_updated", json!({"agent": agent}));
    }

    pub fn set_receiver(&mut self, receiver_id: AccountId, rule: ReceiverRule) {
        self.assert_owner();
        self.insert_receiver(receiver_id, rule);
    }

    pub fn remove_receiver(&mut self, receiver_id: AccountId) {
        self.assert_owner();
        self.receivers.remove(&receiver_id);
        emit("receiver_removed", json!({"receiver_id": receiver_id}));
    }

    /// Owner: pull funds back. `amount = None` withdraws everything available.
    pub fn withdraw(&mut self, amount: Option<NearToken>) -> Promise {
        self.assert_owner();
        let available = self.available();
        let amount = amount.unwrap_or(available);
        require!(amount <= available, "Not enough available balance");
        require!(!amount.is_zero(), "Nothing to withdraw");
        emit("withdraw", json!({"to": self.owner, "amount": amount}));
        Promise::new(self.owner.clone()).transfer(amount)
    }

    pub fn transfer_ownership(&mut self, new_owner: AccountId) {
        self.assert_owner();
        emit(
            "owner_updated",
            json!({"old_owner": self.owner, "new_owner": new_owner}),
        );
        self.owner = new_owner;
    }

    // ------------------------------------------------------------------
    // Views
    // ------------------------------------------------------------------

    pub fn get_status(&self) -> Status {
        let spent = self.spent_today();
        Status {
            owner: self.owner.clone(),
            agent: self.agent.clone(),
            paused: self.paused,
            policy: self.policy.clone(),
            balance: env::account_balance(),
            available: self.available(),
            spent_today: spent,
            remaining_today: self.policy.daily_limit.saturating_sub(spent),
            window_resets_at: U64((env::block_timestamp() / DAY_NS + 1) * DAY_NS),
            approved_count: U64(self.approved_count),
            denied_count: U64(self.denied_count),
            failed_count: U64(self.failed_count),
        }
    }

    pub fn get_receivers(&self) -> Vec<(AccountId, ReceiverRule)> {
        self.receivers
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    pub fn get_receiver(&self, receiver_id: AccountId) -> Option<ReceiverRule> {
        self.receivers.get(&receiver_id).cloned()
    }
}

// ----------------------------------------------------------------------
// Internal logic
// ----------------------------------------------------------------------
impl AgentGuard {
    fn assert_owner(&self) {
        require!(
            env::predecessor_account_id() == self.owner,
            "Only the owner can do this"
        );
    }

    /// Only the configured agent may submit actions. Calls from anyone else
    /// panic (they are not the agent misbehaving, just noise).
    fn assert_agent(&self) {
        require!(
            self.agent.as_ref() == Some(&env::predecessor_account_id()),
            "Only the agent can do this"
        );
    }

    fn assert_policy(p: &Policy) {
        require!(
            p.max_per_tx <= p.daily_limit,
            "max_per_tx must not exceed daily_limit"
        );
        require!(
            p.max_gas_tgas > 0 && p.max_gas_tgas <= 250,
            "max_gas_tgas must be in 1..=250"
        );
    }

    fn insert_receiver(&mut self, receiver_id: AccountId, rule: ReceiverRule) {
        require!(
            receiver_id != env::current_account_id(),
            "The guard itself cannot be an allowed receiver"
        );
        emit(
            "receiver_set",
            json!({"receiver_id": receiver_id, "rule": rule}),
        );
        self.receivers.insert(receiver_id, rule);
    }

    fn current_day() -> u64 {
        env::block_timestamp() / DAY_NS
    }

    fn spent_today(&self) -> NearToken {
        if self.window_day == Self::current_day() {
            self.spent_in_window
        } else {
            NearToken::from_yoctonear(0)
        }
    }

    fn available(&self) -> NearToken {
        let storage = env::storage_byte_cost().saturating_mul(env::storage_usage() as u128);
        env::account_balance()
            .saturating_sub(storage)
            .saturating_sub(BALANCE_BUFFER)
    }

    /// Pure policy evaluation. `spent` is what was already spent in the window.
    fn evaluate(&self, action: &Action, spent: NearToken) -> Verdict {
        let remaining = self.policy.daily_limit.saturating_sub(spent);
        let deny = |reason: &str, detail: String| Verdict {
            approved: false,
            reason: reason.to_string(),
            detail,
            action_id: None,
            remaining_today: remaining,
        };
        let receiver = action.receiver();
        let value = action.value();

        if self.paused {
            return deny("PAUSED", "Owner kill switch is on".into());
        }
        if receiver == &env::current_account_id() {
            return deny(
                "SELF_CALL_FORBIDDEN",
                "The agent may never call the guard contract itself".into(),
            );
        }
        let Some(rule) = self.receivers.get(receiver) else {
            return deny(
                "RECEIVER_NOT_ALLOWED",
                format!("{receiver} is not on the allowlist"),
            );
        };
        match action {
            Action::Transfer { .. } => {
                if !rule.allow_transfer {
                    return deny(
                        "TRANSFER_NOT_ALLOWED",
                        format!("Plain transfers to {receiver} are not allowed"),
                    );
                }
            }
            Action::Call {
                method_name,
                gas_tgas,
                ..
            } => {
                let allowed = rule
                    .methods
                    .iter()
                    .any(|m| m == ANY_METHOD || m == method_name);
                if !allowed {
                    return deny(
                        "METHOD_NOT_ALLOWED",
                        format!("Method `{method_name}` on {receiver} is not allowed"),
                    );
                }
                if *gas_tgas == 0 || *gas_tgas > self.policy.max_gas_tgas {
                    return deny(
                        "GAS_LIMIT_EXCEEDED",
                        format!(
                            "{gas_tgas} TGas requested, policy allows 1..={}",
                            self.policy.max_gas_tgas
                        ),
                    );
                }
            }
        }
        if value > self.policy.max_per_tx {
            return deny(
                "PER_TX_LIMIT_EXCEEDED",
                format!(
                    "{} requested, per-tx limit is {}",
                    value.exact_amount_display(),
                    self.policy.max_per_tx.exact_amount_display()
                ),
            );
        }
        if value > remaining {
            return deny(
                "DAILY_LIMIT_EXCEEDED",
                format!(
                    "{} requested, {} left today",
                    value.exact_amount_display(),
                    remaining.exact_amount_display()
                ),
            );
        }
        if value > self.available() {
            return deny(
                "INSUFFICIENT_FUNDS",
                format!(
                    "{} requested, {} available",
                    value.exact_amount_display(),
                    self.available().exact_amount_display()
                ),
            );
        }
        Verdict {
            approved: true,
            reason: "OK".into(),
            detail: "Allowed by policy".into(),
            action_id: None,
            remaining_today: remaining.saturating_sub(value),
        }
    }

    /// Evaluate a real action, update counters and the window, emit an event.
    /// Ok = approved (caller must create the promise), Err = denied verdict.
    fn record(&mut self, action: &Action, memo: Option<String>) -> Result<(u64, Verdict), Verdict> {
        let today = Self::current_day();
        if self.window_day != today {
            self.window_day = today;
            self.spent_in_window = NearToken::from_yoctonear(0);
        }
        let id = self.next_action_id;
        self.next_action_id += 1;
        let mut verdict = self.evaluate(action, self.spent_in_window);
        verdict.action_id = Some(U64(id));
        let data = json!({
            "action_id": U64(id),
            "agent": env::predecessor_account_id(),
            "kind": action.kind(),
            "receiver_id": action.receiver(),
            "method_name": action.method(),
            "value": action.value(),
            "reason": verdict.reason,
            "detail": verdict.detail,
            "memo": memo,
        });
        if verdict.approved {
            self.approved_count += 1;
            self.spent_in_window = self.spent_in_window.saturating_add(action.value());
            emit("action_approved", data);
            Ok((id, verdict))
        } else {
            self.denied_count += 1;
            emit("action_denied", data);
            Err(verdict)
        }
    }
}

#[cfg(test)]
mod tests;
