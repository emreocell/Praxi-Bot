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

## Long-lived binary responses (live view)

The ADR 0029 live-view routes (`/v1/view/...`, `/v1/control/takeover|release`)
use no new ALPN, port, WebSocket or transport-specific API. A frame stream is an
ordinary `GET /v1/view/sessions/{id}/frames` response with content type
`application/vnd.praxi.live-view.v1`; input, heartbeat, release and cancel are
ordinary requests. A paired phone reuses the same path by speaking HTTP/1.1 on
a `bot/api/1` bi-stream directly (no proxy, no local token).

Passthrough: frames are forwarded unchanged, never collected, re-encoded,
persisted or placed in the event channel. HTTP and QUIC fragmentation is not a
record boundary; clients parse with the `live-view` wire decoder. Status,
content type, `no-store` and `nosniff` are preserved; hop-by-hop headers and
`x-forwarded-*`/`x-peer-*`/`x-praxi-*` request headers are stripped. A cancelled
stream (stall, revoke, link loss, proxy stop, upstream failure) ends the body
with an error, never a clean end, so a cut record is never followed by a newer
one: reconnect and take the newest frame (there is no replay). POST bodies are
never replayed after a reconnect; use request ids through the API.

Deadlines: dial 10 s, response *headers* 30 s (`RESPONSE_TIMEOUT`); the body is
not subject to either. A view body whose next chunk is not requested within
`VIEW_STALL` (10 s) of the previous one is cancelled on both ends.

Stream budget (per proxy and per peer connection; 32 streams): long-lived
bodies share 24 (`STREAMS - CONTROL_RESERVE`), of which at most 4 are view
frame streams (`VIEW_STREAMS`); 8 are reserved for ordinary requests. Daemon
wide: 256 streams, 192 long-lived, 16 view. Classification uses the decoded
request path, `Accept`, and finally the response content type, after the
transport identity is known. Acquisition never waits: a full view/SSE pool
answers 429 `too_many_streams` at once, a full ordinary pool 502 (proxy) or 503.
View routes take credentials only in the `Authorization` header; a credential
in the query is refused with 400 (`/v1/events?token=` is unchanged).

Buffering bounds: request bodies 16 MiB; hyper header buffer 32 KiB; hyper
holds at most one body chunk (a record is one chunk of at most ~2 MiB) plus its
write buffer; the daemon keeps one in-flight plus one newest pending frame. The
remaining transport queue is QUIC flow control: the receiving endpoint's
connection receive window (peer-network setting, RPC budget by default,
`CONNECTION_BODY_BUDGET` on a node that also serves this API). A stalled reader
therefore leaves at most that window of old frames in flight before the stall
deadline cancels the stream (measured about 1.4 MiB with the default client).
Tightening it for this ALPN needs a per-stream window in peer-network.

Real-link smoke (synthetic frames and an input sink, no display): see the
`daemon` tests, which run the real daemon router behind `ApiProxy` over a
paired iroh link (watch, take over, input, Bitirdim, second device, revoke).

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
