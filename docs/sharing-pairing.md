# Phase 2: identities, invitations, and friends

Rust pairing and friend records are implemented behind `sharing`. Android
prototype exposes pairing and friend management. Production Friends/map interface
belongs to Phase 4. Networking and Keystore behavior still require physical-device
validation; desktop tests do not prove Android lifecycle reliability.

## Try pairing

Build/install [Android prototype](android-sharing-probe.md) on two phones. Open
**Friends**, enter each user's name, and follow these steps:

1. Alice taps **Add friend: show invitation QR**. Invitation lasts up to five
   minutes while that pairing session runs.
2. Bob opens invitation link, pastes it into input, or selects **Scan QR image**
   and chooses clear photo/screenshot of Alice's QR. System camera/scanner can
   also open supported `roamlog://pair/…` links. In-app live-camera scanner is not
   implemented; QR-image decoding uses Android document picker without camera
   or storage permissions.
3. Bob reviews invitation and taps **Pair from invitation link**. No connection
   starts merely from opening a link or decoding an image.
4. Both phones display other peer's authenticated identity, name, and matching
   verification code. Compare code through trusted channel or side by side.
5. Both users tap **Codes match: confirm pairing**. Either can reject or stop.
6. Tap **Refresh friends** after restart to inspect saved friends. New friends
   always have location access **Off**. **Trip only** and **Always** record local
   authorization preferences; location messages and actual sharing arrive in
   Phase 3. Pairing never starts GPS or grants location permissions.

Invitation-link handoff may temporarily hide activity. Bounded pairing session
can continue for five minutes while Android keeps process alive. **Stop**, Back,
activity destruction, rejection, and timeout end session. Ordinary probe modes
still stop when activity hides. This is not a foreground service or background
sharing guarantee. Reopening an app killed by Android requires a new invitation.

Invitation contains version, peer ID, display name, endpoint connectivity hints,
and random one-session token. Treat invitation as permission to request pairing,
not permission to receive locations. Copy link only to intended friend. Invitation
is not saved to logs or friend records. Closing its endpoint invalidates it.

## Trust and protocol

ALPN `roamlog/pair/1` separates pairing from probe/location protocols. QUIC
authenticates remote endpoint ID. Versioned request includes invitation token and
name; server compares token in constant time. Frames use a four-byte big-endian
length followed by JSON, capped at 4096 bytes. Invalid versions, names, tokens,
message types, and oversized frames fail. Invitations are limited to 2048 bytes;
QR image decoding accepts at most 2048 × 2048 grayscale pixels and exactly one
valid Roamlog invitation.

Human verification code uses six bytes from TLS exporter, formatted as three
groups of four hex digits. It binds comparison to actual encrypted session.
Names are untrusted labels; matching names alone do not verify identity.
Thirty-second network deadlines and five-minute confirmation/session deadlines
prevent indefinite pending requests. No automatic accept or trust-on-first-use.

Both users must affirm before either creates a friend. After both affirmative
votes, each persists local record and exchanges completion acknowledgement.
Connection/storage failure during completion can leave an Off friend on only
one phone. Inspect lists and pair again; no distributed atomic commit is claimed.
Re-pairing preserves existing local name and permissions and never unblocks a
blocked identity.

## Persistence and authorization

`sharing/friends.json` stores version, local owner ID, peer IDs, names, permissions,
blocked state, and pairing timestamps. No trip or location data enters this file.
Records are private files written through atomic rename, file fsync, and directory
fsync. All reads/mutations use cross-process lock, fresh state, and strict bounded
validation. Duplicate/self identities, malformed records, owner-key changes, and
unknown versions fail closed; corrupt files are preserved.

`Friends::may_share(peer, recording)` is sender-side policy gate for Phase 3:

| Permission | Not recording | Recording |
| --- | --- | --- |
| Off, unknown, removed, or blocked | Deny | Deny |
| Trip only | Deny | Allow |
| Always | Allow | Allow |

**Block** immediately sets local permission Off and rejects re-pairing. **Remove**
deletes record, so future policy checks deny access. Remove blocked record and
explicitly pair again to establish trust anew. No outbound location stream exists
yet; Phase 3 must call current policy gate for every transmission, including
queued requests, and honor global sharing mode. Revocation cannot erase data
previously received by another device.

Identity keys remain in [Keystore-backed storage](android-sharing-identity.md).
Friend database binds to local key: unexpected key replacement cannot reuse old
grants. Lost/reinstalled peer needs fresh pairing; remove its old identity.
Identity reset/recovery UI remains outside this prototype.

## Validation

Automated tests exercise authenticated two-endpoint pairing, matching/session-
specific codes, both-user consent, rejection, abandonment, invalid tokens,
oversized frames, QR encode/decode, default-Off permissions, re-pair behavior,
blocking during confirmation, removal, persistence, owner mismatch, corrupt and
duplicate records, and concurrent writers. Android Rust/Java compile and signed
APK checks complement these tests. Two-phone QR/link/Keystore/JNI tests remain
pending physical devices.
