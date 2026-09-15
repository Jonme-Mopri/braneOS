#!/usr/bin/env python3
"""
tests/boot/test_boot.py — Brane OS Boot Test Harness
=====================================================
Launches Brane OS inside QEMU and verifies that the kernel boots
successfully by inspecting serial output within a configurable timeout.

Exit codes:
  0 — PASS: all expected strings were found in serial output
  1 — FAIL: timeout or expected string not found
  2 — ERROR: build or QEMU setup failed

Usage:
  python3 tests/boot/test_boot.py [--timeout SECONDS] [--no-build]

Environment variables:
  KERNEL_BIN_PATH  — path to pre-built kernel binary (skips cargo build)
  BOOT_TIMEOUT     — override timeout in seconds (default: 60)
"""

import argparse
import json
import os
import re
import socket
import subprocess
import sys
import threading
import time
import shutil
import tempfile
from pathlib import Path
from typing import BinaryIO

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------

REPO_ROOT = Path(__file__).resolve().parents[2]
KERNEL_CRATE = "brane_os_kernel"
TARGET = "x86_64-unknown-none"
BUILD_FLAGS = [
    "-Z", "build-std=core,compiler_builtins,alloc",
    "-Z", "build-std-features=compiler-builtins-mem",
    "--target", TARGET,
]

DEFAULT_TIMEOUT = int(os.environ.get("BOOT_TIMEOUT", "60"))

# Strings that MUST appear in the serial output for the test to pass.
# Order does not matter; all must be present.
REQUIRED_STRINGS = [
    "Brane OS",   # kernel banner
    "[acpi] ACPI subsystem initialized",  # Phase 10 power management
    "[pci]  Enumeration complete",  # Phase 13 shared hardware inventory
    "[block] virtio-blk0 ready",  # Phase 13 hardware-backed block device
    "[block] Block layer ready: 1 registered device(s)",
    "[block] LBA0 read probe: ok",  # synchronous DMA/virtqueue transfer
    "[fat32] Volume ready",  # block-backed FAT32 geometry and root directory
    "[fat32] Read probe: /disk/README.TXT ok",  # FAT chain + VFS read path
    "brane>",     # brsh prompt (signals full userland init)
]

# Strings whose presence immediately fails the test (kernel panics, etc.)
FAIL_STRINGS = [
    "KERNEL PANIC",
    "panicked at",
    "DOUBLE FAULT",
    "STACK OVERFLOW",
]

QEMU_BIN = os.environ.get("QEMU_BIN", "qemu-system-x86_64")


def find_ovmf() -> tuple[Path, Path] | None:
    """Locate UEFI code and variable images on common installations.

    Debian/Ubuntu releases have used both the legacy ``OVMF_CODE.fd`` names
    and the newer 4 MiB variants (``OVMF_CODE_4M.fd``). Homebrew and Fedora
    package the same firmware under edk2/qemu directories, so keep explicit
    pairs first and then discover a matching CODE/VARS pair by filename.
    """
    candidates = [
        (os.environ.get("OVMF_CODE"), os.environ.get("OVMF_VARS")),
        (
            "/usr/local/opt/qemu/share/qemu/edk2-x86_64-code.fd",
            "/usr/local/opt/qemu/share/qemu/edk2-i386-vars.fd",
        ),
        (
            "/opt/homebrew/opt/qemu/share/qemu/edk2-x86_64-code.fd",
            "/opt/homebrew/opt/qemu/share/qemu/edk2-i386-vars.fd",
        ),
        ("/usr/share/OVMF/OVMF_CODE.fd", "/usr/share/OVMF/OVMF_VARS.fd"),
        ("/usr/share/OVMF/OVMF_CODE_4M.fd", "/usr/share/OVMF/OVMF_VARS_4M.fd"),
        ("/usr/share/OVMF/OVMF_CODE_4M.secboot.fd", "/usr/share/OVMF/OVMF_VARS_4M.ms.fd"),
        ("/usr/share/edk2/x64/OVMF_CODE.fd", "/usr/share/edk2/ovmf_vars.fd"),
        ("/usr/share/edk2/x64/OVMF_CODE_4M.fd", "/usr/share/edk2/x64/OVMF_VARS_4M.fd"),
        ("/usr/share/edk2/ovmf/edk2-x86_64-code.fd", "/usr/share/edk2/ovmf/edk2-i386-vars.fd"),
        ("/usr/share/qemu/edk2-x86_64-code.fd", "/usr/share/qemu/edk2-i386-vars.fd"),
    ]
    for code_candidate, vars_candidate in candidates:
        if code_candidate and vars_candidate:
            code = Path(code_candidate)
            variables = Path(vars_candidate)
            if code.exists() and variables.exists():
                return code, variables

    # Fall back to package layouts that add a suffix (for example ``_4M``)
    # or place the files below a versioned edk2 directory. Only pair files
    # whose names differ by CODE versus VARS so a secure-boot image cannot be
    # accidentally combined with an unrelated variable store.
    search_dirs = [
        Path("/usr/share/OVMF"),
        Path("/usr/share/edk2"),
        Path("/usr/share/edk2/x64"),
        Path("/usr/share/qemu"),
        Path("/usr/local/share/qemu"),
        Path("/opt/homebrew/share/qemu"),
    ]
    for directory in search_dirs:
        if not directory.is_dir():
            continue
        for code in sorted(directory.rglob("*.fd")):
            if "code" not in code.name.lower():
                continue
            variables = code.with_name(re.sub("code", "vars", code.name, flags=re.IGNORECASE))
            if variables.is_file():
                return code, variables
    return None

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def info(msg: str) -> None:
    print(f"[boot-test] \033[36mINFO\033[0m  {msg}", flush=True)

def ok(msg: str) -> None:
    print(f"[boot-test] \033[32mPASS\033[0m  {msg}", flush=True)

def warn(msg: str) -> None:
    print(f"[boot-test] \033[33mWARN\033[0m  {msg}", flush=True)

def error(msg: str) -> None:
    print(f"[boot-test] \033[31mFAIL\033[0m  {msg}", flush=True)


class QmpClient:
    """Minimal line-oriented QMP client used for deterministic key injection."""

    def __init__(self, path: Path, deadline: float) -> None:
        self.connection = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        while True:
            try:
                self.connection.connect(str(path))
                break
            except (FileNotFoundError, ConnectionRefusedError):
                if time.monotonic() >= deadline:
                    raise TimeoutError("QMP socket did not become ready")
                time.sleep(0.02)
        self.stream = self.connection.makefile("rwb", buffering=0)
        self._read_response(deadline)
        self.execute("qmp_capabilities", deadline=deadline)

    def _read_response(self, deadline: float) -> dict:
        self.connection.settimeout(max(0.01, deadline - time.monotonic()))
        while True:
            line = self.stream.readline()
            if not line:
                raise RuntimeError("QMP connection closed")
            message = json.loads(line)
            if "event" not in message:
                return message

    def execute(self, command: str, arguments: dict | None = None, *, deadline: float) -> dict:
        request: dict[str, object] = {"execute": command}
        if arguments:
            request["arguments"] = arguments
        self.stream.write(json.dumps(request).encode() + b"\r\n")
        response = self._read_response(deadline)
        if "error" in response:
            raise RuntimeError(f"QMP command {command!r} failed: {response['error']}")
        return response

    def close(self) -> None:
        self.stream.close()
        self.connection.close()


def write_fat32_test_disk(
    disk: BinaryIO,
    *,
    volume_label: bytes = b"BRANEOS    ",
    readme_prefix: bytes = b"Brane OS FAT32 block path ready.\n",
) -> None:
    """Create a deterministic sparse FAT32 superfloppy without host tools."""
    if len(volume_label) != 11:
        raise ValueError("FAT32 volume label must contain exactly 11 bytes")
    sector_size = 512
    total_sectors = 131_072
    reserved_sectors = 32
    fat_sectors = 1_024
    data_sector = reserved_sectors + fat_sectors

    def put_u16(target: bytearray, offset: int, value: int) -> None:
        target[offset:offset + 2] = value.to_bytes(2, "little")

    def put_u32(target: bytearray, offset: int, value: int) -> None:
        target[offset:offset + 4] = value.to_bytes(4, "little")

    def write_sector(lba: int, data: bytes | bytearray) -> None:
        if len(data) != sector_size:
            raise ValueError("FAT32 test sectors must be exactly 512 bytes")
        disk.seek(lba * sector_size)
        disk.write(data)

    def directory_entry(name: bytes, attributes: int, cluster: int, data: bytes) -> bytes:
        if len(name) != 11:
            raise ValueError("FAT32 short name must contain exactly 11 bytes")
        entry = bytearray(32)
        entry[:11] = name
        entry[11] = attributes
        put_u16(entry, 20, cluster >> 16)
        put_u16(entry, 26, cluster & 0xFFFF)
        put_u32(entry, 28, len(data))
        return bytes(entry)

    disk.truncate(total_sectors * sector_size)

    boot = bytearray(sector_size)
    boot[0:3] = b"\xEB\x58\x90"
    boot[3:11] = b"BRANEOS "
    put_u16(boot, 11, sector_size)
    boot[13] = 1
    put_u16(boot, 14, reserved_sectors)
    boot[16] = 1
    boot[21] = 0xF8
    put_u16(boot, 24, 63)
    put_u16(boot, 26, 255)
    put_u32(boot, 32, total_sectors)
    put_u32(boot, 36, fat_sectors)
    put_u32(boot, 44, 2)
    put_u16(boot, 48, 1)
    put_u16(boot, 50, 6)
    boot[64] = 0x80
    boot[66] = 0x29
    put_u32(boot, 67, 0xB4A93201)
    boot[71:82] = volume_label
    boot[82:90] = b"FAT32   "
    boot[510:512] = b"\x55\xAA"
    write_sector(0, boot)
    write_sector(6, boot)

    fs_info = bytearray(sector_size)
    put_u32(fs_info, 0, 0x41615252)
    put_u32(fs_info, 484, 0x61417272)
    put_u32(fs_info, 488, 0xFFFFFFFF)
    put_u32(fs_info, 492, 0xFFFFFFFF)
    put_u32(fs_info, 508, 0xAA550000)
    write_sector(1, fs_info)

    fat = bytearray(sector_size)
    for cluster, value in ((0, 0x0FFFFFF8), (1, 0x0FFFFFFF), (2, 0x0FFFFFFF),
                           (3, 6), (4, 0x0FFFFFFF), (5, 0x0FFFFFFF),
                           (6, 0x0FFFFFFF)):
        put_u32(fat, cluster * 4, value)
    write_sector(reserved_sectors, fat)

    readme_tail = b"FAT32-CHAIN-OK\n"
    readme = readme_prefix + b" " * (sector_size - len(readme_prefix)) + readme_tail
    hello = b"hello from a nested FAT32 directory\n"
    root = bytearray(sector_size)
    root[0:32] = directory_entry(b"README  TXT", 0x20, 3, readme)
    root[32:64] = directory_entry(b"DOCS       ", 0x10, 4, b"")
    write_sector(data_sector, root)

    readme_sector = bytearray(sector_size)
    readme_sector[:] = readme[:sector_size]
    write_sector(data_sector + 1, readme_sector)

    docs = bytearray(sector_size)
    docs[0:32] = directory_entry(b"HELLO   TXT", 0x20, 5, hello)
    write_sector(data_sector + 2, docs)

    hello_sector = bytearray(sector_size)
    hello_sector[:len(hello)] = hello
    write_sector(data_sector + 3, hello_sector)

    readme_tail_sector = bytearray(sector_size)
    readme_tail_sector[:len(readme_tail)] = readme_tail
    write_sector(data_sector + 4, readme_tail_sector)
    disk.flush()

# ---------------------------------------------------------------------------
# Build step
# ---------------------------------------------------------------------------

def build_kernel() -> Path:
    """Build the release kernel and return the binary path.

    The debug ELF includes enough DWARF data to make the BIOS loader exceed
    the 60-second QEMU timeout under TCG.
    """
    info("Building kernel (release)…")
    result = subprocess.run(
        ["cargo", "build", "-p", KERNEL_CRATE, "--release", "--target", TARGET,
         "-Z", "build-std=core,compiler_builtins,alloc",
         "-Z", "build-std-features=compiler-builtins-mem"],
        cwd=REPO_ROOT,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        error("cargo build failed:")
        print(result.stderr, file=sys.stderr)
        sys.exit(2)
    bin_path = REPO_ROOT / "target" / TARGET / "release" / "brane_os_kernel"
    if not bin_path.exists():
        error(f"Kernel binary not found at: {bin_path}")
        sys.exit(2)
    info(f"Kernel binary: {bin_path}")
    return bin_path


def build_disk_image(kernel_path: Path) -> Path:
    """Use the runner crate to produce a BIOS disk image."""
    info("Building BIOS disk image…")
    out_dir = REPO_ROOT / "target" / "boot-test-img"
    out_dir.mkdir(parents=True, exist_ok=True)
    env = {**os.environ, "KERNEL_BIN_PATH": str(kernel_path)}
    result = subprocess.run(
        ["cargo", "run", "--package", "runner"],
        cwd=REPO_ROOT,
        env=env,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        error("runner failed to create disk image:")
        print(result.stderr, file=sys.stderr)
        sys.exit(2)

    # The runner places images in $OUT_DIR which defaults to target/
    candidates = list((REPO_ROOT / "target").glob("**/*.img"))
    bios_imgs = [p for p in candidates if "bios" in p.name]
    if not bios_imgs:
        # Fallback: any .img found
        bios_imgs = candidates
    if not bios_imgs:
        error("No .img file produced by runner.")
        sys.exit(2)
    # Pick the most recently modified one
    img = max(bios_imgs, key=lambda p: p.stat().st_mtime)
    info(f"Disk image: {img}")
    return img

# ---------------------------------------------------------------------------
# QEMU runner
# ---------------------------------------------------------------------------

def run_qemu_test(
    media_path: Path,
    timeout: int,
    media_type: str = "disk",
    cpus: int = 1,
    machine: str | None = None,
    usb_hid: bool = False,
    usb_storage: bool = False,
) -> int:
    """
    Launch QEMU with the given disk image or ISO, capture serial output,
    and return 0 (pass), 1 (fail).
    """
    cmd = [QEMU_BIN, "-m", "256M", "-smp", str(cpus)]
    if machine:
        cmd.extend(["-machine", machine])
    required_strings = list(REQUIRED_STRINGS)
    if usb_storage:
        required_strings.remove("[block] Block layer ready: 1 registered device(s)")
        required_strings.append("[block] Block layer ready: 2 registered device(s)")
    if machine == "q35":
        required_strings.extend([
            "[acpi] MCFG:",
            "[pci]  Config backend: Ecam",
            "[xhci] Controller ready:",
            "[xhci] Runtime ready: reset=ok, running=true, command_probe=ok",
            "[xhci] Ports ready: protocols=2, connected=1",
            "[xhci] Device addressed: slot=1",
        ])
        if usb_storage:
            required_strings.extend([
                "[xhci] Device descriptor:",
                "[xhci] Mass storage ready: slot=1",
                "bulk_in=0x81",
                "bulk_out=0x02",
                "[usb-storage] LUN 0: blocks=131072, block_size=512, read_only=true",
                "[block] usb-storage0 ready: id=1",
                "[block] USB LBA0 read probe: ok",
                "[fat32] USB volume ready: mount=/usb",
                "label=BRANEUSB",
                "[fat32] Read probe: /usb/README.TXT ok",
            ])
        else:
            required_strings.extend([
                "[xhci] Device descriptor: vid=0x0627, pid=0x0001",
                "[xhci] HID keyboard ready: slot=1, interface=0, endpoint=0x81",
            ])
    if usb_hid:
        required_strings.append(
            "[xhci] HID report received: slot=1, endpoint=0x81, len=8, key=u"
        )
    if cpus > 1:
        required_strings.extend([
            f"[smp] CPU boot plan ready: {cpus} enabled CPU(s)",
            "[smp] BSP assigned to CPU slot 0",
            f"[smp] AP startup complete: attempted={cpus - 1}, online={cpus - 1}, failed=0",
            f"[smp] AP interrupt check: attempted={cpus - 1}, responsive={cpus - 1}, failed=0",
            "[sched] Multicore dispatch active:",
            "[sched] Multicore dispatch stress:",
            f"[sched] Multicore task execution: expected={cpus - 1}, observed={cpus - 1}",
        ])
    firmware_vars: Path | None = None
    block_path: Path | None = None
    usb_storage_path: Path | None = None
    qmp_tempdir = None
    qmp_path: Path | None = None
    if media_type == "iso":
        ovmf = find_ovmf()
        if ovmf is None:
            error("UEFI code/variables not found; set OVMF_CODE and OVMF_VARS or install OVMF.")
            return 2
        code, variables = ovmf
        with tempfile.NamedTemporaryFile(prefix="brane-ovmf-vars-", suffix=".fd", delete=False) as copy:
            firmware_vars = Path(copy.name)
        shutil.copyfile(variables, firmware_vars)
        cmd.extend([
            "-drive", f"if=pflash,format=raw,readonly=on,file={code}",
            "-drive", f"if=pflash,format=raw,file={firmware_vars}",
            "-cdrom", str(media_path), "-boot", "d",
        ])
    else:
        cmd.extend(["-drive", f"format=raw,file={media_path}"])

    # A dedicated read-only disk keeps the boot image separate from the device
    # under test. disable-modern forces the legacy I/O/PFN transport currently
    # implemented by the Phase 13 driver.
    with tempfile.NamedTemporaryFile(prefix="brane-virtio-blk-", suffix=".img", delete=False) as disk:
        block_path = Path(disk.name)
        write_fat32_test_disk(disk)
    cmd.extend([
        "-drive", f"if=none,format=raw,readonly=on,file={block_path},id=braneblk",
        "-device", "virtio-blk-pci,drive=braneblk,disable-modern=on",
    ])
    if machine == "q35":
        cmd.extend(["-device", "qemu-xhci,id=branexhci"])
        if usb_storage:
            with tempfile.NamedTemporaryFile(
                prefix="brane-usb-storage-", suffix=".img", delete=False
            ) as disk:
                usb_storage_path = Path(disk.name)
                write_fat32_test_disk(
                    disk,
                    volume_label=b"BRANEUSB   ",
                    readme_prefix=b"Brane OS USB storage path ready.\n",
                )
            cmd.extend([
                "-drive",
                f"if=none,id=usbstick,format=raw,readonly=on,file={usb_storage_path}",
                "-device",
                "usb-storage,bus=branexhci.0,drive=usbstick",
            ])
        else:
            cmd.extend(["-device", "usb-kbd,bus=branexhci.0"])
    if usb_hid:
        # Keep the AF_UNIX path below macOS' short sockaddr_un limit.
        qmp_tempdir = tempfile.TemporaryDirectory(prefix="bhid-", dir="/tmp")
        qmp_path = Path(qmp_tempdir.name) / "qmp.sock"
        cmd.extend(["-qmp", f"unix:{qmp_path},server=on,wait=off"])
    cmd.extend([
        "-serial", "stdio",
        "-nographic",         # no display window — pure serial I/O
        "-monitor", "none",
        "-no-reboot",         # exit on triple fault instead of rebooting
        "-accel", "tcg",      # software emulation — works in any CI/CD
    ])
    info(f"Launching QEMU (timeout={timeout}s):")
    info("  " + " ".join(cmd))

    found = set()
    failed_reason: list[str] = []
    output_lines: list[str] = []
    done_event = threading.Event()
    shell_ready = threading.Event()

    try:
        proc = subprocess.Popen(
            cmd,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            bufsize=1,
        )
    except FileNotFoundError:
        error(f"{QEMU_BIN} not found. Install QEMU and ensure it is in PATH.")
        if firmware_vars:
            firmware_vars.unlink(missing_ok=True)
        if block_path:
            block_path.unlink(missing_ok=True)
        if usb_storage_path:
            usb_storage_path.unlink(missing_ok=True)
        if qmp_tempdir:
            qmp_tempdir.cleanup()
        return 2

    def reader():
        assert proc.stdout is not None
        line_chars: list[str] = []
        scan_window = ""
        max_pattern_len = max(len(s) for s in required_strings + FAIL_STRINGS)

        def emit_partial_line() -> None:
            if not line_chars:
                return
            line = "".join(line_chars).rstrip("\r\n")
            output_lines.append(line)
            print(f"  serial │ {line}", flush=True)
            line_chars.clear()

        try:
            while char := proc.stdout.read(1):
                line_chars.append(char)
                scan_window = (scan_window + char)[-max_pattern_len:]

                for req in required_strings:
                    if req not in found and req in scan_window:
                        found.add(req)
                        ok(f"Found required string: {req!r}")

                if "brane>" in scan_window:
                    shell_ready.set()

                for bad in FAIL_STRINGS:
                    if bad in scan_window:
                        failed_reason.append(f"Detected failure string: {bad!r}")

                if char == "\n":
                    emit_partial_line()

                if failed_reason:
                    emit_partial_line()
                    done_event.set()
                    return

                # Detect the shell prompt immediately even though it has no newline.
                if found == set(required_strings) and not failed_reason:
                    emit_partial_line()
                    done_event.set()
                    return
        except Exception:
            pass
        finally:
            emit_partial_line()
            done_event.set()

    t = threading.Thread(target=reader, daemon=True)
    t.start()

    def inject_usb_key() -> None:
        if not usb_hid or qmp_path is None:
            return
        deadline = time.monotonic() + timeout
        client = None
        try:
            if not shell_ready.wait(timeout=max(0.1, deadline - time.monotonic())):
                raise TimeoutError("shell prompt did not appear before USB HID injection")
            client = QmpClient(qmp_path, deadline)
            client.execute(
                "human-monitor-command",
                {"command-line": "sendkey u"},
                deadline=deadline,
            )
            info("Injected key 'u' through QMP; awaiting the xHCI HID marker.")
        except Exception as exc:
            failed_reason.append(f"USB HID key injection failed: {exc}")
            done_event.set()
        finally:
            if client is not None:
                client.close()

    injector = threading.Thread(target=inject_usb_key, daemon=True)
    injector.start()

    # Wait for success signal or timeout
    triggered = done_event.wait(timeout=timeout)

    # Terminate QEMU
    try:
        proc.terminate()
        proc.wait(timeout=5)
    except Exception:
        proc.kill()

    t.join(timeout=3)
    injector.join(timeout=3)
    if firmware_vars:
        firmware_vars.unlink(missing_ok=True)
    if block_path:
        block_path.unlink(missing_ok=True)
    if usb_storage_path:
        usb_storage_path.unlink(missing_ok=True)
    if qmp_tempdir:
        qmp_tempdir.cleanup()

    # ── Evaluate result ──────────────────────────────────────────────────
    print()
    if failed_reason:
        for msg in failed_reason:
            error(msg)
        return 1

    missing = set(required_strings) - found
    if missing:
        if not triggered:
            error(f"Timeout after {timeout}s — serial output did not contain all required strings.")
        else:
            error("QEMU exited before all required strings were found.")
        error(f"Missing: {[s for s in missing]}")
        info("--- Captured serial output ---")
        for ln in output_lines:
            print(f"  {ln}")
        return 1

    ok("All required strings found in serial output. Boot test PASSED ✓")
    return 0

# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------

def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(description="Brane OS Boot Test")
    p.add_argument("--timeout", type=int, default=DEFAULT_TIMEOUT,
                   help=f"Seconds before giving up (default: {DEFAULT_TIMEOUT})")
    p.add_argument("--no-build", action="store_true",
                   help="Skip build step; use KERNEL_BIN_PATH env var or pre-existing image")
    p.add_argument("--cpus", type=int, default=1,
                   help="Number of virtual CPUs for QEMU (default: 1)")
    p.add_argument("--machine", choices=("pc", "q35"), default=None,
                   help="QEMU machine type; q35 validates the PCIe ECAM path")
    p.add_argument("--usb-hid", action="store_true",
                   help="Inject a key over QMP and require the xHCI HID/TTY path")
    p.add_argument("--usb-storage", action="store_true",
                   help="Attach a read-only USB BOT/SCSI FAT32 disk and require /usb")
    media = p.add_mutually_exclusive_group()
    media.add_argument("--img", type=str, default=None,
                       help="Path to existing .img file, skips build entirely")
    media.add_argument("--iso", type=str, default=None,
                       help="Path to an existing bootable ISO, skips build entirely")
    return p.parse_args()


def main() -> None:
    os.environ["NO_RUN"] = "1"
    args = parse_args()
    if not 1 <= args.cpus <= 32:
        error("--cpus must be between 1 and 32")
        sys.exit(2)
    if args.usb_hid and args.machine != "q35":
        error("--usb-hid requires --machine q35")
        sys.exit(2)
    if args.usb_storage and args.machine != "q35":
        error("--usb-storage requires --machine q35")
        sys.exit(2)
    if args.usb_hid and args.usb_storage:
        error("--usb-hid and --usb-storage are mutually exclusive until xHCI supports multiple slots")
        sys.exit(2)
    start = time.monotonic()

    info(f"Brane OS Boot Test — timeout={args.timeout}s")
    info(f"Repo root: {REPO_ROOT}")

    media_type = "disk"
    if args.iso:
        img_path = Path(args.iso)
        media_type = "iso"
        if not img_path.exists():
            error(f"ISO not found: {img_path}")
            sys.exit(2)
    elif args.img:
        img_path = Path(args.img)
        if not img_path.exists():
            error(f"Image not found: {img_path}")
            sys.exit(2)
    elif args.no_build and os.environ.get("KERNEL_BIN_PATH"):
        kernel_path = Path(os.environ["KERNEL_BIN_PATH"])
        img_path = build_disk_image(kernel_path)
    else:
        kernel_path = build_kernel()
        img_path = build_disk_image(kernel_path)

    rc = run_qemu_test(
        img_path,
        args.timeout,
        media_type,
        args.cpus,
        args.machine,
        args.usb_hid,
        args.usb_storage,
    )

    elapsed = time.monotonic() - start
    info(f"Total elapsed: {elapsed:.1f}s")
    sys.exit(rc)


if __name__ == "__main__":
    main()
