# Group sharing v2 plan

Status (2026-11-16): Phases 1–4 implemented and validated. 140 automated tests
passed; adapted device matrix passed on Pixel 9 plus two Android 15 emulators
(user requested emulation because only one physical phone is available).
See [validation record](group-sharing-validation.md) for evidence and limitations. Builds on [location sharing](location-sharing.md) and
[original plan](location-sharing-plan.md).

## 1. Problem

Groups shipped in nightly on 2026-11-15. The new **Limits** section of
`location-sharing.md` names the weak points. Code review confirms them and finds two
more:

| # | Limit (doc or code) | Where |
|---|---|---|
| L1 | Any member can admit anyone. The admitted person immediately gets every member's group grant, default **Always**. Nobody is told. | `apply_sync_locked` adds `group_only` friends from any vouched sync |
| L2 | **Code:** the `left` list in `GroupSync` is unsigned. Any member can claim another member left, and every phone that hears it drops that member permanently. | `apply_sync_locked` merges `sync.left` |
| L3 | **Code:** member names and IDs in `GroupSync` are unsigned. A member can add arbitrary identities, which get group access immediately on every phone. | same |
| L4 | No way to remove someone from a group. Only **Leave** (yourself) and **Block** (local, per person). | `peers.rs` API |
| L5 | Leaves reach only members connected within the 20 s `LEAVE_GRACE_MS`. Others learn later through gossip, or never if the leaver forgets the group first. | `finish_leaving` |
| L6 | No offline mailbox. If A and C are never online at the same time, C never sees A, even when B was connected to both. | each session sends only its own fix |
| L7 | New groups and joined groups default to **Always**. Joiners never choose it. | `create_group`, `join_group` |
| L8 | Battery and data cost grow as a full mesh. Every member dials every member (N-1 dialers each). | `node::supervise` |

Goals: make the membership tamper-evident and controllable (L1–L5, L7). Let group
members carry each other's last known position (L6). Keep things serverless, with no
accounts, no new dependencies, and no group crypto key.

Non-goals: chat, forwarding trips through groups, per-member exceptions inside a
group, group key encryption (MLS), iOS, and reducing mesh dialing (see section 8).

## 2. Decisions (all recommendations agreed 2026-11-16)

1. **Migration.** Groups have been in nightly for 1 day. **Recommend a clean break:**
   bump the group format. Existing groups show "Recreate this group" and stop
   granting access. Direct friends are not affected. Migrating unsigned history has
   no trustworthy creator to anchor to.
2. **Invite policy.** **Recommend fixing it at creation.** "Managed" means only
   admins invite or remove (this becomes the default). "Open" means any member can
   invite (current behavior). A policy that can change later needs conflict rules.
   That adds complexity and buys little.
3. **Join default access.** **Recommend that joiners pick** Hidden, Trips, or Always
   on the confirm screen, with **Trips** preselected. The creator still picks when
   creating, with Always preselected, since that is why groups get created.
4. **Forwarded position age.** **Recommend up to 24 h.** This matches existing
   retention, and the marker turns gray after 15 min. A shorter window (20 min, the
   current `MAX_AGE_MS`) makes the mailbox nearly useless.

## 3. Phase 1: signed membership log (fixes L1–L5)

Replace `GroupSync` (an unsigned snapshot) with a grow-only set of signed operations.
Each phone derives the same membership by replaying them. Anyone can relay the ops,
because the signatures, not the relayer, prove who did what.

### Data

```rust
// src/sharing/groups.rs (new; group logic moves out of peers.rs)
enum Policy { Managed, Open }

enum OpKind {
    Create { name: String, policy: Policy, creator_name: String },        // author = creator = first admin
    Admit  { member: EndpointId, name: String },
    Leave,                                         // subject = author
    Remove { member: EndpointId },                 // admins only
    Promote { member: EndpointId },                // admins only
}

struct Op { group: [u8; 32], author: EndpointId, at_ms: u64, kind: OpKind }
struct SignedOp { op: Op, sig: ed25519::Signature }
```

- **Create anchor:** Create uses `group = [0; 32]`; all other operations use the
  hash of that signed Create. This avoids a circular hash definition.
- **Group id** = `blake3(postcard(signed Create))`. This binds the id to its creator,
  so nobody else can squat it. `blake3` is already in `Cargo.lock`, pulled in by iroh.
- **Signature** covers `b"roamlog/group-op/1" || postcard(op)`, signed with the
  existing Iroh `SecretKey`. The domain prefix keeps these signatures separate from
  TLS handshake use of the same key.
- **Storage:** `sharing/groups/<id>.ops` (postcard) plus the local fields that stay in
  `friends.json`: local name, `me`, `share`, `leaving`. Bounds: at most 512 ops per
  group, 64 members, and the existing `MAX_GROUPS = 32`.

### Deriving state (pure function, heavily unit-tested)

```
fold(ops sorted by (at_ms, blake3(signed op))):
  Create (must be first, id must match)  -> admins={author}, members={author}
  Admit  valid if author ∈ members && (policy == Open || author ∈ admins)
         -> add absent member with name, inviter, and admission time
  Leave  valid if author ∈ members -> members -= author, admins -= author
  Remove valid if author ∈ admins && member != creator -> members -= member, admins -= member
  Promote valid if author ∈ admins && member ∈ members -> admins += member
  invalid ops are kept but ignored (another order of arrival cannot change outcome)
```

- Duplicate admits of a current member do not change their name, inviter, or
  admission time. Remove/Leave followed by Admit creates a new admission. This
  preserves the history cutoff used by relay and the inviter shown by the UI.
- Create includes a signed creator display name so confirmation and member sheets
  do not depend on unsigned names supplied by the relayer.
- The order is deterministic, so all phones holding the same op set get the same
  result, regardless of arrival order. Re-admitting after a Leave or Remove works
  because a later `Admit` wins.
- Ops with `at_ms > now + FUTURE_SKEW_MS` are rejected on receipt. A member can
  backdate an admit to before their own removal. The Remove UI therefore offers
  "Also remove people they added" (emits extra `Remove` ops). Document this
  rather than solving it with vector clocks.
- `Friend.groups` and `group_only` become derived. They are recomputed on every
  merge, so the existing permission code (`effective_permission`, `may_share`) stays
  unchanged.

### Wire

- Reuse the existing unidirectional stream path in `transport::session`. On connect
  or change, send `GroupOps { group, ops: Vec<SignedOp> }`, but only for groups where
  the receiver is a derived member. Exception: a removed member also gets the op
  set containing their own `Remove` once, so they learn about it. Keep
  `MAX_GROUP_SYNC` (256 KiB). 512 ops × ~180 B fits.
- Receiver: verify every signature. Union into the stored set. Re-derive. Notify if
  anything changed.
- Bump ALPN to `roamlog/location/2`. v1 peers fail the handshake cleanly, and the
  Friends list shows "Update Roamlog".

### Leave delivery (L5)

- **Leave** signs a `Leave` op. Access stops at once (existing behavior). The group
  stays in `leaving` state until **any** member acknowledges the op set containing
  it (they echo a digest), or for 7 days. After that the phone forgets the group.
  This replaces the fixed 20 s grace period. Every other member then gets the leave
  through normal gossip.

### Tests (in `groups.rs` and `node.rs`)

- Same op set in shuffled orders gives the same state (property-style loop over
  permutations).
- A forged `Leave` for someone else, a non-admin `Remove`, a non-admin `Admit` in a
  Managed group, a bad signature, a wrong group id, and a future timestamp are all
  ignored.
- Re-admit after a remove works. "Also remove people they added" cascade works.
- Extend `group_members_gossip_share_and_leave` with these cases: an admin removes C
  and B stops sharing with C; C learns about its removal; a leave while B is offline
  reaches B later through A.

## 4. Phase 2: membership UX (L1, L4, L7)

`src/ui/friends.rs`, group row and confirm screen:

- **Create:** name, plus **Managed / Open** (Managed preselected, with one-line help).
- **Join confirm:** "Pair with Alex and join Family?" now shows who created the group,
  the member count, the policy, and an access picker (Trips preselected).
- **Group row ⋯:** Invite (admins, or anyone in Open groups), Members, Rename, Leave.
- **Members sheet:** each member shows "added by X", with an admin badge. Admins get
  ⋯ → **Remove**, **Remove + people they added**, and **Make admin**. Everyone gets
  **Block**, which shows the warning from Phase 3.
- **Membership notification** (Android, recorder process): "Sam joined Family. They
  can see your location (Always)." Fires on every derived new member while your
  group share is not Hidden. This is the main L1 mitigation for Open groups.
- Legacy groups: row reads "Recreate this group" with a single **Remove** action.

## 5. Phase 3: group relay of last-known positions (L6)

Members carry each other's latest **signed** fix. A phone that never meets A directly
still sees A's position as of the last time anyone in the group met A.

### Message

```rust
enum GroupPayload { Fix(LocationUpdate), Unavailable(Availability) }
struct GroupFix {
    group: [u8; 32], origin: EndpointId, sequence: u64,
    timestamp_ms: i64, payload: GroupPayload,
}
// datagram: Message::GroupFix { fix: GroupFix, sig: Signature }, about 175 B < MAX_FRAME 256
// signed over b"roamlog/group-fix/1" || postcard(fix)
```

- Envelope `timestamp_ms` also dates Unavailable markers. Fix payload timestamp and
  sequence must match the signed envelope; Unavailable cannot claim Active.
- `sequence` comes from the existing global `Metadata.sequence`. Receivers keep the
  existing per-origin `Cursor`. A fix is accepted if `sequence > cursor`, whether it
  arrived directly or forwarded. Replay protection is therefore shared, and no
  second counter is needed.

### Origin rules (sending phone stays the authority)

- Signing happens in `Manager::send_latest`, next to the existing per-peer gate. A
  `GroupFix` is signed for group G only if all of these hold: mode is not Off; G's
  `share` allows it now (Always, or Trips while recording); and **no member of G is
  blocked locally**. Blocking a member therefore turns off relay for that group, and
  you fall back to today's direct, per-peer checked sends. This keeps Block meaningful.
- When access drops (share lowered, mode set to Off or Pause, trip ended under Trips),
  sign `Unavailable` with a higher sequence before closing. Members replace the fix
  and forward the marker instead. This is best effort, and the docs already say
  copies cannot be recalled.

### Forwarder rules

- Store the newest signed `GroupFix` per (group, origin), up to 24 h, in
  `sharing/group-fixes.postcard` with existing retention; published positions remain
  in `sharing/friend-locations.json`.
- Forward to peer P only if P is a derived member of G in your view; P is not the
  origin; P's derived admission time ≤ the fix timestamp (no history before they
  joined); and the fix is newer than the last one sent to P in this session.
- On connect, send all stored fixes that qualify. On a new fix, send it to every
  connected member. The cost is O(N²) small datagrams per update interval, which is
  fine at a 64-member cap.

### Receiver rules

- Verify the signature. Check that the origin is a derived member of G and not
  blocked locally. Check the cursor. Validate against a new `MAX_FORWARD_AGE_MS = 24 h`
  (instead of `MAX_AGE_MS`) for forwarded fixes only.
- When direct and relayed fixes are both present, the higher sequence wins.
- UI: the marker and row show "via Sam, 12 min ago". Fixes older than 15 min are
  gray, as now.

### Tests

- A and C never connect at the same time; B relays; C sees A's fix with the right
  age.
- A fix older than C's admission is not forwarded to C.
- `Unavailable` replaces the fix. A higher sequence wins over a lower one, whether it
  came directly or forwarded. A replayed or forged signature is rejected.
- A blocks C: A signs no new `GroupFix` for that group, so B cannot carry A's new
  updates to C. Previously signed copies cannot be recalled.
- Frame size fits `MAX_FRAME` with the worst-case payload.

## 6. Phase 4: docs and device validation

- Update `location-sharing.md`:
  - **Use**: managed/open groups, members sheet, remove, choosing access on join.
  - **Privacy rules**: signed membership; relay reaches every current member; Block
    disables relay for that group.
  - **Limits**: rewrite Permission scope and the mailbox lines. Relay is
    member-carried, last-known only, up to 24 h. Backdated admits are a known
    gap.
  - Update the README summary to match.
- Device matrix (Pixel 9 and moto g, as before), with 3 phones:
  - Managed remove.
  - Open admit with notification.
  - Leave while one phone is in airplane mode.
  - Relay with A and C alternating airplane mode.
  - Block disables relay.
  - v1 to v2 upgrade shows "Recreate" and "Update Roamlog".

## 7. Order and size

| Phase | Scope | Est. |
|---|---|---|
| 1 | Signed op log, derive, wire, leave ack, tests | 4–6 d |
| 2 | Create/join/members UI, notification, legacy row | 3–4 d |
| 3 | Signed group fixes, forward/store, UI "via", tests | 4–5 d |
| 4 | Docs, 3-phone device runs | 2 d |

Ship Phase 1 alone first: it closes the forgery holes (L2 and L3). Phases 2 and 3 can
ship independently after it.

## 8. Deferred (revisit only with evidence)

- **Mesh reduction:** dial only some members and rely on the relay. Wait for battery
  numbers from the pending 8 h and 2 h tests.
- **Forwarding finished trips through groups:** large payloads and an unclear consent
  model.
- **Per-member exceptions inside a group:** these need per-recipient encryption.
  Block plus Remove covers the real cases.

## 9. Implementation progress (2026-11-16)

Implemented:

- Signed operation types, domain-separated signing/verification using Iroh keys,
  Create hash binding, managed/open authority, deterministic replay and bounds.
- Durable postcard logs under `sharing/groups`, verified merge, derived friend
  grants, legacy groups with zero group access, explicit admission/removal/promotion.
- Reliable bounded per-group sync, digest acknowledgements, immediate leave
  revocation with seven-day delivery retention, removed-member delivery through
  other members, stale-route cleanup after re-admission, interrupted-leave recovery.
- Location ALPN v2 and pairing ALPN v2. Existing direct-friend records retain grants;
  protocol mismatch shows Update Roamlog. Pairing verifies a signed preview before
  confirmation and transmits signed admission after both users accept.
- Managed/Open create picker, creator access choice, Trips-default join choice,
  creator/member-count/policy preview, member provenance/admin badges and actions,
  legacy recreation row, recorder membership notifications with durable checkpoints.
- Signed group fixes/withdrawals, shared replay counters, durable 24-hour relay cache,
  admission cutoffs on forwarding and receipt, blocked-member origin gate,
  best-effort Off/Pause withdrawals, marker/row forwarding provenance.
- README and location-sharing guide updated.

Local verification covers forged operations/fixes, authority and bounds, shuffled
arrival and equal-time ordering, persistence, cascade removal, offline leave carried
after restart, signed group pairing, join access, v1 rejection, notice checkpoints,
three-node remove/readmit/leave, mailbox relay after restart without origin/recipient
overlap, withdrawal/direct ordering, history cutoffs, blocking, trip withdrawal, and
worst-case datagram size. See [validation record](group-sharing-validation.md) for
commands, results, device matrix and evidence.

Device validation complete with agreed adaptation: Pixel 9 plus two Android 15
emulators. Managed remove/re-admit, Open admission and Android notices, Hidden
suppression, offline leave, non-overlapping relay after carrier restart, Block,
withdrawals, promotion/cascade, join/create controls, and actual v1 upgrade/recreation
passed. No physical moto g or battery-duration coverage claimed. Production app,
trips and family sharing were preserved; runs used isolated QA package.
