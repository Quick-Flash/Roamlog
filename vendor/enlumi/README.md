# Enlumi map foundations

`camera.rs` and `tiles.rs` come from Kevin Plaizier’s
[Enlumi](https://github.com/Quick-Flash/enlumi), commit
`0e299e3be5b27be966fe7d075fbd419483a4753e`, under `src/mapping/`.

Project owner authorizes these files under Roamlog’s Apache-2.0 license.
Enlumi’s original repository uses AGPL-3.0-or-later. This license choice applies
to these copies; it does not change Enlumi’s repository license.

Only reuse change: tile HTTP User-Agent uses compiling package name/version.
Keep map foundations together when syncing upstream changes. Moving generic
mapping into zgui-plot remains separate work.
