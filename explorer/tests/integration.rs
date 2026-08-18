#![cfg(test)]

use octraban_contract::{EventInput, ExplorerContract, ExplorerContractClient};
use soroban_sdk::{testutils::Address as _, Address, Bytes, BytesN, Env, String, Vec};

// Integration tests testing multi-contract scenarios
#[test]
fn test_cross_contract_event_registration() {
    let env = Env::default();
    env.mock_all_auths();

    // Deploy explorer
    let explorer_id = env.register_contract(None, ExplorerContract);
    let explorer = ExplorerContractClient::new(&env, &explorer_id);
    let admin = Address::generate(&env);
    explorer.init(&admin, &50000);

    // Simulate Contract A registering Contract B's events (e.g. factory pattern)
    let _contract_a = Address::generate(&env);
    let contract_b_id: BytesN<32> = BytesN::from_array(&env, &[1; 32]);

    let input = EventInput {
        contract_id: contract_b_id.clone(),
        function: soroban_sdk::symbol_short!("mint"),
        ledger: 1000,
        description: String::from_str(&env, "Minted from Contract A"),
        raw_topics: Vec::new(&env),
        raw_data: Bytes::new(&env),
    };

    explorer.submit_event(&admin, &input);

    // Verify Contract B's event is stored correctly
    assert_eq!(explorer.event_count(), 1);
    let stored_event = explorer.get_event(&0);
    assert_eq!(stored_event.contract_id, contract_b_id);
    assert_eq!(stored_event.ledger, 1000);
}
