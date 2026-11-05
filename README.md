# noberu-engines

Visual novel engines written in Rust for [noberu](https://noberu.kitaaura.com),
built for the browser (wasm32, no Emscripten) and testable natively.

Players bring their own game files. Nothing here contains game data.

## Crates

| Crate | What |
|---|---|
| `vfs` | Range reads over game files: native files now, browser Blobs later. Archives are never loaded whole. |
| `luca-pak` | LUCA System PAK archives: reads only the index, entries on demand. |
| `luca-cz` | LUCA System CZ0-CZ4 images. Vendored from lbee-utils' `cz`, with a faster LZW decoder and the encoder behind the `encode` feature. |
| `tools/luca-dump` | Dev tool: list archives, extract entries, decode images to PNG. |

## Status: LUCA System (Little Busters! English Edition)

- PAK: all 19 archives of the Steam release open; index read takes under 8 ms each,
  including VOICE0.PAK (41,266 entries, 4 GB).
- CZ: all 4,580 images in the image archives decode (CZ0, CZ1, CZ3; 8- and 32-bit).

Next: script decompile and a flag/jump simulator.

```
cargo run --release -p luca-dump -- summary /path/to/game/files
cargo run --release -p luca-dump -- png /path/to/BGCG.PAK '#3' bg.png
```

## Credits and license

GPL-3.0-or-later. Formats follow the reverse engineering in
[lbee-utils](https://github.com/G2-Games/lbee-utils) (G2, GPLv3; `luca-cz` is
derived from its `cz` crate) and [LuckSystem](https://github.com/wetor/LuckSystem)
(wetor, MIT).
