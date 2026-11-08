# Remote daemon API

`ApiProtocol::new(state)` (feature `server`) registers the daemon's existing
route table under `peer_network::ALPN_API`. The `PeerNetwork` must own the
handler: it checks its pairing store before dispatch and closes tracked QUIC
connections when a peer is revoked. Never dispatch this handler from an
unauthenticated transport.

`ApiProxy::start(net, peer).await?` returns `base_url`, a fresh 256-bit `token`,
and a `watch::Receiver<ProxyState>`. Keep the proxy alive while using it; dropping
it stops the listener, closes the API connection and cancels streaming bodies.
The caller owns `net` and should call `net.shutdown().await` at process shutdown.
Client-only applications can depend on this crate without the `server` feature
and do not pull in `daemon-api`.

The proxy reuses a QUIC connection and opens one HTTP/1.1 bi-stream per request,
with at most 32 concurrent request/response streams. HTTP connection reuse is
disabled on the peer server. A successful HTTP health probe is required before
publishing `Connected`. Connection loss produces `Reconnecting`; failed attempts
and revocation produce `Unreachable(reason)`. Attempts back off from 100 ms to
5 seconds, with a 10-second dial/probe deadline. Re-pairing permits recovery;
requests are never replayed automatically, so mutations cannot be duplicated by
a retry. Responses are streamed, including indefinite SSE.

The listener binds only `127.0.0.1:0`. Host, Tauri Origin, CORS, bearer and SSE
query-token rules match the daemon's defaults, including public health probes
and signed webhooks (their normal HMAC verification still runs remotely).
Authorization, proxy authorization, hop-by-hop headers, and all decoded `token`
query parameters are removed before forwarding. Pairing, rather than the
remote daemon's loopback bearer, authenticates the remote ingress. The same
API handlers enforce normal tool permissions and approval requirements.

Limits: 16 MiB of request data, including chunked bodies; a 10-second idle
upload deadline and 60-second total upload deadline; 30 seconds to response
headers; 10 seconds for remote HTTP request headers; 32 KiB HTTP header buffer;
32 active streams per QUIC connection and 256 across a server handler. Existing
route-specific body limits (for example JSON and webhook limits) still apply.
The proxy and server do not buffer complete request or response bodies.

Desktop peers have all routes. Phones cannot POST host import/handover, or
POST/PUT any `/v1/secrets*` path. Other roles are refused. Decision requests and
HTTP completion status are appended to the existing SQLite audit log with the
paired public key and actor `phone:<paired name>` or `desktop:<paired name>`;
the normal agent decision audit remains in place.

CLI configuration is `<data>/config.toml`:

```toml
[peer]
enabled = true
relay = "default" # or "none" for direct-only, or a custom relay URL
```

`bot serve --host` enables peers by default with role Host. `bot serve --peer`
forces peer networking on; ordinary `serve` defaults off. An explicit
`enabled = false` overrides the host default. Identity and pairing records live
under `<data>/peer`.

After pairing via `PeerNetwork`, use
`bot --remote <endpoint-id> --data-dir <client-data> status` (or other daemon
client commands). Configuration, setup, secret-store and provider-installation
commands remain local and reject `--remote`. This task does not add a pairing
wizard; the UI pairing flow belongs to card 059.

Hermetic verification uses in-process UDP loopback endpoints with relays and
external discovery disabled, a fake provider, and ephemeral loopback HTTP:

```powershell
$env:CARGO_TARGET_DIR = 'C:/praxi-target-ext'
cargo test -p remote-api --features server
cargo test -p daemon-api
cargo test -p cli
cargo clippy -p remote-api -p daemon-api -p cli --all-targets --features remote-api/server -- -D warnings
cargo fmt --all --check
cargo deny check
```
