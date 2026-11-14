# GPS recovery and map performance

## Matching model

Journal stores submitted GPS fixes. Replay rebuilds distance and coverage when
matching rules or offline map change. Road sections are at most 20 m; coverage
requires evidence over 80% of section length, rather than proximity to one fix.
Repeated trips add distance but do not duplicate cumulative road length.

Trustworthy endpoints up to 180 seconds apart can reconnect across GPS outages.
Weak accuracy fixes retain previous trustworthy anchor. Explicit pauses and
longer outages start separate segments. Inferred distance is labeled in trip
history and GPX exports; original fixes remain available.

Recovery searches offline road graph for gaps up to 5 km, with heading-aware
snapping, connected-route checks, and competing-route rejection. Ordinary GPS
segments use same graph context and bounded multi-fix windows to bridge short
map edges and road-name boundaries. Unresolved gaps do not earn road coverage.
Parallel roads, dense intersections, bridges, and tunnels remain difficult;
matching estimates route, not proof of passage.

## Rendering

Workers rasterize basemap, road overlays, and labels into image tiles. UI retains
images and draws visible tiles. Coarse cached tiles remain beneath requested
sharp tiles. Worker queues discard stale zoom work. Area overlays use same
worker/tile approach to keep geometry work off UI thread.

Frame rate depends on display mode, thermal state, map view, and cache state.
Display refresh rate is not measured rendering throughput. Compare identical
views and gestures with warmed caches; report frame times and missed frames,
not only average FPS. Phone refresh-rate preference remains independent of app
rendering work.

## Verification

`cargo test --locked` covers matching, journal replay, distance accounting,
pauses/outages, competing roads, trip filters, and UI state. Fixtures are public
map data or synthetic paths. `examples/analyze_recovery.rs` inspects local
journals without changing them; never commit personal GPS journals or output.

Android launch and simulated trip rendering have been checked on Pixel 9.
Outdoor distance accuracy, long screen-lock recordings, process termination,
and regional map sizes still need field validation.
