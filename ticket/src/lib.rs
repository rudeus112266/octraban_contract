#![no_std]

// Unit / property / snapshot / stress / gas tests.
#[cfg(test)]
mod test;

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, symbol_short, Address, BytesN, Env,
    String, Symbol,
};

// ── Storage keys ──────────────────────────────────────────────────────────────

const ADMIN: Symbol = symbol_short!("ADMIN");
const MAX_TIX: Symbol = symbol_short!("MAX_TIX");
const PRICE: Symbol = symbol_short!("PRICE");
const NEXT_ID: Symbol = symbol_short!("NEXT_ID");

// ── Data types ────────────────────────────────────────────────────────────────

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub enum TicketStatus {
    Valid,
    Used,
    Transferred,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct Ticket {
    pub id: u64,
    pub owner: Address,
    pub event_name: String,
    pub status: TicketStatus,
    pub original_price: i128,
    pub max_resale_price: i128,
}

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum Error {
    NotFound = 1,
    NotInitialized = 2,
    AlreadyInitialized = 3,
    NegativePrice = 4,
    PriceOverflow = 5,
    PriceExceedsCeiling = 6,
}

// ── Contract ──────────────────────────────────────────────────────────────────

#[contract]
pub struct TicketContract;

#[contractimpl]
impl TicketContract {
    /// Initialize the event. Must be called once by the organizer.
    /// Returns `Error::AlreadyInitialized` if called more than once.
    pub fn initialize(
        env: Env,
        admin: Address,
        event_name: String,
        max_tickets: u64,
        price: i128,
        max_resale_price: i128,
    ) -> Result<(), Error> {
        admin.require_auth();
        if env.storage().instance().has(&ADMIN) {
            return Err(Error::AlreadyInitialized);
        }
        if price < 0 || max_resale_price < 0 {
            return Err(Error::NegativePrice);
        }

        env.storage().instance().set(&ADMIN, &admin);
        env.storage().instance().set(&MAX_TIX, &max_tickets);
        env.storage().instance().set(&PRICE, &price);
        env.storage()
            .instance()
            .set(&symbol_short!("EVT_NAME"), &event_name);
        env.storage()
            .instance()
            .set(&symbol_short!("MAXRESALE"), &max_resale_price);
        env.storage().instance().set(&NEXT_ID, &0u64);
        Ok(())
    }

    /// Mint a ticket to a recipient. Only admin (organizer) can call this.
    /// Returns `Error::NotInitialized` if called before `initialize`.
    pub fn mint_ticket(env: Env, organizer: Address, recipient: Address) -> Result<u64, Error> {
        organizer.require_auth();
        let admin: Address = env
            .storage()
            .instance()
            .get(&ADMIN)
            .ok_or(Error::NotInitialized)?;
        assert!(organizer == admin, "only organizer can mint");

        let max: u64 = env.storage().instance().get(&MAX_TIX).unwrap();
        let next_id: u64 = env.storage().instance().get(&NEXT_ID).unwrap();
        assert!(next_id < max, "sold out");

        let price: i128 = env.storage().instance().get(&PRICE).unwrap();
        let max_resale: i128 = env
            .storage()
            .instance()
            .get(&symbol_short!("MAXRESALE"))
            .unwrap();
        let event_name: String = env
            .storage()
            .instance()
            .get(&symbol_short!("EVT_NAME"))
            .unwrap();

        let ticket = Ticket {
            id: next_id,
            owner: recipient.clone(),
            event_name,
            status: TicketStatus::Valid,
            original_price: price,
            max_resale_price: max_resale,
        };

        env.storage().persistent().set(&next_id, &ticket);
        env.storage().instance().set(&NEXT_ID, &(next_id + 1));

        env.events()
            .publish((symbol_short!("MINTED"), recipient), next_id);

        Ok(next_id)
    }

    /// Transfer a ticket from one user to another, enforcing resale price cap.
    pub fn transfer_ticket(
        env: Env,
        from: Address,
        to: Address,
        ticket_id: u64,
        sale_price: i128,
    ) -> Result<(), Error> {
        from.require_auth();

        if sale_price < 0 {
            return Err(Error::NegativePrice);
        }

        let mut ticket: Ticket = env
            .storage()
            .persistent()
            .get(&ticket_id)
            .ok_or(Error::NotFound)?;

        if ticket.owner != from {
            panic!("not the ticket owner");
        }
        if ticket.status != TicketStatus::Valid {
            panic!("ticket not transferable");
        }
        if sale_price > ticket.max_resale_price {
            return Err(Error::PriceExceedsCeiling);
        }

        ticket.owner = to.clone();
        ticket.status = TicketStatus::Transferred;
        env.storage().persistent().set(&ticket_id, &ticket);

        env.events()
            .publish((symbol_short!("TRANSFER"), from, to), ticket_id);
        Ok(())
    }

    /// Verify and mark a ticket as used at entry.
    /// Returns `Error::NotInitialized` if called before `initialize`.
    pub fn verify_ticket(env: Env, verifier: Address, ticket_id: u64) -> Result<bool, Error> {
        verifier.require_auth();
        let admin: Address = env
            .storage()
            .instance()
            .get(&ADMIN)
            .ok_or(Error::NotInitialized)?;
        assert!(verifier == admin, "only organizer can verify");

        let mut ticket: Ticket = env
            .storage()
            .persistent()
            .get(&ticket_id)
            .expect("ticket not found");

        if ticket.status == TicketStatus::Valid || ticket.status == TicketStatus::Transferred {
            ticket.status = TicketStatus::Used;
            env.storage().persistent().set(&ticket_id, &ticket);
            env.events()
                .publish((symbol_short!("VERIFIED"),), ticket_id);
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Fetch ticket metadata.
    pub fn get_ticket(env: Env, ticket_id: u64) -> Result<Ticket, Error> {
        env.storage()
            .persistent()
            .get(&ticket_id)
            .ok_or(Error::NotFound)
    }

    /// Total tickets minted so far.
    pub fn tickets_sold(env: Env) -> u64 {
        env.storage().instance().get(&NEXT_ID).unwrap_or(0)
    }

    /// Upgrade the running contract WASM (admin/organizer only).
    /// Returns `Error::NotInitialized` if called before `initialize`.
    pub fn upgrade(env: Env, caller: Address, new_wasm_hash: BytesN<32>) -> Result<(), Error> {
        caller.require_auth();
        let admin: Address = env
            .storage()
            .instance()
            .get(&ADMIN)
            .ok_or(Error::NotInitialized)?;
        assert!(caller == admin, "only organizer can upgrade");
        env.deployer()
            .update_current_contract_wasm(new_wasm_hash.clone());
        env.events()
            .publish((symbol_short!("upgrade"),), new_wasm_hash);
        Ok(())
    }
}
