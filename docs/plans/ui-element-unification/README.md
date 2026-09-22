# The remote's screens: what's drawn twice, and what to do about it

Status: **review of `dev` at `95a94a3` (2026-09-21), approved by the owner to act on.** The detail is beside this page: [findings](findings.md) (each variant judged deliberate or accidental, with the proposed shared piece), [plan](plan.md) (small pull requests, pixel-identical ones separated from look changes) and [inventory](inventory.md) (every variant with file and line). Decided since: every screen opened from a row carries the round icon badge in its header, because the badge travels there in the lift.

You asked whether we have unnecessary variants of the same thing. We do — about
a dozen kinds. None of them is a disaster; together they're the reason the
remote reads as several apps rather than one. Here are the ones worth fixing,
plainest first.

**A note on the two you already spotted.** The round icon badge is now **done** —
it merged while I was reading (PR #282), and the device row and the light screen
share one drawing. Two things to know about it: the *home screen's* room rows
weren't included and still have their own copy, which is a two-line follow-up
worth doing; and in the merge the badge on the light screen lost its lighter
tint when the lamp is on, so an on lamp's badge now looks the same as an off
one's. That may be exactly what you wanted — it's the row's badge now, and the
row never tinted it — but you should see it rather than find it. The title sizes
are still only fixed on the light screen. Everything below is the rest.

---

## The twelve worth doing

**1. Every control screen draws its own header.** The back arrow, the name, the
line under the name — six screens, six slightly different versions. The name is
19, 20, 21 or 26 points depending which screen you opened; the back arrow sits
4 pixels lower on the light screen than everywhere else; on the packaged-device
page the second line has fallen out of the header and sits below it. **After:**
one header, identical on all six. *This is the one that matters most, and not
just for tidiness — see the box below.*

**2. The music player doesn't use the colour scheme at all.** It's the oldest
screen and it was written before we had one. Seventeen colours are typed in by
hand, two of them **purple** — you can see it: the highlighted row in the
player's "Sources" list is a different family of colour from the identical row
in the TV's "Choose an input" list. Worse, all its white text is frozen white,
so if you ever change the accent colour in settings, the player ignores you and
the TV screen doesn't. **After:** the player is the same warm brown-grey as
everything else, and it follows your colour choice.

**3. "What OK will press" is drawn nine different ways.** On a room row it's a
white outline *around* the card. On a TV tile it's a thick edge *inside* the
card. On a player button it's a different thickness again. On a thermostat mode
it's a fill change. **After:** one outline, the room-row one, everywhere the
D-pad moves; a fill change only for things that stay switched on (a keyboard
shift key, a blind's chosen button). This one also makes the screens redraw
less, because growing an edge costs more than changing a colour.

**4. A device row doesn't dim when the device is off or unavailable — but a
room row does.** On the room list, "Reading lamp · Unavailable" looks exactly as
alive as "Desk lamp · On · 40%". On the home screen, an idle room visibly sinks
back. **After:** they behave the same, and you can tell at a glance which things
in a room are actually doing something.

**5. Small all-caps labels: nine versions.** "SCENES" on the home screen and
"SCENES" one screen deeper are different colours and different letter spacing.
**After:** one, in two sizes — the eyebrow above a list, and the smaller caption
under a slider.

**6. The list row is drawn five times.** Room row, device row, chooser row,
settings row, scenes card. Same object, five copies — and one copy of the scenes
card lost its bold, so the same card is lighter on the room page than on the
home page. **After:** one row, five fillings.

**7. Bars and meters: four versions.** The volume meter is 10 points tall, the
microphone meter 12, and the two "how far through the track" lines are the same
5 points but with different corner rounding and different track colours — on two
screens that show the same thing. **After:** two, a fine one and a thick one.

**8. The microphone card and the Bluetooth pairing card are the same drawing,
typed out twice** in two files. So are the three dark backdrops behind pop-ups,
at 88%, 94% and 96% black — nobody chose three. **After:** one card, one
backdrop. Nothing visibly changes; about 55 lines disappear.

**9. The pop-up sheets don't match.** The TV's "Choose an input", the player's
"Sources" and the thermostat's mode list are three different designs: rows 74,
76 and 60 tall, titles bold on two of them and not the third, one with a close
button shaped like a circle, one like a square, one with none. **After:** one
sheet.

**10. "End", "Pages" and "Cancel" are grey.** They're the only three things in
the product painted in neutral grey instead of the warm palette, and they sit on
top of the player and TV screens where you see them most. **After:** they look
like the rest of the remote.

**11. Corner roundings.** Fifteen places type in a number that we already have a
name for, and two numbers (18 and 6) are used five and six times without ever
having been named. There's also a 44-pixel button asking for a 30-pixel corner,
which is impossible and quietly becomes a circle. And the standard inner padding
of a card — 18 points, used in fifteen places across five files — has never been
given a name, so it drifts every time someone adds a card. **After:** nothing looks
different; the numbers stop drifting.

**12. Text sizes: twenty-four of them.** For a screen this size that's roughly
three times what's needed, and it happened because of an old constraint — each
extra size used to add about 30KB to the firmware, so people reached for a size
that was already there. **That constraint is gone** (the font system changed in
September). **After:** eight named sizes. Dozens of one- and two-point changes:
invisible one at a time, and the single biggest reason the screens will start
looking like one product.

---

> ### Why the header is the one to do first
>
> When you press a room row today, only a **light or a blind** opens with the
> lift — the row rising into the header with its name and icon riding on it. A
> TV, a thermostat, a packaged device or a speaker opens with nothing, because
> the lift is hard-wired to the light screen: it's the only screen that tells
> the code where its title and icon sit. The finishing point the row rises to
> isn't even read from a screen — it's four numbers typed into the Rust.
>
> One shared header fixes that structurally. Every screen would then say where
> its pieces are, in the same words, and the lift would work on all of them.
> That's the difference between a nice transition on one screen and the way the
> remote opens things.

---

## Also found: five things that are simply wrong

- The panel is painted **#09090B** at boot before the first real frame — the
  background colour is **#15130F**. One character fix.
- The outline that rides the opening window is **hardcoded white**, while the
  row's own outline follows your accent colour. Pick a non-white accent and the
  outline changes colour halfway through.
- The thermostat's second line is the literal words **"Home Assistant"** —
  a thermostat reached any other way still says so.
- The camera's is the literal words **"UniFi Protect"**, same problem.
- Five named colours/sizes in the theme file are referenced by **nothing**,
  while the same values are typed in by hand elsewhere.

---

## What I'd do first

**Three PRs, in this order.**

1. **"Tokens where tokens already exist."** Purely mechanical, nothing changes
   on screen, no review needed beyond a glance. Clears the drift out of the way
   so the real changes are legible.
2. **The shared header, on the light screen only.** The component gets built and
   the light screen keeps its exact current pixels — so it's provably safe, and
   it's already covered by the strongest test we have (the one that checks the
   lift never pops, never leaves a ghost, and plays backwards exactly).
3. **The shared header on the TV and packaged-device screens**, then thermostat,
   camera, activity pages and player. *This* is where you'd want to look: the
   titles on those screens grow to match the room row, which is the same change
   you already approved for the light screen.

Then the lift starts working on every screen you can open from a row (PR 6 in
the plan). Everything else can follow at whatever pace suits.

**One thing I need you to decide, and I've deliberately not:** on the shared
header, does a TV / player / activity page get the round icon badge, or does the
badge appear only where the device has a state worth showing (a light, a blind,
a thermostat)? I can show you both side by side.

**Timing:** all of the header work should wait until the other agent's branch
lands, since we'd otherwise be editing the same four files.
