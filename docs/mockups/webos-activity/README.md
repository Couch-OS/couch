# LG TV activity studies

Three interactive 480×800 screen directions, with icon-only playback controls
and no touch equivalents for hardware power, home, mute, D-pad or color buttons.
The sole navigation button returns to Couch without controlling the TV.

Serve the parent directory so the licensed Kodi-study artwork/font are available:

```sh
python3 -m http.server 8094 --directory docs/mockups
# http://localhost:8094/webos-activity/
node docs/mockups/webos-activity/check.cjs
```

- **A — Cinema (recommended):** artwork and playback first, with input, picture
  and sound selectors always available. Switch to TV-only mode to see the
  source-focused fallback when media metadata is unavailable.
- **B — Source deck:** prominent source identity and an input rail, above a
  compact playback deck. Useful for frequent switching between HDMI devices.
- **C — Dashboard:** media, favorite apps and a denser settings grid.

All actions are simulated. The default linked-Kodi example uses Tears of Steel
artwork and illustrative metadata; it is not content discovered from the LG TV.
The picture-preset preview is explicitly unverified. The normal picture sheet
explains the limitation and proposes a TV-settings shortcut, whose target still
needs verification. The selected named TV owns its inputs, settings and apps;
a linked player would own its movie metadata and timeline. Multiple TVs must
never share an inferred current source or credentials.

## What the actual LG connection exposed

Read-only probes on 2026-09-09 identified an **OLED48B4PUA**. The TV was using
HDMI 1 during the probe. Existing pairing was reused; no settings were changed.

| Capability | Result / design implication |
| --- | --- |
| Active app / input | Verified: HDMI 1, Mac. Suitable for a source title and status. |
| Input list | Verified: Mac, CoreELEC, Sonos Sound Bar, HDMI 4; connected flags available. |
| Apps | Previously verified list and titles; useful for favorite app launch tiles. |
| Sound output | Verified `external_arc`; selection endpoint exists, alternatives need checking. |
| Power, mute, volume, navigation | Already implemented and device-tested; keep on hardware buttons. |
| Play/pause, rewind/fast-forward | SSAP commands exist; behavior is app/input-dependent. |
| Previous/next item | Not a universal SSAP playlist API. Use only where the player supports it. |
| Picture settings | `settings/getSystemSettings` rejected by this pairing. Do not invent a current mode. |
| Detailed media state | `com.webos.media/getForegroundAppInfo` rejected by this pairing. |
| Channel info | Rejected in the tested HDMI state; not evidence that live-TV channel info never works. |

**Currently playing information:** current app/input is available. Movie title,
fanart, clearlogo, duration and seek position are not established through this
TV connection. Some webOS versions/apps expose playback state, which still
isn't equivalent to full movie metadata. For CoreELEC/Kodi on HDMI, use Couch's
Kodi connection directly and explicitly associate it with the TV input. The
TV UI can then combine real player artwork/timeline with TV picture/input/sound
controls. Native streaming apps need their own verified metadata source or the
honest app/source fallback.

**Picture modes are worth pursuing:** maintained third-party code implements
public `settings/setSystemSettings` picture-mode writes for webOS 9/2024 and
newer. This B4 is a candidate, but firmware, permissions and input/HDR mode
matter. First verify reads with appropriate pairing permissions; then test a
reversible mode change with a known original value. Do not ship blind mode
cycling, hidden calibration calls, or fixed HDR/SDR mode lists.

## References and validation

- [SSAP endpoints](https://github.com/home-assistant-libs/aiowebostv/blob/main/aiowebostv/endpoints.py)
- [Maintained TV client](https://github.com/home-assistant-libs/aiowebostv/blob/main/aiowebostv/webos_client.py)
- [Picture-settings implementation](https://github.com/chros73/bscpylgtv/blob/master/bscpylgtv/webos_client.py)
- [Home Assistant limitations and next/previous behavior](https://www.home-assistant.io/integrations/webostv/)

The external [`couch-integration-webos`](https://github.com/dangerouslaser/couch-integration-webos)
repository contains the maintained implementation of these capabilities.
Do not commit its raw output: system info can include a TV serial number.
Browser checks exercise all three designs, playback, input sheets, picture
fallback, offline mode and mobile overflow. PNGs are exported from the browser.

Assets reuse the Kodi study's Inter font (SIL OFL 1.1) and Tears of Steel still
(© Blender Foundation, CC BY 3.0, via Xiph); see its README and license files.
SVG paths come from Couch's Lucide catalog (ISC license).
