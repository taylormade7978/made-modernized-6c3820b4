# MADE Helm chart

Deploys the MADE platform into a dedicated MicroK8s namespace: a Deployment +
Service per component, a shared NGINX Ingress with cert-manager/Let's Encrypt
TLS for the web/api/ws hosts, ConfigMap/Secret for the Postgres/Redis/MinIO/
Temporal connection settings, Kong+OPA edge plugin references, Dapr sidecar
annotations, Prometheus scrape annotations, and resource requests/limits.

## Components

| Component | Image | Port | Probes | Notes |
|-----------|-------|------|--------|-------|
| `server`  | `made-server` | 8080 | `/health` | Authoritative game server — REST `/v1` + `/ws`, `/metrics` scrape target, Dapr-enabled, consumes the ConfigMap + Secret. |
| `web`     | `made-pwa`    | 8080 | `/healthz` | Static React PWA on rootless nginx. No runtime env (build-time configured). |

Components are a map in `values.yaml`; add one and it renders its own
Deployment + Service with the same knobs.

## Render / lint

```sh
helm lint deploy/helm/made
helm lint deploy/helm/made -f deploy/helm/made/values-prod.yaml

# Full render for a tier:
helm template made deploy/helm/made -f deploy/helm/made/values-prod.yaml
helm template made deploy/helm/made -f deploy/helm/made/values-dev.yaml
```

## Install

```sh
# prod — credentials come from an externally-managed Secret:
helm upgrade --install made deploy/helm/made -n made --create-namespace \
  -f deploy/helm/made/values-prod.yaml \
  --set secrets.existingSecret=made-prod-secrets

# dev — chart renders the Secret; inject values at deploy time:
helm upgrade --install made deploy/helm/made -n made-dev --create-namespace \
  -f deploy/helm/made/values-dev.yaml \
  --set secrets.data.DATABASE_URL='postgres://made:dev@postgres:5432/made' \
  --set secrets.data.MADE_REDIS_URL='redis://redis:6379'
```

## Connection settings — no hardcoded credentials

Non-secret settings (hosts, ports, namespaces, tuning) live in `config.data`
and render into a ConfigMap. Credentials (`DATABASE_URL`, `MADE_REDIS_URL`,
`MINIO_ACCESS_KEY`/`MINIO_SECRET_KEY`, `TEMPORAL_API_KEY`) are **never** baked
into the chart:

- `secrets.create: true` renders a Secret from values you supply at deploy time
  (`--set`, CI, or a sealed secret). Empty keys are omitted, and `NOTES.txt`
  warns about any left blank.
- `secrets.existingSecret: <name>` references a Secret managed entirely outside
  the chart; the chart renders none and wires the components to it.

## Edge trust model

Auth is terminated at the Kong/OPA edge, not in the server. The chart enforces
"trust only gateway-set headers" in two layers:

1. The Ingress `configuration-snippet` scrubs client-supplied identity headers
   (`X-Identity`, `X-Consumer-*`, …) so a caller cannot forge them.
2. The `made-identity-headers` KongPlugin removes them at the gateway, and the
   `made-opa` KongPlugin gates every route through the OPA decision point.

`gateway.kong.enabled` / `gateway.dapr.*` guard the Kong/Dapr CRD references so
a cluster without those operators can render a valid release with them off.
