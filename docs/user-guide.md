# Using Roamlog

Map fills screen; trip bar sits at bottom. Create category (**+**), select it, tap **Start**. Grant precise location; pending
start resumes after permission prompt. Android 13+ requests notification permission
once. **Settings (sliders button) → Appearance** explicitly selects Light/Dark independently
of system setting and saves choice. **Pause**
stops GPS; **Resume** starts separate path segment. **Finish** closes trip.
Ongoing notification shows distance and offers Finish. Recorder uses Android's
[location foreground service](https://developer.android.com/develop/background-work/services/fgs/service-types#location)
started from visible app. No background-location permission or Google Play
Services dependency.

## Offline map

**Settings → Offline map** downloads 10, 30, or 80 km square around map center from latest
[Protomaps daily build](https://build-metadata.protomaps.dev/builds.json) with HTTP
range requests (like `pmtiles extract`), after showing download size. New areas
merge into `files/basemap.pmtiles`, so earlier areas stay. Areas over ~20k z15
tiles or 1 GiB are refused. Build files are hosted by Protomaps for downloading
extracts; a widely distributed app should host its own. **Import file** (in same
dialog) imports existing [PMTiles](https://docs.protomaps.com/pmtiles/) file
(v3, MVT, gzip), replacing current map.
Offline map replaces online street tiles and needs no network in **Map** mode while view
stays inside downloaded area. Beyond it, visible vector tiles stream from latest Protomaps
build (HTTP range requests) and render in same style; **Offline** chip stops those
requests. Road coverage still uses downloaded map only.

Zoom keeps nearest cached coarser tile under each sharp tile (no gray while new images
decode), uses finer tiles from up to two levels when zooming out, and prepares coarse
backdrop (two levels up over 2× view, plus one-or-two-tile overview five levels up)
requested before sharp tiles, so pinch zoom shows blurry map instead of gray. Queued
renders for levels view has left are skipped. Labels
render after base tiles.
**Layers → Satellite** optionally shows Esri World Imagery beneath transparent
downloaded roads/labels and trip/coverage overlays. Imagery loads online and uses
provider-separated tile cache; **Offline** reuses cached imagery only. **Map**
restores regular background. Background choice is saved independently of theme.
Satellite colors remain unchanged in dark mode. Imagery attribution stays visible.
**Layers** switches **GPS paths** and **Traveled roads** independently:
GPS only, roads only, both, or neither. Choices persist locally and respect trip/
category selection. GPS uses thin solid lines, inferred gaps use dashes, and
traveled roads use wide solid lines; controls and legend do not depend on color.

Tiles render on CPU (tiny-skia) into images with Organic Maps' day/night colors,
road widths per zoom, and label colors (see NOTICE). Labels render as separate
transparent tile layer above traveled/untraveled road overlays, so names stay readable:
place names and road names
along straight stretches, using system font (Android Roboto). Labels crossing tile
edges are dropped. Zgui canvas paths use coverage rasterizer on Mali-G715, too
slow for map geometry, hence images. Nearby zoom level prefetches near switch
points; cached sharper tiles stand in while zooming out.

Two-finger twist rotates map (twists under 19° while pinching are ignored); compass button
appears while rotated and returns north up. Locate button follows live position while app is visible (network and GPS providers,
not journaled); dragging stops following.

## Trip history and path views

Each **Start** creates separate saved trip with category, GPS path, distance, and
elapsed recording time. **Pause / Resume** continue same trip; **Finish** closes
it. Trips survive restarts. Lifetime totals sum individual trip distances, so
repeated routes still add miles or kilometers.

**History → Trips** selects all trips, one user-created category (walking,
biking, motorcycle, etc.), or uncategorized trips. Selected count and distance
appear in sheet and on map badge; lifetime totals stay visible separately.
**Show on map** fits selected routes. **Show only** beside old trip isolates that
trip and fits its route. **All trips** restores combined history. Trip list follows
selection, while category totals remain totals across all saved history.

Map's blue paths and green traveled-road sections follow history selection.
All-trip road coverage is rebuilt from saved GPS history when returning to
**All trips**; filtering never changes recordings, recording category, or totals.
Background choice and Light/Dark selection continue to work in every history view.

## Road coverage

Coverage roads come from offline map's road layer at its max zoom (z15), loaded
only for tiles near GPS history. Streets and paths count; service roads,
sidewalks, crossings, rail, and ferries don't (sidewalks parallel to streets would
make matches ambiguous). Lines clip to tile bounds so tile buffers aren't counted
twice; named roads share one id across tiles.

Basemap shows roads; green sections mark traveled roads, blue paths show recorded
GPS, and amber dashes show inferred gaps. Drag/pinch
to navigate; **Fit** resets extent. Matching divides centerlines into sections
no longer than 20 m, accumulates nonoverlapping travel evidence, and marks section
after 80% coverage. Matching scores overlapping portions of GPS/map segments;
fast travel across short map edges and road-name boundaries stays continuous.
Outage endpoint snapping uses heading to distinguish nearby crossing ramps.
Road matching uses reported GPS accuracy and nominal road-class width allowance.
It matches ordinary GPS segments too, carrying road endpoint through successive
fixes. Bounded multi-fix windows resolve local ambiguity without crossing pauses;
recorded GPS and observed mileage stay unchanged. Established-road
context resolves nearby frontage-road ambiguity only while that road stays nearest
(within 1 m); context resets across disconnected or unresolved segments. Otherwise
nearby parallel roads and incompatible headings remain unclaimed. This is provisional geometric matching: bridges, tunnels, GPS drift,
and dense roads need further validation. Entire street isn't marked from one block.

Journal retains original submitted fixes, so changing map or matching rules
recomputes distance and coverage without losing source data. Weak fixes (accuracy
worse than 30 m) retain last trustworthy anchor. Good endpoints up to 180 seconds
apart reconnect across outages; intervening weak fixes or gaps over 10 seconds
label incoming segment and distance as inferred. Longer gaps and explicit pauses
remain separate segments. Isolated jumps over 100 m/s are rejected without
replacing anchor; stationary jitter adds no mileage.

For inferred gaps up to 5 km, offline road graph can replace straight estimate
with road path. Ambiguous snaps, disconnected roads, and competing routes remain
unresolved: straight estimate stays amber and earns no road coverage. Matched
inferred paths earn road coverage and remain labeled inferred. Trip detail shows
inferred subtotal; GPX extensions preserve estimated/road-matched flags. These
are estimates, not proof of route taken. Turning GPS off during recording waits
for automatic recovery; permission/service failures still interrupt recording.

## Areas, exploring and stats

**Areas → Draw new area** draws polygon: tap map to add corners (tap near edge splits it), drag dot to
move, tap dot to remove; drag elsewhere still pans; **Undo** or Back steps back. **Done**
measures every offline-map road inside (up to ~100 km across, streamed tile by tile) and
reports traveled length ÷ total road length, per road class and named streets (done ≥90%,
started, total). Areas under ~20 km across also show untraveled roads in red, rendered
into image tiles on worker threads so panning stays at display rate. **Edit** reshapes
shown area; saved area can be updated or saved as new. Each side of divided road is separate map road, so one direction covers
half. Percentage follows Trips selection. **Save area…** keeps it; **Areas** lists saved
areas with last measured percentage.

Signpost map button highlights closest untraveled road from live position (or map
center), preferring roads never traveled, within 1.5 km. **History → Stats** shows lifetime and road totals,
distance per month or week for current selection, and category totals. **Layers → Color by trips on road** colors traveled
roads by trips that covered them (1, 2, 3–4, 5+). Tapping traveled road shows its name,
trip count and first-traveled date. Finished trips announce and list road length they
traveled before any earlier trip ("new roads").

History trip **Edit** changes category, trims start/end, splits finished trip in two,
or deletes it.
Edits are journal events; recorded fixes stay in journal. Trimmed/split recording time is
estimated from GPS times. **Settings → Data → Back up** saves journal (trips, categories, edits,
areas); **Restore** validates picked backup, keeps current journal as
`trips.jsonl.before-restore`, and replaces it. Restore needs no active trip.

## Persistence

`files/trips.jsonl` in private app storage is append-only event journal. Every
accepted command flushes to storage before state changes. Startup replays journal,
discards incomplete trailing write, reports corruption in committed lines, and
pauses interrupted recording at last observation. App doesn't silently bridge
process death. `files/basemap.pmtiles` stores offline map. Categories start
empty. No account, remote sync, or GPS track uploads. Online basemap requests
Protomaps vector tiles or optional Esri imagery for visible area; provider can
infer viewed area. **Offline** uses cached imagery without requests; **Retry**
appears when tiles fail.
On GrapheneOS, allow Network in app permissions. Map error offers **Settings**
shortcut; Network toggle is managed by OS settings rather than standard runtime
permission prompt.
Uninstall removes local data. **Trips** sheet lists trips (show on map, delete
finished trip, km/mi units, road coverage near history) and exports all trips as
GPX via Android save dialog. Deletion is journaled as event, so replay matches.
