# Contributing

Keep changes focused. Tracking policy belongs in Rust core; Android services
belong in Zgui framework. Preserve source fixes and append-only journal format.
Do not change package IDs or signing keys during ordinary app updates.

```bash
cargo xtask install-hooks  # Once per clone
cargo xtask pre-commit     # Format, then check formatting, core/UI tests, Clippy
```

Hook runs same checks before each commit and blocks failures. Formatter changes
are never staged automatically: review/stage them, then retry commit. Tracked
working files must match index so checks cover committed source. CI runs
`cargo xtask verify` without rewriting files.

For Android changes, build APK and verify demo launch, light/dark appearance,
trip filters, and recording lifecycle. Preserve existing device history.
See [build guide](docs/building.md). UI layout work should follow the
[UI redesign](docs/ui-redesign/README.md) mockups.

Tests should use synthetic GPS tracks or attributed public map fixtures. Never
commit personal journals, device backups, signing keys, tokens, or private route
screenshots. `.private/` and build outputs are ignored.

Bug reports should describe app revision, Android version, steps, expected and
observed behavior. Share synthetic reproductions rather than personal GPS data.
Framework fixes should target existing
[Zgui mobile PR](https://github.com/zortax/zgui/pull/50). Mapping foundations
remain attributed to Enlumi under `vendor/enlumi`.
