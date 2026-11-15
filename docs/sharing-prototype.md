# Sharing connectivity prototype

Opt-in Rust transport, separate from GPS, trip recording, UI, and Android services.
Uses Iroh 1.4.1 with its N0 discovery/relay preset. Nothing runs unless caller
enables `sharing` and binds an endpoint. Protocol sends test nonces only.

## Desktop test

Create persistent test identities on both machines:

```sh
cargo run --no-default-features --features sharing --example sharing_probe -- /tmp/alice.key identity
cargo run --no-default-features --features sharing --example sharing_probe -- /tmp/bob.key identity
```

On Alice, substitute Bob's printed peer ID:

```sh
cargo run --no-default-features --features sharing --example sharing_probe -- /tmp/alice.key listen BOB_PEER_ID
```

Copy Alice's printed address JSON to Bob. Keep JSON within single quotes:

```sh
cargo run --no-default-features --features sharing --example sharing_probe -- /tmp/bob.key connect 'ALICE_ADDRESS_JSON'
```

Listener accepts one connection, checks its authenticated peer ID against supplied
ID, then answers one ping. Client authenticates Alice using ID in address JSON.
Both should print `authenticated probe succeeded`. Endpoint closes on completion
or error. Connection setup and message exchange have 30-second deadlines; listener
waits for incoming connection until interrupted. Unexpected peers end test with error.

Dialing uses six attempts with 30-second attempt deadlines and backoff delays of
1, 2, 4, 8, and 16 seconds. Closing the local endpoint cancels pending attempts
and retry waits. No retry grants access or changes the expected peer identity.
If relay startup times out after 30 seconds, the harness prints a warning and
continues using available addresses, allowing LAN tests without a reachable relay.

## Continuous network-switch test

Use `serve` instead of `listen` on Alice and `watch` instead of `connect` on Bob:

```sh
cargo run --no-default-features --features sharing --example sharing_probe -- /tmp/alice.key serve BOB_PEER_ID
cargo run --no-default-features --features sharing --example sharing_probe -- /tmp/bob.key watch 'ALICE_ADDRESS_JSON'
```

`serve` accepts successive sessions from the approved peer. Rejected peers,
malformed messages, idle sessions, and disconnected clients end that session;
the listener stays available. `watch` pings every 10 seconds over the existing
connection. After a connection loss or probe timeout it waits one second, then
reconnects using the bounded dialing policy. Exhausting retries ends the command.
Peer application-close responses (including authorization denial) and malformed
responses end the command immediately. Stop long-running commands with Ctrl-C.

Each successful probe prints the selected path (`direct`, `relay`, or `unknown`)
at that instant, without logging transport addresses, keys, or coordinates.
Switch Wi-Fi/cellular and record recovery time and path changes. Selected path
may change after hole punching; an initial relay result does not imply all later
traffic stays relayed. These modes test networking, not GPS or background services.

Protocol ALPN is `roamlog/probe/1`. Each stream contains exactly 10 bytes:
version (1 byte), kind (1 = ping, 2 = pong), nonce (8 bytes, big endian).
Unknown versions, wrong kinds, oversized/truncated frames, wrong nonces, and
unapproved senders fail. No location access is granted.

Example key file contains **unencrypted** secret bytes. New files use tempfile's
private permissions on Unix and publish without overwriting an existing file.
Use disposable desktop identities. Android now has an explicit
[Keystore-backed identity adapter](android-sharing-identity.md)
and [standalone Android harness](android-sharing-probe.md). Do not reuse this desktop
harness as Android storage.

## Validation remaining

- Android harness execution and Keystore identity runtime validation on devices.
- Same Wi-Fi, separate Wi-Fi, Wi-Fi/cellular, cellular/cellular connectivity.
- Record direct versus relay path and reconnect behavior for each network case.
- Physical network switching and early foreground-service experiment.

Local tests use relay-free endpoints; passing them does not verify internet relays
or Android background execution.

Reference: [Iroh endpoint API](https://docs.rs/iroh/1.4.1/iroh/endpoint/struct.Endpoint.html).
