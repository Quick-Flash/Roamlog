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
- **Update while moving**: shortest gap between sent positions (15/30/60 s).
  Stationary phones sample every 5 minutes and send at most every 10 minutes.
- **Add friend**: enter your name, tap **Show invitation**. Friend scans the QR with
  phone camera (opens Roamlog), opens shared link, or uses **Scan QR image**. Both
  phones show a code; tap **Codes match** only when codes are identical.
- New direct friends see nothing until you pick **Trips** or **Always** for them.
- **Groups**: create a group (e.g. Family), tap **Invite**, pair as above. Group
  members share with every other member automatically; members you never paired
  with are introduced by other members. **Leave** stops sharing with the group and
  tells members still connected; others learn through members.
- **Trips** on a friend: lists their finished trips. **Add to my trips** copies one
  into your history (e.g. a road trip where you forgot to record).
- **Rename**: local labels for friends and groups; nobody else sees them.
- **Layers → Friend locations** toggles map markers. Gray marker: not live.
- Friend status shows connection path: **direct** or **via relay**.

## Privacy rules

- Permissions are checked on the sending phone before every packet.
- Group membership means Always for each member (still subject to your mode).
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
  refused start at boot).

## Limits

- Serverless does not mean always reachable: relays may be needed.
- Sharing is best-effort. Force-stop, power-off, or battery restrictions stop it.
  After reboot Android may refuse a background location service start; Roamlog then
  posts "Location sharing paused" and resumes when opened.
- **Reset sharing identity** (Friends sheet) makes a new key and deletes friends,
  groups, and received data; everyone pairs again. Use after a lost or exposed key,
  or when identity cannot load. Reinstall also loses identity. No cloud backup.
