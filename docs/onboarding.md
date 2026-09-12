# On-device setup

Fresh installs begin with no example connections, rooms, devices, or activities.
Normal first boot opens **Welcome to couch.** on the remote. Choose **Set up
Wi-Fi**, select a scanned network (or enter a hidden SSID), and use the keyboard
to enter its password. **Test connection** checks association and DHCP; it does
not require internet access. **Save network** persists only the tested network.
The existing 60-second trial timeout restores the previous connection if Save
is not selected.

After saving, the remote shows a QR code and its `http://<address>:8090` URL.
Open it from the same network, pair using the PIN displayed on the remote, then
set the timezone/12- or 24-hour clock under Remote settings and add connections,
rooms, and activities. Clock configuration currently happens in the webUI.
**Set up later** allows offline use; a device with no saved networks offers setup
again on its next boot.

## Outages and recovery

Saved networks suppress first-boot onboarding even when association or DHCP
fails. The station supplicant remains running, and the normal remote interface
stays available. Use Settings → Wi-Fi → Change network to repair a connection.
A transient outage never automatically exposes a hotspot.

Settings → Wi-Fi → Recovery hotspot (also on the welcome screen) explicitly
starts the existing `Couch-Setup` portal. This switches the radio away from its
normal connection. Restart the remote to return to normal station mode. The
portal retains its existing physical approval requirement for SSH enrollment.
Headless rescue can explicitly request it with `COUCH_SETUP_AP=1`; it does not
start the graphical onboarding flow.

## Implementation and validation

`stage2/setup-mode.sh` chooses normal/local/recovery boot behavior.
The Rust `couch-system` service owns network trials, persistence, recovery
hotspot requests and physically approved SSH enrollment. The GUI and portal use
its root-only local socket. See [system service](system-service.md) for the boot
boundary and failure behavior. No device credentials belong in an image or Git.

Run `python3 -m unittest discover -s tools/tests` for boot policy regressions
and `cargo test --manifest-path daemon/Cargo.toml -p couch-system` for network
trial/rollback and local API tests. The service refactor still needs physical
acceptance; the following observations describe the prior GUI implementation.
On the HA100, the welcome page, network scan, return navigation, recovery
confirmation, and offline exit were checked with the new GUI; saved credentials
and configuration hashes stayed unchanged. The existing trial/rollback tests
cover network changes. A complete fresh-network Save and QR handoff still need
an end-to-end onboarding test. Do not erase an existing user's networks to test first boot.
