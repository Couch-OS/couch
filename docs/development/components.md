Title: Native components
Description: Compose trusted Couch controls with declarative presentation data.
Order: 4

# Native components

The optional `presentation` array gives an integration a useful control surface
without allowing it to inject UI code. Packages provide data; Couch renders the
controls in its own web and remote interfaces.

This component set is part of the integration package **developer preview**.
It is deliberately small and may evolve before public release.

## Component library

| Component | Fields | Use |
| --- | --- | --- |
| `command_group` | `title`, `commands` | A named group of declared commands. |
| `status_text` | `label`, `field` | A read-only value from device status. |
| `toggle` | `label`, `state`, `on`, `off` | A boolean state with explicit on and off commands. |
| `input_selector` | `label` | The input list returned by the integration. |

An integration may declare up to 16 components. A command group contains 1 to
32 distinct declared capabilities. Labels and titles are plain text, at most
128 bytes, with no control characters.

## Example receiver controls

```json
"presentation": [
  {
    "kind": "toggle",
    "label": "Power",
    "state": "on",
    "on": "power-on",
    "off": "power-off"
  },
  {
    "kind": "command_group",
    "title": "Volume",
    "commands": ["volume-up", "volume-down"]
  },
  {
    "kind": "toggle",
    "label": "Mute",
    "state": "muted",
    "on": "mute-on",
    "off": "mute-off"
  },
  {
    "kind": "input_selector",
    "label": "Source"
  }
]
```

This is a textual component example, not a screenshot from physical hardware.
The installed Couch build decides the final styling and layout.

## Status fields

`status_text` accepts `on`, `playing`, `muted`, `volume`, `input`, or `title`.
Only `on`, `playing`, and `muted` are boolean and may back a toggle.

Use the field your device truly reports. For example, a receiver that reports
decibels should not expose that number as Couch's percentage volume field.
Omit a component when the state is unavailable or ambiguous.

## Validation rules

Couch rejects the manifest before activation when:

- a command group refers to an undeclared or repeated command;
- a toggle uses a nonboolean state;
- its `on` and `off` command IDs are the same or undeclared;
- an input selector is present while `supports_inputs` is false;
- text or component-count limits are exceeded.

If `presentation` is omitted, Couch can still expose the package's basic
command list. A package cannot supply HTML, CSS, JavaScript, Slint, images, or
an arbitrary layout through this API.

## Source references

- [`model/couch-model/src/connection.rs`](https://github.com/dangerouslaser/couch/blob/main/model/couch-model/src/connection.rs)
- [`clients/couch-plugin/src/manifest.rs`](https://github.com/dangerouslaser/couch/blob/main/clients/couch-plugin/src/manifest.rs)
- [`clients/couch-denon/plugin.json`](https://github.com/dangerouslaser/couch/blob/main/clients/couch-denon/plugin.json)
