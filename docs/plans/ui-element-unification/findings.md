# Findings — deliberate difference or drift, and what to unify

Ranked by (inconsistency the owner would notice) x (code removed) / (risk).
`T.` = `Theme.`. Already in hand by the other agent, and **not** re-proposed here:
(a) screen titles unifying on `T.device-name`, (b) the row icon disc and the
control-screen header disc becoming one component.

**Item (b) landed while this audit was running** — PR #282 (`gui/lift-speed-pass`,
merged at `95a94a3`) added `ui/components/device_disc.slint` and put the device
row and the light screen on it. Two notes follow from reading it:
the **home hub's** room-row disc (`home_hub.slint:330-345`) was *not* included and
is still a separate copy; and the light screen's disc lost its `accent-bg` fill in
the process, so an on lamp's badge is now the same colour as an off one's — that
may well be intended (the disc is now the row's disc, and the row never tinted it),
but the owner should be told rather than discover it.
Item (a) has **not** landed: the TV, thermostat, camera, player and activity titles
are still 20/21/21/19/26, so item 1 below stands as written.

Every proposal below is checked against `docs/slint-notes.md`:
no new focus rings that read a changing property, nothing animated that a
layout depends on, no gradient without a `clip:true` parent, no blur or shadow,
and — where it applies — a state change moves one element rather than restyling
many.

---

## 1. `ScreenHeader` — six drawings of the same band  **[top]**

**Verdict: ACCIDENTAL, and it is now load-bearing.**

Inventory section 1 has the table. Summarised as offsets from the top-left of the panel:

```
            back      title                     subtitle                  icon
light       24, 26    88, 20   26/600  W-112    88, 56   16  secondary     52 disc, centred, y94
tv          24, 22    88, 27   20/600  W-112    88, 58   16  secondary     none (bare 52px image at 32,146)
thermostat  24, 22    88, 27   21/600  W-112    88, 59   16  "Home Assistant" literal
                                                                           80 disc at 200,120
camera      24, 22    88, 27   21/600  W-112    24, 95   16  "UniFi Protect" literal
cinema      24, 22    88, 24   19/600  W-120    88, 52   15  #cbcfc8       none
activity    24, 22    88, 24   26/600  270px    24, 86   18  secondary     none
```

Nothing carries meaning. The only defensible difference is the light screen's
`y:26` back button, whose comment (`light.slint:74-75`) says it is *"centred on
the pair of lines rather than on the title alone"* — but every other screen also
has a pair of lines and centres on the title. So even that one is drift; it is
2 px and the owner will not miss it.

**Why this is now the top item.** The lift is wired to exactly one screen. In
`src/main.rs:1665-1698`, the `Opening::Lift` arm calls `lift_geometry(&app, row)`
(`main.rs:147-207`), which reads eleven `ls_*` functions that exist only in
`ui/screens/light.slint:204-239`. A TV, a thermostat, a packaged device and a
media player are all opened from exactly the same kind of row and get no lift at
all. And the destination the card rises to is not read from any screen — it is
`LIFT_CARD_TO: (20, 14, 440, 76, 12)` hardcoded in `src/panel.rs:162`, a
rectangle that corresponds to no `Rectangle` in any `.slint` file.

**Proposal.** One component:

```
ScreenHeader {
    in title, subtitle: string;
    in icon: image;
    in state: idle | active | unavailable;   // drives the disc, as the other agent's IconDisc does
    in has-icon, has-state-line: bool;
    callback back();
    // The same eleven boxes light.slint exports today, exported once here:
    public function title-box-x/y/w/h(), disc-box-x/y/size(), header-h();
}
```

Every screen instantiates it at (0,0,W,header-h) and forwards
`title-box-*`/`disc-box-*`/`header-h` up through `app.slint`. `lift_geometry`
then takes the **screen that is opening** rather than always the light screen,
and `LIFT_CARD_TO` becomes a read of `ScreenHeader`'s own card rect instead of a
constant. Renderer rules: the header is static furniture, nothing in it
animates, and the disc's `colorize` animation (180ms, already there on the row
disc) is a colour change, not geometry.

**Call sites that switch:** `light.slint:76-90`, `tv.slint:83-85`,
`thermostat.slint:23-25`, `camera.slint:11-13`, `cinema.slint:61-63`,
`activity_pages.slint:19-21`.

**What the owner would see.** Titles on the TV, thermostat, camera and player
screens grow to 26px/600 (from 20, 21, 21, 19). The player's subtitle goes from
15px `#cbcfc8` to 16px `T.text-secondary`. The activity-pages device line moves
from (24, 86) up into the header at (88, 56) — that is the most visible single
change, and it is an **improvement**: it stops the packaged-device screen being
the one page whose second line is orphaned below the header band. The back
button moves 4px on the light screen. The thermostat's "Home Assistant" and the
camera's "UniFi Protect" become real properties fed from Rust — today a
thermostat driven by anything else still says Home Assistant, which is wrong,
not merely inconsistent.

**Risk: medium.** It touches six files the other agent is also in (`light.slint`,
the TV screen, `theme.slint`). Sequence it *after* their branch lands.
**Uncertain:** whether the owner wants a disc on the TV / player / activity
headers at all, or only where a device has a state to show. What would settle
it: put the six headers side by side at real size (see `variants.html`) and ask.

---

## 2. `cinema.slint` does not use the theme at all

**Verdict: ACCIDENTAL. It is the oldest screen and it predates the tokens.**

17 of the 51 colour literals in the whole UI are in this one 147-line file
(inventory section 17a). Two of them — `#35303e` (the selected tray row) and
`#30263e` (the message card) — are **violet**; every other surface in the product
is a warm brown-grey. They are visible in `scratchpad/player/after-sheet-*.png`:
the selected "TV" row in the Sources sheet is a different family of colour from
the "Fios" row in the identical sheet on the TV screen
(`scratchpad/core-3-tray.png`).

Worse than the mismatch: `T.accent` is **user-settable**
(`src/home.rs:289-294` reads it from the saved config and derives `accent-bg`
as 15% accent over `bg`). Every `#ffffff` in `cinema.slint` — the screen title,
the track title, the tray title, the tray row title, the message text, the
selected primary-button edge — is frozen white and will **not** follow the
owner's chosen accent or text colour, while the identical element on the TV
screen will. The same is true of `IRIS_RING = 0xffff_ffff` in `src/panel.rs:138`.

**Proposal.** Straight substitution, no new component:
`#111311`->`T.bg`, `#222623`->`T.surface-raised`, `#242823`->`T.surface-raised`,
`#ffffff`->`T.text` (title/body) or `T.accent` (edges), `#f6f6f1`/`#f1f2eb`/
`#eceee8`->`T.text`, `#cbcfc8`/`#d2d5ce`/`#c7cec3`/`#c0c7bc`/`#b7beb2`->
`T.text-secondary`, `#ffffff15`/`#ffffff35`->`T.border-raised`,
`#151319`->`T.bg`, `#35303e`->`T.accent-bg`, `#30263e`->`T.surface-raised`.

**What the owner would see.** The player screen's background warms from
`#111311` to `#15130F`. Body text warms from near-white to `#F2EDE4`. The two
violet panels become warm grey. **Improvement**, and it is the change that makes
a chosen accent colour actually work on the player.

**Risk: low-medium.** This is the one screen with a real pixel golden
(`the_player_screen_is_the_same_picture_for_the_same_speaker`,
`src/media_player.rs:1547-1818`, 29 pictures). All 29 will change, so the
reference set has to be regenerated — but CI only asserts the *text* transcript
(`tests/golden/player-screen.txt`), which will not change at all, so the PR gate
stays green and the owner can compare two artifact sets by eye. That is exactly
the workflow this golden was built for.

---

## 3. Selection: nine drawings, five of which flip geometry

**Verdict: ACCIDENTAL, and it breaks a documented house rule.**

`FocusRing` (`ui/components/focus_ring.slint:13-29`) exists, is correct, and
carries a comment explaining *why* off is a transparent border and not a
zero-width one: *"The width is geometry, and a geometry change claims a larger
dirty region than a colour change."* That is the same rule
`docs/slint-notes.md` states as **"never express as geometry what can be
expressed as colour"**.

Five selection treatments ignore it and flip `border-width` directly on the
element (inventory section 7): `tv.slint:12` (`Tile`, 3px<->0px),
`tv.slint:154` (tray row, 3<->0), `cinema.slint:14` (`Control`, 3<->1),
`cinema.slint:108` (tray row, 2<->0), `activity_pages.slint:26` (tile, 3<->1).
Each flip resizes the element's border box, which is a layout-visible change on
a grid of up to eight tiles.

Three more use a fill change (`thermostat.slint:50`, `light.slint:177`,
`keyboard.slint:535`) and one draws a separate outline rectangle
(`cinema.slint:77`). And the *thickness* is 2px or 3px depending on the file,
against `T.ring-width`'s 3.

**Proposal.** Two named treatments and nothing else:
- `FocusRing` (accent, outside the box, 3px) for anything the D-pad moves onto.
  Tiles on the TV screen, the activity page and the two tray lists all qualify.
- `Selected` as a **fill + edge colour change only** (`accent-bg` fill, `accent`
  edge, both at a fixed 1px border) for a *latched* state — the keyboard's
  shift/mode keys, a blind's chosen button, the thermostat's current mode. This
  is the case where the thing stays marked after focus leaves.

Where a tile is both focused and latched (the blind buttons, `light.slint:188`)
it gets both, as it does today.

**What the owner would see.** Tiles on the TV and packaged-device screens stop
growing a 3px inset edge and instead gain the same outside ring the room rows
have — so "what OK will press" looks identical on every screen. Tray rows the
same. **Improvement**, and it is the one item on this list that also makes
frames cheaper: on `core-1-power.png`'s two-tile screen, a selection move today
dirties two tile rectangles *and* re-lays-out their contents; with the ring it
moves one band.

**Risk: low.** Covered by nothing today (the TV and activity-page screens have
key-routing tests but no pixel checks), which cuts both ways: no test will
break, and no test will catch a mistake either.

---

## 4. Nine spellings of the all-caps label

**Verdict: mostly ACCIDENTAL. Two are deliberate.**

`SectionLabel` (`ui/components/section_label.slint:3-7`, 15px / ls 2.1 /
`text-quaternary`) is used at **one** call site. Six other places write the same
three properties out by hand, four of them exactly right and two subtly wrong:
`room_devices.slint:213` — the room page's own "SCENES", sitting directly below
the hub's "SCENES" in the navigation — is 15px / **ls 1.8** / **text-secondary**,
i.e. a different colour from the identical word one screen up.
`wifi_setup.slint:21` has no letter-spacing at all.

Deliberate: the status bar's title (`status_bar.slint:37-50`) is `T.accent` on
purpose — it is the one line always on screen and the comment says so; and the
light screen's in-card captions are 11-12px because they sit under a 62-68px
bar and a 15px word would not fit (`light.slint:118-121, 155-168`).

**Proposal.** `SectionLabel { size: eyebrow | caption }` — `eyebrow` = today's
15/2.1/quaternary, `caption` = 12/1.5/secondary for in-card use. Replace
`room_devices.slint:213`, `settings.slint:262-273`, `keyboard.slint:353-359`,
`setup.slint:29-35, 72-78, 89-95`, `no_config.slint:32-38`,
`wifi_setup.slint:21`, `pair_overlay.slint:25-31`,
`bt_pair_overlay.slint:33-39`, `thermostat.slint:29`, `tv.slint:88`.
**Mind `docs/slint-notes.md`**: a `SectionLabel` inside a `VerticalLayout` has
its `x` overwritten by the layout's padding and nothing says so — the hub wraps
it in a plain `Rectangle` for this reason (`home_hub.slint:418-426`). The
replacements must do the same.

**What the owner would see.** The room page's "SCENES" dims from `#A39C90` to
`#6E675C` and its tracking widens 0.3px, matching the hub. The setup screen's
three labels grow 14->15px. The two pairing overlays' labels dim from secondary
to quaternary and tighten 2->2.1px. All four are **improvements**; none is
larger than a hair.

**Risk: very low.** No coverage anywhere, but also nothing can go structurally
wrong.

---

## 5. `RowCard` — five list rows, one shape

**Verdict: the *shape* differences are ACCIDENTAL; the *content* differences are DELIBERATE.**

Room row, device row, chooser row, settings row and the two scenes cards are all
the same object: a full-width card, `T.surface`, 1px `T.border`, a title at
21 or 26 / 600, an optional second line, an optional trailing affordance
(inventory section 5). What legitimately differs is what goes in the trailing
slot (status column, chevron, value + arrow, active dot) and the minimum height
(90 for a room or device, which carries a 52px disc; 72 for a chooser or
settings row, which does not). What does not legitimately differ:

- The **scenes card is drawn twice** — `home_hub.slint:429-469` and
  `room_devices.slint:214-220` — and the second copy **drops `font-weight: 600`**
  and pins its arrow at an absolute `(W-40, 26)` instead of laying it out. Two
  screens away from each other, the same card, one of them lighter. Pure drift.
- Radius is `T.r-row` (14) for a room/device row and `T.r-card` (12) for a
  chooser/settings/scenes row. `room_devices.slint:151` even switches between
  them per row. That distinction does carry meaning today — an activity row is a
  "raised" card and a device row is a "flat" one — but it is expressed twice,
  once as radius and once as `surface-raised` vs `surface`, and only the second
  is legible on the panel.
- The **idle/recessed treatment is applied to room rows and not device rows**.
  `home_hub.slint:321-323, 354, 361` recesses the whole card when a room is
  idle; `room_devices.slint:148-150` never does, though the disc inside it does
  (`:182-183`). Look at `room-packaged-children.png`: "Reading lamp ·
  Unavailable" has the same card as "Desk lamp · On · 40%". That is the
  strongest *state* inconsistency in the product.

**Proposal.**

```
RowCard {
    in state: normal | idle | raised;   // fill + edge + radius + title ink, together
    in title, detail: string;
    @children                           // the trailing slot
}
```
No focus ring inside it — the ring stays where it is, one per pane, positioned
by the page's own arithmetic. That constraint is non-negotiable
(`docs/slint-notes.md`, "three focus rings, one per section": three rings cost a
452x654 frame on every move).

**Call sites:** `home_hub.slint:318-404, 429-469`, `room_devices.slint:144-220`,
`chooser.slint:152-200`, `settings.slint:7-47, 280-301`.

**What the owner would see.** The room page's scenes card gains weight 600,
matching the hub's. An unavailable or off device row recesses the way an idle
room row does — a visible, and in my view overdue, improvement. Everything else
is pixel-identical.

**Risk: low-medium.** `lights.rs:3874` asserts `(from.x, from.w, from.r) ==
(20, 440, 14)` on the room row, so the radius must stay 14 there or the lift
test fails loudly — which is the good kind of coverage. The `pops`/`ghosts`
checks in `the_control_screen_opens_as_a_window_out_of_its_row`
(`lights.rs:3805-4358`) exercise the device row's real geometry every run.

---

## 6. `Meter` — four horizontal bars, four sets of numbers

**Verdict: ACCIDENTAL.**

| where | height | radius | track |
|---|---|---|---|
| `volume_overlay.slint:63-73` | 10 | `T.r-meter` (5) | `border-raised` |
| `mic_overlay.slint:72-84` | 12 | 6 | `surface` |
| `tv.slint:114-116` | 5 | 2.5 | `surface-icon` |
| `cinema.slint:74-76` | 5 | 3 | `#ffffff35` |

The two 5px ones are the *same object* — a media progress line — drawn twice
with different radii and different track colours, on two screens that show the
same kind of content. `T.r-meter` is used by one of the four.

**Proposal.** `Meter { in progress: float; in weight: fine | thick; }` —
`fine` = 5px / `T.r-meter` / `T.surface-icon` (media progress),
`thick` = 10px / `T.r-meter` / `T.border-raised` (feedback card, mic).

The two **vertical** bars on the light screen (`light.slint:103-116, 135-153`)
stay separate: they are 62-68px wide, fill from the bottom, are deliberately
barely-rounded (`6px`, with a comment explaining a pill reads as a dragged
knob), and the colour one needs its `clip:true` parent because *the software
renderer does not round a gradient*. Their geometry is also exported for the
lift (`light.slint:220-237`). Leave them alone. If anything is shared it is the
`6px` literal, which should be a token if it stays.

**What the owner would see.** The mic meter thins 12->10px and its track lightens
to `border-raised`. The player's progress track changes from translucent white
to `surface-icon` and its corners from 3 to 2.5px. Both **neutral**.

**Risk: low.** The player goldens will change (pictures 02-05, 10-14, 29).

---

## 7. `NoticeCard` — the mic and Bluetooth cards are the same drawing, twice

**Verdict: ACCIDENTAL, and it is verbatim duplication.**

`mic_overlay.slint:39-67` and `bt_pair_overlay.slint:41-74` are the same
component written out twice: 84px tall, `T.surface-raised`, `T.r-card`, a 2px
border in a `tone` colour, a centred `HorizontalLayout` with a 22px dot in
`tone` and a 26px/700 letter-spaced word. They differ only in the outer layout's
`spacing` (20 vs 18) and `padding` (34 vs 28) — neither of which is deliberate.
`pair_overlay.slint:33-49` is the third instance, with a 1px accent border
instead of 2px `tone` and a 48/700 PIN instead of a 26/700 word.

Their three scrims are `#000000E0`, `#000000F0`, `#000000F5` — 88%, 94%, 96%
black. Nobody chose three.

**Proposal.**

```
NoticeCard { in tone: color; in word: string; in emphatic: bool; }
ModalScrim { }   // one alpha
```
Plus `T.scrim: #000000F0` as a token. The tray's `#00000088`
(`bottom_tray.slint:21`) stays separate — it is a sheet over a live page, not a
takeover, and 53% vs 94% is a real distinction.

**What the owner would see.** Nothing, except the pair overlay's background
going 1% lighter and the mic's 6% darker. **Neutral**, ~55 lines removed.

**Risk: very low.** Zero coverage on any of the three (no render test sets
`pair-shown`, `bt-pair-shown` or `recording`), so also zero safety net.

---

## 8. Radii: fifteen literals that are already tokens, and two tokens that are missing

**Verdict: ACCIDENTAL.**

From inventory section 17c: `14px` x4 = `T.r-row`; `12px` x4 = `T.r-card`;
`26px` x2 = `T.r-disc`; `16px` x2 = `T.r-overlay`; `3px` x3 = `T.r-glyph`
(a token with **zero** references) — fifteen sites. Of 69 `border-radius:`
declarations, 25 use a token and 44 do not. Substituting the fifteen is
mechanical and pixel-identical.

Two values are *de facto* tokens that were never declared:
- **`18px`**, five sites — TV tile, TV "Apps" pill, TV error card, thermostat's
  two info cards. A "tile" radius sitting between `r-overlay` 16 and `r-disc` 26.
- **`6px`**, six sites — both light-screen bar tracks and fills, the mic meter.
  A "track" radius between `r-meter` 5 and `r-control` 10.

And the outliers that should just be corrected: `tv.slint:148` gives a **44x44**
close button `border-radius: 30px`, which is larger than half its height;
`cinema.slint:12` gives the primary transport button `44px` on an 88px box
(correct — a circle) while `tv.slint:22` gives its 88x64 and 70x70 and 44x44
buttons all `30px`.

**Proposal.** Substitute the fifteen. Add `T.r-tile: 18px` and either add
`T.r-track: 6px` or fold both tracks to `T.r-meter` (5px — a 1px change on two
vertical bars, invisible). Replace the three `IconButton` radii with
`self.height / 2` so a circle is a circle at any size.

**What the owner would see.** Nothing. **Pure refactor, pixel-identical**
except the optional 6->5 fold.

**Risk: very low.**

---

## 9. Trays: three sheets, three list designs

**Verdict: ACCIDENTAL.**

`BottomTray` (`ui/components/bottom_tray.slint`) is shared — good. What is
inside it is not:

| | tv `:147-157` | cinema `:104-111` | thermostat `:48-52` |
|---|---|---|---|
| sheet title | 27px/600 `#ffffff` | 28px, **no weight** `#ffffff` | 27px/600 `T.text` |
| close button | `IconButton` 44x44 r30 | `Control` 48x48 r14 | **none** |
| body copy | 19px `text-secondary` | 21px `#c0c7bc` | none |
| row | h74, pitch 82, r12, `surface-raised` | h76, pitch 84, r12, `#242823` | h60, pitch 68, r12, `surface-raised` |
| row title | 21px, no weight | 22px | 22px, no weight |
| row detail | 16px `text-secondary` | 16px `#b7beb2` | none |
| selected | 3px accent edge | 2px accent edge **+ violet fill** | `accent-bg` fill + accent edge |
| scroll | `Flickable`, `viewport-y` arithmetic | `list-scroll` animated 160ms | `y` arithmetic, no animation |

Put `core-3-tray.png` (TV, "Choose an input") next to `after-sheet-*.png`
(player, "Sources") and they are visibly two products.

**Proposal.** `Sheet { title, body }` + `SheetList { items, index }` reusing
`RowCard { state: raised }` and `FocusRing`. One row height (74), one pitch (82),
one scroll animation (160ms, or `T.fade` once that token is actually used).

**What the owner would see.** The player's sheet rows shrink 76->74 and lose
their violet selected fill for the outside ring the TV sheet already has; the
thermostat's mode rows grow 60->74 (which fits: the tray sizes itself,
`thermostat.slint:45`, `min(620px, 102px + n*68px)` -> `min(620, 102 + n*82)`,
so a 7-mode thermostat still fits). **Improvement.**

**Risk: medium** — the thermostat tray's height arithmetic has to move with the
pitch, and a taller row means fewer visible before scrolling. The player
goldens catch the cinema half (pictures 06-09, 17-22).

---

## 10. Buttons: `IconButton` and `Control` are the same component

**Verdict: ACCIDENTAL.**

`tv.slint:18-25` and `cinema.slint:5-19` are two implementations of "an icon on a
rounded fill, primary or not": same idea, different radius rule (fixed 30 vs
`primary ? 44 : 14`), different icon size (28 vs `primary ? 36 : 27`), different
colours (tokens vs literals), one has an optional label and a disabled opacity
and the other does not. Both are used at three different box sizes.

Below them sit four more one-off buttons: the activity pager's 72x52 r14
(`activity_pages.slint:40-47`), the "Media" 136x52 r14 (`:49-52`), the "Pages"
88x44 r22 (`app.slint:695-699`), the "End" 88x44 r22 in `#171717`/`#ffffff`
(`app.slint:700-705`), and the busy "Cancel" h52 r26 in `#292929`/`#ffffff`
(`app.slint:712-715`). The last three are the only things in the UI painted in
neutral grey rather than the warm palette — they look like a different app's
dialog, and they sit on top of the player and TV screens where the owner will
see them.

**Proposal.** `IconButton { icon, primary, label, enabled }` with
`border-radius: self.height / 2` when round and `T.r-row` when square, and
`TextButton { label, primary }` at 52 tall / `T.r-row` / `T.surface` /
`T.text`. Retire `Control`, and retire the three grey app.slint buttons into
`TextButton`.

**What the owner would see.** "End", "Pages" and "Cancel" become warm cards
instead of grey ones and their corners go 22/26 -> 14. **Improvement** — those
three are the most obviously off-brand objects in the product.

**Risk: low-medium.** Player goldens change (transport row, pictures 02-05).

---

## 11. `Tile` — three tile designs on three screens

**Verdict: ACCIDENTAL.**

`tv.slint:5-17` (210x120, r18, `surface-raised`, no border, icon 24 at 18/16,
title 21/600 at 48, detail 17 at 81) vs `activity_pages.slint:22-33` (210x150,
r16, `surface`/`surface-raised`, 1px border, icon **30** at 18/17, label **22**/600
at 58, detail **15** at 116). Same width, same column pitch (222), same purpose —
`core-1-power.png` and `controls.png` are the same screen family, drawn twice.
The thermostat's two info cards (`thermostat.slint:32-43`, W-48 x 94, r18) are
the same object at full width.

**Proposal.** `Tile { icon, title, detail, selected, enabled }`, one geometry,
`T.r-tile` (18). Full-width variant for the thermostat.

**What the owner would see.** Packaged-device tiles grow 120->150 tall or the
activity tiles shrink — pick one; and the icon settles on one of 24/30, the
detail line on one of 15/17. **Needs the owner's eye**: the two grids currently
hold 4 and 6 tiles in the same column, so the height choice changes how many fit.

**Risk: medium**, entirely because of that. Flag it, do not decide it here.

---

## 12. Pagers: dots on one screen, arrows-and-a-counter on another

**Verdict: PARTLY DELIBERATE.** The hub's dots (`home_hub.slint:583-594`) mark
areas you reach with Left/Right and must not move during a page slide — they
are documented as *"the control, and it belongs to the hub, not to a page"*
(`app.slint:63-64`). The activity pages' arrows (`activity_pages.slint:39-48`)
are touch targets on a screen with no left/right hardware meaning.

So the *forms* differ for a reason. What does not: the activity pager's
72x52 r14 buttons and 20px "1 / 2" counter share no tokens with anything, and
the hub reserves its dot slot with hand-written arithmetic (`:101, 117, 412`)
duplicated in `app.slint:65-66` and again as `CONTROLS_HEADER_H`/
`CONTROLS_FOOTER_Y` in `src/main.rs:72-73`.

**Proposal.** Leave the two forms. Fold the activity pager's buttons into
`IconButton` (item 10) and its counter into the text ladder (item 13). Move
`CONTROLS_HEADER_H`/`CONTROLS_FOOTER_Y` out of Rust and into exported functions
on the activity-pages screen, the way `light.slint` already does it — same
change as item 1, same mechanism.

**Risk: low.**

---

## 13. The type ladder: 24 sizes, 32 (size, weight) pairs, 2 token references

**Verdict: ACCIDENTAL — and the reason that produced it has expired.**

`setup.slint:118-121` carries the argument in its own words: *"a size used once
costs the whole charset in the binary, and that one was 30KB for a banner nobody
sees twice."* That was true of bitmap font embedding. `docs/slint-notes.md:72-100`
records that `build.rs` now uses **SDF**: one scalable glyph set per face,
"those per-size costs no longer describe the SDF build", and only the *largest*
size moves the payload. So the pressure that made people reach for an
already-embedded size is gone, and what is left is 24 sizes nobody chose.

The clusters are obvious: {14,15,16,17} for secondary text, {18,19,20,21} for
body, {22,23,24,26,27,28} for headings, {30,32,36,40,46,48,62,76} for readouts.

**Proposal.** Eight tokens on `Theme`, next to `device-name` which already
exists:
```
label      12 / ls 1.5      caption   15 / ls 2.1
secondary  16               body      19
title      21 / 600         device-name 26 / 600   (exists)
readout    36 / 700         display   76 / 700
```
and let the genuinely singular sizes stay literal with a comment: the PIN's 48
(`pair_overlay.slint:41-44`), the dock clock's 62 (`app.slint:938`), the
thermostat's `range ? 46 : 76` (`thermostat.slint:30`), the light screen's
`two-up ? 36 : 48` (`light.slint:100`, it must fit a 204px column).

**What the owner would see.** Dozens of 1-2px changes. Individually invisible;
collectively the thing that makes the UI read as one system. **Improvement, but
it needs his eye** because it is the change with the most surface area.

**Risk: medium-high** in breadth, low in depth. Do it **last**, one screen per
PR, after the components above have already collapsed most call sites.

---

## 14. Copy-paste between files — and one copy that can go wrong

**Verdict: ACCIDENTAL.**

Three fragments exist twice (inventory section 19):

1. The **audio-bar glyph** (three 3px rectangles, 10/16/7 tall, `T.accent`) and
   its sibling play triangle are byte-for-byte identical in
   `home_hub.slint:263-274` and `room_devices.slint:159-170`. Trivial: one
   `ActivityGlyph { kind }`, ~20 lines gone. Both call sites already share the
   activity-row shape, so this falls out of `RowCard` (item 5) for free.
2. The **zero-size focus-ring anchor** — a 0x0 `Rectangle` holding
   `ring-y`/`ring-h`/`ring-r` with `animate ... { duration: anim ? 160ms : 0ms }`,
   written so moving the ring cannot dirty the root that paints the background —
   is duplicated with its comment in `home_hub.slint:214-225` and
   `room_devices.slint:127-135`. The technique is correct and hard-won
   (`docs/slint-notes.md`); only the copy is the problem. `RingAnchor` would hold
   it once, and a third page (`chooser.slint:79-83`, which does the same job with
   bare properties) would join it.
3. **The row's disc geometry, written twice inside one file — and this one can
   produce a wrong result, not merely a divergent one.**
   `room_devices.slint:180-190` lays the disc out as `padding: 18px`, a 52px
   rectangle, `spacing: 16px`. Then `room_devices.slint:115-118` hand-derives the
   same three constants again, in the functions the lift reads:
   ```
   disc-inset()  -> 18px
   disc-box-y()  -> 18px          # added by #282 — the fourth copy
   label-box-x() -> 18px + 52px + 16px
   label-box-w() -> width - 2*pad-side - (18px + 52px + 16px) - 16px - 22px - 18px
   ```
   Change the row's padding in the markup and the transition silently cuts the
   name sprite from the wrong rectangle. The comment at `:109-114` explains why
   these must be arithmetic rather than a layout read-back, and that reasoning is
   right — what is missing is one place for the three numbers to live.

**Proposal.** The numbers move onto the shared row component as `out property`s
(`disc-size`, `disc-inset`, `label-gap`) and both the markup and the exported
functions read them. That is the same move item 1 makes for the header, applied
to the row at the other end of the lift — together they close the loop: the
transition would read both of its endpoints from the elements that draw them.

**What the owner would see:** nothing. **Risk: low** — and the lift test
(`lights.rs:3805-4358`) exercises exactly these values every run, so a mistake
fails loudly.

---

## 15. Small true bugs found on the way

These are not variants; they are wrong, and they are cheap.

1. **`src/main.rs:66` `BACKGROUND: u32 = 0x09090b`** — the colour the
   framebuffer is claimed with at startup (`:329`). `T.bg` is `#15130F`. The
   panel is painted `#09090B` before the first Slint frame lands on it. Fix:
   `0x15130f`. (`T.bg-sleep`, `#0D0C0A`, is the dead token that was probably
   meant here and in `app.slint:931`'s dock clock `#050505`.)
2. **`src/panel.rs:138` `IRIS_RING = 0xffff_ffff`** — the ring the iris rides is
   hardcoded white while the row's own ring is `T.accent`, which the owner can
   change. Pick a non-white accent and the ring changes colour mid-transition.
3. **`room_devices.slint:217`** — the room's scenes card title is missing
   `font-weight: 600` that the hub's has (`home_hub.slint:453`).
4. **`thermostat.slint:25`** — the subtitle is the literal `"Home Assistant"`.
   A thermostat reached through a packaged integration still says so.
5. **`camera.slint:13`** — same, `"UniFi Protect"`, and it is at x24 y95 rather
   than in the header.
6. **`tv.slint:148`** — a 44x44 button with `border-radius: 30px`.
7. **`room_devices.slint:212`** — `x: 20px` where every sibling uses
   `T.pad-side` (which is 20).
8. **Five dead tokens**: `T.bg-sleep`, `T.surface-press`, `T.r-glyph`,
   `T.fade`, `T.volume-hold`. Three of them have live literal equivalents
   elsewhere (`#050505` ~ bg-sleep, `3px` x3 = r-glyph, `160ms` x7 = fade).
   Either wire them up or delete them; a token nobody uses is a false promise
   that the UI is tokenised.

---

## What is DELIBERATE and should stay

Said plainly so nobody "fixes" these:

- **No pressed state anywhere.** `T.surface-press` is dead and should stay dead
  unless touch becomes primary. This is a D-pad product; the ring is the truth
  about what OK will do, and `home_hub.slint:56-61` argues it well.
- **One accent hue.** `theme.slint:17` — *"A second hue on a panel this small
  reads as an error state."* The mic's `#E04030` is the single exception and it
  earns it (`mic_overlay.slint:6-11`).
- **The light screen's vertical bars.** Portrait travel, bottom-filling, barely
  rounded, gradient inside a `clip:true` parent. Do not fold these into a shared
  bar component.
- **The white QR cards** (`setup.slint:51`, `no_config.slint:58`) and
  `image-rendering: pixelated` at native size.
- **Three focus rings in the keyboard** (`keyboard.slint:490, 557, 605`) — three
  bands that cannot travel to one another; the hub's one-ring rule does not
  apply.
- **One ring per pane everywhere else**, positioned by arithmetic, set
  imperatively. Any shared row component must not contain a ring.
- **`r-row` 14 for a room/device row vs `r-card` 12 for a chooser/settings row** —
  arguably meaningful, and `lights.rs:3874` pins the 14.
- **The status bar carrying the screen title** for the chooser, settings,
  keyboard and Wi-Fi pages (`app.slint:533-537`) rather than a header — those
  are modal pages you leave with Back, not device screens you opened from a row.

---

## Coverage reality check

- Only the **player screen** has a pixel golden — 29 pictures, opt-in via
  `COUCH_PLAYER_GOLDENS`, not run in CI. CI sets `COUCH_PLAYER_SCREENSHOTS`
  only and asserts the **text** transcript
  (`ui/couch-gui/tests/golden/player-screen.txt`).
- The **light screen, room list and the lift** have strong non-pixel coverage:
  `the_light_screen_is_one_screen_for_every_shape_of_device`
  (`lights.rs:3629`) and `the_control_screen_opens_as_a_window_out_of_its_row`
  (`lights.rs:3805-4358`), which checks endpoints, reversibility, "nothing pops"
  (40% of a patch's total delta in one frame), "no ghost", and "nothing stronger
  than the card" — twice, once with a scrolled list.
- **Camera, Setup, NoConfig, MicOverlay, PairOverlay, BtPairOverlay, Keyboard,
  WifiSetup, Settings-proper and the Home Hub itself have no rendering test at
  all.** Items 4, 7 and most of 13 land in that gap; items 1, 5 and 6 land partly
  on tested ground.
