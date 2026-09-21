# Plan — small, independently shippable PRs

The governing pattern, applied to every component below:

> **Extract first, unify second.** PR *n* lifts the shape into a component
> while passing each call site its *current* numbers, so the render is
> pixel-identical and the diff is provably safe. PR *n+1* collapses those
> numbers to one set, and that is the PR the owner looks at.

That splits every item into a refactor nobody needs to review visually and a
one-screen decision he can say yes or no to. It also means a look change can be
reverted without losing the component.

Sequencing constraint: the other agent is on `light.slint`, the TV screen,
`panel.rs`, `main.rs` and `theme.slint` in a separate worktree. Half of that
**landed while this audit ran** — PR #282 (`gui/lift-speed-pass`, `95a94a3`)
added `ui/components/device_disc.slint` and put the device row and the light
screen on it. Titles beyond the light screen have not been unified yet, so
Phase 1 still waits for the rest of that branch. Phase 0 touches none of those
files except `theme.slint` (additions only) and can go first or in parallel.

---

## Phase 0 — free, no design decisions

### PR 1 · "tokens where tokens already exist"  — *pure refactor, pixel-identical*
- Replace the fifteen radius literals that equal a token: `14px`→`T.r-row` ×4,
  `12px`→`T.r-card` ×4, `26px`→`T.r-disc` ×2, `16px`→`T.r-overlay` ×2,
  `3px`→`T.r-glyph` ×3.
- `room_devices.slint:212` `x: 20px` → `T.pad-side`.
- Add `T.r-tile: 18px` (5 sites) and `T.r-track: 6px` (6 sites); substitute.
- Add `T.pad-inline: 18px` — the card inner padding used at 15 sites across 5
  files and named nowhere (inventory section 13). Nearest existing token is
  `pad-side` at 20px, so this is a new name for a value already in use, not a
  change to any pixel.
- Replace the seven `160ms` animation literals with `T.fade`, which is exactly
  that value and is referenced by nothing. Leave `chooser.slint:83` and
  `settings.slint:433` at 140ms but **ask**: two of the five moving rings run
  20ms faster than the other three and no comment says why. That reads as drift.
- Decide the five dead tokens (`bg-sleep`, `surface-press`, `r-glyph`, `fade`,
  `volume-hold`): `r-glyph` and `fade` get wired up here (`fade` = the seven
  `160ms` literals); `bg-sleep`, `surface-press` and `volume-hold` get a
  one-line comment saying what they are for, or get deleted.

**Pictures that change: none.** Every substitution is value-for-value.
Note `light.slint:81`'s `26px` literal is already gone — #282 replaced that disc
with `DeviceDisc`, which uses `T.r-disc`. One fewer to do.
**Guarded by:** `lights.rs:3874` pins the room row's `(20, 440, 14)`; the
player goldens would flag any accident on the cinema side.

### PR 1b · the home hub joins `DeviceDisc`  — *near-pixel-identical, tiny*
#282 put the device row and the light screen on the shared disc but not the
**home hub's room row** (`home_hub.slint:330-345`), which is still the same
drawing by hand. Fold it in. The only difference to resolve is that the hub
centres the glyph by arithmetic where `DeviceDisc` pins it at (14,14) — for a
52px disc and a 24px glyph those are the same number, so the render does not
move. Worth doing straight away: the hub's rows are the *other* place a lift
starts, so this closes the same loop #282 opened.
**Pictures that change:** none. **Guarded by:** nothing renders the hub today
(see the coverage gaps below) — this is the cheapest possible first customer
for the hub render test.

### PR 2 · "four small wrongs"  — *visible, but each is a fix*
- `src/main.rs:66` `BACKGROUND: 0x09090b` → `0x15130f` (`T.bg`). Affects the
  first frames after boot only.
- `src/panel.rs:138` `IRIS_RING` → read `T.accent` instead of hardcoded white,
  so the iris ring matches the row ring under a non-white accent.
- `room_devices.slint:217` add the missing `font-weight: 600` to the room
  page's scenes-card title.
- `tv.slint:148` the 44×44 close button's `border-radius: 30px` → `22px`.

**Pictures that change:** the room page (one word gets its weight back), the
boot frames, the TV tray close button under nothing that is currently
photographed. **Guarded by:** nothing renders these; review by eye.

---

## Phase 1 — `ScreenHeader`, and the lift it unblocks

This is the point of the exercise: six screens open from a row and only one
can be lifted, because only `light.slint` exports its header geometry.

### PR 3 · `ScreenHeader`, light screen only  — *pure refactor, pixel-identical*
Create `ui/components/screen_header.slint` carrying the back button, title,
subtitle, icon disc and state line, plus the `title-box-*` / `disc-box-*` /
`header-h` functions moved verbatim out of `light.slint:204-213`. Instantiate
it in `light.slint` with that screen's exact numbers (back at y26, title y20,
subtitle 16px at y56, disc centred at y94). `app.slint:331-338` forwards from
the component instead of from the screen.

**Pictures that change: none.**
**Guarded by, and this is the strong one:**
`the_control_screen_opens_as_a_window_out_of_its_row`
(`ui/couch-gui/src/lights.rs:3805-4358`) rebuilds real `Lift` geometry from
these exact functions and asserts endpoints, reversibility, "nothing pops",
"no ghost" and "nothing stronger than the card", twice (top row and a scrolled
mid-list row). If a box moves, it fails. Also
`the_light_screen_is_one_screen_for_every_shape_of_device` (`lights.rs:3629`).

### PR 4 · `ScreenHeader` on the TV and packaged-device screen  — *look change*
Swap `tv.slint:83-85` for the component; export its geometry.
**Visible:** title 20px/600 → 26px/600; back button y22 → y26; subtitle
stays 16px but moves y58 → y56. The bare 52px accent image at (32,146) and the
"DENON AVR" kind label stay where they are for now — moving them is PR 5's
question, not this one's.
**Pictures that change:** `core-1-power.png`, `core-2-input.png`,
`theater-avr.png` and any TV screenshot. **Guarded by:** nothing visual
(`tv.rs`/`tv_plugin.rs` assert key routing and text only; `COUCH_CORE_SCREENSHOTS`
dumps but never compares). Take a before/after pair by hand.

### PR 5 · `ScreenHeader` on thermostat, camera, activity pages, player  — *look change, needs his eye*
Four screens, one component. **Visible:**
- thermostat title 21 → 26; the hardcoded `"Home Assistant"` becomes a property
  fed from Rust;
- camera title 21 → 26; `"UniFi Protect"` becomes a property and moves from
  (24,95) into the header at (88,56);
- activity pages: the device line moves from (24,86) into the header, title
  width stops being a fixed 270px;
- player: title 19 → 26, subtitle 15px `#cbcfc8` → 16px `T.text-secondary`.

**Open question for the owner, stated here and not decided:** does the TV /
player / activity header get an icon disc, or does the disc only appear where a
device has a state to show (light, blind, thermostat)? `variants.html` draws
both.
**Pictures that change:** `controls.png`, player pictures 01-29 (header band),
every thermostat/camera shot. **Guarded by:** the player's 29-picture golden
will flag the header band on pictures 01-29 — regenerate the reference set.

### PR 6 · the lift learns the other screens  — *the payoff*
- `lift_geometry` (`src/main.rs:147-207`) takes the geometry of *whichever*
  screen is opening, through one trait-ish set of `invoke_*` calls rather than
  the `ls_*` set.
- `LIFT_CARD_TO` (`src/panel.rs:162`) is read from `ScreenHeader`'s own card
  rect rather than being the constant `(20, 14, 440, 76, 12)`.
- `screen_pending` (`src/lights.rs:1630`) and the `Opening::Lift` arm
  (`main.rs:1688-1697`) widen so a TV row, a thermostat row and a packaged
  device row lift out of their row the way a light does.
- Screens with no bar cards pass an empty card list; the lift already handles
  "nothing to reveal" (`LIFT_CARDS_IN` simply has nothing to fade in).

**Pictures that change:** none statically; four new transitions exist.
**Guarded by:** extend `the_control_screen_opens_as_a_window_out_of_its_row`
with a second subject — a packaged-device screen — reusing the existing
`pops`/`ghosts`/`stronger` closures. That is the single highest-value test
addition in this whole plan and it is maybe 40 lines.

---

## Phase 2 — the rest, roughly in value order

### PR 7 · `RowCard` extraction  — *pure refactor, pixel-identical*
`home_hub.slint:318-404, 429-469`, `room_devices.slint:144-220`,
`chooser.slint:152-200`, `settings.slint:7-47, 280-301`. Each call site keeps
its own fill/radius/height. **No focus ring inside the component** — the ring
stays one-per-pane, positioned by the page's arithmetic
(`docs/slint-notes.md`: three rings cost a 452×654 frame per move).
**Pictures: none.** **Guarded by:** `lights.rs:3874` and the lift test.

### PR 8 · `RowCard { state }` unification  — *look change*
Device rows recess when off or unavailable, the way room rows already do
(`surface-idle`, `border-idle`, `text-quaternary`). The room scenes card adopts
the hub's arrow layout.
**Visible and, in my view, the second-best change on this list**: today
"Reading lamp · Unavailable" and "Desk lamp · On · 40%" have the same card
(`room-packaged-children.png`).
**Pictures that change:** every room-page screenshot.
**Guarded by:** `lights.rs` room tests assert text rows only — add a `draw_room`
pixel-count assertion, or just review the dump from `COUCH_ROOM_SCREENSHOTS`.

### PR 9 · `cinema.slint` onto the theme  — *look change*
The 17 colour literals (inventory §17a) → tokens. Two violet panels become warm
grey; frozen white text starts following the owner's accent/text colours.
**Pictures that change:** all 29 player goldens.
**Guarded by:** the text transcript `tests/golden/player-screen.txt` will *not*
change — so CI stays green and proves the change is purely visual. Regenerate
the reference PNGs and compare two sets by eye:
```
COUCH_PLAYER_SCREENSHOTS=/tmp/player-before cargo test -p couch-gui --lib \
  media_player::tests::the_player_screen_is_the_same_picture_for_the_same_speaker \
  -- --exact --nocapture
# apply the PR, then the same into /tmp/player-after, and diff the directories
```

### PR 10 · selection becomes `FocusRing` everywhere  — *look change, also a perf win*
Replace the five `border-width: 3px : 0/1px` flips (`tv.slint:12, 154`,
`cinema.slint:14, 108`, `activity_pages.slint:26`) with the existing outside
ring. Keeps the documented rule that a state change must be colour, not
geometry.
**Pictures that change:** `core-1-power.png`, `core-3-tray.png`, `controls.png`,
player sheet pictures 06-09 and 17-22.

### PR 11 · `Sheet` + `SheetList`  — *look change*
One tray list for the TV, the player and the thermostat (inventory §9). Move
the thermostat's tray-height arithmetic (`thermostat.slint:45`) with the pitch.
**Pictures:** `core-3-tray.png`, player sheets, thermostat modes.

### PR 12 · `IconButton` + `TextButton`  — *look change*
Retire `cinema.slint`'s `Control`; fold the activity pager's buttons, "Pages",
"End" and the busy "Cancel" in. The three grey buttons in `app.slint:695-717`
stop being grey.
**Pictures:** player transport row (02-05), `controls.png`, any screen with an
activity running.

### PR 13 · `Tile`  — *look change, blocked on an owner decision*
TV tiles (210×120, r18) and activity tiles (210×150, r16) become one. **Ask
first**: the two grids hold 4 and 6 tiles in the same column, so the height
choice changes how many fit on `controls.png`.

### PR 14 · `SectionLabel` everywhere  — *look change, tiny*
Eleven call sites (inventory §2). Wrap each in a plain `Rectangle` where it
sits in a `VerticalLayout`, or the layout silently overwrites its `x`.

### PR 15a-f · the type ladder  — *look change, one screen per PR*
Eight size tokens on `Theme`; then hub, room page, light, TV, player,
settings/keyboard, one PR each. **Last on purpose**: by this point PRs 3-14
have collapsed most call sites into components, so the ladder lands in six
files instead of twenty. Note in the PR description that
`setup.slint:118-121`'s "a size used once costs the whole charset" argument is
stale under SDF (`docs/slint-notes.md:72-100`) — only the *largest* size still
moves the payload, so the mid-range consolidation is free.

---

## What exists to catch regressions

| test | where | what it would catch |
|---|---|---|
| `the_player_screen_is_the_same_picture_for_the_same_speaker` | `src/media_player.rs:1547-1818` | 29 pixel goldens of the player screen, **opt-in** via `COUCH_PLAYER_GOLDENS=<dir>`; always asserts the text transcript `tests/golden/player-screen.txt`. CI (`.github/workflows/runtime.yml:105-127`) sets only `COUCH_PLAYER_SCREENSHOTS` and uploads the pictures as a 14-day artifact — **no pixel gate in CI** |
| `the_control_screen_opens_as_a_window_out_of_its_row` | `src/lights.rs:3805-4358` | endpoints equal page A/B; close is frame-for-frame the reverse of open; no frame >90% flat; "nothing pops" (a 32×16 patch doing >40% of its total delta in one frame); "no ghost" left in the row; "nothing stronger than the card". Run twice, top row and scrolled. Pins `(20, 440, 14)` at `:3874` |
| `the_light_screen_is_one_screen_for_every_shape_of_device` | `src/lights.rs:3629-3801` | eight text fields across five device shapes + a "not a flat screen" check. Dump with `COUCH_ROOM_SCREENSHOTS=<dir>` |
| room-list tests (`draw_room`) | `src/lights.rs:2942-3020` | text rows (`"Desk lamp - On · 40% ›"`) |
| compositor unit tests | `src/panel.rs:2157-2792` | 14 tests of `iris_frame`/`lift_frame`/`Sprite`/`Window` on synthetic buffers |
| `tests/runtime_fonts.rs` | — | glyph presence at 19px Regular and 62px Bold, Cyrillic + temperature symbols |
| single-shot dumps | `COUCH_CORE_SCREENSHOTS`, `COUCH_APPLE_SCREENSHOT`, `COUCH_SONOS_SCREENSHOT`, `COUCH_UPDATES_SCREENSHOT`, `COUCH_LIFT_SCREENSHOTS` | nothing — they write, they never compare |

## Where coverage is missing, and the cheapest fills

`CouchPlatform::install` (`src/panel.rs:2126-2145`) is **generic** — `App` is
one root and every screen is a child gated by an `*-shown` property, so
rendering any screen is `app.set_<x>_shown(true)` then draw. Adding a test is
cheap; nobody has.

Screens with **no rendering test at all**: Home Hub (the actual room list!),
Camera, Setup, NoConfig, MicOverlay, PairOverlay, BtPairOverlay, Keyboard,
WifiSetup, Settings proper.

In the order I would add them, each ~40 lines against the existing harness:

1. **Home hub** — a `draw_hub` mirroring `draw_room`: three rooms (one idle,
   one offline, one on), an activity strip, a scenes card; assert the text rows
   and non-blankness. PRs 7, 8 and 14 all touch it and nothing would notice.
2. **Extend the lift test to a second screen** — needed by PR 6 anyway, and it
   turns `ScreenHeader` from "reviewed by eye" into "asserted".
3. **A light-screen pixel golden** mirroring the player's, so PR 3's
   "pixel-identical" claim is machine-checked rather than asserted by me.
   `COUCH_ROOM_SCREENSHOTS` already writes the five pictures; a
   `COUCH_ROOM_GOLDENS` compare arm is ~15 lines copied from
   `media_player.rs:1596`.
4. **The three overlays** (mic, pair, bt-pair) — one render each, assert the
   card's tone pixel and the word. PR 7's `NoticeCard` lands blind otherwise.
5. **Turn on the player pixel gate in CI**, or at least add a step that
   downloads the previous run's `player-screen` artifact and diffs it, so PRs 9,
   10, 11 and 12 fail loudly rather than quietly.
