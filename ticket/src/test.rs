//! Tests for the Ticket contract.
//!
//! Sections
//! --------
//! 1. Original unit tests (preserved)
//! 2. Property-based tests  (proptest)
//! 3. Snapshot / state-diff tests
//! 4. Gas benchmark tests
//! 5. Stress tests
//! 6. Edge-case / error-path tests

#![cfg(test)]

use super::*;
use soroban_sdk::{testutils::Address as _, Env, String};

// ─── helpers ─────────────────────────────────────────────────────────────────

/// Shared setup: deploy + initialise the contract with default parameters.
fn setup() -> (Env, TicketContractClient<'static>, Address, Address) {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TicketContract);
    let client = TicketContractClient::new(&env, &contract_id);

    let organizer = Address::generate(&env);
    let buyer = Address::generate(&env);

    client.initialize(
        &organizer,
        &String::from_str(&env, "Harvesta Live 2025"),
        &100u64,
        &50_000_000i128, // 5 XLM in stroops
        &75_000_000i128, // max resale 7.5 XLM
    );

    (env, client, organizer, buyer)
}

/// Setup with custom capacity.
fn setup_with_capacity(max: u64) -> (Env, TicketContractClient<'static>, Address) {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TicketContract);
    let client = TicketContractClient::new(&env, &contract_id);
    let organizer = Address::generate(&env);
    client.initialize(
        &organizer,
        &String::from_str(&env, "Test Event"),
        &max,
        &1_000i128,
        &2_000i128,
    );
    (env, client, organizer)
}

// ═══════════════════════════════════════════════════════════════════════════
// 1. ORIGINAL UNIT TESTS
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_mint_and_get() {
    let (_env, client, organizer, buyer) = setup();
    let id = client.mint_ticket(&organizer, &buyer);
    assert_eq!(id, 0);

    let ticket = client.get_ticket(&0u64);
    assert_eq!(ticket.owner, buyer);
    assert_eq!(ticket.status, TicketStatus::Valid);
}

#[test]
fn test_transfer() {
    let (env, client, organizer, buyer) = setup();
    client.mint_ticket(&organizer, &buyer);

    let new_owner = Address::generate(&env);
    let result = client.try_transfer_ticket(&buyer, &new_owner, &0u64, &60_000_000i128);
    assert_eq!(result, Ok(Ok(())));

    let ticket = client.get_ticket(&0u64);
    assert_eq!(ticket.owner, new_owner);
    assert_eq!(ticket.status, TicketStatus::Transferred);
}

#[test]
fn test_resale_cap_enforced() {
    let (env, client, organizer, buyer) = setup();
    client.mint_ticket(&organizer, &buyer);

    let new_owner = Address::generate(&env);
    let result = client.try_transfer_ticket(&buyer, &new_owner, &0u64, &100_000_000i128);
    assert_eq!(result, Err(Ok(Error::PriceExceedsCeiling)));
}

#[test]
fn test_verify_ticket() {
    let (_env, client, organizer, buyer) = setup();
    client.mint_ticket(&organizer, &buyer);

    let valid = client.verify_ticket(&organizer, &0u64);
    assert!(valid);

    // Second scan must return false (already used).
    let double_scan = client.verify_ticket(&organizer, &0u64);
    assert!(!double_scan);
}

// ═══════════════════════════════════════════════════════════════════════════
// 2. UNINITIALISED-STATE ERROR PATHS (#9)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_mint_before_initialize_returns_not_initialized() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TicketContract);
    let client = TicketContractClient::new(&env, &contract_id);

    let organizer = Address::generate(&env);
    let buyer = Address::generate(&env);

    assert_eq!(
        client.try_mint_ticket(&organizer, &buyer),
        Err(Ok(Error::NotInitialized))
    );
}

#[test]
fn test_verify_before_initialize_returns_not_initialized() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TicketContract);
    let client = TicketContractClient::new(&env, &contract_id);

    let organizer = Address::generate(&env);

    assert_eq!(
        client.try_verify_ticket(&organizer, &0u64),
        Err(Ok(Error::NotInitialized))
    );
}

#[test]
fn test_upgrade_before_initialize_returns_not_initialized() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TicketContract);
    let client = TicketContractClient::new(&env, &contract_id);

    let caller = Address::generate(&env);
    let hash = soroban_sdk::BytesN::from_array(&env, &[0u8; 32]);

    assert_eq!(
        client.try_upgrade(&caller, &hash),
        Err(Ok(Error::NotInitialized))
    );
}

#[test]
fn test_double_initialize_returns_already_initialized() {
    let (_env, client, organizer, _buyer) = setup();

    assert_eq!(
        client.try_initialize(
            &organizer,
            &String::from_str(&_env, "Harvesta Live 2025"),
            &100u64,
            &50_000_000i128,
            &75_000_000i128,
        ),
        Err(Ok(Error::AlreadyInitialized))
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 3. PRICE VALIDATION TESTS (somzilla issues)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_negative_sale_price_returns_typed_error() {
    let (env, client, organizer, buyer) = setup();
    client.mint_ticket(&organizer, &buyer);

    let new_owner = Address::generate(&env);
    let result = client.try_transfer_ticket(&buyer, &new_owner, &0u64, &-1i128);
    assert_eq!(result, Err(Ok(Error::NegativePrice)));
}

#[test]
fn test_transfer_sale_price_above_max_resale_price_is_rejected() {
    let (env, client, organizer, buyer) = setup();
    client.mint_ticket(&organizer, &buyer);

    let new_owner = Address::generate(&env);
    let result = client.try_transfer_ticket(&buyer, &new_owner, &0u64, &76_000_000i128);
    assert_eq!(result, Err(Ok(Error::PriceExceedsCeiling)));
}

#[test]
fn test_extreme_i128_values_return_typed_error() {
    let (env, client, organizer, buyer) = setup();
    client.mint_ticket(&organizer, &buyer);

    let new_owner = Address::generate(&env);

    // i128::MIN — should be caught as NegativePrice
    let result = client.try_transfer_ticket(&buyer, &new_owner, &0u64, &i128::MIN);
    assert_eq!(result, Err(Ok(Error::NegativePrice)));

    // i128::MAX — above max_resale_price ceiling, so PriceExceedsCeiling
    let result = client.try_transfer_ticket(&buyer, &new_owner, &0u64, &i128::MAX);
    assert_eq!(result, Err(Ok(Error::PriceExceedsCeiling)));
}
