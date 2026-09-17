# Installer corresponding source

`corresponding_source.py` is the installer-owned, self-contained source
collector. It reads source bytes from an exact Git commit, vendors all four
locked installer Cargo workspaces, retains dependency notices, imports the
audited Rust standard-library source/toolchain component, and emits a
deterministic installer-only archive.

From a Git checkout containing the release commit:

```sh
python3 tools/installer/source/corresponding_source.py project \
  --repo . --commit FULL_40_CHARACTER_INSTALLER_COMMIT --output /source/installer
python3 tools/installer/source/corresponding_source.py cargo \
  --output /source/installer
python3 tools/installer/source/corresponding_source.py cargo-notices \
  --output /source/installer --cache /cache/cargo-notice-repositories
python3 tools/installer/source/corresponding_source.py external-rust \
  --directory /inputs/rust-stdlib-source \
  --receipt /inputs/rust-stdlib-source/receipt.json \
  --output /source/installer
python3 tools/installer/source/corresponding_source.py assemble \
  --output /source/installer \
  --archive /release/couch-installer-source.tar.gz
python3 tools/installer/source/corresponding_source.py verify-archive \
  --archive /release/couch-installer-source.tar.gz
```

Use `--offline` for Cargo and notice collection only after their caches are
complete. Notice collection automatically uses the reviewed supplement
manifest and license text beside this file. `external-rust` accepts only a
verified `couch-external-source` receipt for `rust-stdlib`; the receipt must
name and hash the source archive, configuration, build recipe, toolchain
receipt, and corresponding standard-library binary.

The resulting manifest has kind
`couch-installer-corresponding-source-archive`, scope `installer`, and
`os_source_covered: false`. It cannot satisfy the full Couch OS source
contract. Retain the selected OS release's corresponding-source archive and
receipt separately. `rust_source_component_included: true` records the audited
source component; it does not prove that every downloadable native binary used
that compiler or standard library. Publication must still compare each native
build receipt's compiler identity and target sysroot hashes with the selected
Rust source/toolchain receipt.

The compatibility CLI at `tools/release/corresponding_source.py` exposes the
same path by adding `--scope installer` to `project`, `cargo`,
`cargo-notices`, `external`, and `assemble`.

Run the source-scope regression tests from either the Couch checkout or a
standalone repository export:

```sh
python3 -m unittest discover -s tools/installer -p test_corresponding_source.py
```
