# Sharing follow-ups

Requested features (2026-11-16):

1. Timed sharing: clear expiry/countdown; automatic sender-side expiry survives
   restart. Global deadline overrides every direct/group grant.
2. Try to locate: request fresh GPS from authorized, reachable peer; sender checks
   current permissions and rate-limits requests. Show pending/no response honestly;
   cannot wake force-stopped phone through existing peer-only transport.
3. Arrival/departure notifications: user-defined named circular places; opt-in
   alerts for selected people. Handle GPS uncertainty, stale/relayed fixes, first
   observation and restart without false arrivals or duplicate notifications.
4. Optional battery percentage: explicit sender consent, unknown/old status shown
   honestly, no independent access beyond current sharing grant.

All four features implemented alongside 3D marker fix. Timed sharing is a global
deadline; expiry stops all sending/receiving. Battery status stays direct, optional,
and permission gated. Core and Android target checks pass. QA package remains isolated
from production app; multi-phone pairing could not be completed because QA invite
startup timed out on emulator.

## 3D marker fix

Terrain view previously drew only own blue GPS dot. It now projects own and friend
markers against terrain mesh and reuses 2D friend photo/dot/label visuals. Own photo,
layer toggle, stale styling and relay age are supported. Friends render even when
own GPS is unavailable. Headless regression checks cover marker rendering/reactivity;
projection checks cover different locations, viewport clipping and missing terrain.

## Route sharing clarification

Finished routes sync directly between authorized peers when their devices are
connected. Whole group need not be online simultaneously. Group relay stores only
latest signed position (up to 24 hours), not finished routes; another member cannot
carry routes to offline recipient.
