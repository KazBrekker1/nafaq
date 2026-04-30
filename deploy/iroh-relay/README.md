# Iroh Relay Deployment

This directory contains the Compose definition for the Nafaq project-controlled Iroh relay at `https://iroh-relay.sanad.ink`.

## Posture

Nafaq is configured to use its own relay only. The application must not fall back to the public/default n0 relay network, and this relay is not an application backend: it does not store accounts, contacts, conversations, media, files, or call metadata beyond normal Iroh relay operation.

## Ports

- `3340/tcp` inside the container: HTTP relay service used by the hosting platform/reverse proxy for `https://iroh-relay.sanad.ink` and by the local healthcheck.
- `7842/udp` on the host: Iroh QUIC address-discovery port. The Compose file publishes this port, but the current `--dev` command does not enable the QUIC address-discovery listener; see [Production command status](#production-command-status).
- `9090/tcp` inside the container: relay metrics default when enabled by the relay binary. It is not published by this Compose file.

## Healthcheck

Compose probes the relay from inside the container with:

```sh
wget -qO- http://127.0.0.1:3340/generate_204 >/dev/null
```

This command must fail when the HTTP probe fails. Do not add `|| exit 0` or similar fallbacks that mask relay failures.

## Image pinning

The image is pinned to `n0computer/iroh-relay:v0.97.0`, matching the app's current `iroh = "0.97"` dependency in `src-tauri/Cargo.toml` and the `iroh-relay 0.97.0` crate resolved in `src-tauri/Cargo.lock`. Re-test the deployment before moving to another relay image tag.

## Production command status

The Compose file still runs `iroh-relay --dev` intentionally. For Iroh 0.97, upstream documents `--dev` as localhost/development mode: it serves plain HTTP on port `3340`, ignores TLS-related config, and does not run the QUIC address-discovery endpoint by default.

The correct non-dev production command for this hosting setup has not been safely determined because the deployment appears to rely on the platform-provided FQDN/reverse proxy wiring for container port `3340`. Removing `--dev` without a tested relay config would change the default HTTP bind port to `80` and may break the proxy and healthcheck. Before removing `--dev`, test and document a relay config using `--config-path`, TLS/proxy behavior for `https://iroh-relay.sanad.ink`, and whether QUIC address discovery should be enabled on UDP `7842`.

## Operational verification

Validate the Compose file before deployment:

```sh
docker compose -f deploy/iroh-relay/docker-compose.yml config
```

After deployment, verify:

```sh
curl -fsS https://iroh-relay.sanad.ink/generate_204 -o /dev/null
```

Then confirm the container health status is `healthy` and review relay logs for startup errors. If QUIC address discovery is enabled in a future production config, also verify UDP `7842` is reachable from outside the host.
