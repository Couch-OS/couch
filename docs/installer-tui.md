# Installer terminal and bootstrap

The current installer interface is the [native Ratatui terminal](../tools/installer/tui/README.md).
See the [Wi-Fi installer workflow](installer-wifi-wizard.md) for network selection,
backups, reinstall and recovery. Native frontend binaries are built for Linux,
macOS and Windows; complete public installer packaging remains separate work.

The Python interface below remains a reference/simulation and guarded developer
entry point. Its default public gate keeps hardware writes disabled.

## Python reference flow

`tools/installer/couch_tui.py` displays an ASCII `couch.` wordmark and numbered
menu. It supports Linux with Python 3.10+, without a terminal UI dependency.

1. Review the exact target, release, backup destination, protected identity
   partitions, and ordered write/readback plan.
2. Type the full target ID to confirm; blank or mismatched input cancels.
3. Read progress emitted by the existing policy engine. Per-partition bars use measured byte counters,
   with throughput, elapsed time and ETA; there is no overall-run percentage. An error leaves its original
   backups and journal available for recovery.

Default mode reports that physical installation is unavailable. The second menu
option only observes USB descriptors, requiring the existing PyUSB/libusb setup.
An unknown MediaTek device is never labelled as a verified HA100. Simulation is
an explicit `--simulation` option; an unavailable physical adapter never silently
switches to simulation.

```sh
python3 tools/installer/make_demo.py /tmp/couch-terminal-demo
python3 tools/installer/couch_tui.py --simulation \
  --manifest /tmp/couch-terminal-demo/release/manifest.json \
  --device-dir /tmp/couch-terminal-demo/device \
  --identity /tmp/couch-terminal-demo/identity.json \
  --backup-dir /tmp/couch-terminal-demo/originals
```

Use a new empty directory for each demo. The synthetic target confirmation is
`SIMULATED-HA100-001`; these files cannot boot a remote. `--resume` delegates
existing journal verification to the core. For the current private Wi-Fi interface,
see the [Ratatui terminal guide](../tools/installer/tui/README.md).

`CoreAdapter.plan/apply/observe` is the integration boundary. The terminal only
presents the adapter's plan and streams its output; it does not implement USB
writes or weaken checks. A future approved adapter must supply a verified plan
and execute the reviewed policy, rather than treating terminal confirmation as
permission to skip release, identity, backup, or readback checks.

## Controlled private trials

On Linux, a developer can run:

```sh
chmod 600 /private/path/trial.json
python3 tools/installer/couch_tui.py --private-trial /private/path/trial.json
```

The private JSON file requires absolute paths for `manifest`, `baseline`,
`checkout`, `loader`, `preloader`, and `backup_dir`; strings for `loader_sha256`,
`preloader_sha256`, `confirm_cid_sha256`, and USB `ports` (for example `2.1`);
and a positive integer USB `bus`. Optional fields are `lock_dir` (absolute path),
`timeout` (seconds, default 120), and boolean `restore`, `resume`,
`boot_after_install` (all default false). Unknown fields are rejected. Keep this
file, device baseline, originals and release payloads outside Git.

The menu switches install/restore and explicitly enables journal resume. Reviewing
runs the core input validation without opening USB; hashing may take time.
The complete target CID SHA-256 must be typed before starting a child process.
The child is the existing `private_install.py`, invoked with an argument list,
no shell and no stdin; it repeats all release, identity and backup checks.
Changing operation requires a fresh review. No private configuration changes the
public release gate. Restore verifies retained originals and does not require the
candidate OS image bytes to remain available.

Progress updates replace one status line. Only the writer's `Backup`,
`Hash readback`, `Write` and `Verify image` counters produce a bar. Rate and ETA
reset for each phase/partition; ETA remains unknown until bytes advance. At slow
USB rates, userdata backup and its separate hash passes can take hours. Other
logs use a bounded status line; errors remain visible. Ctrl-C interrupts the child
and preserves the journal; after a stuck interrupt, the frontend terminates the
child. Do not disconnect while writing. Completion reports verified readback,
not successful normal startup; retain originals until physical boot is checked.

## Bootstrap contract

`tools/installer/bootstrap.sh` is suitable for a future curl-piped entry point,
but is **not an available public installation command today**. Its approved
version and SHA-256 pins are intentionally empty: it stops before downloading
anything. Publishing requires a separately reviewed installer release and pins.

For a future approved release, it accepts Linux x86_64 only, checks Python/curl,
opens `/dev/tty` for interaction despite piped stdin, and downloads a specific
GitHub release tag over HTTPS. It verifies the pinned SHA-256 before examining
or executing the package; it never selects `latest` or accepts an untrusted URL
or checksum through environment variables.

The package must contain `couch-installer/tools/installer/couch_tui.py` and the
reviewed dependencies/core files in their normal relative layout. Download size
is bounded to 64 MiB, expanded content to 128 MiB/10,000 members. Extraction refuses
absolute/traversal paths, links, device nodes, duplicate names and missing entry
points. Files land in a private temporary directory, removed on exit. This
installer-tool package excludes large OS payload images; payload verification
remains the policy engine's job. Checksums provide integrity relative to reviewed
bootstrap pins, not independent release signing or hardware certification.

Without a terminal, the front end exits with a short explanation. Scripted
planning/testing should use the existing noninteractive core CLI.

## Validation

`python3 -m unittest discover -s tools/installer -p test_tui.py -v`

Tests exercise cancellation, real policy-engine simulation, no physical fallback,
missing terminals, unpublished-release refusal, checksum failure and unsafe
archive members. They never connect to USB or flash hardware.

## Host binary builds

`.github/workflows/installer-binaries.yml` builds Linux x64/ARM64, Windows x64,
and macOS Intel/Apple Silicon executables, including a universal macOS binary.
These CI artifacts are the terminal interface, not a complete approved installer
release. Platform USB drivers, backend dependencies, end-to-end flashing, and
public payload preparation are separate release requirements. Windows uses
private anonymous pipes; Unix uses an inherited local socket. Native subprocess
output is redirected away from both transports.
