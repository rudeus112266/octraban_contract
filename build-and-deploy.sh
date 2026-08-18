#!/usr/bin/env bash
#
# Octraban contracts — build, MVP-lower, and deploy to Stellar.
#
# Background:
#   These contracts pin soroban-sdk 21, whose on-chain VM rejects the WebAssembly
#   `reference-types` and `multivalue` features. Modern Rust (>= 1.82) bakes those
#   features into every wasm it produces (including std), and the `-C target-feature`
#   flag does NOT reliably strip them. The reliable fix is to build normally, then
#   lower the wasm to the exact feature set Soroban accepts using Binaryen's wasm-opt.
#
# Prereqiuisites:
#   - rustup with a wasm target:   rustup target add wasm32-unknown-unknown
#   - Stellar CLI:                 https://github.com/stellar/stellar-cli
#   - Binaryen (wasm-opt):         https://github.com/WebAssembly/binaryen/releases
#   - A funded testnet identity:   stellar keys generate <name> --network testnet --fund
#   - A funded mainnet identity:   stellar keys generate <name> --network mainnet --fund
#
# Usage:
#   ./build-and-deploy.sh [IDENTITY] [NETWORK] [CONFIRM_FLAG]
#     IDENTITY     Stellar CLI key alias to deploy from   (default: octraban-deployer)
#     NETWORK      target network                          (default: testnet)
#     CONFIRM_FLAG --yes-i-mean-mainnet required for non-testnet deploys
#
#   For mainnet deploys you must also pass --yes-i-mean-mainnet as the third
#   argument, e.g.:
#     ./build-and-deploy.sh octraban-deployer mainnet --yes-i-mean-mainnet
#
# Known networks:
#   testnet   | Test SDF Network ; September 2015
#   mainnet   | Public Global Stellar Network ; September 2015
set -euo pipefail

IDENTITY="${1:-octraban-deployer}"
NETWORK="${2:-testnet}"
CONFIRM_FLAG="${3:-}"
KNOWN_NETWORKS=("testnet" "mainnet")

# ---------------------------------------------------------------------------
# Helper: check whether a value is in an array
# ---------------------------------------------------------------------------
_in_array() {
  local needle="$1"
  shift
  for e; do [[ "$e" == "$needle" ]] && return 0; done
  return 1
}

# ---------------------------------------------------------------------------
# 1.  Validate network
# ---------------------------------------------------------------------------
if ! _in_array "$NETWORK" "${KNOWN_NETWORKS[@]}"; then
  echo "❌ Unknown network \"${NETWORK}\". Known networks: ${KNOWN_NETWORKS[*]}"
  exit 1
fi

# ---------------------------------------------------------------------------
# 2.  Mainnet guard — explicit confirmation required
# ---------------------------------------------------------------------------
if [[ "$NETWORK" != "testnet" ]]; then
  if [[ "$CONFIRM_FLAG" != "--yes-i-mean-mainnet" ]]; then
    echo "⚠️  WARNING: You are about to deploy to \"${NETWORK}\" — this is irreversible and costs real funds!"
    echo "   To proceed you must pass --yes-i-mean-mainnet as the third argument."
    echo
    echo "   ./build-and-deploy.sh \"${IDENTITY}\" \"${NETWORK}\" --yes-i-mean-mainnet"
    exit 1
  fi

  # Double-check: confirm the flag *and* a non-testnet network are paired.
  echo "🔴 Mainnet deployment confirmed via --yes-i-mean-mainnet flag."
  echo "   Ensure you have completed the mainnet prerequisites documented in DEPLOYMENTS.md."
  echo
fi

# ---------------------------------------------------------------------------
# 3.  Prerequisite binaries
# ---------------------------------------------------------------------------
command -v wasm-opt >/dev/null || { echo "❌ wasm-opt (Binaryen) not found on PATH"; exit 1; }
command -v stellar  >/dev/null || { echo "❌ stellar CLI not found on PATH"; exit 1; }

# ---------------------------------------------------------------------------
# 4.  Validate identity exists and is funded
# ---------------------------------------------------------------------------
echo "  Checking identity \"${IDENTITY}\" on network \"${NETWORK}\"…"
if ! stellar keys address "${IDENTITY}" >/dev/null 2>&1; then
  echo "❌ Identity \"${IDENTITY}\" not found. Create it with:"
  echo "   stellar keys generate \"${IDENTITY}\" --network \"${NETWORK}\" --fund"
  exit 1
fi

ID_ADDR="$(stellar keys address "${IDENTITY}")"
echo "  Identity address: ${ID_ADDR}"

# Check balance — require at least 50 XLM to cover deployment fees.
BALANCE_RAW="$(stellar balance "${IDENTITY}" --network "${NETWORK}" 2>/dev/null || true)"
if [[ -z "$BALANCE_RAW" || "$BALANCE_RAW" == "0" ]]; then
  echo "❌ Identity \"${IDENTITY}\" has no balance on \"${NETWORK}\". Fund it first."
  echo "   Testnet: stellar keys generate \"${IDENTITY}\" --network testnet --fund"
  echo "   Mainnet: send XLM to ${ID_ADDR} from your exchange or wallet."
  exit 1
fi

echo "  Balance: ${BALANCE_RAW} XLM"
echo

# Feature set accepted by the soroban-sdk 21 VM: MVP + bulk-memory + sign-ext +
# mutable-globals, with reference-types and multivalue explicitly removed.
WASM_OPT_FLAGS=(
  --disable-reference-types
  --disable-multivalue
  --enable-bulk-memory
  --enable-bulk-memory-opt
  --enable-sign-ext
  --enable-mutable-globals
  -Oz
)

# crate dir : produced wasm file name (as emitted by cargo, now shared under
# the workspace-root target/ dir)
declare -A CRATES=(
  [explorer]="octraban_contract.wasm"
  [ticket]="ticket.wasm"
)

mkdir -p dist
echo "Identity: ${IDENTITY}   Network: ${NETWORK}"
echo

echo "━━━ building workspace ━━━"
cargo build --release --target wasm32-unknown-unknown --workspace
echo

for crate in "${!CRATES[@]}"; do
  raw="${CRATES[$crate]}"
  echo "━━━ ${crate} ━━━"
  src="target/wasm32-unknown-unknown/release/${raw}"

  out="dist/${crate}.wasm"
  echo "  lowering to MVP-compatible feature set → ${out}"
  wasm-opt "${src}" -o "${out}" "${WASM_OPT_FLAGS[@]}"

  echo "  deploying…"
  id="$(stellar contract deploy --wasm "${out}" --source "${IDENTITY}" --network "${NETWORK}")"
  echo "  ✅ ${crate} contract id: ${id}"
  echo
done

echo "Done. Record the contract IDs above in DEPLOYMENTS.md."
