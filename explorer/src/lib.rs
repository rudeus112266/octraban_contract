#![no_std]

//! Explorer contract for registering contract metadata and persisting decoded
//! Soroban events in a compact on-chain ring buffer.

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, panic_with_error, symbol_short, Address,
    Bytes, BytesN, Env, String, Symbol, Vec,
};

// ── Error codes ──────────────────────────────────────────────────────────────

#[allow(missing_docs)]
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum Error {
    NotFound = 1,
    Unauthorized = 2,
    AlreadyExists = 3,
    BelowFloor = 4,
    ContractPaused = 5,
    InvalidInput = 6,
    Unsupported = 7,
}

// ── Storage keys ─────────────────────────────────────────────────────────────

/// Composite key used to store historical ABI versions.
#[allow(missing_docs)]
#[contracttype]
#[derive(Clone)]
pub struct VersionKey {
    pub contract_id: BytesN<32>,
    pub abi_version: u32,
}

#[allow(missing_docs)]
#[contracttype]
pub enum DataKey {
    Admin,
    /// Address nominated by the current admin via `transfer_admin`, pending
    /// its own `accept_admin` call. Absent when there is no pending transfer.
    PendingAdmin,
    Contract(BytesN<32>),
    /// Event log entries use persistent storage to ensure they survive ledger archival.
    /// Temporary storage would expire when TTL reaches zero, causing silent data loss.
    EventLog(u64),
    EventSeq,
    MaxEvents,
    Paused,
    ContractVersion(VersionKey),
}

/// Minimum allowed value for `max_events` (prevents accidental data loss).
pub const MIN_MAX_EVENTS: u32 = 1_000;
/// Default ring-buffer capacity used at init when caller passes `0`.
pub const DEFAULT_MAX_EVENTS: u32 = 50_000;
/// Maximum number of events returned by a single `get_events` call.
/// A caller that needs more events must page with the returned cursor.
pub const MAX_PAGE_SIZE: u32 = 500;

// ── Storage TTL policy ───────────────────────────────────────────────────────
//
// Soroban archives an instance/persistent entry once its TTL lapses, after
// which it is unreadable until explicitly restored. Every entry point that
// writes state below also calls `extend_ttl` so the entries it touches keep
// living well past the point they're next expected to be written or read.
// Ledger close time is ~5s, so one day is ~17,280 ledgers.
//
// - Instance storage (admin, pause flag, `MaxEvents`, `EventSeq`) backs every
//   admin-gated call, so it is bumped to a 30-day horizon on every
//   state-changing entry point.
// - Registry entries (`Contract`, `ContractVersion`) are the durable record
//   this contract exists to serve — they are written rarely and read
//   indefinitely, so they get a 90-day horizon on both write and read.
// - Event-log slots are the ring buffer: bounded, and each slot is
//   overwritten as soon as `seq` wraps back around to it (see
//   `submit_event`), so they only need to survive long enough to be
//   consumed by the indexer before either being read or evicted by wraparound
//   — a 7-day horizon. Persistent (not temporary) storage is used
//   deliberately: temporary entries are hard-deleted the instant their TTL
//   hits zero, with no restoration path, which would silently drop event
//   history the indexer hasn't caught up on yet. Persistent entries can be
//   restored (see the README) if this contract's own `extend_ttl` calls ever
//   lapse, e.g. after an extended period with no writes.
pub const LEDGERS_PER_DAY: u32 = 17_280;

/// Instance storage: bumped to this many ledgers ahead on every state-changing call.
pub const INSTANCE_BUMP_AMOUNT: u32 = 30 * LEDGERS_PER_DAY;
/// Instance storage: bump triggers once the remaining TTL drops below this.
pub const INSTANCE_LIFETIME_THRESHOLD: u32 = INSTANCE_BUMP_AMOUNT - LEDGERS_PER_DAY;

/// Registry entries (`Contract` / `ContractVersion`): bump horizon.
pub const REGISTRY_BUMP_AMOUNT: u32 = 90 * LEDGERS_PER_DAY;
/// Registry entries: bump triggers once the remaining TTL drops below this.
pub const REGISTRY_LIFETIME_THRESHOLD: u32 = REGISTRY_BUMP_AMOUNT - LEDGERS_PER_DAY;

/// Event-log ring-buffer slots: bump horizon.
pub const EVENT_BUMP_AMOUNT: u32 = 7 * LEDGERS_PER_DAY;
/// Event-log ring-buffer slots: bump triggers once the remaining TTL drops below this.
pub const EVENT_LIFETIME_THRESHOLD: u32 = EVENT_BUMP_AMOUNT - LEDGERS_PER_DAY;

/// Version marker included as the first data field of every published event.
/// Consumers (e.g. the indexer) should switch on this to handle payload
/// changes across contract upgrades. Bump it whenever a topic or payload
/// shape below changes. See `docs/EVENTS.md` for the full reference.
pub const EVENT_VERSION: u32 = 1;

// ── Data types ────────────────────────────────────────────────────────────────

/// ABI-like metadata for a registered contract.
#[allow(missing_docs)]
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct ContractMeta {
    /// Schema version for forward compatibility.
    pub version: u32,
    /// Monotonic ABI version; incremented on every `update_contract` call.
    pub abi_version: u32,
    /// Ledger sequence at which this ABI version was written.
    pub min_ledger: u32,
    pub name: String,
    pub description: String,
    pub functions: Vec<FunctionAbi>,
    pub registered_by: Address,
}

/// Describes one callable function so the explorer can decode calls.
#[allow(missing_docs)]
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct FunctionAbi {
    pub name: Symbol,
    pub description: String,
    pub params: Vec<ParamDef>,
}

/// One parameter definition.
#[allow(missing_docs)]
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct ParamDef {
    pub name: Symbol,
    pub kind: Symbol,
}

/// A decoded, human-readable event stored on-chain.
#[allow(missing_docs)]
#[contracttype]
#[derive(Clone)]
pub struct DecodedEvent {
    pub seq: u64,
    pub contract_id: BytesN<32>,
    pub function: Symbol,
    pub ledger: u32,
    pub description: String,
    pub raw_topics: Vec<String>,
    pub raw_data: Bytes,
}

/// Event submission parameters.
#[allow(missing_docs)]
#[contracttype]
#[derive(Clone)]
pub struct EventInput {
    pub contract_id: BytesN<32>,
    pub function: Symbol,
    pub ledger: u32,
    pub description: String,
    pub raw_topics: Vec<String>,
    pub raw_data: Bytes,
}

// ── Contract ──────────────────────────────────────────────────────────────────

#[allow(missing_docs)]
#[contract]
pub struct ExplorerContract;

#[contractimpl]
impl ExplorerContract {
    // ── Admin ─────────────────────────────────────────────────────────────────

    /// Initialises the explorer and configures the event ring buffer.
    /// Panics with `AlreadyExists` if called more than once.
    /// Pass `max_events = 0` to use the default capacity.
    pub fn init(env: Env, admin: Address, max_events: u32) {
        if env.storage().instance().has(&DataKey::Admin) {
            panic_with_error!(&env, Error::AlreadyExists);
        }
        let cap = if max_events == 0 {
            DEFAULT_MAX_EVENTS
        } else {
            max_events
        };
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::EventSeq, &0u64);
        env.storage().instance().set(&DataKey::MaxEvents, &cap);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_LIFETIME_THRESHOLD, INSTANCE_BUMP_AMOUNT);
    }

    /// Nominate a new admin (current admin only). This is step one of a
    /// two-step handover: it does *not* change the active admin, so the
    /// current admin keeps control until the nominee calls `accept_admin`.
    /// A pending nomination can be withdrawn with `cancel_admin_transfer`.
    /// This guards against permanently bricking the registry by transferring
    /// to a mistyped or uncontrolled address — every privileged call is
    /// gated on the active admin, and there is no other recovery path.
    pub fn transfer_admin(env: Env, caller: Address, new_admin: Address) {
        caller.require_auth();
        let admin: Address = env.storage().instance().get(&DataKey::Admin).unwrap();
        if caller != admin {
            panic_with_error!(&env, Error::Unauthorized);
        }
        env.storage()
            .instance()
            .set(&DataKey::PendingAdmin, &new_admin);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_LIFETIME_THRESHOLD, INSTANCE_BUMP_AMOUNT);
        // Topics: (adm_nom, caller). Data: (version, new_admin). See docs/EVENTS.md.
        env.events().publish(
            (symbol_short!("adm_nom"), caller),
            (EVENT_VERSION, new_admin),
        );
    }

    /// Accept a pending admin nomination (nominee only). This is step two of
    /// the two-step handover: it promotes `caller` to active admin and
    /// clears the pending nomination.
    /// Panics with `NotFound` if there is no pending nomination, or
    /// `Unauthorized` if `caller` is not the nominated address.
    pub fn accept_admin(env: Env, caller: Address) {
        caller.require_auth();
        let pending: Address = env
            .storage()
            .instance()
            .get(&DataKey::PendingAdmin)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotFound));
        if caller != pending {
            panic_with_error!(&env, Error::Unauthorized);
        }
        env.storage().instance().set(&DataKey::Admin, &caller);
        env.storage().instance().remove(&DataKey::PendingAdmin);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_LIFETIME_THRESHOLD, INSTANCE_BUMP_AMOUNT);
        // Topics: (adm_acc, caller). Data: (version,). See docs/EVENTS.md.
        env.events()
            .publish((symbol_short!("adm_acc"), caller), (EVENT_VERSION,));
    }

    /// Cancel a pending admin nomination (current admin only).
    /// Panics with `NotFound` if there is no pending nomination.
    pub fn cancel_admin_transfer(env: Env, caller: Address) {
        caller.require_auth();
        let admin: Address = env.storage().instance().get(&DataKey::Admin).unwrap();
        if caller != admin {
            panic_with_error!(&env, Error::Unauthorized);
        }
        if !env.storage().instance().has(&DataKey::PendingAdmin) {
            panic_with_error!(&env, Error::NotFound);
        }
        env.storage().instance().remove(&DataKey::PendingAdmin);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_LIFETIME_THRESHOLD, INSTANCE_BUMP_AMOUNT);
        // Topics: (adm_cncl, caller). Data: (version,). See docs/EVENTS.md.
        env.events()
            .publish((symbol_short!("adm_cncl"), caller), (EVENT_VERSION,));
    }

    /// Update the ring-buffer capacity (admin only).
    /// Panics with `BelowFloor` if `new_max < MIN_MAX_EVENTS`.
    pub fn set_max_events(env: Env, caller: Address, new_max: u32) {
        caller.require_auth();
        let admin: Address = env.storage().instance().get(&DataKey::Admin).unwrap();
        if caller != admin {
            panic_with_error!(&env, Error::Unauthorized);
        }
        if new_max < MIN_MAX_EVENTS {
            panic_with_error!(&env, Error::BelowFloor);
        }
        env.storage().instance().set(&DataKey::MaxEvents, &new_max);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_LIFETIME_THRESHOLD, INSTANCE_BUMP_AMOUNT);
    }

    /// Returns `(current_event_count, max_events)`.
    pub fn storage_utilisation(env: Env) -> (u64, u32) {
        let seq: u64 = env
            .storage()
            .instance()
            .get(&DataKey::EventSeq)
            .unwrap_or(0);
        let max: u32 = env
            .storage()
            .instance()
            .get(&DataKey::MaxEvents)
            .unwrap_or(DEFAULT_MAX_EVENTS);
        (seq.min(max as u64), max)
    }

    // ── Pause / unpause ───────────────────────────────────────────────────────

    /// Freeze all state-mutating operations (admin only).
    pub fn pause(env: Env, caller: Address) {
        caller.require_auth();
        let admin: Address = env.storage().instance().get(&DataKey::Admin).unwrap();
        if caller != admin {
            panic_with_error!(&env, Error::Unauthorized);
        }
        env.storage().instance().set(&DataKey::Paused, &true);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_LIFETIME_THRESHOLD, INSTANCE_BUMP_AMOUNT);
        // Topics: (paused,). Data: (version,). See docs/EVENTS.md.
        env.events()
            .publish((symbol_short!("paused"),), (EVENT_VERSION,));
    }

    /// Unfreeze the contract (admin only).
    pub fn unpause(env: Env, caller: Address) {
        caller.require_auth();
        let admin: Address = env.storage().instance().get(&DataKey::Admin).unwrap();
        if caller != admin {
            panic_with_error!(&env, Error::Unauthorized);
        }
        env.storage().instance().set(&DataKey::Paused, &false);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_LIFETIME_THRESHOLD, INSTANCE_BUMP_AMOUNT);
        // Topics: (unpaused,). Data: (version,). See docs/EVENTS.md.
        env.events()
            .publish((symbol_short!("unpaused"),), (EVENT_VERSION,));
    }

    /// Upgrade the running contract WASM (admin only).
    ///
    /// Caller must be the current admin and the contract must not be paused,
    /// mirroring the existing lifecycle/safety contract for admin-gated operations.
    pub fn upgrade(env: Env, caller: Address, new_wasm_hash: BytesN<32>) {
        caller.require_auth();
        if env
            .storage()
            .instance()
            .get(&DataKey::Paused)
            .unwrap_or(false)
        {
            panic_with_error!(&env, Error::ContractPaused);
        }
        let admin: Address = env.storage().instance().get(&DataKey::Admin).unwrap();
        if caller != admin {
            panic_with_error!(&env, Error::Unauthorized);
        }
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_LIFETIME_THRESHOLD, INSTANCE_BUMP_AMOUNT);
        env.deployer()
            .update_current_contract_wasm(new_wasm_hash.clone());
        // Topics: (upgrade,). Data: (version, new_wasm_hash). See docs/EVENTS.md.
        env.events().publish(
            (symbol_short!("upgrade"),),
            (EVENT_VERSION, new_wasm_hash.clone()),
        );
    }

    /// Returns whether the contract is currently paused.
    pub fn is_paused(env: Env) -> bool {
        env.storage()
            .instance()
            .get(&DataKey::Paused)
            .unwrap_or(false)
    }

    // ── Contract Registry ─────────────────────────────────────────────────────

    /// Register ABI metadata for a Soroban contract.
    /// Panics with `AlreadyExists` if the contract ID is already registered.
    /// The contract forces `abi_version = 0` and records `min_ledger` on first write.
    pub fn register_contract(
        env: Env,
        caller: Address,
        contract_id: BytesN<32>,
        meta: ContractMeta,
    ) {
        caller.require_auth();
        if env
            .storage()
            .instance()
            .get(&DataKey::Paused)
            .unwrap_or(false)
        {
            panic_with_error!(&env, Error::ContractPaused);
        }
        let admin: Address = env.storage().instance().get(&DataKey::Admin).unwrap();
        if caller != admin {
            panic_with_error!(&env, Error::Unauthorized);
        }
        let key = DataKey::Contract(contract_id.clone());
        if env.storage().persistent().has(&key) {
            panic_with_error!(&env, Error::AlreadyExists);
        }
        let mut stored = meta;
        stored.abi_version = 0;
        stored.min_ledger = env.ledger().sequence();
        env.storage().persistent().set(&key, &stored);
        env.storage().persistent().extend_ttl(
            &key,
            REGISTRY_LIFETIME_THRESHOLD,
            REGISTRY_BUMP_AMOUNT,
        );

        // Version history entry for abi_version 0.
        let vkey = DataKey::ContractVersion(VersionKey {
            contract_id: contract_id.clone(),
            abi_version: 0,
        });
        env.storage().persistent().set(&vkey, &stored);
        env.storage().persistent().extend_ttl(
            &vkey,
            REGISTRY_LIFETIME_THRESHOLD,
            REGISTRY_BUMP_AMOUNT,
        );
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_LIFETIME_THRESHOLD, INSTANCE_BUMP_AMOUNT);

        // Topics: (c_reg, contract_id).
        // Data: (version, registered_by, contract_version, ledger, name). See docs/EVENTS.md.
        env.events().publish(
            (symbol_short!("c_reg"), contract_id),
            (
                EVENT_VERSION,
                stored.registered_by.clone(),
                stored.version,
                env.ledger().sequence(),
                stored.name.clone(),
            ),
        );
    }

    /// Update registered metadata.
    /// Caller must be the admin or the original registrant.
    /// `meta.abi_version` must equal `existing.abi_version + 1` (optimistic concurrency guard).
    pub fn update_contract(env: Env, caller: Address, contract_id: BytesN<32>, meta: ContractMeta) {
        caller.require_auth();
        if env
            .storage()
            .instance()
            .get(&DataKey::Paused)
            .unwrap_or(false)
        {
            panic_with_error!(&env, Error::ContractPaused);
        }
        let key = DataKey::Contract(contract_id.clone());
        let existing: ContractMeta = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotFound));

        let admin: Address = env.storage().instance().get(&DataKey::Admin).unwrap();
        if caller != existing.registered_by && caller != admin {
            panic_with_error!(&env, Error::Unauthorized);
        }

        // Optimistic concurrency: submitted abi_version must be current + 1.
        let expected = existing.abi_version + 1;
        if meta.abi_version != expected {
            panic_with_error!(&env, Error::Unauthorized);
        }

        let old_abi_version = existing.abi_version;
        let old_version = existing.version;
        let new_abi_version = meta.abi_version;
        let min_ledger = env.ledger().sequence();
        let mut updated = meta;
        updated.min_ledger = min_ledger;
        env.storage().persistent().set(&key, &updated);
        env.storage().persistent().extend_ttl(
            &key,
            REGISTRY_LIFETIME_THRESHOLD,
            REGISTRY_BUMP_AMOUNT,
        );

        let vkey = DataKey::ContractVersion(VersionKey {
            contract_id: contract_id.clone(),
            abi_version: new_abi_version,
        });
        env.storage().persistent().set(&vkey, &updated);
        env.storage().persistent().extend_ttl(
            &vkey,
            REGISTRY_LIFETIME_THRESHOLD,
            REGISTRY_BUMP_AMOUNT,
        );
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_LIFETIME_THRESHOLD, INSTANCE_BUMP_AMOUNT);

        // Topics: (c_abiu, contract_id). Data: (version, old_abi_version, new_abi_version,
        // ledger). See docs/EVENTS.md.
        env.events().publish(
            (symbol_short!("c_abiu"), contract_id.clone()),
            (EVENT_VERSION, old_abi_version, new_abi_version, min_ledger),
        );
        // Topics: (c_upd, contract_id). Data: (version, caller, old_version, new_version,
        // ledger). See docs/EVENTS.md.
        env.events().publish(
            (symbol_short!("c_upd"), contract_id),
            (
                EVENT_VERSION,
                caller,
                old_version,
                updated.version,
                env.ledger().sequence(),
            ),
        );
    }

    /// Extends this registry entry's TTL to `REGISTRY_BUMP_AMOUNT` on read, so
    /// contracts under active query traffic never lapse between writes.
    pub fn get_contract(env: Env, contract_id: BytesN<32>) -> Result<ContractMeta, Error> {
        let key = DataKey::Contract(contract_id);
        let meta: ContractMeta = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(Error::NotFound)?;
        env.storage().persistent().extend_ttl(
            &key,
            REGISTRY_LIFETIME_THRESHOLD,
            REGISTRY_BUMP_AMOUNT,
        );
        Ok(meta)
    }

    /// Fetch a specific historical ABI version.
    /// Returns `None` if that version does not exist.
    /// Extends this entry's TTL to `REGISTRY_BUMP_AMOUNT` on read (see `get_contract`).
    pub fn get_contract_version(
        env: Env,
        contract_id: BytesN<32>,
        abi_version: u32,
    ) -> Option<ContractMeta> {
        let key = DataKey::ContractVersion(VersionKey {
            contract_id,
            abi_version,
        });
        let meta: ContractMeta = env.storage().persistent().get(&key)?;
        env.storage().persistent().extend_ttl(
            &key,
            REGISTRY_LIFETIME_THRESHOLD,
            REGISTRY_BUMP_AMOUNT,
        );
        Some(meta)
    }

    /// Alias for `get_contract` — returns the latest metadata.
    /// Extends this entry's TTL to `REGISTRY_BUMP_AMOUNT` on read (see `get_contract`).
    pub fn get_latest_contract(env: Env, contract_id: BytesN<32>) -> Option<ContractMeta> {
        let key = DataKey::Contract(contract_id);
        let meta: ContractMeta = env.storage().persistent().get(&key)?;
        env.storage().persistent().extend_ttl(
            &key,
            REGISTRY_LIFETIME_THRESHOLD,
            REGISTRY_BUMP_AMOUNT,
        );
        Some(meta)
    }

    /// Deregister a contract.
    /// Caller must be the admin or the original registrant.
    pub fn deregister_contract(env: Env, caller: Address, contract_id: BytesN<32>) {
        caller.require_auth();
        if env
            .storage()
            .instance()
            .get(&DataKey::Paused)
            .unwrap_or(false)
        {
            panic_with_error!(&env, Error::ContractPaused);
        }
        let key = DataKey::Contract(contract_id.clone());
        let existing: ContractMeta = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotFound));

        let admin: Address = env.storage().instance().get(&DataKey::Admin).unwrap();
        if caller != existing.registered_by && caller != admin {
            panic_with_error!(&env, Error::Unauthorized);
        }

        env.storage().persistent().remove(&key);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_LIFETIME_THRESHOLD, INSTANCE_BUMP_AMOUNT);
        // Topics: (c_dereg, contract_id). Data: (version, caller, ledger). See docs/EVENTS.md.
        env.events().publish(
            (symbol_short!("c_dereg"), contract_id),
            (EVENT_VERSION, caller, env.ledger().sequence()),
        );
    }

    // ── Event Decoder ─────────────────────────────────────────────────────────

    /// Submit a decoded event to the on-chain ring buffer.
    /// Only the admin may call this.
    pub fn submit_event(env: Env, caller: Address, input: EventInput) {
        caller.require_auth();
        if input.function == Symbol::new(&env, "") {
            panic_with_error!(&env, Error::InvalidInput);
        }
        if env
            .storage()
            .instance()
            .get(&DataKey::Paused)
            .unwrap_or(false)
        {
            panic_with_error!(&env, Error::ContractPaused);
        }
        let admin: Address = env.storage().instance().get(&DataKey::Admin).unwrap();
        if caller != admin {
            panic_with_error!(&env, Error::Unauthorized);
        }

        let seq: u64 = env
            .storage()
            .instance()
            .get(&DataKey::EventSeq)
            .unwrap_or(0);
        let max: u32 = env
            .storage()
            .instance()
            .get(&DataKey::MaxEvents)
            .unwrap_or(DEFAULT_MAX_EVENTS);

        let slot = seq % (max as u64);
        let evicting = seq >= (max as u64);
        let evicted_seq = if evicting { seq - (max as u64) } else { seq };

        let event = DecodedEvent {
            seq,
            contract_id: input.contract_id.clone(),
            function: input.function.clone(),
            ledger: input.ledger,
            description: input.description.clone(),
            raw_topics: input.raw_topics,
            raw_data: input.raw_data,
        };
        let event_key = DataKey::EventLog(slot);
        env.storage().persistent().set(&event_key, &event);
        env.storage().persistent().extend_ttl(
            &event_key,
            EVENT_LIFETIME_THRESHOLD,
            EVENT_BUMP_AMOUNT,
        );
        env.storage().instance().set(&DataKey::EventSeq, &(seq + 1));
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_LIFETIME_THRESHOLD, INSTANCE_BUMP_AMOUNT);

        // Topics: (ev_sub, contract_id, function). Data: (version, seq, ledger).
        // See docs/EVENTS.md.
        env.events().publish(
            (
                symbol_short!("ev_sub"),
                input.contract_id.clone(),
                input.function.clone(),
            ),
            (EVENT_VERSION, seq, input.ledger),
        );
        if evicting {
            // Topics: (cap_hit,). Data: (version, evicted_seq, seq). See docs/EVENTS.md.
            env.events().publish(
                (symbol_short!("cap_hit"),),
                (EVENT_VERSION, evicted_seq, seq),
            );
        }
        // Topics: (decoded, contract_id, function). Data: (version, description).
        // See docs/EVENTS.md.
        env.events().publish(
            (symbol_short!("decoded"), input.contract_id, input.function),
            (EVENT_VERSION, input.description),
        );
    }

    /// Fetch a single decoded event by sequence number.
    /// Panics with `NotFound` if the sequence is outside the live ring window.
    /// Extends this slot's TTL to `EVENT_BUMP_AMOUNT` on read, so events under
    /// active indexer traffic don't lapse while still inside the ring window.
    pub fn get_event(env: Env, seq: u64) -> DecodedEvent {
        let max: u32 = env
            .storage()
            .instance()
            .get(&DataKey::MaxEvents)
            .unwrap_or(DEFAULT_MAX_EVENTS);
        let slot = seq % (max as u64);
        let event_key = DataKey::EventLog(slot);
        let stored: DecodedEvent = env
            .storage()
            .persistent()
            .get(&event_key)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotFound));
        // Verify the slot still holds the requested seq (not overwritten by ring wrap).
        if stored.seq != seq {
            panic_with_error!(&env, Error::NotFound);
        }
        env.storage().persistent().extend_ttl(
            &event_key,
            EVENT_LIFETIME_THRESHOLD,
            EVENT_BUMP_AMOUNT,
        );
        stored
    }

    /// Returns the number of events currently retained (≤ `max_events`).
    pub fn event_count(env: Env) -> u64 {
        let seq: u64 = env
            .storage()
            .instance()
            .get(&DataKey::EventSeq)
            .unwrap_or(0);
        let max: u32 = env
            .storage()
            .instance()
            .get(&DataKey::MaxEvents)
            .unwrap_or(DEFAULT_MAX_EVENTS);
        seq.min(max as u64)
    }

    /// Fetch a page of decoded events starting from `cursor`.
    ///
    /// # Pagination contract
    ///
    /// | Concept | Behaviour |
    /// |---|---|
    /// | **cursor** | The sequence number of the first event to include. The returned `Vec`
    ///   contains events whose `seq >= cursor`. Pass `0` to start from the
    ///   oldest retained event. |
    /// | **Maximum page size** | At most [`MAX_PAGE_SIZE`] events are returned per call. A
    ///   `limit` larger than [`MAX_PAGE_SIZE`] is silently clamped. |
    /// | **limit == 0** | Panics with [`Error::InvalidInput`] — a zero limit is not meaningful. |
    /// | **Detecting the end** | The caller knows the full set is exhausted when the returned
    ///   `Vec` contains fewer events than the requested (or clamped) limit.
    ///   The next call should use the `seq` of the **last returned event + 1**
    ///   as the new cursor. |
    /// | **Gaps** | Events evicted from the ring buffer are silently skipped. Consecutive
    ///   calls may see longer or shorter gaps as the ring wraps. |
    ///
    /// Skips events evicted from the ring buffer.
    pub fn get_events(env: Env, cursor: u64, limit: u32) -> Vec<DecodedEvent> {
        if limit == 0 {
            panic_with_error!(&env, Error::InvalidInput);
        }
        let clamped = limit.min(MAX_PAGE_SIZE);
        let total_seq: u64 = env
            .storage()
            .instance()
            .get(&DataKey::EventSeq)
            .unwrap_or(0);
        let max: u32 = env
            .storage()
            .instance()
            .get(&DataKey::MaxEvents)
            .unwrap_or(DEFAULT_MAX_EVENTS);
        let oldest = total_seq.saturating_sub(max as u64);
        let start = cursor.max(oldest);
        let mut out: Vec<DecodedEvent> = Vec::new(&env);
        let mut seq = start;
        while out.len() < clamped && seq < total_seq {
            let slot = seq % (max as u64);
            if let Some(ev) = env
                .storage()
                .persistent()
                .get::<DataKey, DecodedEvent>(&DataKey::EventLog(slot))
            {
                if ev.seq == seq {
                    out.push_back(ev);
                }
            }
            seq += 1;
        }
        out
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────////
#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{
        testutils::{
            storage::{Instance as _, Persistent as _},
            Address as _, Events as _, Ledger as _,
        },
        Env, IntoVal,
    };

    /// Returns the `(topics, data)` of every event published by `contract_id`, in order.
    fn events_for(
        env: &Env,
        contract_id: &Address,
    ) -> Vec<(Vec<soroban_sdk::Val>, soroban_sdk::Val)> {
        let mut out = Vec::new(env);
        for (id, topics, data) in env.events().all().iter() {
            if &id == contract_id {
                out.push_back((topics, data));
            }
        }
        out
    }

    /// Returns the `(topics, data)` of the last event published by `contract_id`.
    fn last_event(env: &Env, contract_id: &Address) -> (Vec<soroban_sdk::Val>, soroban_sdk::Val) {
        let events = events_for(env, contract_id);
        events
            .get(events.len() - 1)
            .expect("no event published by contract")
    }

    fn setup() -> (Env, ExplorerContractClient<'static>) {
        let env = Env::default();
        env.mock_all_auths();
        let id = env.register_contract(None, ExplorerContract);
        let client = ExplorerContractClient::new(&env, &id);
        (env, client)
    }

    fn make_input(env: &Env, cid: &BytesN<32>) -> EventInput {
        EventInput {
            contract_id: cid.clone(),
            function: symbol_short!("swap"),
            ledger: 100u32,
            description: String::from_str(env, "test"),
            raw_topics: Vec::new(env),
            raw_data: Bytes::new(env),
        }
    }

    fn make_meta(env: &Env, name: &str, registrant: &Address) -> ContractMeta {
        ContractMeta {
            version: 1,
            abi_version: 0,
            min_ledger: 0,
            name: String::from_str(env, name),
            description: String::from_str(env, "desc"),
            functions: Vec::new(env),
            registered_by: registrant.clone(),
        }
    }

    // ── Basic init + register ─────────────────────────────────────────────────

    #[test]
    fn test_init_and_register() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        client.init(&admin, &0u32);

        let cid: BytesN<32> = BytesN::from_array(&env, &[1u8; 32]);
        client.register_contract(&admin, &cid, &make_meta(&env, "StellarSwap", &admin));
        let fetched = client.get_contract(&cid);
        assert_eq!(fetched.name, String::from_str(&env, "StellarSwap"));
    }

    #[test]
    #[should_panic]
    fn test_register_unauthorized() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        let stranger = Address::generate(&env);
        client.init(&admin, &0u32);

        let cid: BytesN<32> = BytesN::from_array(&env, &[50u8; 32]);
        client.register_contract(
            &stranger,
            &cid,
            &make_meta(&env, "UnauthorizedReg", &stranger),
        );
    }

    #[test]
    fn test_submit_and_get_event() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        client.init(&admin, &0u32);

        let cid: BytesN<32> = BytesN::from_array(&env, &[2u8; 32]);
        let input = EventInput {
            contract_id: cid.clone(),
            function: symbol_short!("swap"),
            ledger: 4521983u32,
            description: String::from_str(&env, "Address GABC... swapped 100 USDC"),
            raw_topics: Vec::new(&env),
            raw_data: Bytes::new(&env),
        };
        client.submit_event(&admin, &input);

        assert_eq!(client.event_count(), 1u64);
        let ev = client.get_event(&0u64);
        assert_eq!(ev.ledger, 4521983u32);
    }

    #[test]
    fn test_cursor_pagination() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        client.init(&admin, &0u32);

        let cid: BytesN<32> = BytesN::from_array(&env, &[3u8; 32]);
        let base = make_input(&env, &cid);

        for _ in 0..5 {
            client.submit_event(&admin, &base);
        }
        assert_eq!(client.event_count(), 5u64);

        let page1 = client.get_events(&0u64, &2u32);
        assert_eq!(page1.len(), 2);
        assert_eq!(page1.get(0).unwrap().seq, 0);
        assert_eq!(page1.get(1).unwrap().seq, 1);

        let page2 = client.get_events(&2u64, &2u32);
        assert_eq!(page2.len(), 2);
        assert_eq!(page2.get(0).unwrap().seq, 2);

        let page3 = client.get_events(&4u64, &2u32);
        assert_eq!(page3.len(), 1);
        assert_eq!(page3.get(0).unwrap().seq, 4);

        let empty = client.get_events(&10u64, &5u32);
        assert_eq!(empty.len(), 0);
    }

    #[test]
    #[should_panic]
    fn test_double_init_panics() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        client.init(&admin, &0u32);
        client.init(&admin, &0u32);
    }

    // ── Ring buffer ───────────────────────────────────────────────────────────

    #[test]
    fn test_ring_buffer_wraps_correctly() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        client.init(&admin, &5u32);
        let cid: BytesN<32> = BytesN::from_array(&env, &[10u8; 32]);
        let base = make_input(&env, &cid);

        for _ in 0..5 {
            client.submit_event(&admin, &base);
        }
        assert_eq!(client.event_count(), 5u64);

        for _ in 0..10 {
            client.submit_event(&admin, &base);
        }
        assert_eq!(client.event_count(), 5u64);

        let evs = client.get_events(&0u64, &20u32);
        assert_eq!(evs.len(), 5);
        assert_eq!(evs.get(0).unwrap().seq, 10);
        assert_eq!(evs.get(4).unwrap().seq, 14);
    }

    #[test]
    fn test_storage_utilisation() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        client.init(&admin, &1000u32);
        let (cur, max) = client.storage_utilisation();
        assert_eq!(cur, 0u64);
        assert_eq!(max, 1000u32);
    }

    // ── set_max_events ────────────────────────────────────────────────────────

    #[test]
    #[should_panic]
    fn test_set_max_events_below_floor_rejected() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        client.init(&admin, &0u32);
        client.set_max_events(&admin, &999u32);
    }

    #[test]
    fn test_set_max_events_accepted() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        client.init(&admin, &0u32);
        client.set_max_events(&admin, &1000u32);
        let (_, max) = client.storage_utilisation();
        assert_eq!(max, 1000u32);
    }

    // ── Diagnostic events (#275) ──────────────────────────────────────────────

    #[test]
    fn test_register_emits_c_reg_event() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        client.init(&admin, &0u32);

        let cid: BytesN<32> = BytesN::from_array(&env, &[20u8; 32]);
        let meta = make_meta(&env, "TestDex", &admin);
        client.register_contract(&admin, &cid, &meta);

        let (topics, data) = last_event(&env, &client.address);
        assert_eq!(topics, (symbol_short!("c_reg"), cid.clone()).into_val(&env));
        let decoded: (u32, Address, u32, u32, String) = data.into_val(&env);
        assert_eq!(
            decoded,
            (
                EVENT_VERSION,
                admin.clone(),
                meta.version,
                env.ledger().sequence(),
                meta.name.clone(),
            )
        );
    }

    #[test]
    fn test_update_emits_c_abiu_and_c_upd_events() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        client.init(&admin, &0u32);

        let cid: BytesN<32> = BytesN::from_array(&env, &[21u8; 32]);
        let meta_v0 = make_meta(&env, "Dex", &admin);
        client.register_contract(&admin, &cid, &meta_v0);

        let meta_v1 = ContractMeta {
            version: 2,
            abi_version: 1, // must be existing (0) + 1
            ..meta_v0
        };
        client.update_contract(&admin, &cid, &meta_v1);

        let events = events_for(&env, &client.address);
        let (abiu_topics, abiu_data) = events.get(events.len() - 2).unwrap();
        assert_eq!(
            abiu_topics,
            (symbol_short!("c_abiu"), cid.clone()).into_val(&env)
        );
        let abiu_decoded: (u32, u32, u32, u32) = abiu_data.into_val(&env);
        assert_eq!(
            abiu_decoded,
            (EVENT_VERSION, 0u32, 1u32, env.ledger().sequence())
        );

        let (upd_topics, upd_data) = events.get(events.len() - 1).unwrap();
        assert_eq!(upd_topics, (symbol_short!("c_upd"), cid).into_val(&env));
        let upd_decoded: (u32, Address, u32, u32, u32) = upd_data.into_val(&env);
        assert_eq!(
            upd_decoded,
            (EVENT_VERSION, admin, 1u32, 2u32, env.ledger().sequence())
        );
    }

    #[test]
    fn test_update_contract_by_owner() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        client.init(&admin, &0u32);

        let owner = Address::generate(&env);
        let cid: BytesN<32> = BytesN::from_array(&env, &[25u8; 32]);
        let meta_v0 = make_meta(&env, "MyContract", &owner);
        client.register_contract(&owner, &cid, &meta_v0);

        let meta_v1 = ContractMeta {
            version: 2,
            abi_version: 1, // must be existing (0) + 1
            ..meta_v0
        };
        client.update_contract(&owner, &cid, &meta_v1);

        let updated = client.get_contract(&cid);
        assert_eq!(updated.version, 2);
        assert_eq!(updated.abi_version, 1);
    }

    #[test]
    fn test_submit_emits_ev_sub_and_decoded_events() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        client.init(&admin, &0u32);

        let cid: BytesN<32> = BytesN::from_array(&env, &[22u8; 32]);
        let input = make_input(&env, &cid);
        client.submit_event(&admin, &input);

        let events = events_for(&env, &client.address);
        assert_eq!(events.len(), 2);

        let (ev_sub_topics, ev_sub_data) = events.get(0).unwrap();
        assert_eq!(
            ev_sub_topics,
            (symbol_short!("ev_sub"), cid.clone(), input.function.clone()).into_val(&env)
        );
        let ev_sub_decoded: (u32, u64, u32) = ev_sub_data.into_val(&env);
        assert_eq!(ev_sub_decoded, (EVENT_VERSION, 0u64, input.ledger));

        let (decoded_topics, decoded_data) = events.get(1).unwrap();
        assert_eq!(
            decoded_topics,
            (symbol_short!("decoded"), cid, input.function).into_val(&env)
        );
        let decoded_decoded: (u32, String) = decoded_data.into_val(&env);
        assert_eq!(decoded_decoded, (EVENT_VERSION, input.description));
    }

    #[test]
    fn test_cap_hit_event_emitted_on_eviction() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        client.init(&admin, &5u32);
        let cid: BytesN<32> = BytesN::from_array(&env, &[23u8; 32]);
        let base = make_input(&env, &cid);

        for _ in 0..5 {
            client.submit_event(&admin, &base);
        }
        client.submit_event(&admin, &base);

        let events = events_for(&env, &client.address);
        let (cap_hit_topics, cap_hit_data) = events.get(events.len() - 2).unwrap();
        assert_eq!(cap_hit_topics, (symbol_short!("cap_hit"),).into_val(&env));
        let cap_hit_decoded: (u32, u64, u64) = cap_hit_data.into_val(&env);
        assert_eq!(cap_hit_decoded, (EVENT_VERSION, 0u64, 5u64));
    }

    #[test]
    #[should_panic]
    fn test_submit_event_rejects_empty_function_name() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        client.init(&admin, &0u32);

        let cid: BytesN<32> = BytesN::from_array(&env, &[24u8; 32]);
        let mut input = make_input(&env, &cid);
        input.function = Symbol::new(&env, "");
        client.submit_event(&admin, &input);
    }

    // ── get_events pagination (#somzilla) ─────────────────────────────────────

    #[test]
    #[should_panic(expected = "Error(Contract, #6)")]
    fn test_get_events_rejects_zero_limit() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        client.init(&admin, &0u32);
        client.get_events(&0u64, &0u32);
    }

    #[test]
    fn test_get_events_clamps_limit_to_max_page_size() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        client.init(&admin, &5u32);
        let cid: BytesN<32> = BytesN::from_array(&env, &[25u8; 32]);
        let base = make_input(&env, &cid);
        // Fill all 5 slots.
        for _ in 0..5 {
            client.submit_event(&admin, &base);
        }
        // Request a limit far above MAX_PAGE_SIZE — only the available 5 are returned.
        let page = client.get_events(&0u64, &(MAX_PAGE_SIZE + 1000));
        assert_eq!(page.len(), 5);
    }

    #[test]
    fn test_get_events_paging_returns_each_event_exactly_once() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        client.init(&admin, &10u32);
        let cid: BytesN<32> = BytesN::from_array(&env, &[26u8; 32]);
        let base = make_input(&env, &cid);

        // Submit 10 events.
        for _ in 0..10 {
            client.submit_event(&admin, &base);
        }

        // Page through with a small limit.
        let mut cursor: u64 = 0;
        let mut all_seqs: Vec<u64> = Vec::new(&env);
        loop {
            let page = client.get_events(&cursor, &3u32);
            if page.is_empty() {
                break;
            }
            for ev in page.iter() {
                all_seqs.push_back(ev.seq);
            }
            // Advance cursor past the last returned event.
            cursor = page.get(page.len() - 1).unwrap().seq + 1;
        }

        assert_eq!(all_seqs.len(), 10);
        // Verify every seq from 0..10 appears exactly once.
        for expected in 0..10u64 {
            assert_eq!(all_seqs.iter().filter(|&s| s == expected).count(), 1);
        }
    }

    // ── ABI versioning (#272) ─────────────────────────────────────────────────

    #[test]
    fn test_register_sets_version_zero() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        client.init(&admin, &0u32);

        let cid: BytesN<32> = BytesN::from_array(&env, &[30u8; 32]);
        let meta = ContractMeta {
            abi_version: 99, // contract overwrites to 0
            ..make_meta(&env, "Test", &admin)
        };
        client.register_contract(&admin, &cid, &meta);

        let fetched = client.get_contract(&cid);
        assert_eq!(fetched.abi_version, 0);

        let v0 = client.get_contract_version(&cid, &0u32).unwrap();
        assert_eq!(v0.abi_version, 0);
        assert_eq!(v0.name, String::from_str(&env, "Test"));
    }

    #[test]
    fn test_sequential_updates_increment_abi_version() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        client.init(&admin, &0u32);

        let cid: BytesN<32> = BytesN::from_array(&env, &[31u8; 32]);
        let meta_v0 = make_meta(&env, "App", &admin);
        client.register_contract(&admin, &cid, &meta_v0);

        let meta_v1 = ContractMeta {
            abi_version: 1,
            ..meta_v0.clone()
        };
        client.update_contract(&admin, &cid, &meta_v1);
        assert_eq!(client.get_contract(&cid).abi_version, 1);

        let meta_v2 = ContractMeta {
            abi_version: 2,
            ..meta_v0
        };
        client.update_contract(&admin, &cid, &meta_v2);
        assert_eq!(client.get_contract(&cid).abi_version, 2);

        assert!(client.get_contract_version(&cid, &0u32).is_some());
        assert!(client.get_contract_version(&cid, &1u32).is_some());
        assert!(client.get_contract_version(&cid, &2u32).is_some());
        assert!(client.get_contract_version(&cid, &3u32).is_none());
    }

    #[test]
    #[should_panic]
    fn test_stale_write_rejected() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        client.init(&admin, &0u32);

        let cid: BytesN<32> = BytesN::from_array(&env, &[32u8; 32]);
        let meta_v0 = make_meta(&env, "X", &admin);
        client.register_contract(&admin, &cid, &meta_v0);

        let meta_v1 = ContractMeta {
            abi_version: 1,
            ..meta_v0.clone()
        };
        client.update_contract(&admin, &cid, &meta_v1);

        // abi_version 1 again — should panic (expected 2)
        let meta_stale = ContractMeta {
            abi_version: 1,
            ..meta_v0
        };
        client.update_contract(&admin, &cid, &meta_stale);
    }

    #[test]
    fn test_get_contract_not_found() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        client.init(&admin, &0u32);

        let cid: BytesN<32> = BytesN::from_array(&env, &[33u8; 32]);
        assert_eq!(
            client.try_get_contract(&cid),
            Err(Ok(crate::Error::NotFound))
        );
    }

    #[test]
    fn test_get_latest_contract_returns_none_for_missing() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        client.init(&admin, &0u32);

        let cid: BytesN<32> = BytesN::from_array(&env, &[33u8; 32]);
        assert!(client.get_latest_contract(&cid).is_none());
        assert!(client.get_contract_version(&cid, &0u32).is_none());
    }

    // ── Deregistration (#271) ─────────────────────────────────────────────────

    #[test]
    fn test_admin_deregisters_contract() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        client.init(&admin, &0u32);

        let cid: BytesN<32> = BytesN::from_array(&env, &[40u8; 32]);
        client.register_contract(&admin, &cid, &make_meta(&env, "ToRemove", &admin));
        assert!(client.get_latest_contract(&cid).is_some());

        client.deregister_contract(&admin, &cid);
        assert!(client.get_latest_contract(&cid).is_none());
    }

    #[test]
    fn test_registrant_deregisters_contract() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        client.init(&admin, &0u32);

        let registrant = Address::generate(&env);
        let cid: BytesN<32> = BytesN::from_array(&env, &[41u8; 32]);
        client.register_contract(&admin, &cid, &make_meta(&env, "RegOwned", &registrant));
        client.deregister_contract(&registrant, &cid);
        assert!(client.get_latest_contract(&cid).is_none());
    }

    #[test]
    #[should_panic]
    fn test_stranger_cannot_deregister() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        client.init(&admin, &0u32);

        let registrant = Address::generate(&env);
        let stranger = Address::generate(&env);
        let cid: BytesN<32> = BytesN::from_array(&env, &[42u8; 32]);
        client.register_contract(&admin, &cid, &make_meta(&env, "Secure", &registrant));
        client.deregister_contract(&stranger, &cid);
    }

    #[test]
    #[should_panic]
    fn test_deregister_missing_id_panics() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        client.init(&admin, &0u32);

        let cid: BytesN<32> = BytesN::from_array(&env, &[99u8; 32]);
        client.deregister_contract(&admin, &cid);
    }

    #[test]
    fn test_deregister_emits_c_dereg_event() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        client.init(&admin, &0u32);

        let cid: BytesN<32> = BytesN::from_array(&env, &[43u8; 32]);
        client.register_contract(&admin, &cid, &make_meta(&env, "EventTest", &admin));
        client.deregister_contract(&admin, &cid);

        let (topics, data) = last_event(&env, &client.address);
        assert_eq!(topics, (symbol_short!("c_dereg"), cid).into_val(&env));
        let decoded: (u32, Address, u32) = data.into_val(&env);
        assert_eq!(decoded, (EVENT_VERSION, admin, env.ledger().sequence()));
    }

    // ── transfer_admin / accept_admin / cancel_admin_transfer ───────────────────

    #[test]
    fn test_transfer_admin_emits_adm_nom_event() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        let new_admin = Address::generate(&env);
        client.init(&admin, &0u32);
        client.transfer_admin(&admin, &new_admin);

        let (topics, data) = last_event(&env, &client.address);
        assert_eq!(topics, (symbol_short!("adm_nom"), admin).into_val(&env));
        let decoded: (u32, Address) = data.into_val(&env);
        assert_eq!(decoded, (EVENT_VERSION, new_admin));
    }

    #[test]
    fn test_admin_retains_control_after_nomination() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        let new_admin = Address::generate(&env);
        client.init(&admin, &0u32);
        client.transfer_admin(&admin, &new_admin);

        // The nomination alone must not change who is in control.
        let cid: BytesN<32> = BytesN::from_array(&env, &[9u8; 32]);
        client.submit_event(
            &admin,
            &EventInput {
                contract_id: cid,
                function: symbol_short!("ping"),
                ledger: 1u32,
                description: String::from_str(&env, "old admin still in control"),
                raw_topics: Vec::new(&env),
                raw_data: Bytes::new(&env),
            },
        );
        assert_eq!(client.event_count(), 1u64);
    }

    #[test]
    #[should_panic]
    fn test_nominee_cannot_act_before_accepting() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        let new_admin = Address::generate(&env);
        client.init(&admin, &0u32);
        client.transfer_admin(&admin, &new_admin);

        let cid: BytesN<32> = BytesN::from_array(&env, &[10u8; 32]);
        client.submit_event(
            &new_admin,
            &EventInput {
                contract_id: cid,
                function: symbol_short!("ping"),
                ledger: 1u32,
                description: String::from_str(&env, "nominee jumps the gun"),
                raw_topics: Vec::new(&env),
                raw_data: Bytes::new(&env),
            },
        );
    }

    #[test]
    #[should_panic]
    fn test_transfer_admin_unauthorized() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        let attacker = Address::generate(&env);
        let new_admin = Address::generate(&env);
        client.init(&admin, &0u32);
        client.transfer_admin(&attacker, &new_admin);
    }

    #[test]
    fn test_accept_admin_emits_adm_acc_event() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        let new_admin = Address::generate(&env);
        client.init(&admin, &0u32);
        client.transfer_admin(&admin, &new_admin);
        client.accept_admin(&new_admin);

        let (topics, data) = last_event(&env, &client.address);
        assert_eq!(topics, (symbol_short!("adm_acc"), new_admin).into_val(&env));
        let decoded: (u32,) = data.into_val(&env);
        assert_eq!(decoded, (EVENT_VERSION,));
    }

    #[test]
    fn test_accept_admin_promotes_new_admin() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        let new_admin = Address::generate(&env);
        client.init(&admin, &0u32);
        client.transfer_admin(&admin, &new_admin);
        client.accept_admin(&new_admin);

        let cid: BytesN<32> = BytesN::from_array(&env, &[11u8; 32]);
        client.submit_event(
            &new_admin,
            &EventInput {
                contract_id: cid,
                function: symbol_short!("ping"),
                ledger: 1u32,
                description: String::from_str(&env, "new admin test"),
                raw_topics: Vec::new(&env),
                raw_data: Bytes::new(&env),
            },
        );
        assert_eq!(client.event_count(), 1u64);
    }

    #[test]
    #[should_panic]
    fn test_old_admin_loses_access_after_accept() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        let new_admin = Address::generate(&env);
        client.init(&admin, &0u32);
        client.transfer_admin(&admin, &new_admin);
        client.accept_admin(&new_admin);

        let cid: BytesN<32> = BytesN::from_array(&env, &[12u8; 32]);
        client.submit_event(
            &admin,
            &EventInput {
                contract_id: cid,
                function: symbol_short!("ping"),
                ledger: 1u32,
                description: String::from_str(&env, "stale admin attempt"),
                raw_topics: Vec::new(&env),
                raw_data: Bytes::new(&env),
            },
        );
    }

    #[test]
    #[should_panic]
    fn test_only_pending_admin_can_accept() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        let new_admin = Address::generate(&env);
        let attacker = Address::generate(&env);
        client.init(&admin, &0u32);
        client.transfer_admin(&admin, &new_admin);
        client.accept_admin(&attacker);
    }

    #[test]
    #[should_panic]
    fn test_accept_admin_without_pending_panics() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        let stranger = Address::generate(&env);
        client.init(&admin, &0u32);
        client.accept_admin(&stranger);
    }

    #[test]
    fn test_cancel_admin_transfer_emits_adm_cncl_event() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        let new_admin = Address::generate(&env);
        client.init(&admin, &0u32);
        client.transfer_admin(&admin, &new_admin);
        client.cancel_admin_transfer(&admin);

        let (topics, data) = last_event(&env, &client.address);
        assert_eq!(topics, (symbol_short!("adm_cncl"), admin).into_val(&env));
        let decoded: (u32,) = data.into_val(&env);
        assert_eq!(decoded, (EVENT_VERSION,));
    }

    #[test]
    fn test_current_admin_can_cancel_pending_transfer() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        let new_admin = Address::generate(&env);
        client.init(&admin, &0u32);
        client.transfer_admin(&admin, &new_admin);
        client.cancel_admin_transfer(&admin);

        // Old admin is still in control...
        let cid: BytesN<32> = BytesN::from_array(&env, &[13u8; 32]);
        client.submit_event(
            &admin,
            &EventInput {
                contract_id: cid,
                function: symbol_short!("ping"),
                ledger: 1u32,
                description: String::from_str(&env, "admin after cancel"),
                raw_topics: Vec::new(&env),
                raw_data: Bytes::new(&env),
            },
        );
        assert_eq!(client.event_count(), 1u64);
    }

    #[test]
    #[should_panic]
    fn test_accept_admin_after_cancel_panics() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        let new_admin = Address::generate(&env);
        client.init(&admin, &0u32);
        client.transfer_admin(&admin, &new_admin);
        client.cancel_admin_transfer(&admin);
        client.accept_admin(&new_admin);
    }

    #[test]
    #[should_panic]
    fn test_cancel_admin_transfer_unauthorized() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        let new_admin = Address::generate(&env);
        let attacker = Address::generate(&env);
        client.init(&admin, &0u32);
        client.transfer_admin(&admin, &new_admin);
        client.cancel_admin_transfer(&attacker);
    }

    #[test]
    #[should_panic]
    fn test_cancel_admin_transfer_without_pending_panics() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        client.init(&admin, &0u32);
        client.cancel_admin_transfer(&admin);
    }

    #[test]
    fn test_transfer_admin_to_self_then_accept_is_noop() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        client.init(&admin, &0u32);
        client.transfer_admin(&admin, &admin);
        client.accept_admin(&admin);

        let cid: BytesN<32> = BytesN::from_array(&env, &[14u8; 32]);
        client.submit_event(
            &admin,
            &EventInput {
                contract_id: cid,
                function: symbol_short!("ping"),
                ledger: 1u32,
                description: String::from_str(&env, "self transfer test"),
                raw_topics: Vec::new(&env),
                raw_data: Bytes::new(&env),
            },
        );
        assert_eq!(client.event_count(), 1u64);
    }

    #[test]
    fn test_pause_and_unpause_emit_events() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        client.init(&admin, &0u32);

        client.pause(&admin);
        let (paused_topics, paused_data) = last_event(&env, &client.address);
        assert_eq!(paused_topics, (symbol_short!("paused"),).into_val(&env));
        let paused_decoded: (u32,) = paused_data.into_val(&env);
        assert_eq!(paused_decoded, (EVENT_VERSION,));

        client.unpause(&admin);
        let (unpaused_topics, unpaused_data) = last_event(&env, &client.address);
        assert_eq!(unpaused_topics, (symbol_short!("unpaused"),).into_val(&env));
        let unpaused_decoded: (u32,) = unpaused_data.into_val(&env);
        assert_eq!(unpaused_decoded, (EVENT_VERSION,));
    }

    // ── upgrade ├──────────────────────────────────────────────────────────────

    #[test]
    fn test_admin_can_upgrade() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        client.init(&admin, &0u32);

        let hash = BytesN::from_array(&env, &[7u8; 32]);
        client.upgrade(&admin, &hash);
    }

    #[test]
    #[should_panic]
    fn test_non_admin_cannot_upgrade() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        let stranger = Address::generate(&env);
        client.init(&admin, &0u32);

        let hash = BytesN::from_array(&env, &[7u8; 32]);
        client.upgrade(&stranger, &hash);
    }

    #[test]
    fn test_upgrade_emits_event() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        client.init(&admin, &0u32);

        let hash = BytesN::from_array(&env, &[9u8; 32]);
        let before = env.events().all().len();
        client.upgrade(&admin, &hash);
        assert!(env.events().all().len() > before);
    }

    #[test]
    fn test_admin_can_upgrade_after_transfer() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        let new_admin = Address::generate(&env);
        client.init(&admin, &0u32);
        client.transfer_admin(&admin, &new_admin);
        client.accept_admin(&new_admin);

        let hash = BytesN::from_array(&env, &[11u8; 32]);
        client.upgrade(&new_admin, &hash);
    }

    #[test]
    #[should_panic]
    fn test_old_admin_cannot_upgrade_after_transfer() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        let new_admin = Address::generate(&env);
        client.init(&admin, &0u32);
        client.transfer_admin(&admin, &new_admin);
        client.accept_admin(&new_admin);

        let hash = BytesN::from_array(&env, &[12u8; 32]);
        client.upgrade(&admin, &hash);
    }

    #[test]
    #[should_panic]
    fn test_upgrade_while_paused() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        client.init(&admin, &0u32);
        client.pause(&admin);

        let hash = BytesN::from_array(&env, &[13u8; 32]);
        client.upgrade(&admin, &hash);
    }

    // ── Storage TTL (#6) ──────────────────────────────────────────────────────

    #[test]
    fn test_init_extends_instance_ttl_to_bump_amount() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        client.init(&admin, &0u32);

        let ttl = env.as_contract(&client.address, || env.storage().instance().get_ttl());
        assert_eq!(ttl, INSTANCE_BUMP_AMOUNT);
    }

    #[test]
    fn test_instance_data_survives_past_min_floor_after_state_changing_call() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        client.init(&admin, &0u32);

        // Advance past the network's minimum persistent-entry floor (4096
        // ledgers). Without `extend_ttl` in `init`, admin/pause/config would
        // already be archived here and every call below would panic.
        env.ledger().with_mut(|li| li.sequence_number += 20_000);

        client.pause(&admin);
        assert!(client.is_paused());

        // The state-changing call re-extends the TTL for the next window.
        let ttl = env.as_contract(&client.address, || env.storage().instance().get_ttl());
        assert_eq!(ttl, INSTANCE_BUMP_AMOUNT);
    }

    #[test]
    fn test_register_contract_extends_registry_entry_ttl() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        client.init(&admin, &0u32);

        let cid: BytesN<32> = BytesN::from_array(&env, &[60u8; 32]);
        client.register_contract(&admin, &cid, &make_meta(&env, "TTLTest", &admin));

        let key = DataKey::Contract(cid);
        let ttl = env.as_contract(&client.address, || env.storage().persistent().get_ttl(&key));
        assert_eq!(ttl, REGISTRY_BUMP_AMOUNT);
    }

    #[test]
    fn test_registry_entry_survives_past_min_floor_and_read_extends_it() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        client.init(&admin, &0u32);

        let cid: BytesN<32> = BytesN::from_array(&env, &[61u8; 32]);
        client.register_contract(&admin, &cid, &make_meta(&env, "TTLTest2", &admin));

        // Past the minimum persistent floor, still well inside the 90-day
        // registry horizon. Without the write-time `extend_ttl` in
        // `register_contract`, this entry would already be archived and
        // unreadable at this point.
        env.ledger().with_mut(|li| li.sequence_number += 20_000);

        let fetched = client.get_contract(&cid);
        assert_eq!(fetched.name, String::from_str(&env, "TTLTest2"));

        // The read path in `get_contract` re-extends the TTL back to the full horizon.
        let key = DataKey::Contract(cid);
        let ttl = env.as_contract(&client.address, || env.storage().persistent().get_ttl(&key));
        assert_eq!(ttl, REGISTRY_BUMP_AMOUNT);
    }

    #[test]
    fn test_submit_event_extends_event_slot_ttl() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        client.init(&admin, &0u32);

        let cid: BytesN<32> = BytesN::from_array(&env, &[62u8; 32]);
        client.submit_event(&admin, &make_input(&env, &cid));

        let key = DataKey::EventLog(0);
        let ttl = env.as_contract(&client.address, || env.storage().persistent().get_ttl(&key));
        assert_eq!(ttl, EVENT_BUMP_AMOUNT);
    }

    #[test]
    fn test_event_survives_past_min_floor_and_get_event_extends_it() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        client.init(&admin, &0u32);

        let cid: BytesN<32> = BytesN::from_array(&env, &[63u8; 32]);
        client.submit_event(&admin, &make_input(&env, &cid));

        // Past the minimum persistent floor, still inside the 7-day event horizon.
        env.ledger().with_mut(|li| li.sequence_number += 20_000);

        let ev = client.get_event(&0u64);
        assert_eq!(ev.contract_id, cid);

        let key = DataKey::EventLog(0);
        let ttl = env.as_contract(&client.address, || env.storage().persistent().get_ttl(&key));
        assert_eq!(ttl, EVENT_BUMP_AMOUNT);
    }
}
