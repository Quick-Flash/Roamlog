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

3D mode reuses Enlumi’s adaptive quadtree with split/merge hysteresis,
parent-tile handover, shared terrain mesh/skirt indices, and retained GPU
textures/materials. Vector styling, labels, and path overlays rasterize on
workers; orbiting updates uniforms without rerasterizing those tiles. Surface
producer runs on worker, bounds in-flight work to one submission, and rebuilds
after device replacement. Tile requests skip work no longer wanted by view.

Contact moves coalesce once per display frame, so two-finger input does not
present a half-updated pan/pinch between contacts. Camera frames reuse retained
scene; selection, scene assembly, and download demand update at most once every
50 ms, including final drag position. Identical camera/scene updates do not
queue another GPU frame. Terrain tile uniforms share one retained staging
buffer and one GPU queue write per frame.

Satellite imagery LOD follows projected device pixels independently of terrain
mesh LOD. Cached fine tiles remain eligible after zooming out in both 2D and
3D. Coarser or evicted raw imagery does not overwrite sharper composite;
source-quality metadata lives as long as cached composite. Tile budgets still
bound retained detail and request work.

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

### Two-finger pan check

One paired Pixel 9 demo run used same 360 two-contact moves over satellite
terrain, with Android instrumentation and existing Zgui latency trace. Measured
`f.begin` → `f.painted` during gesture, excluding startup and warm-up:

| UI-frame work | Before | After |
| --- | ---: | ---: |
| Mean | 8.40 ms | 5.44 ms |
| 95th percentile | 16.45 ms | 10.69 ms |
| Maximum | 49.97 ms | 70.65 ms |

This measures framework frame work, not physical display FPS or complete
finger-to-display latency. Input-injection handoff time stayed roughly unchanged.
Average/tail frame cost improved in this pair; isolated stalls remain and need
further GPU/upload analysis. Raw traces and device screenshots stay private.
