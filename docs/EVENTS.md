# `explorer` — Event Reference

This is the single source of truth for every Soroban event the `explorer` contract
(`explorer/src/lib.rs`) publishes. It exists so the on-chain contract and the
backend indexer's `decodedEvent.schema.json` / `contractRegistry.schema.json` can be
validated against one definition instead of drifting independently.

## Versioning

Every event's data payload carries a leading `version: u32` field, taken from the
`EVENT_VERSION` constant in `explorer/src/lib.rs`. Consumers should switch on this
field (not the absence/presence of other fields) to handle payload changes across
future contract upgrades. The current version is **1**. Bump `EVENT_VERSION` and
update this table whenever a topic or payload shape below changes.

## Topic convention

Every event's first topic is a short symbol identifying the event kind. Events tied
to a specific registered contract carry that contract's id (`BytesN<32>`) as the
next topic; events tied to an admin action carry the acting `Address` instead.
Ring-buffer events additionally carry the decoded event's `function` symbol so
indexers can filter per-function without decoding the data payload.

## Events

| # | Symbol | Emitted by | Topics | Data (in order) |
|---|--------|-----------|--------|------------------|
| 1 | `adm_nom`   | `transfer_admin`     | `(adm_nom, caller: Address)` | `(version: u32, new_admin: Address)` |
| 2 | `adm_acc`   | `accept_admin`       | `(adm_acc, caller: Address)` | `(version: u32,)` |
| 3 | `adm_cncl`  | `cancel_admin_transfer` | `(adm_cncl, caller: Address)` | `(version: u32,)` |
| 4 | `paused`    | `pause`              | `(paused,)` | `(version: u32,)` |
| 5 | `unpaused`  | `unpause`            | `(unpaused,)` | `(version: u32,)` |
| 6 | `upgrade`   | `upgrade`            | `(upgrade,)` | `(version: u32, new_wasm_hash: BytesN<32>)` |
| 7 | `c_reg`     | `register_contract`  | `(c_reg, contract_id: BytesN<32>)` | `(version: u32, registered_by: Address, contract_version: u32, ledger: u32, name: String)` |
| 8 | `c_abiu`    | `update_contract`    | `(c_abiu, contract_id: BytesN<32>)` | `(version: u32, old_abi_version: u32, new_abi_version: u32, ledger: u32)` |
| 9 | `c_upd`     | `update_contract`    | `(c_upd, contract_id: BytesN<32>)` | `(version: u32, caller: Address, old_contract_version: u32, new_contract_version: u32, ledger: u32)` |
| 10 | `c_dereg`  | `deregister_contract`| `(c_dereg, contract_id: BytesN<32>)` | `(version: u32, caller: Address, ledger: u32)` |
| 11 | `ev_sub`   | `submit_event`       | `(ev_sub, contract_id: BytesN<32>, function: Symbol)` | `(version: u32, seq: u64, ledger: u32)` |
| 12 | `cap_hit`  | `submit_event` (only when the ring buffer evicts an entry) | `(cap_hit,)` | `(version: u32, evicted_seq: u64, seq: u64)` |
| 13 | `decoded` | `submit_event`       | `(decoded, contract_id: BytesN<32>, function: Symbol)` | `(version: u32, description: String)` |

Notes:
- `contract_version` (events 7, 9) is `ContractMeta.version`, the caller-supplied
  schema version — distinct from `abi_version`, which the contract manages
  internally and which is what `c_abiu` reports.
- `update_contract` always emits `c_abiu` immediately followed by `c_upd` in the
  same call, in that order.
- Admin handover is two-step (Issue #27): `transfer_admin` only nominates a new
  admin (`adm_nom`) — it does not change who is in control. The nominee must
  call `accept_admin` (`adm_acc`) to actually take over; until then the current
  admin can call `cancel_admin_transfer` (`adm_cncl`) to withdraw the nomination.
  This avoids permanently bricking the registry via a mistyped or uncontrolled
  `new_admin`.
- `submit_event` always emits `ev_sub`, then `cap_hit` only if the ring buffer is
  full and about to evict the oldest entry, then always `decoded` — in that order.
- Prior to this reference, `register_contract` additionally emitted a second,
  redundant `register` event carrying only the contract name; it has been folded
  into `c_reg`'s `name` field so each action now emits exactly one event per topic
  kind.

## Reconciling with the indexer

The backend indexer's schemas (`decodedEvent.schema.json`, `contractRegistry.schema.json`,
in the `octraban_backend` repo) should be checked against the table above whenever
either side changes:

- Every topic symbol and field name/order the indexer decodes must match a row here.
- If the indexer's schema disagrees with this table, one of the two is stale — fix
  the drift by updating this file and `EVENT_VERSION`/payloads together, then land
  a matching change in `octraban_backend`.
- This repo does not vendor the indexer's schema files, so cross-checking today is
  manual; coordinate the actual schema update as a follow-up PR against
  `octraban_backend`.
