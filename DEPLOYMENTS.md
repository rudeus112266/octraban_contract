# Octraban Contract Deployments

On-chain deployments of the Octraban Soroban contracts. Reproduce with
[`build-and-deploy.sh`](./build-and-deploy.sh).

## Testnet

Network passphrase: `Test SDF Network ; September 2015`
RPC: `https://soroban-testnet.stellar.org`

| Contract | Crate | Contract ID | Explorer |
|----------|-------|-------------|----------|
| Explorer | `explorer` (`octraban-contract`) | `CBKPNRQ4D3KTAAE7MMJ4HL6JNF2J2EBG2PSSRW4YHOMHTRHUU734CFWJ` | [view](https://stellar.expert/explorer/testnet/contract/CBKPNRQ4D3KTAAE7MMJ4HL6JNF2J2EBG2PSSRW4YHOMHTRHUU734CFWJ) |
| Ticket   | `ticket`   | `CDX3V6OE72KUIEEJTBLFCQZFXZCAKOYWYXK2KPRM57M6FLZFAVUSVL42` | [view](https://stellar.expert/explorer/testnet/contract/CDX3V6OE72KUIEEJTBLFCQZFXZCAKOYWYXK2KPRM57M6FLZFAVUSVL42) |

Deployer account: `GDKQB6LSSCL6HPYTRG7HDQWNWWYMLJRI3F3R2EINFGULH2OUVV3E3GOG`

## Build notes

These contracts pin `soroban-sdk 21`, whose VM rejects the WebAssembly
`reference-types` and `multivalue` features. Modern Rust (>= 1.82) emits those
features into every wasm it builds — including the standard library — and
`-C target-feature=-reference-types` does not reliably remove them.

The working pipeline is therefore **build normally, then lower with `wasm-opt`**:

```
cargo build --release --target wasm32-unknown-unknown --workspace
wasm-opt <in.wasm> -o <out.wasm> \
  --disable-reference-types --disable-multivalue \
  --enable-bulk-memory --enable-bulk-memory-opt \
  --enable-sign-ext --enable-mutable-globals -Oz
stellar contract deploy --wasm <out.wasm> --source <identity> --network testnet
```

The retained feature set (bulk-memory, sign-ext, mutable-globals) is required
because the contracts use `memory.copy`; only `reference-types` and `multivalue`
are stripped.

## Mainnet

**Not yet deployed.** When ready, follow the procedure below.

### Prerequisites

Before any mainnet deployment, ensure the following are in place:

| Prerequisite | Description |
|---|---|
| **Funded identity** | A Stellar CLI identity with a funded mainnet account holding sufficient XLM (≥ 50 XLM recommended to cover deployment fees). Create with: `stellar keys generate <name> --network mainnet` |
| **RPC configuration** | The Stellar CLI must be configured for the mainnet RPC endpoint. The default mainnet RPC is `https://soroban-rpc.stellar.org`. Verify with: `stellar network list` |
| **Passphrase configuration** | Mainnet network passphrase: `Public Global Stellar Network ; September 2015` |
| **Review / Audit gate** | All contract code must pass internal review. If applicable, a third-party audit must be completed and any findings resolved before deployment. |
| **Deployer account funded** | Send XLM to the deployer address from an exchange or wallet. Record the deployer address: |
| **Confirmation flag** | The script requires `--yes-i-mean-mainnet` as the third argument (see below). |

### Procedure

1. Complete all prerequisites above.
2. Run the deployment script with the confirmation flag:
   ```bash
   ./build-and-deploy.sh <identity> mainnet --yes-i-mean-mainnet
   ```
3. After successful deployment, update the table below with the returned contract IDs.

### Deployed Contracts

| Contract | Crate | Contract ID | Explorer |
|----------|-------|-------------|----------|
| Explorer | `explorer` (`octraban-contract`) | — | — |
| Ticket   | `ticket` | — | — |

> **Note:** Testnet entries are kept above. Mainnet entries will be filled after the first successful mainnet deployment.

## Upgrade procedure

Both contracts are **upgradeable on chain**. Treat the deployed testnet address as
the stable reference; patch releases are applied by WASM replacement rather than by
deploying a new contract and migrating client state.

Explorer upgrade entrypoint: `upgrade(caller, new_wasm_hash)`
Ticket upgrade entrypoint: `upgrade(caller, new_wasm_hash)`

Authorisation model
- The caller must be the current admin / organizer recorded in instance storage.
- The contract must not be paused.
- The call must authenticate with `require_auth` for the stored admin.
- Successful upgrades emit an `upgrade` event with the applied WASM hash.

Recommended steps
1. Prepare the WASM release and record its hash.
2. Notify referrers/integrators that pin the deployed address.
3. Only after the agreed-on upgrade window, the admin calls `upgrade(new_wasm_hash)`.
4. Inspect the emitted upgrade event and verify peer behavior.
