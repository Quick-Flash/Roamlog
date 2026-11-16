# Location sharing

Optional, encrypted, phone-to-phone location sharing. No account, no Roamlog server.
Android only. Connections use [Iroh](https://www.iroh.computer/) QUIC with its public
discovery and relay servers; relays forward encrypted packets and cannot read them.

## Use

Open **Friends** on map.

- **Share my location**: Off, During trips, or Always. Off closes all sharing
  connections. During trips sends only while recording; friends' positions still
  arrive. Always sends in background. Any mode other than Off keeps a foreground
  notification with **Pause sharing**.
- **You** card (top of Friends): tap your picture to choose one; ⋯ changes name,
  update interval while moving (15/30/60 s), removes picture, or resets identity. Stationary phones sample every 5 minutes and send
  at most every 10 minutes.
- **+ Add friend** (Friends tab): tap **Show my QR**. Friend scans the QR with
  phone camera (opens Roamlog), opens shared link, or uses **Scan QR image**. Both
  phones show a code; tap **Codes match** only when codes are identical.
- Each friend and each group has an access dropdown: **Hidden**, **Trips**, or
  **Always**. A person sees the higher of their own access and their groups' access;
  their row says when a group grants more. New direct friends start Hidden; new
  groups start Always. ⋯ on a row holds Their trips, Rename, Block, Remove (or Invite,
  Rename, Leave for groups).
- **Groups**: create a group (e.g. Family), ⋯ → **Invite someone**, pair as above.
  Members you never paired with are introduced by other members. **Leave** stops sharing with the group and
  tells members still connected; others learn through members.
- Friends' finished trips sync automatically on connect and every 6 hours, kept apart
  from yours. **History → Showing** picks whose trips (several people, or you plus a
  group) to see together with their miles, map paths, and road coverage; read-only.
  ⋯ → **Their trips** → **Add to my trips** copies one into your own history.
- **Rename**: local labels for friends and groups; nobody else sees them.
- **Layers → Friend locations** toggles map markers. Gray marker: not live.
- People button (map controls) lists friends with a location; tap one to center the
  map on them. ⋯ → **Show on map** on a friend does the same. Friends list shows a
  search box once you have four or more friends.
- Profile picture: center square of chosen photo, 128 px. Your own map dot uses it;
  friends fetch it on connect and every 30 minutes and show it instead of your dot,
  and in the People list. Remove deletes it; friends drop it on their next fetch.
- Friend status shows connection path: **direct** or **via relay**.

## Privacy rules

- Permissions are checked on the sending phone before every packet.
- Group access applies to every member (still subject to your mode).
  Any member can admit new members; only join groups of people you trust.
- Friend trips are shared only with friends who can currently see your location,
  and only while your mode is not Off. Estimated gap segments are not shared.
- Received positions stay on this phone only, newest per friend, for 24 hours.
- Remove/Block stop future updates; they cannot erase what friend already received.
- A received position authenticates the sender, not the GPS measurement.

## Design

- Identity: Keystore-wrapped Ed25519 key, shared by UI and recorder processes.
- Recorder service process (`:recorder`) runs one sharing endpoint whenever mode is
  not Off, alongside trip recording. Trip GPS feeds sharing; without a trip, Always
  mode uses its own adaptive watch (5 min still, user interval moving).
- Both friends dial each other; connection opened by lower peer ID wins on both
  sides. Dial retry backs off 15 s → 2 min.
- Location: postcard datagrams with sequence numbers and replay cursors.
  Group member lists, trip requests: reliable QUIC streams on same connection.
- UI process reads `sharing/friend-locations.json`, written by the service.
  Pairing runs in UI process; service endpoint pauses during pairing.
- App launch restarts sharing service if mode is not Off (after force-stop or
  refused start at boot). A 15-minute watchdog alarm retries after Android kills
  the service; if Android refuses, it posts "Location sharing paused".

## Limits

- **Platform and connectivity:** sharing is Android only. Both phones need network
  access at the same time to exchange data. Direct connections are not always
  possible; public Iroh discovery and relays are external dependencies, and
  restrictive networks or outages can prevent sharing. Relays forward encrypted
  traffic; they are not an offline mailbox for missed updates.
- **Background availability:** force-stop, power-off, or battery restrictions can
  stop sharing. After reboot Android may refuse a background location service
  start; Roamlog then posts "Location sharing paused" and resumes when opened.
  The 15-minute watchdog retries service recovery, but cannot guarantee a restart
  or uninterrupted delivery. Pairing temporarily pauses the service endpoint.
- **Update frequency and accuracy:** the moving interval (15/30/60 s) is a target,
  not a promise of a new location at each interval. Movement filtering, GPS
  availability, Android scheduling, and network conditions affect updates.
  Stationary phones sample every 5 minutes and send at most every 10 minutes.
  Datagram delivery is not guaranteed, and reconnection retries back off from
  15 seconds to 2 minutes. An authenticated sender can still provide an inaccurate
  or fabricated GPS position.
- **Old positions:** a marker is the latest received fix, not proof that someone
  is still there or reachable. Fixes older than 15 minutes are marked stale;
  only the newest position per friend is retained for up to 24 hours. Location
  sharing does not provide a continuous history of a friend's movements.
- **Permission scope:** During trips sends only while recording; Off also stops
  receiving. A group grant can give someone access despite their individual
  Hidden setting. New groups default to Always, and any member can admit others.
  Leaving-group notifications propagate through connected members, so disconnected
  phones may learn about the departure later.
- **Copies cannot be recalled:** blocking, removing, leaving, or reducing access
  stops future sharing under those permissions; it cannot erase data already
  received or copied. The 24-hour location retention rule does not expire received
  finished trips, screenshots, exports, or copies kept by someone else.
- **Trip and profile sync:** finished trips sync on connection and every 6 hours,
  not continuously. Trip access follows current location permission and requires
  sharing to be enabled; there is no independent trip-only access setting.
  Estimated gap segments are excluded. Trip responses are limited to 16 MiB, so
  oversized transfers can fail. Profile pictures refresh on connection and every
  30 minutes; changes and removals may remain visible until the next fetch.
- **Reset sharing identity** (Friends sheet) makes a new key and deletes friends,
  groups, and received data; everyone pairs again. Use after a lost or exposed key,
  or when identity cannot load. Reinstall also loses identity. No cloud backup.
