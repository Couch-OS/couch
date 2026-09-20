# The packaged Hue integration on real hardware: what the trial showed

Status: **results of a one-day trial, 2026-09-20.** Not a plan for users. No
packaged Hue reaches a user before T5 of the
[extraction roadmap](integration-extraction-roadmap.md); this was run to find
out whether protocol 3's first three steps
([groundwork](protocol-3-t1-groundwork.md),
[children and domain controls](protocol-3-t2-children-and-domain-controls.md),
[pairing and isolation](protocol-3-t3-pairing-and-isolation.md)) hold up against
a real bridge before the media step is built on them.

## What was done

- A **preview build** with protocol 3 switched on was installed on the
  development remote only (tags `…191.p3.dev` and `…193.p3.dev`, Dev channel,
  red notice in the web UI). The published preview releases were deleted the
  same day.
- A **Hue package** (`couch-integration-hue`, `0.1.0_pre3`) was installed from a
  scratch package source signed with a throwaway key, never from the official
  feed.
- The owner paired it with his bridge by pressing the link button, the first
  real use of the pairing dialog.
- Only one Hue room was ever written to, through a helper that refuses any
  other id; every write was restored. The built-in Hue connection was left
  alone throughout.

## What worked

| Thing | Result |
| --- | --- |
| Pairing | "Paired with Hue bridge …913FB6". Key file mode 600 in a 700 folder, package running as its own user (60003). |
| Listing | 243 things (48 lights, 14 rooms and zones, 181 scenes) in 0.13 s, every light with a room hint. |
| One lamp | On, off, toggle, brightness, colour temperature, clamping at both ends, a colour request refused. 65 to 970 ms. |
| A room | A dragged brightness: no write refused, the last value lands. |
| A scene | Recalled. |
| Reads | 50 in a row: median 37 ms, slowest 281 ms, none refused. |
| Memory | 4.1 to 4.4 MB after fifteen minutes, same process throughout. |
| Pairing again, then cancelling or letting it run out | The existing pairing is untouched and the lamps keep answering meanwhile. Expiry at 117 s with its own message. |
| A second connection at an address nothing lives at | Refused as unpaired in 0.2 s; the working connection was not slowed. |
| An ordinary build installed over the preview | Package folder, user table, key and pairing files byte-identical; no package process; the package hidden from the list; the connection, its four devices and its scene kept in the newest layer of `config.json`. |
| The preview installed again | Lamps answer straight away: same key, same user, no pairing. |

One caution for whoever checks this again: `config.json` is layered, and its
top level is the oldest runtime's view, which has no packaged connections by
design. Look at `integration_config_v3`, or ask the API.

## What was wrong

**In Couch**

1. **Two things asking one connection at once refuse each other.** A
   connection answers one request at a time and the second was told "busy"
   (503) immediately: about one read in ten with two readers, half with eight.
   The panel reading rows while a phone has the page open is two readers. The
   package SDK's queue of eight was never reached. Fix: #268, a quarter-second
   wait before "busy".
2. **The list of a bridge's things is cached for five minutes**, and when it
   lapses a lamp that is not yet a Couch device answers 404 until somebody
   refreshes. A page left open will hit this.
3. Wording: the pairing dialog says "The device refused to pair." twice; a
   connection that has never been paired says it "needs to be paired again"; an
   ordinary build asked about a protocol 3 package says "Invalid integration
   settings or package" rather than that the package needs a newer Couch.
4. The first read after the daemon starts waits a second and answers "unknown".

**In the package** (fixed in `couch-integration-hue` #3 and #4)

5. A real bridge refuses `"generateclientkey": false`; pairing failed at once.
   The fake bridge now refuses it too.
6. A dragged room could end on the value before last, and its level stepped
   backwards for a moment (40, 45, 40).
7. At an address nothing lives at, pairing asked for a button press for two
   minutes. It now says nobody answered after two unanswered tries.
8. Still open: after a fast drag the room's level is right immediately and the
   single lamps follow five or six seconds later (a second and a half after
   one write).

## What it means for the train

Nothing found argues against building the media step on T1 to T3. Item 1 has to
be in before any bridge-like package is used from the panel and a browser at
once; item 2 wants an answer before T5. Two gaps the trial could not reach are
still open: the admission harness cannot hand a key to a package that pairs,
and feed metadata is still compared to the literal 2.
