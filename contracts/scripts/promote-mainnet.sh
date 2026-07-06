#!/usr/bin/env bash
#
# Broadcast an audited, testnet-proven contract release to mainnet.
#
# This runs ONLY from the mainnet-promotion workflow's final job, which is:
#   * gated on both the testnet and audit attestation jobs (GitHub `needs:`), and
#   * bound to the protected `mainnet` GitHub Environment (required reviewers /
#     wait timer live there).
# So by the time control reaches here, both gates have already passed. The
# broadcast itself is delegated to the contract toolchain command wired in
# CONTRACT_DEPLOY_CMD (a forge/hardhat deploy), kept out of this repo so the
# gate lives with CI while the toolchain lives with the contracts. Credentials
# arrive as environment secrets; the script refuses to run without them rather
# than silently no-op.
set -euo pipefail

VERSION="${CONTRACT_VERSION:?CONTRACT_VERSION is required}"
: "${MAINNET_RPC_URL:?MAINNET_RPC_URL secret is required for a mainnet broadcast}"
: "${MAINNET_DEPLOYER_KEY:?MAINNET_DEPLOYER_KEY secret is required for a mainnet broadcast}"

echo "promote-mainnet: broadcasting contract ${VERSION} to mainnet (${MAINNET_RPC_URL%%\?*})"

if [[ -z "${CONTRACT_DEPLOY_CMD:-}" ]]; then
  echo "::error::CONTRACT_DEPLOY_CMD is not configured; nothing to broadcast" >&2
  echo "set the org/repo variable CONTRACT_DEPLOY_CMD to your forge/hardhat deploy invocation" >&2
  exit 1
fi

# The deploy command reads MAINNET_RPC_URL / MAINNET_DEPLOYER_KEY / CONTRACT_VERSION
# from the environment; never echo the key.
eval "$CONTRACT_DEPLOY_CMD"

echo "promote-mainnet: contract ${VERSION} promoted to mainnet"
