# Brane OS release guide

## Build a local release candidate

Requirements: pinned Rust nightly, QEMU, Python 3, `xorriso` and OVMF.

```bash
make release-test VERSION=1.0.0-rc1
```

The command builds the kernel, boots the ISO with OVMF and validates the
artifact set. It creates these files in `dist/`:

- `brane_os_v<VERSION>.iso` — UEFI El Torito ISO.
- `brane_os_v<VERSION>-bios.img` — standalone BIOS disk image.
- `brane_os_v<VERSION>-uefi.img` — standalone UEFI disk image.
- `brane_os_v<VERSION>.iso.sha256` — ISO checksum.
- `brane_os_v<VERSION>_release.tar.gz` — complete release archive.

## Verify the checksum

Linux:

```bash
cd dist
sha256sum --check brane_os_v1.0.0.iso.sha256
```

macOS:

```bash
cd dist
shasum -a 256 -c brane_os_v1.0.0.iso.sha256
```

## Test manually in QEMU

The recommended automated command is `make iso-test`. It locates OVMF through
`OVMF_CODE` and `OVMF_VARS` or through common Linux/Homebrew paths, including
the Ubuntu 4 MiB firmware names `OVMF_CODE_4M.fd` and `OVMF_VARS_4M.fd`.

The standalone BIOS image can be tested with:

```bash
qemu-system-x86_64 \
  -m 256M \
  -drive format=raw,file=dist/brane_os_v1.0.0-bios.img \
  -serial stdio -nographic -accel tcg
```

## Publish from a tag

1. Update `CHANGELOG.md` and replace the Unreleased heading with the version.
2. Ensure the main CI workflow is green.
3. Create and push a signed or annotated `v<MAJOR>.<MINOR>.<PATCH>` tag.
4. The Release workflow builds and boot-tests all artifacts, verifies the
   checksum, and creates the corresponding GitHub Release.

Manual `workflow_dispatch` runs validate and upload workflow artifacts but do
not publish a GitHub Release.

## OS images versus `bpkg`

This workflow publishes whole-system boot artifacts. The `.sha256` file detects
accidental or out-of-band changes only when the checksum itself comes from a
trusted channel; it is not a package signature, repository trust root or update
freshness proof.

The future `bpkg` path uses a separate deterministic package format, repository
roles, trusted-time policy and transactional generations. Node/Brane identity
keys are not release keys. See [`PACKAGE_MANAGER.md`](PACKAGE_MANAGER.md) and
[`ADR-012`](ADR/ADR-012-signed-packages-transactional-activation.md).
