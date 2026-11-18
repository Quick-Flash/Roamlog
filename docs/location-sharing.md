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
- **Share for this long**: enter amount and choose Minutes, Hours or Days (up to 365 days). Starts Always
  using existing friend/group grants, then switches globally Off (including
  receiving). Countdown and deadline survive restart. Choosing another mode cancels timer.
- **Battery sharing**: Off by default. Opt in to send battery percentage directly
  to currently authorized friends/group members. Stale/disconnected status disappears;
  battery status is not forwarded through group relay.
- **Friend ⋯ → Try to locate** asks an online, authorized sender for fresh GPS.
  Result says locating, fresh received, offline, or no fresh location. Requests are
  limited to once per minute per person and never override Hidden, Block, timer expiry,
  or inactive Trips. A short GPS watch runs for up to 20 seconds when needed.
  This cannot wake a force-stopped or offline phone.
- **Alert areas**: open Friends → Alert areas → **+ New**. Name the area and
  adjust the circle (10 m–10 km) on the map or with the radius slider. Choose
  **Draw shape** to use the same corner tools as regular areas. **Next: add
  alerts** saves the boundary and lets you choose a friend or a group and Arrive,
  Leave, or Both. A group alert covers every member, including ones who join
  later, except members you mark Skipped. Tap a friend on the map to outline the
  areas that alert about them; tap empty map to hide them. Layers → **Show alert
  areas**, a friend's page and the Alert areas list draw areas with their names;
  the bar under them filters by group or friend. Tap a name on the map to open
  that area for editing.
  Definitions stay on this phone. First observation sets a
  baseline; two distinct, accurate updates at least 15 seconds apart confirm changes.
  Boundary uncertainty, stale/relayed fixes and gaps over five minutes cannot invent
  an arrival. A pending change requests one fresh GPS fix after 15 seconds, subject
  to Try to locate cooldown. Keep connections enabled and grant notification
  permission. GPS/update cadence can delay alerts; this is not a guaranteed safety
  notification service.
- **You** card (top of Friends): tap your picture to choose one; ⋯ changes name,
  update interval while moving (15/30/60 s), removes picture, or resets identity. Stationary phones sample every 5 minutes and send
  at most every 10 minutes.
- **+ Add friend** (Friends tab): tap **Show my QR**. Friend scans the QR with
  phone camera (opens Roamlog), opens shared link, or uses **Scan QR image**. Both
  phones show a code; tap **Codes match** only when codes are identical.
- Each friend and each group has an access dropdown: **Hidden**, **Trips**, or
  **Always**. A person sees the higher of their own access and their groups' access;
  their row says when a group grants more. New direct friends start Hidden; new
  groups offer Always when creating. Join confirmations offer Trips and let you choose.
  ⋯ on a friend holds Their trips, Rename, Block, Remove.
- **Groups**: choose **Managed** (default: only admins invite or remove) or **Open**
  (any member may invite; admins remove). Policy is fixed at creation. ⋯ → **Invite
  someone** pairs through an authorized inviter. Confirmation shows creator, member
  count, policy, and your access choice. Members may introduce people you never
  paired with; each admission is signed. New-member notifications describe the
  access you grant, unless group access is Hidden.
- **Group ⋯ → Members** shows who added each member and admin badges. Admins can
  **Remove**, **Remove + people they added**, or **Make admin**. Members can Block.
  **Leave** stops group access immediately. Signed leave stays available until any
  remaining member acknowledges it, or seven days pass; members gossip it onward.
- Old unsigned groups show **Recreate this group** and grant no access. Remove and
  recreate them. Direct friends retain their permissions. **Update Roamlog** means
  a friend's location protocol version differs; update both phones.
- Friends' finished trips sync automatically on connect and every 6 hours, kept apart
  from yours. **History → Showing** picks whose trips (several people, or you plus a
  group) to see together with their miles, map paths, and road coverage; read-only.
  ⋯ → **Their trips** → **Add to my trips** copies one into your own history.
- **Rename**: local labels for friends and groups; nobody else sees them.
- **Layers → Friend locations** toggles map markers. Group members carry signed
  last-known positions for up to 24 hours. Relayed markers and friend rows show
  **via Sam, 12 min ago**. Gray marker: not live or older than 15 minutes.
- People button (map controls) lists friends with a location; tap one to center the
  map on them. ⋯ → **Show on map** on a friend does the same. Friends list shows a
  search box once you have four or more friends.
- Profile picture: center square of chosen photo, 128 px. Your own map dot uses it;
  friends fetch it on connect and every 30 minutes and show it instead of your dot,
  and in the People list. Remove deletes it; friends drop it on their next fetch.
- Both 2D and 3D maps show own/friend profile pictures, names and stale/relay labels.
- Friend status shows connection path: **direct** or **via relay**.

## Privacy rules

- Permissions are checked on the sending phone before every packet.
- Group access applies to every current member (still subject to your mode).
  Membership is a signed operation log anchored to its creator. Managed groups
  restrict admissions/removals to admins; Open groups let every member invite.
- Signed group fixes may be carried by other members to every current member for
  up to 24 hours. Forwarders and receivers reject fixes predating the recipient's
  current admission. No group key or server mailbox is used.
- Blocking anyone disables relay of your own location for every group containing
  them. Direct sends to other members retain their individual permission checks.
  Copies signed before the block may still be carried; they cannot be recalled.
- Friend trips are shared only with friends who can currently see your location,
  and only while your mode is not Off. Estimated gap segments are not shared.
- Direct positions stay on this phone; signed group positions can be forwarded
  within their group. Only the newest signed position per group/origin is cached,
  for up to 24 hours. Sequence replay protection is shared with direct updates.
- Lowering access, ending a Trips-only recording, or switching Off/Pause signs an
  unavailable marker with a newer sequence. Delivery before closing is best effort;
  members that receive it replace and forward the marker.
  Missing GPS alone preserves the last-known signed fix until retention expires.
- Removing a direct friend clears their individual grant; shared-group grants
  still apply. Use group Members → Remove or Block to end group access. Removing
  a blocked friend locally clears Block, so shared-group grants can resume.
  These changes cannot erase what someone already received.
- A received position authenticates the sender, not the GPS measurement.

## Design

- Identity: Keystore-wrapped Ed25519 key, shared by UI and recorder processes.
- Recorder service process (`:recorder`) runs one sharing endpoint whenever mode is
  not Off, alongside trip recording. Trip GPS feeds sharing; without a trip, Always
  mode uses its own adaptive watch (5 min still, user interval moving).
- Both friends dial each other; connection opened by lower peer ID wins on both
  sides. Dial retry backs off 15 s → 2 min.
- Location: postcard datagrams with sequence numbers and replay cursors.
  Signed group membership and digest acknowledgements, trip requests: reliable QUIC
  streams on the same connection. Location ALPN is `roamlog/location/3`; pairing is
  `roamlog/pair/2`. Group logs live in `sharing/groups/<id>.ops`; relay cache lives in
  `sharing/group-fixes.postcard`.
- UI process reads `sharing/friend-locations.json`, written by the service.
  Pairing runs in UI process; service endpoint pauses during pairing.
- App launch restarts sharing service if mode is not Off (after force-stop or
  refused start at boot). A 15-minute watchdog alarm retries after Android kills
  the service; if Android refuses, it posts "Location sharing paused".

## Limits

- **Platform and connectivity:** sharing is Android only. Each connection needs
  both phones online at the same time. Direct connections are not always
  possible; public Iroh discovery and relays are external dependencies, and
  restrictive networks or outages can prevent sharing. Relays forward encrypted
  traffic; they are not an offline mailbox. Group members can carry each other's
  last-known signed fixes for up to 24 hours even when origin and recipient are
  never online together. Some overlapping path through members is still required.
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
  Hidden setting. Creators choose access with Always preselected; joiners choose
  with Trips preselected. Managed groups require admin admission; Open groups let
  any member invite. Membership changes propagate through connected members, so
  disconnected phones may learn about removal or departure later. A malicious member
  can backdate an admit before their removal; admins can remove their additions
  too. Signed timestamps do not prove when an operation was really made.
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
