Roamlog — Peer-to-Peer Location Sharing Implementation Plan

Implementation status (2026-11-15): Phases 1–5 implemented in main app, Phase 6
partly (adaptive GPS, reconnect backoff), Phase 7 partly (see below). Groups, friend
trip viewing, and trip import added beyond original plan. Usage and design:
[location sharing guide](location-sharing.md). Earlier phase notes:
[prototype](sharing-prototype.md), [pairing](sharing-pairing.md),
[identity](android-sharing-identity.md), [probe](android-sharing-probe.md).

Verified on Pixel 9 (Android 17) and moto g stylus 2023 (Android 14): Keystore
identity tests, QR/link pairing with matching codes, two-way live sharing, map
markers, UI closed + screen locked, Wi-Fi→LTE, both on LTE, airplane-mode recovery
(~10 s), auto-resume after reinstall, group join/leave, trip view/import, rename.
Reboot (Moto, Android 14): boot event arrives ~1 min after unlock; Android refuses
background location service start, Roamlog posts "Location sharing paused", and
opening app resumes sharing.

Not yet done: 8-hour stationary and 2-hour driving battery tests, OS process-kill
recovery, notification "Pause sharing" tap on device (Android files ongoing silent
notification in collapsed shelf), external security review.
Path measured (Friends list shows it): direct on same Wi-Fi, Wi-Fi↔LTE, and LTE↔LTE.
Identity reset added (Friends → Reset sharing identity); device test pending.

1. Project goals
   Extend Roamlog with optional, encrypted, peer-to-peer location sharing between friends, without requiring users to create accounts or depend on a centralized Roamlog server.

The system should support Google Maps-style continuous location sharing while preserving Roamlog's local-first design.
Core requirements
- Serverless architecture: Every phone acts as a peer, capable of sending and receiving locations.
- 24/7 sharing: Optionally continue sharing when Roamlog is not visible, including when the screen is off.
- Trip-only sharing: Share location only while recording a trip.
- Per-friend permissions: Independently control who can see your location.
- End-to-end encryption: Location data must not be readable by relay operators.
- Low battery consumption: Adapt GPS and network activity to movement and user preferences.
- Offline resilience: Reconnect automatically and show the last known location when a peer is unavailable.
- No accounts: Pair devices using QR codes or invitation links.
- Cross-platform core: Keep the networking and sharing logic in Rust, with Android-specific services handling platform integration.
- Local-first: Road recording and trip history must continue working without internet access.
  Non-goals for the initial release
  Group chat, messaging, sharing full trip histories, cloud backups, and location-sharing through a browser should be deferred. These could be added later without changing the core architecture.
2. Proposed architecture
   Phone A

Roamlog UI

Rust sharing manager

Iroh P2P endpoint

Local storage


Phone B

Roamlog UI

Rust sharing manager

Iroh P2P endpoint

Local storage



Direct encrypted connection

Optional Iroh discovery / relay

Used for connectivity when direct connections cannot be established

No plaintext location storage


The architecture consists of four primary components.
Component	Responsibility
Android location service	Collect GPS positions and manage background execution
Rust sharing manager	Decide when and with whom to share
Iroh networking layer	Establish encrypted connections and exchange messages
Roamlog UI	Display friends, sharing controls, and map markers

Important architectural distinction: Iroh can make this serverless at the application level, but internet connectivity may still require discovery and relay infrastructure. Fully infrastructure-free operation should be supported on compatible networks, not promised universally.
3. Integration with the existing Roamlog codebase
   The repository currently has a Rust/Zgui application, an android/ integration layer, a src/ directory, and a local append-only trips.jsonl journal. It also already supports a GPS foreground service and offline maps.
   GitHub


The implementation should build on those existing capabilities rather than introduce a separate tracking system.

I recommend this proposed module structure, subject to matching the existing source layout:
src/
sharing/
mod.rs
identity.rs        # Device identity and keys
peers.rs           # Friends and permissions
protocol.rs        # Network message definitions
transport.rs       # Iroh integration
manager.rs         # Sharing state machine
location.rs        # Update filtering and scheduling
storage.rs         # Friends and settings persistence

ui/
friends.rs         # Friend management
sharing.rs         # Sharing settings
friend_markers.rs  # Map overlays

android/
...                 # Existing location service
...                 # Background networking lifecycle
...                 # Notification and permission handling


Avoid duplicating GPS collection. Both trip recording and location sharing should consume a common location-update source.
Suggested data flow
Android location provider
|
v
Shared GPS event stream
|
+----> Trip recording
|      (existing behavior)
|
+----> Location sharing manager
|
+--> Sharing policy
+--> Movement filtering
+--> Peer authorization
|
v
Iroh transport
|
v
Friends' devices


This separation matters because a user should be able to share their location without recording a trip, or record a trip without sharing their location.
4. Development phases
   Implementation roadmap
   Estimated engineering time

1

Networking proof of concept

2–4 days

Connect two devices with Iroh, exchange encrypted messages, and test Wi-Fi-to-cellular connectivity.


2

Identity and friend pairing

2–4 days

Generate persistent device identities, implement QR pairing, trust verification, and friend removal.


3

Location-sharing protocol

2–3 days

Add location messages, permissions, timestamps, update policies, and reconnect handling.


4

Map and friends interface

2–4 days

Display friends as markers, show location freshness, and expose per-friend sharing controls.


5

24/7 Android integration

5–10 days

Integrate location services, persistent networking, notifications, permissions, and process recovery.


6

Battery and reliability optimization

4–7 days

Implement adaptive updates, reconnection backoff, power testing, and real-world cellular testing.


7

Security and release validation

3–5 days

Test revocation, identity changes, malformed messages, app restarts, and privacy guarantees.



These estimates represent roughly 20–37 engineering days, not calendar days. The largest uncertainty is Android background behavior across different devices.
5. Detailed technical requirements
   Phase 1 — Networking
   Use Iroh for P2P connectivity.

Implementation tasks:
- Initialize an Iroh endpoint using a persistent device identity.
- Establish a connection between two devices.
- Implement a versioned application protocol over QUIC.
- Exchange test messages in both directions.
- Test same-LAN, different-Wi-Fi, and cellular-to-cellular connections.
- Support connection establishment through relays when direct NAT traversal fails.
- Handle lost connections, network switching, and exponential reconnection backoff.
  Acceptance criteria: Two Android phones can exchange authenticated messages over different networks, including cellular, without a Roamlog account or a Roamlog-operated server.

Relay-assisted connections are acceptable. Direct-only operation is not required.
Phase 2 — Identity and pairing
Each installation receives a cryptographic identity.

Use a secure random keypair and store its private key in encrypted app storage, protected using Android Keystore where appropriate.

A friend invitation should contain:
struct FriendInvite {
version: u8,
peer_id: PeerId,
display_name: String,
// Optional connectivity hints
}


The exact Iroh identifier types should be selected from the version integrated into Roamlog.

Pairing workflow:
1. Alice opens Friends → Add Friend.
2. Roamlog displays a QR code or shareable invitation.
3. Bob scans the QR code.
4. The devices establish an encrypted connection.
5. Both users confirm the pairing.
6. Each chooses what location access to grant.
   Pairing must not automatically enable location sharing.

Friend records should store identities, display names, permissions, and the latest received location separately from trip data.
Phase 3 — Location protocol
Use a small, versioned binary protocol. Postcard or another compact serialization format would work well.

Example:
struct LocationUpdate {
sequence: u64,
timestamp_ms: i64,
latitude_e7: i32,
longitude_e7: i32,
accuracy_cm: u32,
speed_cm_s: Option<u32>,
heading_cdeg: Option<u16>,
}


Using integer coordinates avoids unnecessary floating-point serialization and provides sufficient precision.

Required protocol messages:
Message	Purpose
Hello	Protocol version and connection setup
LocationUpdate	Send current position
LocationRequest	Request latest permitted position
SharingStatus	Indicate sharing availability
PermissionUpdate	Update local sharing authorization state
Ping	Optional connectivity check

All incoming messages must be authenticated, size-limited, validated, and checked against the current authorization policy.

Use sequence numbers and timestamps to reject stale or out-of-order updates.

Important: A location received from another phone is not proof that the device is physically there. The protocol authenticates the sender, not the GPS measurement.
Phase 4 — Friend map integration
Reuse Roamlog's existing map rendering.

Each friend marker should show:
- Friend name or nickname.
- Last known position.
- Location age.
- Accuracy radius, when useful.
- Optional heading and speed.
- Whether sharing is currently active.
  Illustrative friends panel
  UI concept


Alex

Updated 12 seconds ago

Live



Sam

Last seen 24 minutes ago

Offline


Show friends on map



Friend markers should be a separate overlay, independently toggled from trip paths and traveled-road coverage.
Phase 5 — Background Android support
This is the most difficult phase.

Roamlog already has a GPS foreground service for trips. The sharing implementation should extend or coordinate with that service rather than creating competing location collectors.

Required capabilities:
- A user-enabled location-sharing foreground service.
- Appropriate foreground-service type and location permissions.
- Optional background-location permission where the lifecycle requires it.
- Persistent notification with a Stop Sharing action.
- Restoration of sharing preferences after process restart.
- Controlled recovery after device reboot, subject to Android restrictions.
- Reconnection when connectivity returns.
- Clear behavior when the OS force-stops the application.
  A foreground service improves reliability but does not guarantee uninterrupted execution. Force-stop, permission revocation, aggressive battery management, and some reboot scenarios can interrupt sharing.

Acceptance criteria: On tested devices, sharing continues when the UI is closed, the screen is locked, and the network changes, without requiring the user to reopen Roamlog during normal operation.
Phase 6 — Battery optimization
Do not maintain high-accuracy GPS polling simply because sharing is enabled.

Start with a configurable policy:
State	Initial update target
Stationary	Every 5–15 minutes
Walking	Every 30–60 seconds
Driving	Every 10–30 seconds
Live-follow mode	Every 3–10 seconds
No connectivity	Queue only the latest position

These are initial engineering targets, not guaranteed Android delivery intervals.

Use movement thresholds, batched updates where appropriate, and avoid transmitting identical positions repeatedly.

The app should distinguish location sampling frequency from network transmission frequency. A GPS update need not always generate a network message.

For continuous sharing, prefer event-driven location delivery over periodic wakeups where possible.
Phase 7 — Security and privacy
Location sharing is sensitive, so these are release-blocking requirements.
- Sharing is disabled by default.
- Pairing does not grant location access automatically.
- Permissions are enforced locally on the sending phone.
- A friend can be blocked or removed immediately.
- Revocation stops future authorized transmissions; it cannot erase previously received locations.
- Relays cannot decrypt location payloads.
- Location history is not shared unless explicitly enabled in a future feature.
- Friends' locations are stored separately from personal trip history.
- Stored friend locations have a defined retention policy.
- Network logs must not include coordinates or secret keys.
- Identity recovery, lost devices, and key rotation have documented behavior.
  I would also include a Pause All Sharing control directly in the app and its persistent notification.
6. Recommended sharing settings
   Proposed settings interface
   Interactive mockup

Location sharing mode
Off

Only while recording trips

24/7 adaptive sharing

Continuous live sharing



Moving update interval30 seconds



More frequent

Less battery use



[x] Show friends on map
[x] Allow encrypted relay connections
These controls illustrate the proposed behavior; they do not change Roamlog.
Copy example configuration
7. Testing strategy
   The following scenarios should be tested before calling the implementation reliable:
   Test	Expected result
   Both phones on same Wi-Fi	Direct P2P works
   One phone on cellular	Direct or relayed connection
   Both phones on cellular	Connection succeeds when relay reachable
   Phone screen locked	Sharing continues under supported service conditions
   Roamlog UI closed	Sharing continues if enabled
   Android process killed	Recovers when Android permits restart
   App force-stopped	Sharing stops; no guarantee of automatic recovery
   Airplane mode toggled	Reconnects when network returns
   GPS disabled	Shows unavailable/stale location
   Friend removed	Future updates denied
   Device reboot	Recovers according to platform restrictions
   8-hour stationary test	Measures actual battery consumption
   2-hour driving test	Measures accuracy, delivery latency, and battery

I would use two physical Android phones for networking tests, since emulators won't reproduce real cellular NAT or battery behavior reliably.
8. Important limitations to document
   There are three guarantees Roamlog should not make.

First, serverless does not mean universally reachable. When both phones are behind restrictive NATs, a relay may be necessary. Users who disable relays should expect some connections to fail.

Second, 24/7 sharing is best-effort. A phone that is powered off, disconnected, force-stopped, or prevented from running by Android cannot provide live updates. Unlike a centralized service, Roamlog will not have an always-available copy of its latest location unless you deliberately introduce replicated storage.

Third, encrypted transport is not the entire privacy model. Iroh's authenticated transport protects connections, but application-level permissions, secure key storage, metadata exposure, and stale-location handling still need to be designed and tested.
9. Release milestones
   Milestone A — P2P prototype

Two phones connect, pair, and exchange a current location. No background operation required.



Milestone B — Usable friend sharing

Friend management, map markers, permissions, and trip-only sharing work reliably.



Milestone C — Persistent location sharing

24/7 mode, adaptive GPS, foreground service, and network recovery are implemented.



Milestone D — Stable release

Real-world battery tests, security review, process-recovery testing, and documentation are complete.



10. Final implementation recommendation
    I would make Iroh the initial transport, Rust the source of truth for permissions and sharing state, and Android responsible only for platform-specific lifecycle and location services.

One adjustment I'd make to the implementation order is to test Android background networking early, immediately after establishing the first Iroh connection. If maintaining a P2P endpoint in a foreground service proves unreliable on your target devices, that discovery should happen before investing heavily in the UI.

The first concrete development task should be:
Implement a minimal Iroh transport in Roamlog that connects two Android devices using persistent identities and exchanges authenticated, versioned messages. Verify direct and relayed connectivity across Wi-Fi and cellular before integrating location data.

Once that works, the rest can be developed incrementally without compromising Roamlog's existing offline trip recording.

The eventual result would be a local-first, account-free alternative to Google Maps location sharing, with the explicit tradeoff that peer availability—not a central cloud database—determines how fresh a friend's location can be.
