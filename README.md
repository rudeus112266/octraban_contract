<div align="center">

# Octraban — Contracts

**Soroban smart contracts powering the Octraban explorer on Stellar.**

An on-chain contract registry & event ledger, plus a standalone event-ticketing contract — written in Rust for the Soroban VM.

[![Rust](https://img.shields.io/badge/Rust-1.8x-000000?logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![Soroban SDK](https://img.shields.io/badge/soroban--sdk-21-7D00FF?logo=stellar&logoColor=white)](https://soroban.stellar.org/)
[![Network](https://img.shields.io/badge/Testnet-live-brightgreen)](https://stellar.expert/explorer/testnet/contract/CBKPNRQ4D3KTAAE7MMJ4HL6JNF2J2EBG2PSSRW4YHOMHTRHUU734CFWJ)
[![License: MIT](https://img.shields.io/badge/License-MIT-green.svg)](./LICENSE)

</div>

---

## 🟢 Live on Testnet

Both contracts are **deployed and verifiable on the Stellar test network**:

| Contract | Crate | Contract ID | Stellar Explorer |
|----------|-------|-------------|------------------|
| **Explorer / registry** | `explorer` (`octraban-contract`) | `CBKPNRQ4D3KTAAE7MMJ4HL6JNF2J2EBG2PSSRW4YHOMHTRHUU734CFWJ` | [View ↗](https://stellar.expert/explorer/testnet/contract/CBKPNRQ4D3KTAAE7MMJ4HL6JNF2J2EBG2PSSRW4YHOMHTRHUU734CFWJ) |
| **Ticket** | `ticket` | `CDX3V6OE72KUIEEJTBLFCQZFXZCAKOYWYXK2KPRM57M6FLZFAVUSVL42` | [View ↗](https://stellar.expert/explorer/testnet/contract/CDX3V6OE72KUIEEJTBLFCQZFXZCAKOYWYXK2KPRM57M6FLZFAVUSVL42) |

> **Mainnet:** See [`DEPLOYMENTS.md`](./DEPLOYMENTS.md#mainnet) for mainnet prerequisites, procedure, and contract IDs once deployed.
>
> **Decision (Issue #8):** these contracts are **treated as upgradeable**.
> Onchain upgradeability is needed to avoid forced migrations from peer-deployed
> address changes fixed earlier, and from bugs or storage/tuning issues. Each
> contract exposes an admin-gated `upgrade` entry point that calls
> `update_current_contract_wasm`. Referrer/integrators should **pin** the
> deployed address and monitor its registry/support channels before upgrades.

> **Network:** `Test SDF Network ; September 2015` · **RPC:** `https://soroban-testnet.stellar.org`
> **Deployer:** `GDKQB6LSSCL6HPYTRG7HDQWNWWYMLJRI3F3R2EINFGULH2OUVV3E3GOG`

Full deployment details and a one-command redeploy live in [`DEPLOYMENTS.md`](./DEPLOYMENTS.md).

---

## 📋 Overview

This workspace contains two independent Soroban contracts:

- **`explorer/` — the Octraban registry & event ledger.** The on-chain backbone of the explorer: an admin-governed **contract metadata registry** (with versioning) and an append-only **decoded-event ring buffer** that the indexer and UI read from.
- **`ticket/` — an event-ticketing contract.** A self-contained example/utility contract for minting, transferring (with sale price), and verifying tickets on-chain.

Both are `no_std`, built against **`soroban-sdk 21`**.

> **Integrating from `octraban_backend` or `octraban_frontend`?** See
> [`docs/INTERFACE.md`](./docs/INTERFACE.md) for the full public interface —
> function signatures, argument/return types, custom types, and error codes
> for both contracts — and [`docs/EVENTS.md`](./docs/EVENTS.md) for event
> topics and payload shapes.

---

## 🧩 `explorer` — Registry & Event Ledger

### Administration & lifecycle
| Function | Description |
|---|---|
| `init(admin, max_events)` | Initialise the contract with an admin and the event-buffer capacity |
| `transfer_admin(caller, new_admin)` | Nominate a new admin (step 1 of 2) — does not change the active admin |
| `accept_admin(caller)` | Nominee accepts, becoming the active admin (step 2 of 2) |
| `cancel_admin_transfer(caller)` | Admin withdraws a pending nomination |
| `set_max_events(caller, new_max)` | Resize the event ring buffer |
| `pause(caller)` / `unpause(caller)` | Emergency freeze / resume of state-changing calls |
| `is_paused() -> bool` | Query pause state |
| `storage_utilisation() -> (u64, u32)` | Current event count and configured capacity |
| `upgrade(caller, new_wasm_hash)` | Admin-gated WASM upgrade |

### Contract registry (versioned)
| Function | Description |
|---|---|
| `register_contract(…)` | Register a contract's metadata/ABI |
| `update_contract(caller, contract_id, meta)` | Publish a new metadata version |
| `get_contract(contract_id) -> ContractMeta` | Fetch current metadata (errors if absent) |
| `get_contract_version(…)` | Fetch a specific historical version |
| `get_latest_contract(contract_id) -> Option<ContractMeta>` | Fetch latest metadata, if any |
| `deregister_contract(caller, contract_id)` | Remove a contract from the registry |

### Event ledger
| Function | Description |
|---|---|
| `submit_event(caller, input)` | Admin-only: append a decoded event to the ring buffer |
| `get_event(seq) -> DecodedEvent` | Read a single event by sequence number |
| `get_events(cursor, limit) -> Vec<DecodedEvent>` | Paginated event read |
| `event_count() -> u64` | Total events submitted |

**Safety properties:** admin-gated writes (`require_auth`), typed errors, pausability, input validation on submitted events, and a bounded ring buffer so storage never grows unbounded.

### Events

Every event `explorer` publishes — topics, payload shape, and version — is documented in [`docs/EVENTS.md`](./docs/EVENTS.md). Cross-reference it against the indexer's `decodedEvent.schema.json` / `contractRegistry.schema.json` in `octraban_backend` before changing either side.

### Storage TTL & retention (Issue #6)

Soroban archives an instance or persistent entry once its TTL (time-to-live, in ledgers) lapses; an archived entry is unreadable until it is explicitly restored. `explorer` calls `extend_ttl` on every write path (and on a handful of hot read paths) so its state stays live under normal traffic. Ledger close time is ~5s, so 1 day ≈ 17,280 ledgers (`LEDGERS_PER_DAY` in `explorer/src/lib.rs`).

| Storage | Entries | Bump horizon | Bumped on |
|---|---|---|---|
| Instance | admin, pause flag, `MaxEvents`, `EventSeq` | 30 days (`INSTANCE_BUMP_AMOUNT`) | Every state-changing call: `init`, `transfer_admin`, `accept_admin`, `cancel_admin_transfer`, `set_max_events`, `pause`, `unpause`, `upgrade`, `register_contract`, `update_contract`, `deregister_contract`, `submit_event` |
| Persistent — registry | `Contract`, `ContractVersion` | 90 days (`REGISTRY_BUMP_AMOUNT`) | Write (`register_contract`, `update_contract`) **and** read (`get_contract`, `get_contract_version`, `get_latest_contract`) |
| Persistent — event ring buffer | `EventLog` slots | 7 days (`EVENT_BUMP_AMOUNT`) | Write (`submit_event`) **and** single-item read (`get_event`) |

Each bump extends the TTL back out to the full horizon once the remaining TTL drops below `horizon - 1 day` (the `*_LIFETIME_THRESHOLD` constants) — this avoids paying to re-extend on every single call while still keeping a full day of margin.

**Why registry entries get a longer horizon than events:** registry metadata is written rarely and is meant to be queried indefinitely, so it's bumped generously on every read. Event-log slots are the ring buffer — bounded, and each slot is overwritten as soon as `seq` wraps back around to it (`submit_event`) — so they only need to survive long enough for the indexer to consume them before either being read or naturally evicted by wraparound; a shorter horizon is both sufficient and cheaper.

**Why the event ring buffer stays on persistent storage instead of `temporary`:** temporary entries are hard-deleted the instant their TTL hits zero, with no restoration path — that would silently drop event history the indexer hasn't caught up on yet if a bump is ever missed (e.g. no writes for an extended period). Persistent entries, by contrast, can be restored (see below) if `explorer`'s own `extend_ttl` calls ever lapse. `get_events` (the paginated read) intentionally does **not** bump every slot it touches, to avoid the per-call cost scaling with `limit`; `get_event` (single-item read) does.

**Restoring an archived entry:** if an entry is ever allowed to lapse (e.g. the contract goes untouched for longer than its bump horizon), Soroban requires an explicit on-chain restore before it can be read or written again — the contract's own transactions cannot "un-archive" it implicitly. Use the Stellar CLI against the relevant ledger key(s):

```bash
stellar contract restore \
  --id <EXPLORER_CONTRACT_ID> \
  --source <funded-identity> \
  --network testnet \
  --durability persistent   # or `instance` for the contract's instance entry
```

This submits a restoration op that pays the current archival-recovery fee and brings the entry back to a fresh (short) TTL — the next admin-gated call against it will then re-extend it via the paths above. See the [Stellar CLI docs](https://developers.stellar.org/docs/tools/cli/stellar-cli) for the full `contract restore` reference.

---

## 🎟️ `ticket` — Event Ticketing

| Function | Description |
|---|---|
| `initialize(…)` | Set up organizer, supply, and ticketing parameters |
| `mint_ticket(organizer, recipient) -> u64` | Mint a ticket to a recipient; returns the ticket id |
| `transfer_ticket(from, to, ticket_id, sale_price)` | Transfer ownership, recording sale price |
| `verify_ticket(verifier, ticket_id) -> bool` | Verify a ticket's validity at the gate |
| `get_ticket(ticket_id) -> Ticket` | Fetch ticket details (errors if absent) |
| `tickets_sold() -> u64` | Total tickets minted |
| `upgrade(caller, new_wasm_hash)` | Admin-gated WASM upgrade |

Includes a property-based test suite (`test.rs`).

---

## 📁 Layout

```
.
├── Cargo.toml            # workspace root — members: explorer, ticket
├── Cargo.lock
├── explorer/            # octraban-contract — registry & event ledger
│   └── src/lib.rs
├── ticket/              # ticket — event ticketing
│   ├── src/lib.rs
│   └── src/test.rs
├── docs/
│   ├── EVENTS.md        # explorer event topics, payloads, and versioning
│   └── INTERFACE.md     # full public interface: functions, types, errors
├── build-and-deploy.sh  # build → MVP-lower (wasm-opt) → deploy
├── DEPLOYMENTS.md        # live contract IDs + reproduction steps
├── LICENSE / NOTICE
```

---

## 🛠️ Building & Deploying

### Prerequisites
- **Rust** with a wasm target: `rustup target add wasm32-unknown-unknown`
- **[Stellar CLI](https://github.com/stellar/stellar-cli)**
- **[Binaryen](https://github.com/WebAssembly/binaryen/releases)** (`wasm-opt`)
- A funded testnet identity: `stellar keys generate octraban-deployer --network testnet --fund`

### One command
```bash
./build-and-deploy.sh            # builds, lowers to MVP wasm, deploys to testnet
```

### ⚠️ Important build note
These contracts pin **`soroban-sdk 21`**, whose on-chain VM rejects the WebAssembly `reference-types` and `multivalue` features. Modern Rust (≥ 1.82) emits those features into **every** wasm it builds — including the standard library — and `-C target-feature=-reference-types` does **not** reliably strip them.

The working pipeline is therefore **build normally, then lower with `wasm-opt`**:

```bash
cargo build --release --target wasm32-unknown-unknown --workspace

wasm-opt <in.wasm> -o <out.wasm> \
  --disable-reference-types --disable-multivalue \
  --enable-bulk-memory --enable-bulk-memory-opt \
  --enable-sign-ext --enable-mutable-globals -Oz

stellar contract deploy --wasm <out.wasm> --source octraban-deployer --network testnet
```

The retained features (`bulk-memory`, `sign-ext`, `mutable-globals`) are required because the contracts use `memory.copy`; only `reference-types` and `multivalue` are stripped. `build-and-deploy.sh` encapsulates all of this.

### Testing
`explorer` and `ticket` share a single Cargo workspace rooted at the repo root, so `build`, `test`, `clippy`, and `fmt` all run across both crates from one place:
```bash
cargo build --release --target wasm32-unknown-unknown --workspace   # both crates
cargo test --workspace                                              # both crates' test suites
cargo clippy --workspace --lib --bins
cargo fmt --all

cargo test -p ticket                                                 # a single crate
```

### Fuzzing
```bash
cargo install cargo-fuzz
cd ticket/fuzz && cargo +nightly fuzz run <target> -- -max_total_time=60
```
`cargo-fuzz` requires a **nightly** toolchain (`rustup toolchain install nightly`) because it builds with `-Z sanitizer=address`, a nightly-only flag. See [`ticket/fuzz/README.md`](./ticket/fuzz/README.md) for the list of targets, the invariant each one checks, and how regression seeds are organised.

---

## 🗺️ How it fits together

Octraban is split across three repositories:

- **octraban_contract** *(this repo)* — the Soroban contracts, deployed to testnet.
- **[octraban_backend](https://github.com/octraban/octraban_backend)** — API + indexer that reads on-chain data and serves it.
- **[octraban_frontend](https://github.com/octraban/octraban_frontend)** — the explorer & developer workspace UI.

---

## 📄 License

Released under the [MIT License](./LICENSE). "Soroban" refers to Stellar's smart-contract platform and is used here in that technical sense.
