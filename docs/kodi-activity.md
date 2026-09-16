# Kodi Cinema activity screen

The native Rust/Slint activity screen implements the selected Cinema design:
full-screen fanart and clearlogo, playback progress, play/pause, seek, chapters,
audio/subtitles, with immediate physical control of Kodi. The touch back arrow
closes a sheet or returns to Couch without stopping playback.

Add a Kodi connection, assign its player to a room, and create an activity
with that device as the source. Attach the activity to a room or area. A
room's Kodi device row also opens the player directly. Startup command steps
are not executed yet; this screen controls playback already running in Kodi.
Enable Kodi's TCP remote-control service (default port 9090).

Opening a Kodi device or activity immediately routes D-pad/OK/Back/Home to Kodi,
including while nothing is playing. Menu opens Kodi's context menu, mute toggles
Kodi mute, volume adjusts Kodi volume, and channel keys skip available chapters.
No gamepad button or navigation overlay is required. Physical controls continue
to target Kodi while a touch sheet is open; choose chapters/tracks and seek by touch.
Touch the back arrow to return to Couch. Both entry and exit use the standard
180ms framebuffer slide, including the full-screen header. A connected idle player
is distinct from a failed connection and does not show a Reconnect button.

Artwork uses Kodi's HTTP service. Set each connection's web port and private
credentials in **Connections → Kodi web access**. See [Connections](connections.md).
The old host-keyed `kodi-web.json` is still readable as a compatibility fallback.

Text and the dark background remain
usable when artwork is unavailable. Artwork decoding and networking run on
workers with bounded queues, download/decode limits and a small decoded-image
cache. Media identity and navigation generations discard stale replies.

Validation: Kodi protocol unit tests, GUI tests, ARM release build, and a
physical-device fixture covering playback, artwork, pause, chapter selection,
volume, Back, and launch from room and area activities. A physical CoreELEC
player answers TCP and HTTP ping; physical input routing is also covered by the
on-device authenticated Kodi fixture.
