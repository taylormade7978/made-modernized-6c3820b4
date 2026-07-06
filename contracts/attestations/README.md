# Smart-contract promotion gates

The MADE card tokens are ERC-1155 contracts. Promoting a contract release to
**mainnet** is blocked unless two attestations have been committed here first,
so the gate is auditable in git history and enforced by CI — not by convention.

The [`Contract promotion (mainnet)`](../../.github/workflows/contract-promotion.yml)
workflow (manual `workflow_dispatch`, input `contract_version`) runs two required
gate jobs before the `mainnet` job can start:

| Gate | File | Enforced by |
| --- | --- | --- |
| **testnet-first** | `testnet/<version>.json` | `verify-attestation.sh testnet <version>` |
| **third-party audit** | `audit/<version>.json` | `verify-attestation.sh audit <version>` |

The `mainnet` job declares `needs: [testnet-gate, audit-gate]` **and** runs in the
protected `mainnet` GitHub Environment, so there is no path to mainnet that skips
either gate.

## Satisfying the gates

Commit a JSON attestation named for the exact `<version>` being promoted.

`testnet/<version>.json` — must carry `"status": "passed"`, the testnet
`network`, and the deployment `txHash`:

```json
{
  "version": "1.4.0",
  "status": "passed",
  "network": "sepolia",
  "txHash": "0x…",
  "deployedAt": "2026-07-06T00:00:00Z"
}
```

`audit/<version>.json` — must carry `"status": "passed"`, the `auditor`, and a
`report` reference (URL/URI or content hash of the signed report):

```json
{
  "version": "1.4.0",
  "status": "passed",
  "auditor": "Trail of Bits",
  "report": "https://…/made-erc1155-1.4.0.pdf",
  "auditedAt": "2026-07-05T00:00:00Z"
}
```

See `testnet/_TEMPLATE.json` and `audit/_TEMPLATE.json` for copy-ready starting
points. Anything other than `"status": "passed"`, or a missing required field,
fails the gate and blocks the promotion.
