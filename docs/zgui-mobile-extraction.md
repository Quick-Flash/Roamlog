# Zgui mobile boundary

Roamlog uses reusable Android APIs from Zgui’s
[Android mobile PR](https://github.com/zortax/zgui/pull/50).
Cargo pins exact framework revision; no sibling checkout is required.

Zgui owns activity/window hosting, input, keyboard/insets, accessibility,
permission callbacks, location watches, notifications, foreground-service
lifetime, document pickers, and Java/JNI glue. APIs live under
`zgui::app::android`. Zgui ships prebuilt `android/zgui.dex`; packaging copies
it into APK as `classes.dex`. Roamlog contains no Java source.

Roamlog owns recording policy, journal commands, category/trip state, distance,
road matching, imports/exports, and service notification content. Its
`android_background!` handler records in separate `:recorder` process, allowing
recording to continue when activity exits. Writers synchronize journal with
`flock`; UI catches up on refresh. Recorder lock detects lost service process.

Enlumi camera and tile foundations are preserved under `vendor/enlumi` so fresh
clones build independently. Moving generic mapping into zgui-plot is follow-up;
this release does not introduce another camera/tile implementation.
