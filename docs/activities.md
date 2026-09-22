# Activities

An activity brings devices together for a task such as watching a movie. Configure it in **Activities → Create activity** in the web UI:

1. Choose a name, room, and included devices. Devices may come from other rooms.
2. Under **Devices & sequences**, build the **On sequence** and **Off sequence** from the command library. Select a device, search its commands, and click a command to append it. Denon inputs and LG inputs/apps are discovered when available. Add delays where a device needs time to start; drag steps or use the arrows to reorder them.
3. Under **Physical buttons**, map short and long presses to included devices. Long Back is reserved for returning to Couch.
4. Under **Remote screen**, choose a Kodi/TV source screen or build custom command pages. Optionally add the activity to areas; it is also listed in its room.

This follows the device selection, sequence, and button mapping workflow described in [Unfolded Circle's activity guide](https://support.unfoldedcircle.com/hc/en-us/articles/13269248303516-How-to-create-an-activity). Couch retains its existing Kodi and TV screens alongside custom command pages.

## Custom command pages

In **Remote screen → Custom pages**, click **Add page**, name it, then select an included device and function and click **Add button**. Edit the button label and choose any icon from the existing Lucide picker. Use the arrows to reorder pages and buttons; remove/re-add a button to change its device or function. Each page contains up to six buttons in a fixed two-column, three-row grid, so controls never scroll off the screen. Up to eight pages are supported.

Select **Open custom pages first** to use them as the initial activity view. A custom-only activity needs no Kodi or TV source. If a source is selected, **Media** opens its existing control screen; **Pages** returns to custom controls. Switching views does not replay the On sequence. Deleting the last page resets the initial view to the retained source; choose a source before starting if none remains.

Touch a button or highlight it with the D-pad and press OK. Left/right at a row edge changes pages; the bottom arrows also wrap between pages. Physical activity mappings take precedence over this default navigation. Back returns to Couch, leaving the activity running. End, and holding Power, retain normal activity shutdown behavior.

Widget commands execute on a bounded worker using the same validated command dispatcher as physical mappings. One command is accepted at a time; navigation discards late feedback and queued commands from the old page session. A command waiting longer than 750 ms expires instead of firing late. Already dispatched commands cannot be recalled. Editing pages takes effect when the view is reopened.

This first editor supports labeled command buttons and icons, not free-position widgets, live-state tiles, sliders, arbitrary values, or artwork widgets. The picker offers the integration's standard typed command catalog; discovered app/input shortcuts remain available through activity sequences and physical mappings. Dynamic `input:`/`app:` actions already stored by API clients are validated and executable in page buttons.

## Running and ending

Opening an activity runs its On sequence before opening the control screen. Returning to Couch leaves it running; reopening it does not replay On. Tap **End**, or **hold the physical Power button** for about half a second, to run Off and return to Couch. The hold works from anywhere, including a room list or a light's controls.

A *short* press of Power is never taken by the activity any more. It belongs to whatever is on screen: the highlighted room row switches, a receiver or TV row sends its own power command, and an activity that maps Power itself still gets it. If nothing on screen wants the key while an activity is running, the remote says **Hold Power to end <activity>** and ends nothing. This is the one change to muscle memory: a tap used to end the activity, and now a hold does.

**Keep remote awake while running** prevents display standby for the lifetime of that activity, including while viewing another screen.

Commands run in order on a worker, outside the GUI event loop. A failure stops at that step and shows an error. There are no automatic retries or rollback commands. Cancel (or hold Back during a sequence) prevents subsequent steps; an already dispatched command may still finish. Completed commands are never undone automatically. Repeating a failed sequence starts it from the beginning, so prefer explicit on/off commands over toggles.

Running activities and their configuration snapshots are held in memory. GUI restart clears running state; reopening afterward runs On again. Off uses the configuration captured when the activity started. Changing configuration does not alter an already running activity's shutdown sequence.

## Compatibility and limits

`Activity.setup` stores included device IDs, description, keep-awake, and ordered `on`/`off` steps. Each step is a typed command or a delay. Sequences allow 64 steps, delays of 1–30,000 ms, and at most two minutes of total delay each. Commands must be supported by an included device. Removing a device cleans up its sequence commands and page buttons. Missing custom-page fields default to the existing source-screen behavior.

Older `Activity.steps` were saved drafts, not executed commands. They remain visible for reference and are never automatically run. Older API clients omitting `setup` preserve the saved setup. Physical mappings and existing source screens retain their behavior.

Validation: model and daemon regression tests cover membership, command/delay limits, cleanup, legacy updates, and atomic rejection. GUI tests cover ordered dispatch, delays, cancellation, and stopping on failure. Browser coverage exercises create/edit, discovered inputs, reordering, persistence, and mobile layout; an isolated HA100 fixture additionally verified ordered startup, return/reopen without replay, Power-triggered shutdown, restart after End, and long-Back cancellation before a queued command. The fixture used a fake Kodi endpoint and preserved production configuration.

Custom-page validation: model tests cover legacy defaults, labels, page/button limits, unsupported functions, and removed-device cleanup. Headless Slint tests render the actual 480×800 component and dispatch D-pad/touch events to verify selection, edge paging, empty pages, command callbacks, source/page switching, and closing the custom view. Browser tests cover page/button creation, icon selection, ordering, initial-view selection, reload persistence, mobile layout, and final-page deletion. Custom-page physical-device validation is pending; these checks did not contact real devices.
