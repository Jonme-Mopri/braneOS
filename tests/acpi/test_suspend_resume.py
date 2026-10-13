#!/usr/bin/env python3
"""QEMU integration test for the ACPI S3 suspend/resume path."""

import argparse
import json
import os
import socket
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from qemu_serial import serial_lines


REPO_ROOT = Path(__file__).resolve().parents[2]
QEMU_BIN = os.environ.get("QEMU_BIN", "qemu-system-x86_64")
DEFAULT_TIMEOUT = int(os.environ.get("BOOT_TIMEOUT", "120"))
FAIL_STRINGS = ("KERNEL PANIC", "DOUBLE FAULT", "PAGE FAULT", "GENERAL PROTECTION FAULT")


def info(message: str) -> None:
    print(f"[acpi-s3]  \033[36mINFO\033[0m  {message}", flush=True)


def passed(message: str) -> None:
    print(f"[acpi-s3]  \033[32mPASS\033[0m  {message}", flush=True)


def failed(message: str) -> None:
    print(f"[acpi-s3]  \033[31mFAIL\033[0m  {message}", flush=True)


class QmpClient:
    def __init__(self, path: Path, deadline: float):
        while not path.exists():
            if time.monotonic() >= deadline:
                raise TimeoutError("QMP socket was not created")
            time.sleep(0.05)

        self.socket = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.socket.settimeout(1.0)
        self.socket.connect(str(path))
        self.stream = self.socket.makefile("rwb", buffering=0)
        self.events: list[dict] = []
        greeting = self._read_message(deadline)
        if "QMP" not in greeting:
            raise RuntimeError(f"invalid QMP greeting: {greeting}")
        self.execute("qmp_capabilities", deadline=deadline)

    def close(self) -> None:
        self.stream.close()
        self.socket.close()

    def _read_message(self, deadline: float) -> dict:
        while time.monotonic() < deadline:
            try:
                line = self.stream.readline()
            except TimeoutError:
                continue
            if not line:
                raise RuntimeError("QMP connection closed")
            return json.loads(line)
        raise TimeoutError("timed out waiting for QMP message")

    def execute(self, command: str, arguments: dict | None = None, deadline: float | None = None) -> dict:
        deadline = deadline or (time.monotonic() + 10)
        request_id = f"brane-{time.monotonic_ns()}"
        payload: dict = {"execute": command, "id": request_id}
        if arguments:
            payload["arguments"] = arguments
        self.stream.write((json.dumps(payload) + "\n").encode())

        while True:
            message = self._read_message(deadline)
            if "event" in message:
                self.events.append(message)
                continue
            if message.get("id") != request_id:
                continue
            if "error" in message:
                raise RuntimeError(f"QMP {command} failed: {message['error']}")
            return message

    def wait_event(self, name: str, deadline: float) -> dict:
        while True:
            for index, event in enumerate(self.events):
                if event.get("event") == name:
                    return self.events.pop(index)
            message = self._read_message(deadline)
            if "event" in message:
                self.events.append(message)


def send_text(qmp: QmpClient, text: str, deadline: float) -> None:
    key_names = {"\n": "ret", " ": "spc"}
    for character in text:
        key = key_names.get(character, character)
        qmp.execute(
            "human-monitor-command",
            {"command-line": f"sendkey {key}"},
            deadline,
        )
        time.sleep(0.06)


def send_text_explicit(qmp: QmpClient, text: str, deadline: float) -> None:
    """Inject key-down/up pairs without relying on QEMU's virtual key timer.

    This is used after S3 because some QEMU versions do not expire HMP
    ``sendkey`` holds once the guest virtual clock has crossed suspend.
    """
    key_names = {"\n": "ret", " ": "spc"}
    for character in text:
        key = key_names.get(character, character)
        key_event = {"type": "qcode", "data": key}
        qmp.execute(
            "input-send-event",
            {
                "events": [
                    {"type": "key", "data": {"down": True, "key": key_event}},
                ]
            },
            deadline,
        )
        time.sleep(0.03)
        qmp.execute(
            "input-send-event",
            {
                "events": [
                    {"type": "key", "data": {"down": False, "key": key_event}},
                ]
            },
            deadline,
        )
        time.sleep(0.03)


def qemu_xhci_support() -> bool | None:
    """Return whether this QEMU exposes its emulated qemu-xhci device.

    The xHCI S3 smoke test is deliberately opt-in.  A QEMU build without the
    device model must therefore skip that optional case rather than making
    the baseline ACPI test depend on a host controller or a particular QEMU
    package layout.  ``None`` means that QEMU itself could not be executed.
    """
    try:
        result = subprocess.run(
            [QEMU_BIN, "-device", "help"],
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            check=False,
        )
    except OSError:
        return None
    return any('"qemu-xhci"' in line for line in result.stdout.splitlines())


def run_test(image: Path, timeout: int, xhci: bool = False) -> int:
    if xhci:
        support = qemu_xhci_support()
        if support is None:
            failed(f"{QEMU_BIN} could not be executed")
            return 2
        if not support:
            info("SKIP: this QEMU has no qemu-xhci device; ACPI baseline remains valid")
            return 0

    deadline = time.monotonic() + timeout
    shell_ready = threading.Event()
    resumed = threading.Event()
    acpi_status = threading.Event()
    scheduler_resumed = threading.Event()
    xhci_resume = threading.Event()
    xhci_results: list[str] = []
    failures: list[str] = []

    with tempfile.TemporaryDirectory(prefix="brane-acpi-") as temp_dir:
        qmp_path = Path(temp_dir) / "qmp.sock"
        command = [QEMU_BIN, "-m", "256M"]
        if xhci:
            # Keep the controller unconnected: this exercises the PCI/xHCI
            # bootstrap and ACPI resume path without requiring a physical USB
            # device or introducing a second input source into the harness.
            command.extend(["-machine", "q35", "-device", "qemu-xhci,id=branexhci"])
        command.extend(
            [
                "-drive",
                f"format=raw,file={image}",
                "-serial",
                "stdio",
                "-nographic",
                "-monitor",
                "none",
                "-qmp",
                f"unix:{qmp_path},server=on,wait=off",
                "-no-reboot",
                "-accel",
                "tcg",
            ]
        )
        info(f"Launching QEMU (timeout={timeout}s)")
        proc = subprocess.Popen(
            command,
            cwd=REPO_ROOT,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            bufsize=1,
        )

        def read_serial() -> None:
            assert proc.stdout is not None
            for line in serial_lines(proc.stdout, ("brane>", *FAIL_STRINGS)):
                print(f"  serial │ {line}", flush=True)
                if "brane>" in line:
                    shell_ready.set()
                if "[acpi] Resume complete" in line:
                    resumed.set()
                if "ACPI initialized: true" in line:
                    acpi_status.set()
                if "[sched] Resumed." in line:
                    scheduler_resumed.set()
                if "[xhci] S3 resume result:" in line:
                    xhci_results.append(line.rsplit(":", 1)[-1].strip())
                    xhci_resume.set()
                for marker in FAIL_STRINGS:
                    if marker in line:
                        failures.append(marker)
                        resumed.set()

        reader = threading.Thread(target=read_serial, daemon=True)
        reader.start()
        qmp = None
        try:
            qmp = QmpClient(qmp_path, deadline)
            if not shell_ready.wait(max(0, deadline - time.monotonic())):
                raise TimeoutError("shell prompt did not appear")
            passed("Kernel booted with an interactive shell")
            shell_ready.clear()

            send_text(qmp, "suspend\n", deadline)
            qmp.wait_event("SUSPEND", deadline)
            passed("QEMU observed the guest entering ACPI S3")

            qmp.execute("system_wakeup", deadline=deadline)
            qmp.wait_event("WAKEUP", deadline)
            passed("QEMU delivered the wake event")

            if not resumed.wait(min(8, max(0, deadline - time.monotonic()))):
                registers = qmp.execute(
                    "human-monitor-command",
                    {"command-line": "info registers"},
                    deadline,
                ).get("return", "")
                low_memory = qmp.execute(
                    "human-monitor-command",
                    {"command-line": "xp /128bx 0x1000"},
                    deadline,
                ).get("return", "")
                info("CPU state after failed wake:\n" + registers)
                info("Wake page after failed wake:\n" + low_memory)
                raise TimeoutError("kernel did not report resume completion")
            if failures:
                raise RuntimeError(f"kernel failure after wake: {failures[0]}")
            passed("Kernel restored platform state after S3")
            if xhci:
                if not xhci_resume.wait(max(0, deadline - time.monotonic())):
                    raise TimeoutError("xHCI S3 resume gate did not publish a result")
                if xhci_results[-1] not in {"Retained", "PollingFallback", "Quarantined"}:
                    raise RuntimeError(f"unexpected xHCI S3 result: {xhci_results[-1]}")
                passed("Optional xHCI S3 smoke completed without requiring post-resume USB hardware")

            if not shell_ready.wait(max(0, deadline - time.monotonic())):
                raise TimeoutError("shell prompt did not return after resume")
            shell_ready.clear()
            # QEMU may publish the prompt before its post-wake input path is
            # ready. Give the guest a short settling window before injecting
            # the first command.
            time.sleep(0.1)
            send_text_explicit(qmp, "acpi\n", deadline)
            if not acpi_status.wait(min(5, max(0, deadline - time.monotonic()))):
                # Older QEMU builds can accept HMP sendkey after S3 while
                # rejecting input-send-event. Retry through the compatible
                # path before treating the guest as unresponsive.
                info("post-resume input-send-event produced no command; retrying HMP sendkey")
                send_text(qmp, "acpi\n", deadline)
            if not acpi_status.wait(max(0, deadline - time.monotonic())):
                raise TimeoutError("shell did not execute a command after resume")
            passed("brsh accepted a command after resume")

            shell_ready.clear()
            time.sleep(0.1)
            send_text_explicit(qmp, "yield\n", deadline)
            if not scheduler_resumed.wait(min(5, max(0, deadline - time.monotonic()))):
                info("post-resume input-send-event produced no yield; retrying HMP sendkey")
                send_text(qmp, "yield\n", deadline)
            if not scheduler_resumed.wait(max(0, deadline - time.monotonic())):
                raise TimeoutError("BSP did not resume its CPU-local context after yield")
            passed("BSP restored its isolated scheduler context after yield")
            return 0
        except (OSError, RuntimeError, TimeoutError) as error:
            failed(str(error))
            return 1
        finally:
            if qmp is not None:
                qmp.close()
            proc.terminate()
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                proc.kill()
            reader.join(timeout=2)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--img", type=Path, required=True)
    parser.add_argument("--timeout", type=int, default=DEFAULT_TIMEOUT)
    parser.add_argument(
        "--xhci",
        action="store_true",
        help="opt-in Q35/xHCI S3 smoke; skips when QEMU lacks qemu-xhci",
    )
    args = parser.parse_args()
    if not args.img.exists():
        failed(f"disk image not found: {args.img}")
        return 2
    return run_test(args.img.resolve(), args.timeout, args.xhci)


if __name__ == "__main__":
    raise SystemExit(main())
