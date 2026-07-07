# MADE async data service — GraphQL queries + WS subscriptions + REST mutations.
FROM node:22-bookworm-slim AS deps
WORKDIR /app
COPY package.json package-lock.json* ./
RUN npm install --omit=dev --no-audit --no-fund

FROM gcr.io/distroless/nodejs22-debian12:nonroot AS runtime
WORKDIR /app
COPY --from=deps /app/node_modules ./node_modules
COPY server.js package.json ./
ENV PORT=8080
EXPOSE 8080
CMD ["server.js"]
