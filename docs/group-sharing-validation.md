# Group sharing v2 validation

Status (2026-11-16): implementation and adapted three-device matrix passed;
140 automated tests passed again after device runs.
Pixel 9 connected. Two Android 15 x86_64 emulators run with KVM and host GPU;
release QA builds render successfully on all three endpoints. User has one physical phone
and requested emulation of the other two. Tests use isolated
`dev.traveler.groupqa` app data; production app, trips, and live sharing stay intact.

## Automated coverage

- Signed membership replay: all permutations of an op set, equal timestamps,
  duplicate operations, authority checks, signature tampering, future timestamps,
  wrong group IDs, promotion/revocation, re-admission, member/operation bounds.
- Persistence: verified logs, cascade removal, removal carried by another member,
  leave carried after restart while another member is offline, digest checking,
  seven-day expiry/log deletion, interrupted leave metadata write recovery,
  historical inviter names, and failed sequence/cache-write reservation recovery.
- Pairing/migration: authenticated signed membership preview, both-user confirmation,
  chosen join access, preserved creator access, managed-member denial, legacy group
  grants disabled with direct grants preserved, v1 handshake rejection.
- Notices: newly admitted members, durable checkpoint, Hidden access, re-admission.
- Three live local nodes: gossip, sharing without direct pairing, admin removal,
  removed member learning removal, re-admission, leave and acknowledgement.
- Mailbox: A and C never connect; B persists A's fix, restarts, then relays to C.
  One-hour age and forwarding provenance survive; forged/replayed fixes fail;
  withdrawal clears position, survives restart, and loses to newer direct sequence.
  GPS loss alone preserves last-known relay data. Identical direct duplicates are
  suppressed; late valid packets are dropped without breaking membership gossip.
- Privacy/bounds: sender and receiver admission cutoffs, Block prevents new group
  signatures, Trips transition emits withdrawal, worst-case datagram fits 256 bytes.

Commands:

```sh
cargo test --no-default-features --features sharing
cargo test --features sharing
cargo clippy --no-default-features --features sharing --all-targets -- -D warnings
cargo fmt --all -- --check
git diff --check
```

Android compilation/lint uses SDK `/tmp/zgui-sdk`, NDK
`/tmp/zgui-sdk/android-ndk-r28c`, ARM64 target, and API 26 Clang:

```sh
cargo clippy --target aarch64-linux-android --features sharing --lib -- -D warnings
```

Environment must set `ANDROID_HOME`, `ANDROID_NDK_HOME`,
`CC_aarch64_linux_android`, and `CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER`.
Automated endpoints do not prove Android notification delivery, background lifecycle,
real airplane-mode recovery, or rendered controls.

## Adapted three-device matrix — passed

Adapted run: A = Pixel 9 (Android 17), B = roamlog-b35 emulator,
C = roamlog-c35 emulator (Android 15 x86_64). User requested two emulators because
only one physical phone is available. Real moto g behavior is not covered. Record device/OS, APK hash,
chosen access, timestamps, screenshots and relevant recorder logs for each case.
Keep existing user trips and identities; use a new test group. No destructive app
reset is needed.

| Case | Procedure | Required observation | Result |
|---|---|---|---|
| Managed remove | A creates Managed group; B/C join with Always. A removes C. | B stops sharing with C; C sees removal; creator cannot be removed. Re-admit C and verify sharing resumes. | Passed: removal UI and grant loss; new confirmed admission restored B↔C grants and sharing. |
| Open admit | A creates Open group; B invites C. Repeat with A's group access Hidden. | A receives joined/access notification in first run; none while Hidden. Members shows B as inviter. | Passed: B admitted C; Android notification delivered; added-by B shown; repeat while Hidden emitted no new notification. |
| Offline leave | Keep A/C online, put B in airplane mode. C leaves, receives A's acknowledgement, then goes offline. Bring B online with A. | C's group access stops immediately; A carries signed leave to B; C may forget group after acknowledgement. | Passed: B stopped, C left/forgot after A ack, C stopped, restarted B dropped C through A. |
| Mailbox | A/C alternate airplane mode so they never overlap. B meets A, then A goes offline and C meets B. | C sees A via B with original age. Repeat after restarting B; fix still arrives. | Passed: roles rotated; emulator B origin, Pixel A carrier, emulator C receiver. Pixel QA restarted before C reconnect; exact newer timestamp and via A verified. |
| Block | With all online, A blocks C. Change A's location and reconnect B/C. | A produces no new signed group fixes; B cannot carry A's new position to C. A can still send directly to allowed B. Previously signed copies may remain. | Passed with B origin/A carrier/C blocked: A received new B direct fix, C retained older fix despite A connection. |
| v1 upgrade | Retain v1 test group and direct pairing. Upgrade A first while C stays v1, then upgrade C. | A's old group says Recreate and grants no access; C says Update Roamlog. Direct grants survive. Recreated v2 group pairs successfully. | Passed: actual v1 native APK rejected by both v2 endpoints; upgrade kept direct Always, legacy Recreate shown; RecreatedQA paired with Trips default. |
| Join/access UI | Inspect Managed/Open create controls, creator/member count/policy preview, Hidden/Trips/Always join picker, members menu and badges. | Managed/Always creation defaults; Trips join default; explicit choice persists; non-admin Managed member cannot invite/remove. | Passed: defaults, all three join choices, picker round trips, creator/admin badges, inviter provenance and restricted member actions verified. |
| Withdrawals | Send a group fix, then lower group access, end Trips recording, or choose Off/Pause. Reconnect through B where possible. | Higher-sequence unavailable marker replaces previous position when delivered; no old marker revival after restart. Delivery before closing remains best effort. | Passed: Hidden cleared A via B on C; Trips Pause cleared B on A, stayed clear after A restart. Off delivered on reconnect; immediate close delivery not observed. |

For the initial sharing/relay cases choose Always explicitly on join; otherwise the
Trips default requires active recording. Restore test settings after each case.
Exercise access-picker round trips (Trips → Hidden → Trips on join, Always → Hidden
→ Always on creation) and verify the final choice persists.
For the backdated-admit limitation, verify Remove + people they added removes the
current signed provenance chain; do not claim protection against arbitrary clock
manipulation or recall of copied positions.

## Local run record (2026-11-16)

| Check | Result |
|---|---|
| Full-feature unit/integration tests | 140 passed (104 unit + 21 + 15 integration) |
| Core/sharing Clippy, all targets, warnings denied | Passed |
| ARM64 Android Clippy, warnings denied | Passed |
| Workspace formatting and tracked diff whitespace | Passed |
| Debug Android APK build and signing verification | Passed |
| ADB devices | Pixel 9 connected; two AVDs configured and booted |

Release APK: `target/release/apk/roamlog.apk`.
SHA-256: `5a78776054cda2db6a614b0f76aee45452d69e745950f995a08119fe34428148`.
QA ARM64 SHA-256: `f06bfaaf62ce04cb1eb05b23871766e1ac67c5a743c3333f3d33342072fc198d`.
QA x86_64 SHA-256: `75b5804aee792aaefd2bc460060644f99306475eb64bad933c3d0f3ee5b84a86`.

Isolated QA builds installed on Pixel 9 and both emulators. Matrix above used release native binaries; rendered controls, recorder service,
Android notifications, offline/restart behavior were exercised. Production app has not been replaced or reset.

### Device progress (21:51 UTC)

- Pixel 9 QA upgraded from v1: Legacy-QA renders “Recreate this group”.
- A created Managed group ManageA with Always default. B and C joined using
  separate QR invitations and matching codes on both endpoints. Both previews
  showed creator QA-A, correct current member count, and Managed policy.
- B join access round trip Trips → Hidden → Trips → Always persisted Always
  in device storage. C chose Always; direct permissions remain Off.
- B non-admin group menu has Members/Rename/Leave, no Invite. Creator's
  member action exposes Block only to B; no Remove or Promote.
- Membership gossip created group-only B↔C records without direct pairing.
  C rendered QA-A “via QA-B, 1 min ago”; this proves live relay but not yet
  controlled non-overlap/restart mailbox case.
- A removed C through Members → Remove → Yes. B's group-only C record
  disappeared; C lost all group-derived grants, retaining direct A record Off.
  Re-admission and full removal UI observation subsequently passed.
- Evidence under target/group-sharing-emulators: confirm-*.png,
  members-three.png, removed-c-group.png.

- C removal UI verified: “Removed from group” (removed-c-group.png).
- C re-admission completed with new mutual confirmation and Always access.
- Emulator console GPS injection works on both Android 15 profiles.
- Debug native builds hit existing AccessKit Android empty-text underflow in
  pinned dependency; release builds used for device runs. No dependency patch made.

### Controlled mailbox — passed

Roles rotated: QA-B origin, QA-A carrier, QA-C receiver. C airplane mode plus
Wi-Fi/data disabled; Android reported no default network. B generated newer GPS
fix during isolated QA trip. Phone received timestamp 1794891202657 while C still
held 1794890965781. B QA force-stopped; phone QA force-stopped and restarted.
Only then C reconnected. C received 1794891202657 with via QA-A; B remained
stopped. UI showed “via QA-A, 44 s ago”. Evidence: mailbox-newfix-*.json,
mailbox-poll-*.json, mailbox-after-restart.png. Production app unaffected.

- Offline leave passed: C signed leave while B stopped; C forgot after A ack.
  C then stopped. Restarted B dropped C group-only record through A.
  Evidence: leave-ack-c.png, leave-after-*.json; B final friends.json inspected.
- OpenQA created Open/Always after creator access round trip Always → Hidden → Always.

### Open admission, Hidden, Block

- OpenQA id 6100c2fca039761f16d3a59dac0855f619eed581817398d4cecdb515916c74a6.
  Non-admin B admitted C. Pixel Android notification: “QA-C joined OpenQA.
  They can see your location (Always).” Members shows Added by QA-B.
- A set OpenQA Hidden; C left and rejoined through B choosing Hidden.
  A learned new admission, but Android retained only original C notice id
  653417155; no new joined notification. Evidence notifications-open-qa.txt,
  notifications-hidden-qa.txt and open-members-provenance.png.
- B explicitly granted A Always directly, then blocked C. New GPS while QA
  recording reached A at 1794891722830; C stayed at 1794891639824 while
  connected to A. B record blocked=true; C session gone. Open group relay
  stopped; Managed group A/B remained usable. Evidence block-before/after-*.json,
  block-warning.png, block-no-new-relay.png.

- Emulator host session terminated (exit 143) after Block run; both AVDs
  restarted from persistent disk. Off test had not executed; no result claimed.

### Withdrawals

- A OpenQA Hidden caused C's A marker to become unavailable via B.
- B Off closed sessions; immediate withdrawal on A was not observed. On
  reconnect with During trips inactive, unavailable arrived and cleared marker.
- B resumed isolated QA trip, A received position. B Pause cleared A's B
  marker; restarting A QA retained None. Evidence trips-active-later-a.json,
  trips-paused-a.json, withdrawal-restart-a.json. Delivery before close is best effort.

### Promotion/cascade and compatibility progress

- A promoted B in OpenQA, then removed B + people they added. B retained only
  Managed group grant; C lost OpenQA grants. Persistent Block on B survived C's
  removal. Evidence promoted-b.png, cascade-after-b/c.json.
- For real v1 interoperability, QA A was stopped, v2 metadata backed up, and
  original QA v1 legacy fixture restored with same direct records/identity.
  Direct A→B Always retained. Original v1 native APK installed only on QA package.
- Both v2 emulators published update_required=true against actual v1 endpoint
  (actual-v1-b/c.json). Phone then upgraded to QA v2; direct Always retained and
  Legacy-QA displayed “Recreate this group” (v2-upgrade-legacy.png).
- Legacy row removed; RecreatedQA v2 created. Pairing completed; B retained Trips default in persistent storage.

### Final audit

- Phases 1–3: automated coverage above maps signed membership, authority,
  persistence, replay/forgery rejection, leave/removal delivery, UI consent/defaults,
  relay retention/history gates, direct ordering and bounds.
- Phase 4: README and location-sharing guide updated; adapted device matrix
  passed on Pixel 9 + two Android 15 emulators as requested by user.
- Fresh full suite: 104 + 21 + 15 = 140 passed after device matrix.
- Promotion Admin badge and cascade removal verified; B's unrelated Managed
  grant survived Open removal, C lost all Open grants.
- Upgrade fixture retained same identity and direct A→B Always; both v2
  endpoints set update_required against actual v1. After update, Legacy-QA
  rendered Recreate. RecreatedQA pairing succeeded and B stored trips_only.
- Scope limits: no physical moto g coverage, no battery-duration claim. Receiver
  network isolation used airplane/Wi-Fi/data controls; physical QA force-stop
  preserved production family sharing. Offline-leave B used process stop.
- Observed limitation: Update Roamlog can appear transiently while remote peer
  owns identity exclusively for pairing; location session reconnect clears it.
- Debug-only upstream AccessKit empty-text underflow remains outside this change.
  Shipping/release builds used throughout completed matrix.

### Cleanup and production preservation

All isolated QA processes stopped and global modes Off. C's stopped QA metadata
was reset to Off directly after UI automation failed to open Friends; no production
metadata changed. Test emulator processes shut down after validation.
Production APK SHA-256 matched original phone backup exactly:
494515cf1dcc685d8aac55bf9083e1b882914ea2d8f51c7ff2ad8d608a49a8bc.
Production UI still showed “All trips · 8 trips · 58.07 mi”.
Evidence: final-mode-a/b/c.json, production-apk-after.sha256, production-after-ui.xml.
