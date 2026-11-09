//! Unit tests: pure policy logic in a mocked NEAR VM (`cargo test --lib`).
use super::*;
use near_sdk::test_utils::{VMContextBuilder, get_logs};
use near_sdk::{PromiseResult, testing_env};

fn acc(s: &str) -> AccountId {
    s.parse().unwrap()
}
fn near(n: u128) -> NearToken {
    NearToken::from_near(n)
}
fn milli(n: u128) -> NearToken {
    NearToken::from_millinear(n)
}

const T0: u64 = 1_700_000_000 * 1_000_000_000;

fn ctx(predecessor: &str, ts: u64) -> VMContextBuilder {
    let mut b = VMContextBuilder::new();
    b.current_account_id(acc("guard.near"))
        .predecessor_account_id(acc(predecessor))
        .signer_account_id(acc(predecessor))
        .account_balance(near(10))
        .block_timestamp(ts);
    b
}

fn policy() -> Policy {
    Policy {
        max_per_tx: near(1),
        daily_limit: near(2),
        max_gas_tgas: 50,
    }
}

fn setup() -> AgentGuard {
    testing_env!(ctx("guard.near", T0).build());
    AgentGuard::new(
        acc("owner.near"),
        Some(acc("agent.near")),
        policy(),
        Some(vec![
            (
                acc("shop.near"),
                ReceiverRule {
                    allow_transfer: true,
                    methods: vec!["buy".into()],
                },
            ),
            (
                acc("api.near"),
                ReceiverRule {
                    allow_transfer: false,
                    methods: vec![ANY_METHOD.into()],
                },
            ),
        ]),
    )
}

fn as_agent(ts: u64) {
    testing_env!(ctx("agent.near", ts).build());
}
fn as_owner() {
    testing_env!(ctx("owner.near", T0).build());
}

#[test]
fn allowed_transfer_is_approved_and_counted() {
    let mut c = setup();
    as_agent(T0);
    let v = c.agent_transfer(acc("shop.near"), milli(500), Some("coffee".into()));
    assert!(v.approved, "{v:?}");
    assert_eq!(v.reason, "OK");
    assert_eq!(v.action_id, Some(U64(0)));
    assert_eq!(v.remaining_today, milli(1500));
    let s = c.get_status();
    assert_eq!(s.spent_today, milli(500));
    assert_eq!(s.approved_count, U64(1));
    assert!(
        get_logs()
            .iter()
            .any(|l| l.contains("\"event\":\"action_approved\""))
    );
}

#[test]
fn unknown_receiver_is_denied_without_panic() {
    let mut c = setup();
    as_agent(T0);
    let v = c.agent_transfer(acc("attacker.near"), milli(1), None);
    assert!(!v.approved);
    assert_eq!(v.reason, "RECEIVER_NOT_ALLOWED");
    let s = c.get_status();
    assert_eq!(s.denied_count, U64(1));
    assert_eq!(s.spent_today, milli(0));
    assert!(
        get_logs()
            .iter()
            .any(|l| l.contains("\"event\":\"action_denied\""))
    );
}

#[test]
fn per_tx_limit() {
    let mut c = setup();
    as_agent(T0);
    let v = c.agent_transfer(acc("shop.near"), milli(1001), None);
    assert_eq!(v.reason, "PER_TX_LIMIT_EXCEEDED");
}

#[test]
fn daily_limit_and_reset_next_day() {
    let mut c = setup();
    as_agent(T0);
    assert!(c.agent_transfer(acc("shop.near"), near(1), None).approved);
    assert!(c.agent_transfer(acc("shop.near"), near(1), None).approved);
    let v = c.agent_transfer(acc("shop.near"), milli(1), None);
    assert_eq!(v.reason, "DAILY_LIMIT_EXCEEDED");
    // next UTC day: window resets
    as_agent(T0 + DAY_NS);
    let v = c.agent_transfer(acc("shop.near"), near(1), None);
    assert!(v.approved, "{v:?}");
    assert_eq!(c.get_status().spent_today, near(1));
}

#[test]
fn transfer_flag_and_method_allowlist() {
    let mut c = setup();
    as_agent(T0);
    assert_eq!(
        c.agent_transfer(acc("api.near"), milli(1), None).reason,
        "TRANSFER_NOT_ALLOWED"
    );
    assert!(
        c.agent_call(
            acc("shop.near"),
            "buy".into(),
            None,
            Some(milli(10)),
            Some(30),
            None
        )
        .approved
    );
    assert_eq!(
        c.agent_call(acc("shop.near"), "drain".into(), None, None, None, None)
            .reason,
        "METHOD_NOT_ALLOWED"
    );
    // wildcard
    assert!(
        c.agent_call(acc("api.near"), "anything".into(), None, None, None, None)
            .approved
    );
}

#[test]
fn gas_cap() {
    let mut c = setup();
    as_agent(T0);
    let v = c.agent_call(acc("shop.near"), "buy".into(), None, None, Some(51), None);
    assert_eq!(v.reason, "GAS_LIMIT_EXCEEDED");
}

#[test]
fn deposit_on_call_counts_against_limits() {
    let mut c = setup();
    as_agent(T0);
    let v = c.agent_call(
        acc("shop.near"),
        "buy".into(),
        None,
        Some(near(2)),
        None,
        None,
    );
    assert_eq!(v.reason, "PER_TX_LIMIT_EXCEEDED");
}

#[test]
fn kill_switch_blocks_everything_until_unpaused() {
    let mut c = setup();
    as_owner();
    c.pause();
    as_agent(T0);
    assert_eq!(
        c.agent_transfer(acc("shop.near"), milli(1), None).reason,
        "PAUSED"
    );
    as_owner();
    c.unpause();
    as_agent(T0);
    assert!(c.agent_transfer(acc("shop.near"), milli(1), None).approved);
}

#[test]
#[should_panic(expected = "Only the owner can do this")]
fn agent_cannot_unpause() {
    let mut c = setup();
    as_owner();
    c.pause();
    as_agent(T0);
    c.unpause();
}

#[test]
#[should_panic(expected = "Only the owner can do this")]
fn agent_cannot_change_policy() {
    let mut c = setup();
    as_agent(T0);
    c.set_policy(Policy {
        max_per_tx: near(100),
        daily_limit: near(100),
        max_gas_tgas: 100,
    });
}

#[test]
#[should_panic(expected = "Only the agent can do this")]
fn strangers_cannot_act() {
    let mut c = setup();
    testing_env!(ctx("random.near", T0).build());
    c.agent_transfer(acc("shop.near"), milli(1), None);
}

#[test]
#[should_panic(expected = "Only the agent can do this")]
fn revoked_agent_cannot_act() {
    let mut c = setup();
    as_owner();
    c.set_agent(None);
    as_agent(T0);
    c.agent_transfer(acc("shop.near"), milli(1), None);
}

#[test]
fn self_call_is_always_forbidden() {
    let mut c = setup();
    as_agent(T0);
    let v = c.agent_call(
        acc("guard.near"),
        "on_action_result".into(),
        None,
        None,
        None,
        None,
    );
    assert_eq!(v.reason, "SELF_CALL_FORBIDDEN");
}

#[test]
#[should_panic(expected = "The guard itself cannot be an allowed receiver")]
fn owner_cannot_allowlist_the_guard() {
    let mut c = setup();
    as_owner();
    c.set_receiver(
        acc("guard.near"),
        ReceiverRule {
            allow_transfer: true,
            methods: vec!["*".into()],
        },
    );
}

#[test]
fn insufficient_funds() {
    let mut c = setup();
    as_owner();
    c.set_policy(Policy {
        max_per_tx: near(50),
        daily_limit: near(50),
        max_gas_tgas: 50,
    });
    as_agent(T0);
    let v = c.agent_transfer(acc("shop.near"), near(20), None);
    assert_eq!(v.reason, "INSUFFICIENT_FUNDS");
}

#[test]
fn dry_run_does_not_change_state() {
    let c = setup();
    as_agent(T0);
    assert!(c.check_transfer(acc("shop.near"), near(1)).approved);
    assert_eq!(
        c.check_call(acc("shop.near"), "nope".into(), None, None)
            .reason,
        "METHOD_NOT_ALLOWED"
    );
    let s = c.get_status();
    assert_eq!(s.approved_count, U64(0));
    assert_eq!(s.denied_count, U64(0));
}

#[test]
fn failed_action_refunds_allowance() {
    let mut c = setup();
    as_agent(T0);
    assert!(c.agent_transfer(acc("shop.near"), near(1), None).approved);
    assert_eq!(c.get_status().spent_today, near(1));
    // callback with a failed promise result
    testing_env!(
        ctx("guard.near", T0).build(),
        near_sdk::test_vm_config(),
        near_sdk::RuntimeFeesConfig::test(),
        Default::default(),
        vec![PromiseResult::Failed],
    );
    assert!(!c.on_action_result(U64(0), near(1), U64(T0 / DAY_NS)));
    let s = c.get_status();
    assert_eq!(s.spent_today, near(0));
    assert_eq!(s.failed_count, U64(1));
}

#[test]
fn successful_callback_keeps_spend() {
    let mut c = setup();
    as_agent(T0);
    assert!(c.agent_transfer(acc("shop.near"), near(1), None).approved);
    testing_env!(
        ctx("guard.near", T0).build(),
        near_sdk::test_vm_config(),
        near_sdk::RuntimeFeesConfig::test(),
        Default::default(),
        vec![PromiseResult::Successful(vec![])],
    );
    assert!(c.on_action_result(U64(0), near(1), U64(T0 / DAY_NS)));
    assert_eq!(c.get_status().spent_today, near(1));
}

#[test]
#[should_panic(expected = "max_per_tx must not exceed daily_limit")]
fn invalid_policy_rejected() {
    let mut c = setup();
    as_owner();
    c.set_policy(Policy {
        max_per_tx: near(3),
        daily_limit: near(2),
        max_gas_tgas: 50,
    });
}

#[test]
fn owner_can_withdraw_available_only() {
    let mut c = setup();
    as_owner();
    let _ = c.withdraw(Some(near(5)));
}

#[test]
#[should_panic(expected = "Not enough available balance")]
fn owner_cannot_withdraw_more_than_available() {
    let mut c = setup();
    as_owner();
    let _ = c.withdraw(Some(near(10)));
}

#[test]
fn owner_can_initialise_in_batch() {
    testing_env!(ctx("owner.near", T0).build());
    let c = AgentGuard::new(acc("owner.near"), None, policy(), None);
    assert_eq!(c.get_status().owner, acc("owner.near"));
}

#[test]
#[should_panic(expected = "Only the guard account or the owner can initialise")]
fn stranger_cannot_initialise() {
    testing_env!(ctx("random.near", T0).build());
    AgentGuard::new(acc("owner.near"), None, policy(), None);
}
