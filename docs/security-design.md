# Session security design

## Decision

LanPlay will authenticate the control connection before it accepts any media or input binding. A LAN address and the current clear-text `SessionToken` are not identities and are not authentication; the existing token remains a stale-session guard only.

Pairing creates one long-lived public-key identity for each endpoint. The client stores its private key in the macOS Keychain and the Windows host stores its private key with DPAPI protected to the LanPlay service account. A pairing record contains the peer public key, stable peer identity, display name and the last discovered endpoint. The endpoint is a hint and may change; the public key is the identity.

The first pairing requires explicit user confirmation on both endpoints. A later connection accepts only the remembered peer public key. Unpaired discovery may show a host, but it may not start a session or inject input.

## Control channel

The control TCP connection will use TLS 1.3 with mutual authentication. The certificate identity is the pinned pairing public key rather than a public certificate authority. The handshake binds both endpoint identities, the protocol version, the session generation and the negotiated capability transcript. A changed transcript or a repeated generation fails closed.

The existing `ClientHello`, `ServerHello` and `SessionToken` remain useful inside the authenticated channel for protocol diagnostics and stale-session rejection. They are not a substitute for the TLS authentication decision.

## Input authorization

The host accepts video and telemetry from an authenticated session, but it accepts keyboard, mouse and gamepad input only after the session has explicitly negotiated input capability. The authorization is scoped to the current session generation and is revoked before teardown, timeout or reconnect. An unauthenticated UDP packet, a packet with the wrong generation or a packet from an unbound source is discarded without reaching an operating-system injection API or a virtual controller backend.

The host keeps the existing neutralization invariant. Authentication failure, session expiry and teardown all neutralize held keyboard, mouse-button and gamepad state before the virtual device is destroyed.

## Replay protection

Every authenticated datagram carries a session generation, direction, monotonically increasing sequence and an AEAD nonce derived from that sequence. The receiver keeps a bounded replay window per direction and rejects an old or repeated sequence before decoding the payload. A reconnect creates a new generation and a new traffic key, so packets from the previous session cannot become valid by arriving late.

Reliable control messages retain their event identifiers and idempotent semantics. Authentication prevents off-path forgery; event deduplication prevents an authenticated retransmission from becoming a second OS action.

## Key storage and rotation

Private keys never enter `TASKS.md`, logs, diagnostics or the pairing JSON. Pairing JSON stores public identities and non-secret endpoint metadata only. Removing a pairing deletes the peer public key and makes the next connection require explicit confirmation. Rotating a local key invalidates all peer records and requires re-pairing rather than silently accepting a new identity.

The implementation will use platform stores rather than a repository secret or a user-managed plaintext file. A failure to open the platform store refuses pairing and leaves input disabled; it does not generate an ephemeral fallback that would make identity unpredictable.

## Deferred implementation boundary

This document fixes the security contract. TLS integration, platform key-store adapters, pairing UX and authenticated UDP framing remain implementation work. Until that work is complete, the current LAN control token and input path are suitable for the lab but are not a distributable security boundary.
