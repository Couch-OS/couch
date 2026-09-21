# Couch on-device UI — inventory of visual element kinds and their variants

Read-only audit of `/Users/bryanhoban/Projects/couch` on `dev`. Started at
`b3bd307`; **re-checked against `95a94a3`** (PR #282, `gui/lift-speed-pass`),
which landed the other agent's shared `DeviceDisc` mid-audit. Everything below
describes `95a94a3`; the disc rows in section 4 note what that PR changed.
All paths below are relative to `ui/couch-gui/` unless written in full.
`T.` is shorthand for `Theme.` (`ui/theme.slint`).

Token values for reference (`ui/theme.slint:6-69`):

```
bg #15130F   bg-sleep #0D0C0A  surface #1F1C17  surface-recessed #1B1814
surface-raised #241F19  surface-icon #2E2820  surface-press #2A251E
border #2C271F  border-dim #262218  border-raised #3A342B  border-icon #3D362C
accent #FFFFFF (user-settable)  accent-bg #383633 (derived: 15% accent over bg)
text #F2EDE4  text-secondary #A39C90  text-tertiary #8A8175
text-quaternary #6E675C  text-disabled #4E483F
surface-idle #1B1814  border-idle #262218  surface-icon-idle #221F1A  border-icon-idle #302B24
r-glyph 3  r-meter 5  r-control 10  r-card 12  r-row 14  r-overlay 16  r-disc 26
pad-top 18  pad-side 20  pad-bottom 22  overlay-inset 36
device-name 26px / weight 600
ring-width 3  ring-offset 3
fade 160ms  volume-hold 1600ms
```

**Dead tokens** (zero references in `.slint` and in `src/`):
`bg-sleep`, `surface-press`, `r-glyph`, `fade`, `volume-hold`.
`fade` is 160ms and 160ms is written out as a literal 7 times; `surface-press`
is dead because **no control in the UI has a pressed state at all** (see §10).

---

## 1. Screen headers (back button + title + subtitle + icon)

This is the top group: every one of these screens is, or will be, opened from a
room row by the lift, and the lift today only knows the light screen.

| screen | file:line | back | title | subtitle | icon |
|---|---|---|---|---|---|
| Light / blind | `ui/screens/light.slint:76-90` | `MediaBack` x24 **y26** | x88 y20 w=W−112 h34, **`T.device-name` (26px/600)**, `T.text`, elide | x88 y56 h24, **16px**, `T.text-secondary` | `DeviceDisc` **centred** at ((W−52)/2, 94) — since #282 the same component the row uses (52px, `T.r-disc`, `T.surface-icon`/`-idle`, 1px `T.border-icon`/`-idle`, glyph 24px). Plus a **state line** x24 y156 h28, 20px, centred, `active ? T.accent : T.text-secondary` |
| TV / packaged device | `ui/screens/tv.slint:83-90` | `MediaBack` x24 **y22** | x88 y27 w=W−112 h27, **20px/600** | x88 y58 h25, **16px**, `T.text-secondary` | **no disc.** A bare 52px `Image` at (32,146) tinted `T.accent`; below it a kind label (16px, ls 2px, `T.accent`) and a 40px/700 source name |
| Thermostat | `ui/screens/thermostat.slint:23-31` | `MediaBack` x24 **y22** | x88 y27 w=W−112 h28, **21px/600** | x88 y59 h24, **16px**, and the text is the **hardcoded literal `"Home Assistant"`** | 80×80 disc at (200,120), `r:40px`, `T.accent-bg`, **no border**, glyph 36px |
| Camera | `ui/screens/camera.slint:11-13` | `MediaBack` x24 **y22** | x88 y27 w=W−112 h32, **21px/600** | **x24 y95** (not x88, not in the header band), 16px, hardcoded literal `"UniFi Protect"` | none |
| Media player (Cinema) | `ui/screens/cinema.slint:61-63` | `MediaBack` x24 **y22** | x88 y24 w=**W−120** h26, **19px/600**, colour **`#ffffff`** (literal) | x88 y52 h22, **15px**, colour **`#cbcfc8`** (literal) | none |
| Activity / packaged-device pages | `ui/screens/activity_pages.slint:19-21` | `MediaBack` x24 **y22** | x88 y24 w=**270px fixed** h28, **26px/600** | **x24 y86** (below the header, full width), **18px**, `T.text-secondary` | none |
| Chooser, Settings, Keyboard, Wi-Fi setup | `ui/app.slint:529-543` | none | the **status bar** carries it: 15px, ls 0.12×15, `T.accent`, centred | — | — |
| Room device list | `ui/components/room_devices.slint` | none (Back key only) | status-bar title | — | — |

Differences, enumerated:
- **Title size**: 19, 20, 21, 26 px — four values for the same line.
- **Title y**: 20, 24, 27 — three values; **title x is 88 everywhere** (good).
- **Title width**: `W−112`, `W−120`, `270px`.
- **Back button y**: 26 on the light screen, 22 on all five others.
- **Subtitle**: exists on 5 of 6; sizes 15/16/18; y 52/56/58/59/86/95; x 88 or 24;
  colour `T.text-secondary` or `#cbcfc8`.
- **Header icon**: three drawings (52 disc centred, 80 disc off-centre, 52 bare
  image) and three absences.
- Only `light.slint` exports its header geometry (`title-box-*`, `disc-box-*`,
  `header-h`, `cards-*`, `track-box-*`, `fill-h`, `marker-*`, `footer-y`:
  `ui/screens/light.slint:204-239`), re-exported through `ui/app.slint:331-350`
  and consumed by `src/main.rs:147-207` (`lift_geometry`). No other screen has any.
- The lift's destination card is **hardcoded in Rust**, not read from a screen:
  `src/panel.rs:162` `LIFT_CARD_TO: (20, 14, 440, 76, 12)` — x20 y14 w440 h76 r12.
  That rectangle does not correspond to any `Rectangle` in any `.slint` file.

### 1a. Back affordance
One component, used consistently: `ui/widgets/media-back.slint:2-8` — 48×48,
`border-radius:14px` (a literal; `T.r-row` is also 14), `T.surface`, 1px
`T.border-raised`, `arrow-big-left` at 27px, `T.text`. Six call sites, all at
x24, y22 except light at y26. No focus ring, no pressed state (Back is a
hardware key).

---

## 2. Section labels / all-caps eyebrow text

Nine spellings of the same idea.

| # | file:line | size | letter-spacing | colour |
|---|---|---|---|---|
| 1 | `ui/components/section_label.slint:3-7` (the component) | 15 | 0.14×15 = 2.1 | `T.text-quaternary` |
| 2 | `ui/components/room_devices.slint:213` `"SCENES"` | 15 | **1.8** | **`T.text-secondary`** |
| 3 | `ui/components/settings.slint:262-273` panel header | 15 | 0.14×15 | `T.text-quaternary` (inline copy of #1) |
| 4 | `ui/components/keyboard.slint:353-359` title | 15 | 0.14×15 | `T.text-quaternary` (inline copy of #1) |
| 5 | `ui/screens/setup.slint:29-35` | 15 | 0.14×15 | `T.text-quaternary` (inline copy of #1) |
| 6 | `ui/screens/setup.slint:72-78`, `:89-95`; `ui/screens/no_config.slint:32-38` | **14** | 0.14×14 = 1.96 | `T.text-quaternary` |
| 7 | `ui/components/wifi_setup.slint:21` caption | 15 | **none** | `T.text-quaternary` |
| 8 | `ui/components/pair_overlay.slint:25-31`, `ui/components/bt_pair_overlay.slint:33-39` | 15 | **2px** | **`T.text-secondary`** |
| 9 | `ui/components/status_bar.slint:37-50` screen title | 15 | **0.12×15 = 1.8** | **`T.accent`** |

Plus three more in-card captions that are the same species:
- `ui/screens/light.slint:118-121` `"BRIGHTNESS"` / `:164-168` `"COLOUR TEMP"`: **12px, ls 1.5, `T.text-secondary`**
- `ui/screens/light.slint:155-163` `"COOL"` / `"WARM"`: **11px, ls 1.2, `T.text-tertiary`**
- `ui/screens/thermostat.slint:29` `"TARGET TEMPERATURE"`: **16px, ls 2, `T.text-secondary`**
- `ui/screens/tv.slint:88` kind label (`"SONOS"`, `"DENON AVR"`): **16px, ls 2, `T.accent`**

`SectionLabel` is used at exactly **one** call site (`ui/screens/home_hub.slint:420-425`).

---

## 3. Body / secondary / hint text

| role | file:line | size | colour |
|---|---|---|---|
| Room-row second line | `home_hub.slint:357-363` | 19 | `idle ? text-quaternary : text-secondary` |
| Device-row second line | `room_devices.slint:194` | 19 | `active ? accent : text-secondary` |
| Activity-strip second line | `home_hub.slint:290-298`, `room_devices.slint:175`, `chooser.slint:182-188` | 14 + ls 0.06×14 | `text-secondary` |
| Room-page detail blurb | `room_devices.slint:212` | 16 | `text-secondary` (x is a literal `20px`, not `T.pad-side`) |
| Light detail | `light.slint:192` | **17** | `text-secondary` |
| Light hint (key legend) | `light.slint:193-196` | **15** | **`text-tertiary`** |
| Thermostat detail | `thermostat.slint:44` | **19** | `text-secondary` |
| Activity-pages status | `activity_pages.slint:35` | **18** | `text-secondary` |
| Camera message | `camera.slint:15` | **18** | `text-secondary` |
| Camera hint | `camera.slint:16` | **16** | `text-secondary` |
| Settings notes (×3) | `settings.slint:354-361, 386-393, 414-421` | 15 | `text-secondary` |
| Settings footer hint | `settings.slint:446-456` | **14** | `text-quaternary` |
| Wi-Fi setup detail | `wifi_setup.slint:23` | 18 | `text-secondary` |
| Wi-Fi setup footer hint | `wifi_setup.slint:36` | **16** | `text-tertiary` |
| TV tray body copy | `tv.slint:149` | 19 | `text-secondary` |
| TV error body | `tv.slint:163` | 20 | **`#ffffff`** |
| Cinema tray body | `cinema.slint:106` | 21 | **`#c0c7bc`** |
| Cinema not-ready title | `cinema.slint:93` | 27 | **`#f1f2eb`** |
| Cinema message card | `cinema.slint:118` | 20 | **`#ffffff`** |
| Mic detail / hint | `mic_overlay.slint:103-109`, `:111-122` | 18 / 19 | `text-secondary` |
| Pair "Enter this in the browser" | `pair_overlay.slint:51-56` | 19 | `text` |
| Bt-pair instruction | `bt_pair_overlay.slint:76-87` | 21 | `text` |
| No-config address / body | `no_config.slint:39-45`, `:73-78` | 19 / 19 | `text-secondary` / `text-tertiary` |
| Activity-busy progress / step | `app.slint:710-711` | 26 / 20 | **`#ffffff` / `#bbbbbb`** |
| Dock-clock caption | `app.slint:944-952` | 20 | **`#737373`** |

**Hint lines alone** are 14, 15, 16 px in three colours.

---

## 4. Icon discs / badges / dots

| kind | file:line | size | radius | fill | edge | glyph |
|---|---|---|---|---|---|---|
| `DeviceDisc` (device row + light screen) | `components/device_disc.slint:9-30`, used `room_devices.slint:186`, `light.slint:88-91` | 52 | `T.r-disc` | `known && !active ? surface-icon-idle : surface-icon` | 1px `border-icon-idle`/`border-icon` | 24px at (14,14), tri-state colorize, `animate colorize 180ms ease-out` — **new in #282; this is the unification already in hand** |
| Room-row disc | `home_hub.slint:330-345` | 52 | `T.r-disc` | `power-state==0 ? surface-icon-idle : surface-icon` | same | 24px, centred by arithmetic rather than fixed (14,14) — **still its own copy; #282 did not reach the hub** |
| Thermostat disc | `thermostat.slint:26-28` | **80** | **40px** | `accent-bg` always | **none** | 36px |
| TV header icon | `tv.slint:87` | 52 | — | **no disc** | — | 52px image, `accent` |
| TV no-art placeholder | `tv.slint:82` | 64 | — | none | — | 64px, `text-tertiary` |
| Room-row "on" indicator | `home_hub.slint:374-387` | 19 ring + 9 dot | 9.5 / 4.5 | transparent / accent | 1px `accent.with-alpha(0.4)` | — |
| Chooser "active" dot | `chooser.slint:193-197` | 12 | 6 | accent | — | — |
| TV "live" dot | `tv.slint:112` | 8 | 4 | accent | — | — |
| Mic / Bt-pair state dot | `mic_overlay.slint:50-54`, `bt_pair_overlay.slint:52-56` | 22 | 11 | `tone` | — | — |
| Pager dot | `home_hub.slint:590-594` | 6 | 3 | `accent` / `border-raised` | — | — |

Six circle sizes (6, 8, 9, 12, 19, 22) and two disc sizes (52, 80).
After #282 the 52px disc is drawn **twice**: `DeviceDisc`, and the home hub's
own copy at `home_hub.slint:330-345`, which is the same drawing with the glyph
centred by arithmetic instead of pinned at (14,14). Folding the hub onto
`DeviceDisc` is a two-line follow-up to work that has already landed, and it
matters for the lift: the hub's room rows are the other place a lift starts.

---

## 5. List rows and cards

| row kind | file:line | height | fill | edge | radius | title | second line | trailing |
|---|---|---|---|---|---|---|---|---|
| Room row | `home_hub.slint:318-404` | `row-h` ≥ 90 | `idle ? surface-idle : surface` | 1px `border-idle`/`border` | `T.r-row` | `T.device-name` 26/600 | 19px | status column (dot + "n ON" 14px) |
| Device row | `room_devices.slint:178-208` | `row-h` ≥ 90 | `surface` | 1px `border` | `T.r-row` | `T.device-name` 26/600 | 19px | chevron 22px `text-quaternary` |
| Activity row (in a room) | `room_devices.slint:154-177` | `row-h` | `surface-raised` | 1px `border-raised` | `T.r-card` | **21/600** | 14px + ls | bars/play glyph on the **left** |
| Activity strip (hub) | `home_hub.slint:242-301` | 66 | `surface-raised` | 1px `border-raised` | `T.r-card` | **21/600** | 14px + ls | same |
| Chooser row | `chooser.slint:152-200` | `row-h` ≥ 72 | `surface` | 1px `active ? accent : border` | `T.r-card` | **21/600** | 14px + ls | 12px accent dot if active |
| Scenes card (hub) | `home_hub.slint:429-469` | 74 | `surface` | 1px `border` | `T.r-card` | **21/600** | — | `arrow-big-right` 22px, laid out |
| Scenes card (room) | `room_devices.slint:214-220` | 74 | `surface` | 1px `border` | `T.r-card` | **21px, no weight 600** | — | arrow 22px at **absolute (W−40, 26)** |
| SettingsRow | `settings.slint:7-47` | 72 | `surface` | 1px `border` | `T.r-card` | 21/600 | — | value 21px `accent` + arrows **24px** |
| Settings section row | `settings.slint:280-301` | 72 | `surface` | 1px `border` | `T.r-card` | 21/600 | — | arrow **26px** |
| TV tray choice | `tv.slint:153-157` | 74 (pitch 82) | `surface-raised` | 0/3px `accent` | **12px literal** | **21px, no weight** `#ffffff` | 16px | — |
| Cinema tray choice | `cinema.slint:108-111` | 76 (pitch 84) | **`#35303e` / `#242823`** | 0/2px `accent` | **12px literal** | **22px** `#ffffff` | 16px `#b7beb2` | — |
| Thermostat mode row | `thermostat.slint:50-52` | 60 (pitch 68) | `accent-bg` / `surface-raised` | 1px `accent`/`border` | **12px literal** | **22px, no weight** | — | — |

Row minimum heights: 90 (rooms/devices), 74 (scenes card), 72 (chooser, settings),
76/74/60 (the three tray lists). Row gaps: 12 (rooms, devices, chooser), 10
(settings), 82−74=8 (tv tray), 84−76=8 (cinema tray), 68−60=8 (thermostat tray).

### 5a. Non-row cards / panels

| card | file:line | size | fill | edge | radius |
|---|---|---|---|---|---|
| Light bar card | `light.slint:93-96`, `:125-128` | 204 or 220 × 352/452 | `surface` | 1px `border` | `T.r-card` |
| Thermostat info card | `thermostat.slint:32-36` | W−48 × 94 | `surface` | 1px `border` | **18px** |
| Thermostat mode card | `thermostat.slint:37-43` | W−48 × 94 | `surface` | 1px `accent` or `border` | **18px** |
| TV tile | `tv.slint:5-17` | 210×120 (one 432×120) | `surface-raised` | 0 or 3px `accent` | **18px** |
| Activity-pages tile | `activity_pages.slint:22-33` | 210×150 | `surface` / `surface-raised` | 1 or 3px | **16px** |
| TV error card | `tv.slint:162-165` | W−48 × 230 | `surface-raised` | none | **18px** |
| Cinema message card | `cinema.slint:117-119` | W−48 × 100 | **`#30263e`** | none | **16px** |
| Keyboard field | `keyboard.slint:364-369` | 64 tall | `surface-recessed` | 1px `border` | `T.r-card` |
| QR card | `setup.slint:48-52` 320², `no_config.slint:55-59` 208² | — | `#FFFFFF` (deliberate) | none | `T.r-row` |

**Card radius is 12 (`r-card`), 16, or 18 depending on the file.**

---

## 6. Buttons

| button | file:line | shape | idle fill | selected | label |
|---|---|---|---|---|---|
| `MediaBack` | `media-back.slint:2-8` | 48×48 r14 | `surface` + 1px `border-raised` | — | icon 27 |
| TV `IconButton` (transport) | `tv.slint:18-25`, used `:92-96` | 88×64 **r30** | `primary ? accent : surface-raised` | — | icon 28, `primary ? T.bg : T.text` |
| TV `IconButton` (media view) | `tv.slint:119-122` | 70×70 r30 | same | — | icon 28 |
| TV `IconButton` (tray close X) | `tv.slint:148` | **44×44 r30** — radius exceeds half the height | same | — | icon 28 |
| TV "Apps" pill | `tv.slint:123-127` | r18 | `surface-raised` | — | icon 24 + 22/600 |
| TV error retry | `tv.slint:164` | W−40 × 44 **r12** | `accent` | — | 20px `T.bg` |
| Cinema `Control` (round) | `cinema.slint:5-19`, used `:84` | 88×88 **r44** | `accent` | 3px `#ffffff` | icon 36, colorize **`#151319`** |
| Cinema `Control` (square) | `cinema.slint:5-19`, used `:82-90` | 70×70 / 76 tall **r14** | **`#222623`** | 3px `accent` (idle edge **`#ffffff15`**) | icon 27, colorize **`#f6f6f1`**, label 17px **`#eceee8`** |
| Cinema tray close X | `cinema.slint:105` | 48×48 r14 | `#222623` | — | icon 27 |
| Light blind Open/Stop/Close | `light.slint:175-189` | (W−48)/3−10 × 74 `T.r-card` | `surface` | `accent-bg` + 1px `accent` **and** a `FocusRing` | 20px, disabled → `text-disabled` |
| Keyboard key | `keyboard.slint:463-488` | 38×63 `T.r-control` | `surface-raised`, **no border** | `FocusRing` only | 21/600 |
| Keyboard fn key | `keyboard.slint:531-556` | `fn-w`×64 `T.r-control` | `surface` + 1px `border` | latched → `accent-bg` + `accent` edge + `accent` text | 15px + ls |
| Keyboard commit key | `keyboard.slint:579-604` | `commit-w`×64 `T.r-control` | `surface` / `accent` for OK | `FocusRing` | 15px + ls |
| Activity-pages pager ← / → | `activity_pages.slint:40-47` | 72×52 **r14** | `surface` | — | icon 30 `T.text` |
| Activity-pages "Media" | `activity_pages.slint:49-52` | 136×52 r14 | `surface` | — | 20px `T.text` |
| App "Pages" | `app.slint:695-699` | 88×44 **r22** | `T.surface` | — | 18px `T.text` |
| App "End" | `app.slint:700-705` | 88×44 r22 | **`#171717`** | — | 18px **`#ffffff`** |
| App busy "Cancel" | `app.slint:712-715` | h52 **r26** | **`#292929`** | — | 20px **`#ffffff`** |
| Setup approval banner | `setup.slint:107-128` | W−48 × 108 `T.r-row` | `accent` | — | 21/600 `T.bg` |

Button radii in use: 12, 14, 16, 18, 22, 26, 30, 44, `r-card`, `r-control`, `r-row`.
Three "close / X" buttons (`tv.slint:148` 44×44 r30, `cinema.slint:105` 48×48 r14,
plus Back at 48×48 r14).

---

## 7. Focus rings and selection treatments

**One component** — `ui/components/focus_ring.slint:13-29`: drawn as a sibling
inset by `−(ring-offset + ring-width)` = −6px, `border-width: T.ring-width` (3),
`border-color: focused ? accent : transparent` (off is transparent, never
zero-width — deliberate, per `docs/slint-notes.md`).

Ring call sites:
- `home_hub.slint:476-505` — one ring for the whole pane, `opacity` faded, 200ms
- `room_devices.slint:224-228` — one ring for the page
- `chooser.slint:203-215` — one ring, 140ms
- `settings.slint:427-440` — one ring, `animate y 140ms`
- `keyboard.slint:490-503, 557-570, 605-618` — **three rings**, 160ms, one per band
- `light.slint:188` — one per blind button (3 instances, one focused)

**Six other, unrelated "this is selected" drawings:**

| # | file:line | how |
|---|---|---|
| 1 | `tv.slint:12` (`Tile`) | `border-width: selected ? 3px : 0px`, `border-color: accent` |
| 2 | `tv.slint:154` (tray row) | `border-width: 3px : 0px`, accent |
| 3 | `cinema.slint:14-15` (`Control`) | `border-width: 3px : 1px`; colour `primary ? #ffffff : accent` over idle `#ffffff15` |
| 4 | `cinema.slint:108` (tray row) | `border-width: 2px : 0px` **and** background `#35303e` vs `#242823` |
| 5 | `cinema.slint:77` (seek bar) | a separate 2px accent rectangle, r8, at y30 h38 |
| 6 | `activity_pages.slint:26-27` (tile) | `border-width: 3px : 1px`, colour `accent` vs `border-raised`, **plus** fill `surface-raised` vs `surface` |
| 7 | `thermostat.slint:50` (mode row) | fill `accent-bg` vs `surface-raised`, edge `accent` vs `border` |
| 8 | `light.slint:177-188` (blind button) | fill `accent-bg`, edge `accent`, **and** a `FocusRing` |
| 9 | `keyboard.slint:535-546` (fn latch) | fill `accent-bg`, edge `accent`, text `accent` |

Items 1, 2, 3, 4, 6 all change `border-width` — i.e. **geometry** — which
`docs/slint-notes.md` ("DirtyRegion holds three rectangles") explicitly says not
to do: *"never express as geometry what can be expressed as colour"*. `FocusRing`
was written to obey that rule; five call sites do not use it.

---

## 8. Sliders / bars / progress / meters

| bar | file:line | orientation | track | fill | radius |
|---|---|---|---|---|---|
| Light level | `light.slint:103-116` | vertical, 62 or 68 wide × `bars-h−134` | `surface-icon` | `accent` | **6px both** |
| Light colour temperature | `light.slint:135-153` | vertical, same box | `@linear-gradient(0deg, #FFB463, #FFE9C8 45%, #FFFFFF 62%, #C9DCFF)` inside a `clip:true` parent | marker 10px tall, `T.text` with **2px `T.bg` border** | 6px |
| Volume/feedback meter | `volume_overlay.slint:63-73` | horizontal, h10 | `border-raised` | `accent` | `T.r-meter` (5) |
| Mic level meter | `mic_overlay.slint:72-84` | horizontal, **h12** | **`surface`** | `accent` | **6px** |
| TV media progress | `tv.slint:114-116` | horizontal, **h5** | **`surface-icon`** | `accent` | **2.5px** |
| Cinema progress | `cinema.slint:74-76` | horizontal, **h5** | **`#ffffff35`** | `accent` | **3px** |
| Wi-Fi signal bars | `status_bar.slint:83-95` | 4 rects 3px wide, 5/8/11/14 tall | `border-raised` | `text-secondary` | 1px |

Four horizontal meters: heights 5, 5, 10, 12; radii 2.5, 3, 5, 6; four
different track colours. `T.r-meter` is used by exactly one of them.

---

## 9. Overlays: feedback cards, toasts, trays, modals

| overlay | file:line | box | fill | edge | radius | motion |
|---|---|---|---|---|---|---|
| `VolumeOverlay` (5 instances) | `volume_overlay.slint:15-76`; instantiated `app.slint:871-927` | h140, inset `T.overlay-inset` | `surface-raised` | 1px `border-raised` | `T.r-overlay` | `reveal` 200ms ease-out |
| Toast bar | `app.slint:842-868` | h**72**, same inset | `surface-raised` | 1px `border-raised` | `T.r-overlay` | 200ms ease-out |
| `BottomTray` | `bottom_tray.slint:5-38` | inset `T.overlay-inset`, h ≤ 620 | `surface` | 1px `border-raised` | **24px** | 200ms ease-out; scrim `#00000088` |
| Mic card | `mic_overlay.slint:39-67` | h84 | `surface-raised` | **2px** `tone` | `T.r-card` | none |
| Bt-pair card | `bt_pair_overlay.slint:41-74` | h84 | `surface-raised` | **2px** `tone` | `T.r-card` | none |
| Pair-PIN card | `pair_overlay.slint:33-49` | h84 | `surface-raised` | 1px `accent` | `T.r-card` | none |
| Activity-busy modal | `app.slint:706-717` | full screen | **`#101010`** | — | — | none |
| Dock clock | `app.slint:928-953` | full screen | **`#050505`** | — | — | none |

**Mic card and Bt-pair card are byte-for-byte the same drawing** (84px,
`surface-raised`, `r-card`, 2px `tone` border, a 22px dot, then a 26px/700
letter-spaced word) written out twice in two files: `mic_overlay.slint:39-67`
vs `bt_pair_overlay.slint:41-74`. Their outer layouts (`alignment:center`,
`spacing:18/20`, `padding 28/34`) differ by 2px and 6px respectively.

**Scrim alphas:** `#000000F5` (pair, 96%), `#000000F0` (bt-pair, 94%),
`#000000E0` (mic, 88%), `#00000088` (tray, 53%), `#101010` opaque (busy),
`#050505` opaque (dock). Six.

---

## 10. States: active / idle / unavailable / disabled / pending / empty

- **Idle / off** on a room row: a *recessed* surface (`surface-idle`,
  `border-idle`, `surface-icon-idle`, `border-icon-idle`) plus demoted text
  (`text-quaternary`). `home_hub.slint:321-323, 334-336, 354, 361, 397-400`.
  **Device rows do not do this**: `room_devices.slint:148-150` is always
  `surface`/`border`; only the disc recesses (`:182-183`).
- **Unavailable**: no treatment at all — it is the word `"Unavailable"` in the
  second line (built in `src/light_screen.rs`), same colour as any other state text.
- **Disabled**: `T.text-disabled` (light blind Stop, `light.slint:183`; keyboard
  placeholder, `keyboard.slint:379-386`), `opacity: 0.55` (activity tile,
  `activity_pages.slint:28`), `opacity: 0.35` (cinema control, `cinema.slint:16`).
  Three treatments.
- **Dimmed** (on but under half): `accent.with-alpha(0.55)` on the count text,
  `home_hub.slint:399`.
- **Pending**: the string `"Updating…"` replaces the state line —
  `light.slint:90`, `thermostat.slint:31`. No spinner, no dimming anywhere.
- **Empty**: one hand-written sentence per screen. `activity_pages.slint:34`
  ("Add command buttons…", 24px centred), `tv.slint:149` (three different
  sentences, 19px left), `cinema.slint:93` (27px centred), `no_config.slint`
  (whole screen). Four layouts.
- **Pressed**: **nothing anywhere has one.** `T.surface-press` is unreferenced;
  `grep pressed` over `ui/` finds only key handlers and the cinema seek drag.
  Deliberate (a D-pad UI with a moving ring), but worth writing down.

---

## 11. Status bar

`ui/components/status_bar.slint:10-96`. Height 56 (also hardcoded a second time
as `status-h: 56px` in `ui/app.slint:62`, which the transitions read).
- 1px rule, `T.border-dim` (its only use in the codebase), `:23-27`
- Clock: 19px `T.text`, x `T.pad-side` `:29-35`
- Title: 15px, ls 0.12×15, `T.accent`, elided, width clamped off `wifi.x` `:37-50`
- Battery icon 24px `text-secondary`, 5 source images `:55-66`
- Battery % 15px `text-secondary`, 8px gap `:68-75`
- Wi-Fi: 4 bars, 3px wide, heights 5/8/11/14, 5px pitch, `r 1px`, lit
  `text-secondary` / unlit `border-raised` `:83-95`

---

## 12. Pagers and dot indicators

- Hub area pager: `home_hub.slint:583-594` — 6px dots, r3, 6px spacing, centred,
  `accent` / `border-raised`. Slot reserved at `:101, 117, 412`.
- Activity-pages pager: `activity_pages.slint:39-48` — two 72×52 r14 buttons
  with 30px arrows and a **"1 / 2" text counter**, 20px `text-secondary`,
  110px wide, at y720/730.

Two entirely different pager designs for the same idea.

---

## 13. Dividers and spacing constants

- Only one divider in the UI: the status-bar rule, `status_bar.slint:23-27`.
- Page padding: `T.pad-top` 18 / `T.pad-side` 20 / `T.pad-bottom` 22 used by
  hub, chooser, settings, keyboard, room list. **Not** used by: `light.slint`
  (x 24/36/88), `tv.slint` (24/32/40), `cinema.slint` (24/28/32/36/38),
  `thermostat.slint` (22/24/70/88), `activity_pages.slint` (18/24/88),
  `camera.slint` (24/88), `setup.slint` (28 top), `wifi_setup.slint` (26 top),
  `no_config.slint` (40 top).
- Card inner padding is `18px` at **15 sites across 5 files** (`chooser.slint:168-169`;
  `room_devices.slint:155 ×2, 179`; `settings.slint:20-21, 287 ×2`;
  `volume_overlay.slint:33-34`; `home_hub.slint:256-257, 327, 443-444`) — 58% of all
  hardcoded padding in the UI, and never a token. The nearest is `T.pad-side` at 20px.
  It is a de-facto `pad-inline: 18px` that was never named. A further 13 `x: 18px`
  offsets in the absolutely-positioned screens are the same inset by hand.
- Ring bleed `6px` written out in `home_hub.slint:89`, `room_devices.slint:86,
  130, 138-142`, `chooser.slint:60`, and as `IRIS_RING_BLEED` in
  `src/panel.rs:125`. `T.ring-offset + T.ring-width` is the same number and is
  only used inside `focus_ring.slint:19`.
- List gaps: 12 (hub rows, device rows, chooser), 10 (settings), 6 (keyboard),
  14 (hub blocks), 8 (tray lists, implied by pitch).

---

## 14. Animation durations and easings

Every animation in the UI, with its duration:

| file:line | property | duration | easing |
|---|---|---|---|
| `home_hub.slint:134` | `scroll` | `anim ? 160ms : 0ms` | ease-out |
| `home_hub.slint:221-224` | ring `y,h` | `anim ? 160ms : 0ms` | ease-out |
| `home_hub.slint:341` | disc `colorize` | **180ms** | ease-out |
| `home_hub.slint:495-498` | ring `opacity` | `hidden ? 0ms : 200ms` | ease-out |
| `room_devices.slint:35` | `scroll` | `anim ? 160ms : 0ms` | ease-out |
| `room_devices.slint:134` | ring `y,h` | `anim ? 160ms : 0ms` | ease-out |
| `room_devices.slint:188` | disc `colorize` | **180ms** | ease-out |
| `chooser.slint:83` | `scroll, ring-y` | **`anim ? 140ms : 0ms`** | ease-out |
| `settings.slint:433` | ring `y` | **140ms** | ease-out |
| `keyboard.slint:496, 562, 610` | ring `x,y` | `anim ? 160ms : 0ms` | ease-out |
| `cinema.slint:54` | `list-scroll` | 160ms | ease-out |
| `bottom_tray.slint:14` | `reveal` | 200ms | ease-out |
| `volume_overlay.slint:18` | `reveal` | 200ms | ease-out |
| `app.slint:845` | toast `reveal` | 200ms | ease-out |

Durations: **140, 160, 180, 200 ms**. `T.fade` (160ms) exists and is used by
none of them. Easing is `ease-out` everywhere — consistent.

Host-side (not Slint), `src/panel.rs`: `SLIDE` 180ms `:113`, `IRIS` 300ms `:121`,
`LIFT` 320ms `:143`, plus 12 phase constants `:165-211`.

---

## 15. Keyboard

`ui/components/keyboard.slint`, 622 lines. The most token-disciplined file in
the project: every fill is `T.surface*`/`T.accent*`, every radius is
`T.r-control` or `T.r-card`, every text colour is a token. Its own geometry
(`:157-170`) is derived arithmetic, documented in `docs/keyboard.md`.
Only divergences: 3 focus rings instead of 1 (justified — three separate
bands, none of which can travel to another), and `21px/600` key faces with a
comment explaining the size was chosen because it was already embedded.

---

## 16. Artwork frames

- `VolumeOverlay` art: 84×84 square, `image-fit: cover`, **no radius, no border**,
  at (W−18−84, 18). `volume_overlay.slint:28-31`
- TV media art: full-bleed `image-fit: cover` + a 4-stop black gradient scrim
  `@linear-gradient(180deg, #00000090, #00000020 20%, #000000c0 48%, #000000 78%)`.
  `tv.slint:76-81`
- Cinema fanart: full-bleed `image-fit: cover`, **no scrim at all** — the text
  sits directly on it. `cinema.slint:60`
- Cinema logo: `image-fit: contain`, 125px tall, left-aligned. `cinema.slint:66`
- Camera frame: `image-fit: contain`, full width × 270. `camera.slint:14`
- QR: `image-rendering: pixelated`, native size, on a white `T.r-row` card.
  `setup.slint:59-66` (320px card), `no_config.slint:60-67` (208px card)

---

## 17. Literal values used instead of a `Theme` token

### 17a. Colour literals outside `theme.slint`

| literal | count | where | nearest token |
|---|---|---|---|
| `#ffffff` | 8 | `tv.slint:147,155,163`; `cinema.slint:15,62,67,104,109,118` | `T.text` `#F2EDE4` or `T.accent` |
| `#00000088` | 1 | `bottom_tray.slint:21` | — |
| `#000000E0` | 1 | `mic_overlay.slint:29` | — |
| `#000000F0` | 1 | `bt_pair_overlay.slint:18` | — |
| `#000000F5` | 1 | `pair_overlay.slint:15` | — |
| `#00000090/20/c0/000000` | 1 gradient | `tv.slint:80` | — |
| `#E04030` | 1 | `mic_overlay.slint:31` (recording red) | none — deliberate, the only second hue |
| `#FFB463 #FFE9C8 #FFFFFF #C9DCFF` | 1 gradient | `light.slint:146` (kelvin ramp) | none — deliberate |
| `#FFFFFF` (QR card) | 2 | `setup.slint:51`, `no_config.slint:58` | deliberate |
| `#111311` | 1 | `cinema.slint:59` (page background) | `T.bg` `#15130F` |
| `#222623` | 1 | `cinema.slint:13` (control fill) | `T.surface-raised` `#241F19` |
| `#ffffff15` | 1 | `cinema.slint:15` (control idle edge) | `T.border-raised` |
| `#151319` | 1 | `cinema.slint:17` (icon on accent) | `T.bg` |
| `#f6f6f1` | 1 | `cinema.slint:17` | `T.text` |
| `#eceee8` | 1 | `cinema.slint:18` | `T.text` |
| `#cbcfc8` | 1 | `cinema.slint:63` | `T.text-secondary` |
| `#d2d5ce` | 1 | `cinema.slint:68` | `T.text-secondary` |
| `#c7cec3` | 1 | `cinema.slint:73` | `T.text-secondary` |
| `#ffffff35` | 1 | `cinema.slint:74` (progress track) | `T.border-raised` |
| `#f1f2eb` | 1 | `cinema.slint:93` | `T.text` |
| `#c0c7bc` | 1 | `cinema.slint:106` | `T.text-secondary` |
| `#35303e` | 1 | `cinema.slint:108` (selected tray row) | none — **purple, breaks the warm palette** |
| `#242823` | 1 | `cinema.slint:108` | `T.surface-raised` |
| `#b7beb2` | 1 | `cinema.slint:110` | `T.text-secondary` |
| `#30263e` | 1 | `cinema.slint:117` (message card) | none — **purple** |
| `#171717` | 1 | `app.slint:702` ("End") | `T.surface` |
| `#101010` | 1 | `app.slint:707` (busy scrim) | `T.bg` |
| `#bbbbbb` | 1 | `app.slint:711` | `T.text-secondary` |
| `#292929` | 1 | `app.slint:712` | `T.surface-raised` |
| `#050505` | 1 | `app.slint:931` (dock) | `T.bg-sleep` `#0D0C0A` — the dead token! |
| `#b4b4b4` | 1 | `app.slint:940` | `T.text-secondary` |
| `#737373` | 1 | `app.slint:951` | `T.text-tertiary` |

**Totals from a brace-aware parse of all 23 files: 51 colour literals outside
`theme.slint`. Sixteen of them are `#ffffff`/`#FFFFFF` — an exact match for
`T.accent` — written as raw white instead of the token (`app.slint:703,710,713`;
`cinema.slint:15,62,67,72,104,109,118`; `tv.slint:147,155,163`; `light.slint:146`;
`no_config.slint:58`; `setup.slint:51`). `cinema.slint` alone holds 17 of the 51.**
(`transparent` appears 24 more times; it is a Slint keyword, not a colour literal,
and `theme.slint` deliberately has no token for it.) It is the one
screen that does not use the theme, and because `T.accent` is user-settable
(`src/home.rs:289-294`), every `#ffffff` there is a value that **will not follow
the owner's chosen accent colour** while the same element on the TV screen does.

In Rust:
- `src/main.rs:66` `BACKGROUND: u32 = 0x09090b` — the colour the framebuffer is
  claimed with at startup (`:329`). It is **not** `T.bg` (`#15130F`).
- `src/panel.rs:147, 157, 159` `LIFT_BG 0xff0f_1315`, `LIFT_SURFACE 0xff17_1c1f`,
  `LIFT_BORDER 0xff1f_272c` — byte-swapped copies of `T.bg`, `T.surface`,
  `T.border`, kept in sync by comment only.
- `src/panel.rs:138` `IRIS_RING 0xffff_ffff` — hardcoded white; will be wrong
  the moment the owner picks a non-white accent.

### 17b. Font sizes (24 distinct values, 2 token references)

| px | count | px | count |
|---|---|---|---|
| 15 | 21 | 23 | 3 |
| 21 | 17 | 28 | 2 |
| 16 | 13 | 32 | 2 |
| 20 | 12 | 36 | 2 |
| 19 | 12 | 12 | 2 |
| 18 | 12 | 11 | 2 |
| 26 | 11 | 48, 62, 40, 30, 24 | 1 each |
| 14 | 8 | `two-up ? 36 : 48` | 1 |
| 22 | 6 | `range ? 46 : 76` | 1 |
| 17 | 5 | **`T.device-name`** | **2** |
| 27 | 3 | | |

Distinct (size, weight) pairs in use: 11/—, 12/—, 14/—, 15/—, 16/—, 17/—, 18/—,
19/—, 19/600, 20/—, 20/600, 21/—, 21/600, 22/—, 22/600, 23/—, 24/—, 26/—,
26/600, 26/700, 27/—, 27/600, 28/600, 30/600, 32/600, 32/700, 36/700, 40/700,
46/700, 48/700, 62/600, 76/700. **32 pairs.** `docs/slint-notes.md` notes that
the SDF payload grows with the *largest* size, so the long tail is cheap but the
26/32/36/40/46/48/62/76 head is not free.

### 17c. Radii

| value | count | token equivalent |
|---|---|---|
| `T.r-card` | 13 | — |
| **6px** | 6 | none (between `r-meter` 5 and `r-control` 10) |
| **18px** | 5 | none (between `r-overlay` 16 and `r-disc` 26) |
| `T.r-row` | 4 | — |
| **14px** | 4 | **= `T.r-row`** |
| **12px** | 4 | **= `T.r-card`** |
| `T.r-control` | 3 | — |
| **3px** | 3 | **= `T.r-glyph`** (the dead token) |
| `T.r-overlay` | 2 | — |
| `T.r-meter` | 2 | — |
| `T.r-disc` | 2 | — |
| **26px** | 2 | **= `T.r-disc`** |
| **16px** | 2 | **= `T.r-overlay`** |
| **22px** | 2 | none |
| **2.5px** | 2 | none |
| **11px** | 2 | none |
| 1, 4, 4.5, 8, 9.5, 24, 30, 40, 44 | 1 each | none |

**Fifteen** bare radii exactly equal an existing token (sixteen counting the
`14px` branch of `cinema.slint:12`'s ternary). Of 69 `border-radius:`
declarations in the UI, 25 use a token and 44 are literals.

### 17d. Border widths

`1px` × 26, `2px` × 4, `T.ring-width` × 1, and five `selected ? 3px : 0/1px`
ternaries (see §7).

### 17e. Recurring fixed pixel sizes

- **52px**: icon disc ×3 (`home_hub:331-332`, `room_devices:181`, `light:81`),
  TV header image (`tv:87`), and the height of three pager/Media buttons
  (`activity_pages:40,45,49`) — and 52px is also the `activity-busy` Cancel
  button height (`app:712`).
- **48px**: `MediaBack` (`media-back:4`), cinema tray close (`cinema:105`),
  light level read-out box height (`light:98`).
- **44px**: TV tray close (`tv:148`), "Pages"/"End" height (`app:696,701`),
  TV retry (`tv:164`).
- **24px**: glyph size in status bar, settings rows, TV tiles, TV Apps pill,
  thermostat chevron — 9 sites. **22px**: chevron, scenes arrow, mic/bt dot — 5.
  **26/27/28/30/36px**: settings section arrow 26, `MediaBack` 27, cinema
  control 27, TV `IconButton` 28, activity tile + pager 30, thermostat disc 36.
  **Six glyph sizes.**

---

## 18. Geometry duplicated across the Slint / Rust boundary

| value | Slint | Rust |
|---|---|---|
| Status bar height 56 | `status_bar.slint:19` | `app.slint:62` `status-h`, read by `main.rs:1652` |
| Ring bleed 6 | `home_hub:89`, `room_devices:86`, `chooser:60` | `panel.rs:125` `IRIS_RING_BLEED` |
| Ring width 3 | `T.ring-width` | `panel.rs:127` `IRIS_RING_WIDTH` |
| Accent colour | `T.accent` (settable) | `panel.rs:138` `IRIS_RING = 0xffff_ffff` |
| `T.bg` / `surface` / `border` | `theme.slint:6,8,13` | `panel.rs:147,157,159` |
| Activity-pages header 130 / footer 644 | `activity_pages.slint:20-23, 35-49` | `main.rs:72-73` |
| The lift's header card (20,14,440,76,r12) | **nowhere** | `panel.rs:162` |
| Light-screen boxes | `light.slint:204-239` (exported) | `main.rs:147-207` (read) — the only screen that does this |

---

## 19. Markup copied between files rather than shared

Three fragments exist twice, verbatim or near-verbatim, in two files each:

1. **The audio-bar glyph** — three `Rectangle`s, 3px wide, 10/16/7px tall, `T.accent`,
   3px spacing, inside a centred `HorizontalLayout`.
   `ui/screens/home_hub.slint:263-269` and `ui/components/room_devices.slint:159-165`.
   Byte-for-byte the same. Its sibling (the play triangle, 14×18 `glyph-play.png`
   colorized `T.accent`) is duplicated in the same two places:
   `home_hub.slint:270-274` / `room_devices.slint:166-170`.
2. **The zero-size focus-ring anchor** — a `Rectangle` of 0×0 holding
   `ring-y`/`ring-h`/`ring-r` with an `animate ... { duration: anim ? 160ms : 0ms }`,
   written so that moving the ring cannot dirty the root that paints the background.
   `home_hub.slint:214-225` and `room_devices.slint:127-135`. Same pattern, same
   comment, two copies. (The technique is correct and documented — it is only the
   copy that is the problem.)
3. **The row's disc geometry, written twice inside one file.**
   `room_devices.slint:180-190` lays the disc out as `padding: 18px`, a 52px
   rectangle, `spacing: 16px`; and then `room_devices.slint:115-118` hand-derives
   the *same* three constants again in the functions the lift reads:
   `disc-inset() -> 18px`, **`disc-box-y() -> 18px`** (added by #282),
   `label-box-x() -> 18px + 52px + 16px`, and
   `label-box-w() -> width - 2*pad-side - (18px + 52px + 16px) - 16px - 22px - 18px`.
   That is four hand-written copies of the row's `padding: 18px` inside one file,
   and #282 added the fourth — the count is going up, not down.
   Change the row's padding and the transition silently flies the label to the wrong
   place. The comment at `:109-114` explains why it must be arithmetic rather than a
   layout read-back — that reasoning is sound; what is missing is one place for the
   three numbers to live.

Item 3 is the one that matters: it is the only duplication here that can produce a
*wrong* result rather than merely a divergent one, and it sits directly under the
lift.
