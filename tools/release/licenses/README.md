# Lato notice provenance

`Lato-OFL.txt` is the upstream Google Fonts notice from
https://raw.githubusercontent.com/google/fonts/main/ofl/lato/OFL.txt,
retrieved September 9, 2026. It is copied unchanged into clean Couch payloads.

The existing GUI fonts were compared byte-for-byte with the corresponding
`Lato-Regular.ttf` and `Lato-Bold.ttf` from that same upstream directory:

- Regular SHA-256: `d636e4683231f931eda222d588e944d082bfd3bdba02f928bee461c0f185b251`
- Bold SHA-256: `8a0aace75d33794eece4b28187bfc1df0bbd2888b5d8a56e01788c8d65d16be1`

Both matched. The GUI embeds rasterized font resources; retaining a readable
notice in `/opt/couch/licenses` makes its attribution available in the image.
Inter and Lucide notices are already tracked under `assets/` and are included
separately. This notice inventory does not approve the remaining project,
Slint/Rust dependency, kernel/BusyBox or vendor redistribution requirements.

# BlueZ notice

`BlueZ-GPL-2.0.txt` is the notice for `couch-bluetoothd`, the patched BlueZ
5.79 bluetoothd in the runtime bundle (`third_party/bluez`). It names the
program, the modification and where its corresponding source is published,
reproduces the BSD 2-Clause notice of `src/shared/ecc.c` (Kenneth MacKay,
standard SPDX BSD-2-Clause wording), and appends BlueZ 5.79's own `COPYING`
(GPL-2.0, SHA-256 `b499eddebda05a8859e32b820a64577d91f1de2b52efa2a1575a2cb4000bc259`)
and `COPYING.LIB` (LGPL-2.1, SHA-256
`ec60b993835e2c6b79e6d9226345f4e614e686eb57dc13b6420c15a33a8996e5`) unchanged,
from `bluez-5.79.tar.xz` (SHA-256
`4164a5303a9f71c70f48c03ff60be34231b568d93a9ad5e79928d34e6aa0ea8a`). The
licences of the individual source files were read from their SPDX headers:
GPL-2.0-or-later (src, lib, gdbus, btio, attrib), LGPL-2.1-or-later (ell, most
of src/shared) and BSD-2-Clause (src/shared/ecc.c).
