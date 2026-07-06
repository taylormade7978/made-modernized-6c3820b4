#!/usr/bin/env bash
#
# Verify a committed smart-contract promotion gate.
#
# The MADE card tokens are ERC-1155 contracts. Promotion of a contract release
# to mainnet is gated on two attestations that must be committed to the repo
# *first* (so the gate is auditable in git history and enforced in CI, not by
# convention):
#
#   testnet — the release was deployed to a testnet and its post-deploy checks
#             passed  (contracts/attestations/testnet/<version>.json)
#   audit   — an independent third party audited the release and signed off
#             (contracts/attestations/audit/<version>.json)
#
# This script fails unless the requested attestation exists and asserts a
# passing status with the fields that make it meaningful. The mainnet-promotion
# workflow runs it as two separate, both-required gate jobs.
#
# Usage: verify-attestation.sh <testnet|audit> <version>
set -euo pipefail

KIND="${1:?usage: verify-attestation.sh <testnet|audit> <version>}"
VERSION="${2:?usage: verify-attestation.sh <testnet|audit> <version>}"

case "$KIND" in
  testnet | audit) ;;
  *)
    echo "verify-attestation: unknown gate kind '$KIND' (expected testnet|audit)" >&2
    exit 2
    ;;
esac

# Guard against path traversal in the (dispatch-supplied) version.
if ! printf '%s' "$VERSION" | grep -Eq '^[0-9A-Za-z][0-9A-Za-z._-]*$'; then
  echo "verify-attestation: invalid version '$VERSION'" >&2
  exit 2
fi

FILE="contracts/attestations/${KIND}/${VERSION}.json"
if [[ ! -f "$FILE" ]]; then
  echo "::error::no ${KIND} attestation for contract ${VERSION} (expected ${FILE})"
  echo "mainnet promotion is blocked until a passing ${KIND} attestation is committed" >&2
  exit 1
fi

status="$(jq -r '.status // "missing"' "$FILE")"
if [[ "$status" != "passed" ]]; then
  echo "::error::${KIND} attestation for ${VERSION} has status '${status}', not 'passed'" >&2
  exit 1
fi

# Kind-specific fields that make the attestation trustworthy rather than a bare
# "passed" flag.
require() {
  local field="$1" value
  value="$(jq -r "$field // \"\"" "$FILE")"
  if [[ -z "$value" || "$value" == "null" ]]; then
    echo "::error::${KIND} attestation for ${VERSION} is missing required field ${field}" >&2
    exit 1
  fi
}

if [[ "$KIND" == "testnet" ]]; then
  require '.network'   # e.g. sepolia
  require '.txHash'    # deployment transaction on the testnet
else
  require '.auditor'   # the independent third party
  require '.report'    # URL/URI or content hash of the signed audit report
fi

echo "verify-attestation: ${KIND} gate for contract ${VERSION} PASSED (${FILE})"
