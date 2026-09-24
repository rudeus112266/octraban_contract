# Interface Reference — `explorer` & `ticket`

A complete reference of the public API both contracts expose, so downstream
repos (`octraban_backend`, `octraban_frontend`) can integrate without reading
the Rust source. For event topics/payloads specifically, see
[`docs/EVENTS.md`](./EVENTS.md) — this document covers function signatures,
types, and errors.

## Deployed testnet contract IDs

See [`DEPLOYMENTS.md`](../DEPLOYMENTS.md) for the full deployment record
(network, RPC, deployer account). Current addresses:

| Contract | Contract ID | Explorer |
|---|---|---|
| `explorer` | `CBKPNRQ4D3KTAAE7MMJ4HL6JNF2J2EBG2PSSRW4YHOMHTRHUU734CFWJ` | [view](https://stellar.expert/explorer/testnet/contract/CBKPNRQ4D3KTAAE7MMJ4HL6JNF2J2EBG2PSSRW4YHOMHTRHUU734CFWJ) |
| `ticket` | `CDX3V6OE72KUIEEJTBLFCQZFXZCAKOYWYXK2KPRM57M6FLZFAVUSVL42` | [view](https://stellar.expert/explorer/testnet/contract/CDX3V6OE72KUIEEJTBLFCQZFXZCAKOYWYXK2KPRM57M6FLZFAVUSVL42) |

---

## `explorer` (`explorer/src/lib.rs`)

### Types

```rust
struct ContractMeta {
    version: u32,             // caller-supplied schema version
    abi_version: u32,         // contract-managed; 0 on register, +1 per update_contract
    min_ledger: u32,          // ledger sequence this ABI version was written at
    name: String,
    description: String,
    functions: Vec<FunctionAbi>,
    registered_by: Address,
}

struct FunctionAbi {
    name: Symbol,
    description: String,
    params: Vec<ParamDef>,
}

struct ParamDef {
    name: Symbol,
    kind: Symbol,
}

struct DecodedEvent {
    seq: u64,
    contract_id: BytesN<32>,
    function: Symbol,
    ledger: u32,
    description: String,
    raw_topics: Vec<String>,
    raw_data: Bytes,
}

struct EventInput {          // submit_event argument; same shape as DecodedEvent minus `seq`
    contract_id: BytesN<32>,
    function: Symbol,
    ledger: u32,
    description: String,
    raw_topics: Vec<String>,
    raw_data: Bytes,
}
```

### `ContractMeta` field constraints

`register_contract` and `update_contract` validate the caller-supplied `meta`
before writing it to persistent storage. Metadata that violates any of the
bounds below is rejected with `Error::InvalidInput` (value 6); nothing is
written. The same constraints apply to both entry points, so every registered
or updated entry satisfies them.

| Field | Constraint |
|---|---|
| `name` | Non-empty; at most `MAX_NAME_LEN` (64) bytes |
| `description` | At most `MAX_DESCRIPTION_LEN` (1,024) bytes; may be empty |
| `functions` | At most `MAX_FUNCTIONS` (64) entries |
| `functions[i].name` | Non-empty `Symbol` |
| `functions[i].description` | At most `MAX_DESCRIPTION_LEN` (1,024) bytes; may be empty |
| `functions[i].params` | At most `MAX_PARAMS` (32) entries |
| `functions[i].params[j].name` | Non-empty `Symbol` |
| `functions[i].params[j].kind` | Non-empty `Symbol` |

Lengths are measured in bytes of the UTF-8 encoding. `version`, `abi_version`,
`min_ledger`, and `registered_by` are contract-managed and are not validated
against caller input.

### Errors (`Error` enum)

| Value | Variant | Meaning |
|---|---|---|
| 1 | `NotFound` | Requested contract/version/event does not exist, or `accept_admin`/`cancel_admin_transfer` called with no pending admin nomination |
| 2 | `Unauthorized` | Caller is not the admin (or, where applicable, the registrant) |
| 3 | `AlreadyExists` | `init` called twice, or `register_contract` called with an already-registered `contract_id` |
| 4 | `BelowFloor` | `set_max_events` called with `new_max < MIN_MAX_EVENTS` (1,000) |
| 5 | `ContractPaused` | State-changing call attempted while the contract is paused |
| 6 | `InvalidInput` | `submit_event` called with an empty `function` symbol; `get_events` called with `limit == 0`; or `register_contract`/`update_contract` called with `ContractMeta` that violates the field constraints above |
| 7 | `Unsupported` | Reserved; not currently returned by any entry point |

### Functions

| Function | Args | Returns | Access |
|---|---|---|---|
| `init` | `admin: Address, max_events: u32` | `()` | Anyone, once (panics `AlreadyExists` on re-init) |
| `transfer_admin` | `caller: Address, new_admin: Address` | `()` | Admin only; nominates `new_admin` (step 1 of 2) — does **not** change the active admin |
| `accept_admin` | `caller: Address` | `()` | Nominated address only; promotes `caller` to admin (step 2 of 2). Panics `NotFound` if no pending nomination |
| `cancel_admin_transfer` | `caller: Address` | `()` | Admin only; withdraws a pending nomination. Panics `NotFound` if no pending nomination |
| `set_max_events` | `caller: Address, new_max: u32` | `()` | Admin only |
| `storage_utilisation` | — | `(u64, u32)` — `(current_event_count, max_events)` | Read-only |
| `pause` | `caller: Address` | `()` | Admin only |
| `unpause` | `caller: Address` | `()` | Admin only |
| `upgrade` | `caller: Address, new_wasm_hash: BytesN<32>` | `()` | Admin only; panics `ContractPaused` if paused |
| `is_paused` | — | `bool` | Read-only |
| `register_contract` | `caller: Address, contract_id: BytesN<32>, meta: ContractMeta` | `()` | Admin only; panics `ContractPaused`/`AlreadyExists`/`InvalidInput` (invalid `meta`) |
| `update_contract` | `caller: Address, contract_id: BytesN<32>, meta: ContractMeta` | `()` | Admin or original registrant; `meta.abi_version` must equal `existing.abi_version + 1`; panics `InvalidInput` (invalid `meta`) |
| `get_contract` | `contract_id: BytesN<32>` | `Result<ContractMeta, Error>` | Read-only |
| `get_contract_version` | `contract_id: BytesN<32>, abi_version: u32` | `Option<ContractMeta>` | Read-only |
| `get_latest_contract` | `contract_id: BytesN<32>` | `Option<ContractMeta>` | Read-only; alias for `get_contract` returning `Option` instead of `Result` |
| `deregister_contract` | `caller: Address, contract_id: BytesN<32>` | `()` | Admin or original registrant |
| `submit_event` | `caller: Address, input: EventInput` | `()` | Admin only; panics `InvalidInput` on empty `function` |
| `get_event` | `seq: u64` | `DecodedEvent` | Read-only; panics `NotFound` if `seq` fell outside the live ring window |
| `event_count` | — | `u64` | Read-only |
| `get_events` | `cursor: u64, limit: u32` | `Vec<DecodedEvent>` | Read-only; `limit` clamped to `MAX_PAGE_SIZE` (500); panics `InvalidInput` if `limit == 0` |

All admin-gated and mutating functions call `caller.require_auth()`.

---

## `ticket` (`ticket/src/lib.rs`)

### Types

```rust
enum TicketStatus { Valid, Used, Transferred }

struct Ticket {
    id: u64,
    owner: Address,
    event_name: String,
    status: TicketStatus,
    original_price: i128,
    max_resale_price: i128,
}
```

### Errors (`Error` enum)

| Value | Variant | Meaning |
|---|---|---|
| 1 | `NotFound` | Ticket id does not exist |
| 2 | `NotInitialized` | Called before `initialize` |
| 3 | `AlreadyInitialized` | `initialize` called twice |
| 4 | `NegativePrice` | `price`, `max_resale_price`, or `sale_price` is negative |
| 5 | `PriceOverflow` | Reserved; not currently returned by any entry point |
| 6 | `PriceExceedsCeiling` | `transfer_ticket` called with `sale_price > ticket.max_resale_price` |

### Functions

| Function | Args | Returns | Access |
|---|---|---|---|
| `initialize` | `admin: Address, event_name: String, max_tickets: u64, price: i128, max_resale_price: i128` | `Result<(), Error>` | Anyone, once |
| `mint_ticket` | `organizer: Address, recipient: Address` | `Result<u64, Error>` — new ticket id | Admin/organizer only; panics `"sold out"` at `max_tickets` |
| `transfer_ticket` | `from: Address, to: Address, ticket_id: u64, sale_price: i128` | `Result<(), Error>` | Ticket owner (`from`) only; panics if not owner or ticket not `Valid` |
| `verify_ticket` | `verifier: Address, ticket_id: u64` | `Result<bool, Error>` — `true` if newly marked `Used` | Admin/organizer only |
| `get_ticket` | `ticket_id: u64` | `Result<Ticket, Error>` | Read-only |
| `tickets_sold` | — | `u64` | Read-only |
| `upgrade` | `caller: Address, new_wasm_hash: BytesN<32>` | `Result<(), Error>` | Admin/organizer only |

Events published: `MINTED (recipient) -> ticket_id`, `TRANSFER (from, to) -> ticket_id`,
`VERIFIED () -> ticket_id`, `upgrade () -> new_wasm_hash`. These are not yet
covered by `docs/EVENTS.md`, which currently documents `explorer` only.
