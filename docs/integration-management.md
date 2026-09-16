# Integration management

The integration-capable development runtime includes **Integrations** in the
paired configuration web UI at `/integrations`. The released `.170` runtime
predates the host and management API. These instructions describe the source
capabilities after a suitable core update; they do not imply that a new core or
an independently sourced Denon release has been deployed.

## Install and manage packages

Open **Integrations**, then **Refresh packages**. Couch downloads and verifies
signed indexes for its built-in Stable and Preview sources and your confirmed
custom repositories. The page separates installed packages from available
catalog entries. A signed empty feed is valid; Stable stays empty until a
package has production-tier hardware evidence.

Choose **Install** beside a catalog entry. After installation, create a
connection on **Connections** and enter that integration's settings. Installing
a package alone does not create a device connection or convert a built-in one.
The web API accepts catalog selections by integration and repository ID, never
an arbitrary APK URL. A signed index is checked again before download, and the
selected identity/version must still match. The downloaded package is verified,
audited and handshake-tested before activation.

Installed cards offer **Update to …** when the recorded repository has a newer
manifest version, and **Restore previous version** when a distinct verified
previous slot exists. **Remove package** retains connections and private
settings, so a compatible reinstall can restore execution. Removing a package
does not delete the room's devices or their activity bindings.

The page reports operation progress and failures. One package-management
operation runs at a time. Package status distinguishes a usable current
version, a verified previous-version fallback, an invalid selection, and a
missing package whose connection settings are retained. A missing or invalid
package needs a compatible reinstall from a trusted repository. Downloads and
installation require a user action; there are no automatic package updates.

## Repository trust

Official feed URLs and their public signing key are built into `couch-confd`,
which is delivered by the signed core updater. They require no manual SSH key
provisioning. The web manager uses separate trust directories under the package
store's `management/keys`, keyed by repository identity and fingerprint.

To add a custom repository:

1. Enter a short ID, name and HTTPS base URL. The base contains an `armv7`
   directory; omit that suffix from the URL.
2. Paste its public PEM signing key from a source you trust.
3. Select **Check public-key fingerprint**, compare the displayed SHA-256 of
   the normalized PEM with the owner's fingerprint, and confirm the comparison.
4. Select **Trust repository**, then **Refresh packages** to load its catalog.

Couch persists the custom URL and key only after confirmation. It does not fetch
and trust a custom key automatically. Each repository's key is scoped to that
source, and no integration key is added to Alpine's system trust directory.
Removing a custom repository stops its catalog use while leaving its installed
packages and saved connections intact. To change a custom key, remove the
repository and review the replacement key as a new trust decision.

Packages execute trusted native code with reduced privileges and LAN access;
the process boundary is not a complete sandbox. See the
[native-code boundary](integration-packages.md#native-code-boundary).

## Core rollback and recovery

The first integration-capable core update is sufficient even when the rollback
slot contains an older core. The daemon writes a single atomic configuration
envelope: legacy fields keep built-in connections usable and external controls
inactive, while `integration_config` retains the complete current configuration.
The current daemon and GUI read that complete document. No rollback slot is
removed or rewritten to enable integrations.

If an old core saves configuration while rolled back, its changes become
authoritative on re-upgrade. Couch does not automatically restore deleted
devices or bindings. The **Saved integration configuration found** notice offers
a confirmed export for deliberate recovery. Download and inspect it, export the
current house, then use the normal whole-config import to restore it if wanted.
An import replaces the entire configuration, including changes made during
rollback. A separately retained pending export may describe an interrupted save;
it is not offered as confirmed recovery. See
[the storage and recovery procedure](runtime-updates.md#integration-configuration-across-core-rollback).

## Manual CLI and publishing

The [signed-package CLI](integration-packages.md#install-from-a-development-host)
remains available for sideloads, repository installs, listing, rollback and
removal. Official commands use the embedded key. Explicit custom `--keys-dir`
paths still select their own provisioned public keys; manual `--repository`
arguments do not register a persistent web repository.

Every change to installed package bytes must bump the integration's manifest
version and matching APK `pkgver`. Slots are immutable by integration ID and
manifest version; an APK-only change from `-r0` to `-r1` cannot replace one.
See [packaging and signing](development/packaging.md) for build and feed policy.

## Paired API

All management routes require the existing paired session:

| Route | Purpose |
| --- | --- |
| `GET /api/integrations/catalog` | Installed state, available catalog entries, repositories and refresh errors |
| `POST /api/integrations/refresh` | Verify and refresh repository indexes |
| `POST /api/integrations/{install,update,rollback,remove}` | Start an explicit operation; removal must preserve connection configuration |
| `GET /api/integrations/operations/current` | Reattach to the current operation after returning to the page |
| `GET /api/integrations/operations/ID` | Poll the returned operation ID |
| `POST /api/integrations/repositories` | Stage a custom URL/key for fingerprint review |
| `POST /api/integrations/repositories/ID/confirm` | Persist the reviewed fingerprint's repository |
| `DELETE /api/integrations/repositories/ID` | Remove a custom repository without deleting packages |
| `GET /api/integrations/recovery` | Distinguish confirmed recovery and pending candidate paths |
| `GET /api/integrations/recovery/config` | Read the confirmed whole-config recovery export |

`GET /api/integrations` remains the installed manifest catalog used by connection
editors. Management does not replace that endpoint or the separate core updater.
