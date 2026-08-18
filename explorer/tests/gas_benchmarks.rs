#![cfg(test)]

use octraban_contract::{EventInput, ExplorerContract, ExplorerContractClient};
use soroban_sdk::{testutils::Address as _, Address, Bytes, BytesN, Env, String, Vec};

// Gas usage regression tests
#[test]
fn test_submit_event_gas_benchmark() {
    let env = Env::default();
    env.mock_all_auths();

    // Enable gas tracking in the mock environment
    env.budget().reset_unlimited();

    let explorer_id = env.register_contract(None, ExplorerContract);
    let explorer = ExplorerContractClient::new(&env, &explorer_id);
    let admin = Address::generate(&env);
    explorer.init(&admin, &50000);

    let contract_id: BytesN<32> = BytesN::from_array(&env, &[3; 32]);
    let input = EventInput {
        contract_id,
        function: soroban_sdk::symbol_short!("bench"),
        ledger: 1000,
        description: String::from_str(&env, "Benchmarking gas usage"),
        raw_topics: Vec::new(&env),
        raw_data: Bytes::new(&env),
    };

    let start_cpu_insns = env.budget().cpu_instruction_cost();

    explorer.submit_event(&admin, &input);

    let cpu_insns_used = env.budget().cpu_instruction_cost() - start_cpu_insns;

    // Fail if gas exceeds our optimized budget threshold (e.g. 50,000 instructions)
    assert!(cpu_insns_used < 1_000_000, "Gas exceeded budget threshold!");
}
