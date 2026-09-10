<p align="center">
	<img src="./assets/brane-os-mark.png" alt="Brane OS multidimensional membrane logo" width="240" />
</p>

<h1 align="center">Brane OS</h1>

<p align="center">
	<strong>The multidimensional operating system</strong>
</p>

<p align="center">
	<sub>Modular · Secure · Adaptive · AI-native</sub>
</p>

> **Brane OS** is a custom, modular, secure, and extensible operating system designed to integrate an Artificial Intelligence layer controlled by policies, capabilities, and strict auditing.

The logo represents interconnected membranes forming a single adaptive system. Brand assets and usage guidance are available in [`assets/`](assets/README.md).

| Status | Version | Architecture | Primary Language |
|--------|---------|--------------|------------------|
| Phase 13 · Hardware I/O | `v0.1.0` | `x86_64` | Rust |

Current milestone: virtio-blk and read-only FAT32 are operational; xHCI resets
and addresses a USB keyboard in Q35. USB control transfers and HID interrupt
reports are the next implementation cut. Physical hardware validation and the
v1.0 tag are still release gates.

---

## 🚀 Vision

The goal is not just to build a kernel, but a complete platform featuring:
- A reliable, small, **hybrid modular kernel**.
- Decoupled system services in user space.
- A **capability-based security model**.
- Comprehensive observability and auditing.
- An **AI Subsystem** capable of observing, analyzing, suggesting, and executing restricted actions under strict control.

---

## 🧠 Core Features

### 1. Adaptability (Brane)
The OS acts as an intelligent **membrane** — a *brane* — that dynamically adapts its behavior, resource allocation, and service topology based on workload, context, and environment. Modules can be loaded, unloaded, and reconfigured at runtime without rebooting.

### 2. External Device Connection (External Branes)
Brane OS treats every connected device as an **external brane** — a peer membrane with its own capabilities. Through a secure discovery and pairing protocol, devices can share resources, delegate tasks, and form **brane clusters** for distributed computing, all mediated by the capability broker and policy engine.

### 3. Mobile Integration
First-class support for mobile device integration. Phones and tablets can act as **companion branes**, enabling:
- Remote system monitoring and control.
- Notification forwarding and AI alert delivery.
- Secure file and context sharing via the brane protocol.

### 4. AI Integration
A native AI subsystem that operates under strict capability-based security:
- **Observe** system telemetry and detect anomalies.
- **Suggest** optimizations and incident responses.
- **Execute** restricted, reversible actions when authorized by policy.
- All AI actions are fully auditable and revocable.

---

Start with the [documentation index](docs/README.md). Key references are the
[roadmap](docs/ROADMAP.md), [architecture](docs/ARCHITECTURE.md),
[runbook](docs/RUNBOOK.md), [test plan](docs/TEST_PLAN.md), and
[architecture decisions](docs/ADR/README.md), and [changelog](CHANGELOG.md).

---

## 🛠 Prerequisites

To build and run Brane OS locally, you'll need the following tools:

- **Rust Nightly** (the exact version is pinned by `rust-toolchain.toml`)
- Rust components: `rust-src`, `llvm-tools-preview`
- **QEMU** (`qemu-system-x86_64`) for emulation
- `make`

### macOS Setup (Homebrew)
```bash
# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup toolchain install nightly-2026-03-11
rustup component add rust-src llvm-tools-preview rustfmt clippy --toolchain nightly-2026-03-11

# Install QEMU
brew install qemu
```

---

## 🏗 Build & Run

The project includes a `Makefile` to simplify builds and testing.

```bash
# Build the kernel binary (debug)
make build

# Build the kernel binary (release, with LTO)
make build-release

# Build and launch QEMU
make run

# Run formatting and linting
cargo fmt --all -- --check
make clippy

# Run host-side unit tests
make test

# Run deterministic stress and mutation-fuzz suites
make stress-test

# Build, verify and boot-test release artifacts
make release-test VERSION=dev
```

For the full local execution and CI checklist, see [`docs/RUNBOOK.md`](docs/RUNBOOK.md).

---

## 📁 Repository Structure

Based on §20 of the master specification:

- `boot/` — Bootloader and early initialization
- `kernel/` — Core kernel (scheduler, memory manager, syscalls, IPC)
- `services/` — System services (init, process manager, capability broker, policy engine)
- `drivers/` — Hardware drivers (serial, timer, disk, input, net)
- `userland/` — Shell and admin tools
- `ai/` — AI Subsystem (context collector, model runtime, decision planner)
- `tests/` — Multi-level testing strategy
- `tools/` — QEMU runners and build tools

---

## 🛡 Security & AI Rules

Any AI agent or human contributor working on Brane OS must follow these core principles:
1. **Security before automation.**
2. **The kernel must remain small and maintainable.**
3. **The AI does not have direct, free access to the system.** Every sensitive action must pass through the `capability_broker`, `policy_engine`, and `audit_service`.
4. **All relevant actions are auditable.**

---

## 📄 License

This project is licensed under the [MIT License](LICENSE).
