//! End-to-end tests against a local NEAR sandbox node (`cargo test --test sandbox`).
//! Builds the real wasm with cargo-near, deploys it and drives it with real transactions.
use near_api::{AccountId, NearGas, NearToken, NetworkConfig, Signer};
use near_sdk::serde_json::{Value, json};
use std::sync::Arc;

struct Env {
    sandbox: near_sandbox::Sandbox,
    net: NetworkConfig,
    signer: Arc<Signer>,
}

async fn create(env: &Env, name: &str, near: u128) -> testresult::TestResult<AccountId> {
    let id: AccountId = name.parse()?;
    env.sandbox
        .create_account(id.as_str().parse()?)
        .initial_balance(NearToken::from_near(near))
        .send()
        .await?;
    Ok(id)
}

async fn balance(env: &Env, id: &AccountId) -> testresult::TestResult<NearToken> {
    Ok(near_api::Account(id.clone())
        .tokens()
        .near_balance()
        .fetch_from(&env.net)
        .await?
        .total)
}

async fn view(
    env: &Env,
    guard: &AccountId,
    method: &str,
    args: Value,
) -> testresult::TestResult<Value> {
    Ok(near_api::Contract(guard.clone())
        .call_function(method, args)
        .read_only()
        .fetch_from(&env.net)
        .await?
        .data)
}

/// Call `method` on `guard` as `who`; returns (tx succeeded, JSON return value, logs).
async fn call(
    env: &Env,
    who: &AccountId,
    guard: &AccountId,
    method: &str,
    args: Value,
) -> testresult::TestResult<(bool, Value, Vec<String>)> {
    let res = near_api::Contract(guard.clone())
        .call_function(method, args)
        .transaction()
        .gas(NearGas::from_tgas(150))
        .with_signer(who.clone(), env.signer.clone())
        .send_to(&env.net)
        .await?;
    let logs: Vec<String> = res.logs().iter().map(|s| s.to_string()).collect();
    match res.into_result() {
        Ok(ok) => Ok((true, ok.json::<Value>().unwrap_or(Value::Null), logs)),
        Err(_) => Ok((false, Value::Null, logs)),
    }
}

fn near(n: &str) -> String {
    // decimal NEAR -> yocto string
    NearToken::from_yoctonear((n.parse::<f64>().unwrap() * 1e6) as u128 * 10u128.pow(18))
        .as_yoctonear()
        .to_string()
}

#[tokio::test]
async fn agent_guard_end_to_end() -> testresult::TestResult<()> {
    let wasm_path = cargo_near_build::build_with_cli(Default::default())?;
    let wasm = std::fs::read(wasm_path)?;

    let sandbox = near_sandbox::Sandbox::start_sandbox().await?;
    let net = NetworkConfig::from_rpc_url("sandbox", sandbox.rpc_addr.parse()?);
    let signer = Signer::from_secret_key(
        near_sandbox::config::DEFAULT_GENESIS_ACCOUNT_PRIVATE_KEY.parse()?,
    )?;
    let env = Env {
        sandbox,
        net,
        signer,
    };

    let owner = create(&env, "owner.sandbox", 20).await?;
    let agent = create(&env, "agent.sandbox", 10).await?;
    let shop = create(&env, "shop.sandbox", 1).await?;
    let attacker = create(&env, "attacker.sandbox", 1).await?;
    let nocode = create(&env, "nocode.sandbox", 1).await?;
    let guard = create(&env, "guard.sandbox", 20).await?;
    let merchant = create(&env, "merchant.sandbox", 10).await?;

    let policy = json!({"max_per_tx": near("1"), "daily_limit": near("2"), "max_gas_tgas": 50});
    // The merchant is another guard instance: we only use its payable `deposit` method.
    for (acct, receivers) in [
        (
            &guard,
            json!([
                [shop, {"allow_transfer": true, "methods": []}],
                [merchant, {"allow_transfer": false, "methods": ["deposit"]}],
                [nocode, {"allow_transfer": false, "methods": ["*"]}]
            ]),
        ),
        (&merchant, json!([])),
    ] {
        near_api::Contract::deploy(acct.clone())
            .use_code(wasm.clone())
            .with_init_call(
                "new",
                json!({"owner": owner, "agent": agent, "policy": policy, "receivers": receivers}),
            )?
            .with_signer(env.signer.clone())
            .send_to(&env.net)
            .await?
            .assert_success();
    }

    // 1. allowed transfer moves money
    let shop_before = balance(&env, &shop).await?;
    let (ok, v, logs) = call(
        &env,
        &agent,
        &guard,
        "agent_transfer",
        json!({"receiver_id": shop, "amount": near("0.5"), "memo": "buy coffee"}),
    )
    .await?;
    assert!(ok && v["approved"] == true, "{v}");
    assert!(logs.iter().any(|l| l.contains("action_approved")));
    assert!(logs.iter().any(|l| l.contains("action_succeeded")));
    assert_eq!(
        balance(&env, &shop).await?.as_yoctonear() - shop_before.as_yoctonear(),
        NearToken::from_millinear(500).as_yoctonear()
    );

    // 2. unknown receiver: denied, recorded, no money moves
    let att_before = balance(&env, &attacker).await?;
    let (ok, v, logs) = call(
        &env,
        &agent,
        &guard,
        "agent_transfer",
        json!({"receiver_id": attacker, "amount": near("1")}),
    )
    .await?;
    assert!(ok, "denials do not revert");
    assert_eq!(v["reason"], "RECEIVER_NOT_ALLOWED");
    assert!(logs.iter().any(|l| l.contains("action_denied")));
    assert_eq!(balance(&env, &attacker).await?, att_before);

    // 3. per-tx limit
    let (_, v, _) = call(
        &env,
        &agent,
        &guard,
        "agent_transfer",
        json!({"receiver_id": shop, "amount": near("1.5")}),
    )
    .await?;
    assert_eq!(v["reason"], "PER_TX_LIMIT_EXCEEDED");

    // 4. allowlisted function call with deposit reaches the merchant contract
    let m_before = balance(&env, &merchant).await?;
    let (_, v, logs) = call(&env, &agent, &guard, "agent_call",
        json!({"receiver_id": merchant, "method_name": "deposit", "deposit": near("0.3"), "gas_tgas": 20})).await?;
    assert_eq!(v["approved"], true, "{v}");
    assert!(
        logs.iter().any(|l| l.contains("\"event\":\"deposit\"")),
        "{logs:?}"
    );
    assert!(balance(&env, &merchant).await?.as_yoctonear() > m_before.as_yoctonear());

    // 5. method not on the list
    let (_, v, _) = call(
        &env,
        &agent,
        &guard,
        "agent_call",
        json!({"receiver_id": merchant, "method_name": "withdraw"}),
    )
    .await?;
    assert_eq!(v["reason"], "METHOD_NOT_ALLOWED");

    // 6. approved call that fails downstream: allowance is refunded via callback
    let (_, v, logs) = call(&env, &agent, &guard, "agent_call",
        json!({"receiver_id": nocode, "method_name": "anything", "deposit": near("0.2"), "gas_tgas": 20})).await?;
    assert_eq!(v["approved"], true);
    assert!(logs.iter().any(|l| l.contains("action_failed")), "{logs:?}");
    let s = view(&env, &guard, "get_status", json!({})).await?;
    assert_eq!(s["spent_today"], near("0.8"), "{s}");
    assert_eq!(s["failed_count"], "1");

    // 7. daily limit: 0.8 spent, +1 ok, then +0.5 exceeds 2 NEAR/day
    let (_, v, _) = call(
        &env,
        &agent,
        &guard,
        "agent_transfer",
        json!({"receiver_id": shop, "amount": near("1")}),
    )
    .await?;
    assert_eq!(v["approved"], true);
    let (_, v, _) = call(
        &env,
        &agent,
        &guard,
        "agent_transfer",
        json!({"receiver_id": shop, "amount": near("0.5")}),
    )
    .await?;
    assert_eq!(v["reason"], "DAILY_LIMIT_EXCEEDED");

    // 8. self-call through agent_call is impossible
    let (_, v, _) = call(
        &env,
        &agent,
        &guard,
        "agent_call",
        json!({"receiver_id": guard, "method_name": "unpause"}),
    )
    .await?;
    assert_eq!(v["reason"], "SELF_CALL_FORBIDDEN");

    // 9. kill switch
    let (ok, _, _) = call(&env, &owner, &guard, "pause", json!({})).await?;
    assert!(ok);
    let (_, v, _) = call(
        &env,
        &agent,
        &guard,
        "agent_transfer",
        json!({"receiver_id": shop, "amount": near("0.01")}),
    )
    .await?;
    assert_eq!(v["reason"], "PAUSED");
    let (ok, _, _) = call(&env, &agent, &guard, "unpause", json!({})).await?;
    assert!(!ok, "agent must not be able to lift the kill switch");
    let (ok, _, _) = call(&env, &agent, &guard, "set_policy",
        json!({"policy": {"max_per_tx": near("100"), "daily_limit": near("100"), "max_gas_tgas": 100}})).await?;
    assert!(!ok, "agent must not be able to change policy");

    // 10. strangers cannot act as the agent
    let (ok, _, _) = call(
        &env,
        &attacker,
        &guard,
        "agent_transfer",
        json!({"receiver_id": shop, "amount": near("0.01")}),
    )
    .await?;
    assert!(!ok);

    // 11. owner pulls the remaining funds back
    let owner_before = balance(&env, &owner).await?;
    let (ok, _, _) = call(&env, &owner, &guard, "withdraw", json!({})).await?;
    assert!(ok);
    assert!(
        balance(&env, &owner).await?.as_yoctonear()
            > owner_before.as_yoctonear() + NearToken::from_near(15).as_yoctonear()
    );

    let s = view(&env, &guard, "get_status", json!({})).await?;
    assert_eq!(s["approved_count"], "4", "{s}");
    assert_eq!(s["denied_count"], "6", "{s}");
    assert_eq!(s["paused"], true);
    Ok(())
}
