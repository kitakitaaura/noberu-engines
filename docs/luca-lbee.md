# LUCA System: Little Busters! English Edition notes

What we know about the Steam release's files and script VM, and how we
know it. "Inferred" means worked out from how the scripts use something,
not from the executable.

## Archives (PAK)

19 archives in `files/`. Index-only reads open each in under 8 ms (VOICE0.PAK:
41,266 entries, 4 GB). Six archives have no names (BGCG, CHARCG, EVENTCG,
FACECG, GENCG, VOICE*): scripts refer to entries by id (`id_start + index`).

## Images (CZ)

All 4,580 images in the image archives decode: CZ0 and CZ3 (32-bit) almost
everywhere, CZ1 in PARTS, 8-bit CZ0 in OTHCG.

## Scripts (SCRIPT.PAK)

169 entries: 156 `SEENxxxx` scenes, `SEEN8500`/`SEEN8501` (battle item
tables, UTF-16 names, not scripts) and 11 `_` data tables (`_VARSTR`,
`_SAYAVOICE`, `_KEYWORD`, ...).

Record: `u16 len, u8 opcode, u8 fixed_flag, params, pad to even`. 107 opcodes
(names from LuckSystem's `data/LB_EN/OPCODE.txt`). Text is UTF-16LE with
Japanese and English side by side; expressions and file names are Shift-JIS.
Jump targets are byte offsets in the scene.

Checked against LuckSystem's decompiler: all 478,968 commands line up, and
every decoded one matches exactly. We decode more: `ONGOTO` (expr + u32 jump
table), `SUB`/`OR`/`AND` (var + exprs), and indexed writes.

**Indexed writes** (inferred): when a write's variable has bit 15 set, the
base is `var & 0x3fff` and an index expression comes first. `0xDFFC` is the
battle HP array `#8188[...]`. EQUN's value is then a u16 after the index.

## Asset ids

Ids are global: each PAK's `id_start` gives it a range (BGCG 1+, EVENTCG
1001+, FACECG 3001+, CHARCG 10001+, OTHCG 25001+, BATTLE 30001+, PT 31001+,
GM 32001+, GENCG 55001+, SYSCG 60000+, SYSCG2 63000+, PARTS 64001+, VOICE2
65000+, MUSIC 100001+, SE 200001+). VOICE0 (1+) is its own namespace: the
first u16 of MESSAGE.

| Command | Params (first bytes) | Checked |
|---|---|---|
| `IMAGELOAD` | u16 mode (0 background, 1 sprite; 0x100 bit seen), u16 image id, sprites then u16 0, u16 x, u16 y (640, 720 = bottom center) | every id resolves except 65000/65001 (plain black/white) |
| `BGM` | u32 MUSIC id; `00000000 xxxx` = stop, fade frames | all play ids in MUSIC.PAK |
| `SE` | u8 0xff, u32 SE id, ... | all 1,859 in SE.PAK |
| `MESSAGE` | u16 voice id (0 = none) | all 41,171 voiced lines in VOICE0 |
| `MOVIE` | ASCII name (`OP00`) | `files/movie/OP00.webm` |

`EQUN (8001, id)` after a background load records the current background.

## Expressions

C-like: `#N`, `#N[e]` (= variable N+e), integers, `|| && | & == != < <= > >=
+ - * / %`, unary `-`/`!`, parentheses. 180 distinct shapes in the game.

## Variables (inferred)

| Range | Use |
|---|---|
| 0-1999 | story flags for this playthrough (route points are 0/1 flags) |
| 6000s | system: date (`#6002` month, `#6003` day), SELECT result `#6001`, minigame state |
| 8000s | display state; battle HP `#8188`/`#8189` |
| 10000s | read by many scenes, never written by scripts: engine/system settings |
| 12000-12999 | persistent between playthroughs: cleared routes, unlocks |
| 20000-20999 | **local to a FARCALL frame** (saved on call, restored on return) |
| 22000s | minigame temporaries |

Why 20000s are frame-local: battle loops count rounds in `#20001` and call
`SEEN8693`, which uses `#20001` for its own RANDOM. With globals the loop
never ends.

Gating flags: `#12006`-`#12010` are set by the Komari, Haruka, Kud,
Kurugaya and Mio clears; `#12024` by Rin's first route (`SEEN2004`); the
Refrain and Sasami branches in `SEEN0528`/`SEEN2700` test them.

## Scenes

| Scenes | Route |
|---|---|
| 0513-0528 | common days (May 13-28) |
| 1000-1004 | Komari |
| 1200-1203 | Sasami |
| 2000-2004 | Rin (first) |
| 2500, 2513-2523, 2600-2603 | Refrain |
| 2800-2888 | Saya |
| 3000-3001, 3900 | Haruka |
| 3600-3601 | Kanata |
| 4000-4003, 4102-4103 | Kud |
| 5000-5006 | Kurugaya |
| 6000-6003 | Mio |
| 4444 | no-route ending |
| 2700 | credits (FARCALLed by each ending) |
| 8xxx | minigames: 8030 baseball game, 85xx-87xx battles |

Endings call `TROPHY` (Steam achievement ids 1-27).

## Minigames in the simulator

The executable runs minigames; scripts wait in loops for its results. The
simulator stubs them: `BTFUNC` ends a battle by setting one side's HP to 0,
`GMFUNC 1/3/4` (baseball) put random results in `#6211`/`#6213`/`#6215` in
the ranges the script tests.

## Tools

```
luca-dump scripts SCRIPT.PAK out/      # decompile, check every jump
python3 -I tools/scripts/diff_lucksystem.py out/ lucksystem-out/
luca-dump sim SCRIPT.PAK 300 7         # random playouts
luca-dump explore SCRIPT.PAK 20000     # breadth-first search over choices
luca-dump campaign SCRIPT.PAK 20000    # repeat it, carrying #12xxx flags
```
