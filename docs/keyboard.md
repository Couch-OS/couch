# The on-screen keyboard

`ui/couch-gui/ui/components/keyboard.slint`. Built ahead of its first consumer,
which is Wi-Fi passphrase entry: the setup screen currently hands that job to a
phone and a captive portal, and the panel should be able to do it alone.

Nothing imports it yet. It is a self-contained component - theme tokens and
`FocusRing` are its only dependencies - and it is exercised by
`ui/keyboard-demo`, a desktop harness, and priced by `ui/keyboard-probe`, which
cross-compiles it for the device twice to measure what it costs.

## The shape of it

480x744: the panel less the 56px status bar. Geometry is computed from the
width it is given, and at 480 it lands on whole pixels everywhere - 38px keys,
6px gutters, a 3px inset inside the 20px margins - because a rounded rectangle on a fractional
boundary is anti-aliased along all four edges, and anti-aliased rounded
rectangles are what this renderer spends its frame on.

| band | height |
|---|---|
| title | 19px |
| field | 64px |
| air (absorbs whatever height the caller gave over) | 108px at 744 |
| key grid, 10 x 5 at 38x63 with 6px gutters | 339px |
| function bar | 64px |
| commit bar | 64px |

Vertical slack lives *above* the grid, not below it: air between the field and
the keys reads as spacing, air between the keys and the buttons reads as a
mistake. Give it less height and that band closes first.

## Layouts

Three pages, each the same 10x5 rectangle, and **the digit row is identical
on all three**:

```
    abc                        ABC                        #+=
    1 2 3 4 5 6 7 8 9 0        1 2 3 4 5 6 7 8 9 0        1 2 3 4 5 6 7 8 9 0
    q w e r t y u i o p        Q W E R T Y U I O P        ! " # $ % & ' ( ) *
    a s d f g h j k l '        A S D F G H J K L '        + , - . / : ; < = >
    z x c v b n m , . ?        Z X C V B N M , . ?        ? @ [ \ ] ^ _ ` { |
    - _ @ / : ; ( ) ! &        - _ @ / : ; ( ) ! &        } ~ . , - _ @ ' ? !
```

Between them and the SPACE key that is **every printable ASCII character**,
32 through 126, which is exactly the set a WPA passphrase is allowed to contain.
The symbol page carries all 32 punctuation marks in ASCII order, with the eight
commonest repeated to fill its last row. The fifth row of the letter pages is
the punctuation an address or a passphrase needs without a page switch.

Why QWERTY: it began alphabetical, on the argument that a D-pad hunts and
hunting is faster when a key's position follows from the alphabet. Tried on
the panel, familiarity won - people look for keys where every other keyboard
puts them, and ten columns at 480px is still a keyboard a thumb can use. Ten
columns cost the key width: 38px keys with 6px gutters and a 21px face (a
size the card titles already embed), against the 56px keys the 7x6 grid had.

Why the fixed digit row:

- The digits and the four characters an address needs (`- _ . @`) are never
  behind a page switch, which is most of what passphrases and SSIDs are made
  of after the letters.
- The focus index still means something across a switch, so SHIFT leaves the
  ring on the capital of the letter it was on.
- A switch redraws four rows instead of six. They are a separate constant array
  from the three that swap, precisely so their bindings are never invalidated -
  sharing one array would leave the values identical and mark them dirty
  anyway, and the renderer redraws what was marked.

The two spare cells on the letter pages are `'` and `?`: the apostrophe is the
punctuation ordinary typed text needs most once `. - _ @` are already on row
two, and the question mark is the most visually distinct thing to put beside
it. Everything else is one press of `#+=` away.

The symbol page is in ASCII order. That is arbitrary, but it is the one order
somebody might predict, and it keeps the bracket pairs near each other.

## D-pad navigation

Three regions stacked vertically: the grid, the function bar, the commit bar.
Focus is a region plus a cell - a pair of integers - rather than Slint's
per-element focus, because the keypad delivers key codes with no geometry
attached and the ring is authored in reading order rather than derived from
layout.

**Left and right** step through the grid as one linear sequence in reading
order. Left at the left edge therefore goes to the **end of the row above**,
not to the end of the same row, and left from the very first cell wraps to the
very last. The alphabet is a sequence, so the two keys either side of `a` are
`@` and `b`; no cell is a dead end and nothing needs seven presses to escape.
In the two bars, left and right wrap within the bar.

**Up and down** step between rows, and off the top or bottom of a region they
move to the next region. The whole keyboard is one vertical ring: up from the
top row of the grid is DONE, down from DONE is back in the grid. On a panel
with eight rows of stops that is worth several presses.

**Each region remembers where focus was when it left it.** Step down to SPACE
and back up and the ring is on the letter you were on. The bars start on their
most likely key - SPACE and DONE - rather than on their first.

**OK** activates. **BACK** deletes a character while there is one to delete,
and only cancels once the field is empty: the remote has no backspace key, and
losing a half-typed passphrase to the button people press by reflex is the
worse of the two failures. **SHIFT** is a one-shot: the next character is a
capital and the page drops back to lower case. Pressed twice before typing it
latches caps lock and the key reads CAPS; a press after that releases it. The
symbol page and SHOW are plain latches.

SHIFT, `#+=` and SHOW are latches, not one-shots. A one-shot flips the page out
from under the cursor after every keystroke, which is disorienting when the
cursor is the thing you are steering; and a passphrase with four capitals would
pay for the convenience four times. SHIFT from the symbol page returns to the
letters, in the other case.

One ring moves rather than 42 borders switching on and off - the rule from
`docs/slint-notes.md`. The ring for a region that does not have focus stays
parked where it was, so returning to it does not animate in from wherever some
other region's index happened to point.

## Touch

The digitizer (`mtk-tpd`, event3) has not been confirmed to deliver events. The
component is built so that this does not matter: everything works from the
D-pad, and touch is Slint's ordinary `TouchArea` handling, which costs nothing
at runtime if no pointer event ever arrives.

A press **moves the same ring the D-pad moves** and the release activates what
is under it. That means there is no per-key pressed state to draw, the two
input methods share one visual language, and a finger that slides drags the
ring with it and commits wherever it lifts. The ring's animation is switched
off for pointer input - a touch should land instantly under the finger, a key
press should be followed by the eye.

The grid has **one** touch area for all 42 keys; the cell is arithmetic on the
coordinates. The gutters belong to the key on their left, which makes every hit
target slightly larger than the key it looks like.

Key targets are 56x63 with 8px gutters, so 64x71 of hit area - comfortably over
the 44px a thumb wants. Verified in the harness by dispatching synthetic
pointer events (`tap:x:y` in the script language), which is as far as this can
be taken without the hardware.

## Editing

The text lives in a `TextInput`, not in a string property, because **`.slint`
can concatenate a string but cannot shorten one** - there is no substring
function, so a backspace written in Slint is not possible. `TextInput` brings
the cursor, the password mask and the scroll-to-cursor with it, and its
`key-pressed` callback runs *before* its own handling, which is how the arrows
stay with the grid instead of moving the caret.

It is driven rather than typed into. The hardware has six keys and none of them
produce characters, so:

- **insert** appends to `text`;
- **DEL** selects the last character and calls `cut()`. `cut` copies before it
  deletes, but `Platform::set_clipboard_text` is a no-op by default and the
  device's platform does not implement it, so nothing leaves the process. On a
  desktop with a real clipboard it does, which is worth knowing before using
  this harness to type a real passphrase.

Two consequences worth knowing:

- **The caret is always at the end.** Left and right belong to the grid, so
  there is no way to move it and no way to get it back if it moves. A `changed
  text` handler pins it, which also fixes text a caller seeds - a fresh
  `TextInput` starts its cursor at offset zero, so seeded text would otherwise
  show the caret in front of it.
- **The buffer is assumed to be ASCII**, which is all this keyboard can
  produce. `character-count` is a byte offset only while that holds. Seed it
  with something wider and the backspace offset lands mid-character, Slint
  rounds it up to the end of the string, and the selection comes out empty: a
  key that does nothing rather than one that eats the tail.

The blinking caret invalidates a few pixels twice a second while the keyboard
is on screen, which is a periodic wake-up on a battery device. It is the signal
that the field is live, and it stops when the keyboard is dismissed.

## What it costs

The measurements in this section describe the original bitmap-font build.
The current GUI uses [SDF font embedding](slint-notes.md#glyph-embedding-happens-at-compile-time),
which shares one scalable glyph set per face; the historical per-size font
costs below no longer apply to it.

Measured, not estimated. `ui/keyboard-probe` builds the same minimal binary for
`armv7-unknown-linux-musleabihf` three ways, with couch-gui's Slint feature set
(`default-features = false`, no `std`) and its release profile:

| probe build | bytes | delta |
|---|---|---|
| baseline: text at 15px/400, 26px/400, 26px/600 | 665,972 | - |
| baseline + a bare `TextInput` | 771,788 | +105,816 |
| baseline + `Keyboard` | 883,644 | **+217,672** |

So roughly half the price is the editable field's machinery, which arrives
whole and is the cost of not writing a text editor.

Integrated for real - a copy of couch-gui outside the tree with the keyboard
mounted as a full-screen overlay in `app.slint`, built for the same target:

| couch-gui build | bytes |
|---|---|
| as it ships today | 1,696,372 |
| with the keyboard | 1,908,268 |
| **delta** | **+211,896 (207 KiB, +12.5%)** |

The two numbers agreeing within 6KB says the component shares almost nothing
with what is already there, and that no font data is being double-counted.

**No new font sizes.** Glyphs are rasterised into the binary per pixel size for
the whole character set, so a size is expensive and the cost grows with its
square. The keyboard uses only combinations couch-gui already pays for -
15px/400 for labels, 26px/400 for the field, 26px/600 for the key faces. The
discipline is worth what it sounds like it is worth: changing the key faces
from 26px to 24px, a difference nobody would see, was measured at **+41,232
bytes** on top of the integrated build. (`docs/slint-notes.md` records 162KB
for a size; that figure is for 48px. At 20px it is 32KB. It is the same rule.)

## Dirty regions

The harness prints the same accounting `COUCH_REGION=1` prints on the device.
These are hardware-independent; what they cost in milliseconds is not, and is
worth confirming on the panel.

| action | dirty | % of panel |
|---|---|---|
| step within the grid | 6,120 px | 1% |
| step across a row edge (wrap) | 10,344 px | 2% |
| step within a bar | 8,512 px | 2% |
| step between regions | 29,496 px | 7% |
| type or delete a character | 25,856 px | 6% |
| switch page | ~143,000 px | 36-38% |

The common case - moving the ring - is 1-2%, which is the point of moving one
element instead of restyling many. Typing redraws the whole field because
`TextInput` invalidates its own box. A page switch redraws four rows of glyphs
and is the only expensive thing here; it happens when somebody presses `#+=`.

## Running the demo

```
cd ui/keyboard-demo
cargo run                      # a 480x800 window, software-rendered
```

Arrow keys, Enter and Escape are the remote's six keys; the mouse stands in for
the digitizer; a real keyboard also types, because anything the component does
not claim falls through to the `TextInput`.

Headless, for screenshots and region numbers:

```
cargo run -- --shoot /tmp/shots --script "ok,r,ok,d,d,d,d,shot:typed"
```

Script tokens are `u d l r ok back home`, `tap:X:Y` (window coordinates), and
`shot:NAME` which writes `NAME.png`. The clock is virtual, so animations settle
in zero real time and two runs of the same script produce identical pixels.
`--plain`, `--title`, `--placeholder` and `--text` seed the field.

Both modes use the software renderer deliberately. Femtovg would anti-alias
differently, lay out text differently, and hide exactly the costs this
component was designed around.

Sizes, on a machine with the `armv7-unknown-linux-musleabihf` target installed:

```
cd ui/keyboard-probe
cargo build --release --target armv7-unknown-linux-musleabihf
cargo build --release --target armv7-unknown-linux-musleabihf --features keyboard
ls -l target/armv7-unknown-linux-musleabihf/release/keyboard-probe
```

Both demo and probe are their own workspaces, so neither can touch
`ui/Cargo.lock` or what ships.

## Using it

```slint
import { Keyboard } from "components/keyboard.slint";

if root.asking-for-passphrase: Keyboard {
    x: 0px;
    y: 56px;                            // under the status bar
    width: root.width;
    height: root.height - 56px;
    title: "WI-FI PASSWORD";
    placeholder: "Passphrase";
    password: true;
    text <=> root.passphrase;
    accepted(t) => { root.join(t); }
    cancelled() => { root.asking-for-passphrase = false; }
}
```

| property | | |
|---|---|---|
| `title` | `in string` | the letterspaced label above the field |
| `placeholder` | `in string` | shown while the field is empty |
| `password` | `in bool` | masks the field and adds the SHOW key |
| `text` | `in-out string` | the current text, in and out |
| `accepted(string)` | callback | DONE; carries the text |
| `cancelled()` | callback | CANCEL, or BACK on an empty field |
| `take-focus()` | function | re-take keyboard focus |
| `reset()` | function | clear the text, the latches and the ring |

**Instantiate it conditionally rather than keeping it hidden.** It takes focus
in `init`, because a modal keyboard that ignores the D-pad until somebody
remembers to call a function is a trap; an instance that exists behind another
screen would take focus away from it.

The component is sized by its caller - a component's own root cannot see a
parent it does not have - so give it a width and a height. It wants about 715px
of height before the air band runs out.

A consumer needs no Rust. `text` is `in-out`, so a Rust host reads it with
`get_text()` after `accepted`, or watches it with a `changed text` handler in
`.slint`.

## Do not delete the key faces

The arrays `head`, `tail-lower`, `tail-upper` and `tail-symbol` near the top of
`keyboard.slint` are not only data. **Glyphs are embedded at compile time from
string literals that appear in `.slint`**, and everything a keyboard emits is a
runtime string, so those literals are part of why the font contains what it
contains.

Slint seeds its own coverage with the alphabet, the digits and most
punctuation, so most of these are covered twice. The character that is not is
the **backtick**: it is in no default set, and the only reason the panel can
draw one is that it is written out in `tail-symbol`. Generate these faces in
Rust instead of listing them and that key goes blank - which is the same
failure the `·` separators hit, recorded in `docs/slint-notes.md`.

The same applies to the labels in `fn-label()` and the commit bar - `SHIFT`,
`#+=`, `abc`, `ABC`, `SPACE`, `DEL`, `SHOW`, `HIDE`, `CANCEL`, `DONE`. Words
rather than symbols partly for that reason: Lato has no `⌫`, `⇧`, `␣` or `⏎`,
and the house style already says `CHG`, `HUB`, `OFFLINE` and `IDLE`.

## Open questions

- **Touch is unverified.** Nothing in the component depends on it, but the key
  size and the press-moves-the-ring behaviour are guesses until the digitizer
  is known to deliver events.
- **BACK deletes.** It is the right default for a passphrase and it is not what
  BACK means anywhere else in couch-gui. If a consumer needs BACK to leave a
  half-typed field, that wants a property rather than a change here.
- **No caret movement.** Fixing a typo in the middle means deleting back to it.
  A long-press or a second mode could add it; nothing has asked yet, and the
  keys it would need are the keys the grid uses.
- **207KB.** Half of it is `TextInput`, and the alternative to `TextInput` is
  writing text editing in Rust and passing it through the component's boundary,
  which is a worse component for a smaller binary.

## Injecting keys: the kernel drops codes the device does not declare

Writing an `input_event` to `/dev/input/eventN` is the way to drive the panel
from a shell without a finger, but the input core runs every event - injected
or real - through `is_event_supported` against the device's `keybit` bitmap,
and silently drops any key code the device did not register. `mt_gpio_kpd`
declares only the codes in its keymap (the D-pad, OK, back, home, mic), so a
mapped key can be injected and an unmapped one cannot: the write succeeds, the
event never arrives, and nothing logs. A real press of an unmapped key does
arrive - on whichever of the two keypad nodes owns it - which is how "wake on
any key" can be true for hardware and untestable by injection. Verify the
mapped-key paths by injection; verify the unmapped path by pressing a real
button that the log shows as `unmapped key code N`.
