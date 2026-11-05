# Little Busters! EE minigames: what the scripts tell us

The minigames run inside the executable (packed, so no code to read). Each
one is driven by script commands the engine answers (`BATTLE`, `BTFUNC`,
`GMFUNC`, `PTFUNC`, `PT`, `LBFUNC`, `TASK`) and by variables both sides read
and write. Goal (decided 2026-11-05): close copies, rebuilt from the real
game's behaviour, using its art and the scripts' inputs and outputs. Battles
first.

## Battles

Scenes: `SEEN8530` (main loop), `8540`, `8580`, `8590` (commentary, 9,444
commands), `8600`/`8601`, `8610`, `8620`/`8621`, `8691`-`8695`, and per-day
entries `8750`-`8761` (called from the common route).

Art: BATTLE.PAK, 419 named images: `BT_WP_ICON*` (137 weapon icons),
`BT_VS_*` (VS screen: names, faces, rank, background), `BT_FC_CG*` (fighter
face cut-ins per character), `BT_RK_BD_*` / `BT_TOP_*` (ranking board),
`BT_GU_BTN`, `BT_STA_SIGHT`, `BT_EFC_*` / `BT_WPH_EFC` (effects).

Flow in `SEEN8530`, as far as the script shows it:

1. Setup (`seen8540`), fighters' stats into `#325`/`#326`/`#316`, two
   `BTFUNC` calls that load each fighter.
2. Reset: time `#6167 = 24`, attacker/defender `#6226 = 0` / `#6227 = 1`,
   HP-like pairs `#8152`-`#8161` (even = fighter 0, odd = fighter 1),
   per-fighter state `#6170`-`#6175`, `#6236`-`#6243`, `#6260`-`#6278`,
   event flags `#331`-`#342`.
3. `BATTLE 1` opens the screen; each round swaps `#6226`/`#6227`
   (`seen8621` steps), `BTFUNC 0` runs a move (engine), the script then
   prints commentary (`BATTLE 102` lines: "Masato attacks!", "Critical
   hit!", "Unsuccessful leg sweep!", per-weapon lines) and checks for the end.
4. The fight ends when a fighter's `#8188[i] <= 0` (HP), time `#6167 <= 0`,
   or `#312` is set; `#309 = 1` marks "over". `BATTLE 101` lines announce
   "X is victorious!" / "X falls!".
5. Results go into the ranking (`#6451[...]` ranks, `#6136`/`#6153`) and
   story flags.

`BATTLE` command codes seen: 1-10 (screen setup), 100-114 (messages; 102 =
commentary line with jp/en text, 101 = result line), 200-252 (effects,
cut-ins, ranking board). `BT_ACCESSORY_SELECT` is the weapon pick.

What the scripts don't say, and needs the real game:

- the weapon pick screen: how weapons are offered and chosen, and each
  weapon's effect on stats;
- how `BTFUNC 0` picks a move and computes damage; whether the player has any
  input during the fight (timing, guard: `BT_GU_BTN`);
- HP bar and time display, cut-in timing, how long each line stays up;
- the ranking board between battles.

## Batting practice (PT.PAK)

`PTFUNC 0` (23 calls) and `PT` with Shift-JIS arguments (`#6003`, day
numbers). Art: field map (`PT_MAP`), radar, bat, fielder faces (`PT_F_*`),
score board, rest menu. Real-time; almost entirely engine-side.

## Baseball game, May 26 (GM.PAK)

`SEEN8030`, `GMFUNC 1`-`30` and `100`. The script reads each step's result
from `#6211` (pitch), `#6213`, `#6215` (outcome) and counts outs/innings
itself. Art: player cut-ins (`GM_CG*`), score board, batting order, run icon.

## Saya's labyrinth

`SEEN2804`-`2808`, the final arc `2810`-`2819`. `TASK 45` is one move; the
engine puts the tile in `#10003`/`#10004`; `LBBG` sets the corridor
background; `LBFUNC n` engine checks (result in `#20001`). Days 3-5 add
puzzle counters `#6336`-`#6349` and Saya's position `#6073`/`#6074`.
Game over is `TASK 54` "REPLAY / GAME END" (result `#6005`).

## Reference needed

Recordings of each minigame in the real game (Steam release), with saves
just before them, so timing and rules can be matched frame by frame.
