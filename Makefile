# ============================================================
# Brane OS — Build System
# ============================================================

KERNEL_BIN     := target/x86_64-unknown-none/debug/brane_os_kernel
KERNEL_RELEASE := target/x86_64-unknown-none/release/brane_os_kernel
TEST_IMAGE     := target/brane_os-bios.img
VERSION        ?= dev
RELEASE_ISO    := dist/brane_os_v$(VERSION).iso
BUILD_FLAGS    := -Z build-std=core,compiler_builtins,alloc \
                  -Z build-std-features=compiler-builtins-mem \
                  --target x86_64-unknown-none

.PHONY: build build-release run run-release test fmt clippy \
        stress-test test-image boot-test pcie-test smp-test acpi-test security-test integration-test e2e-test test-all \
        docs iso iso-test release-test release parallels-deploy parallels-start parallels-stop parallels-status clean help

# --- Build -------------------------------------------------------------------

build: ## Build kernel (debug)
	cd kernel && cargo build $(BUILD_FLAGS)

build-release: ## Build kernel (release, with LTO)
	cd kernel && cargo build --release $(BUILD_FLAGS)

# --- Run in QEMU -------------------------------------------------------------

run: build ## Build and run in QEMU (debug)
	KERNEL_BIN_PATH=$(KERNEL_BIN) cargo run --package runner

run-release: build-release ## Build and run in QEMU (release)
	KERNEL_BIN_PATH=$(KERNEL_RELEASE) cargo run --package runner --release

# --- Quality -----------------------------------------------------------------

fmt: ## Format all Rust code
	cargo fmt --all

clippy: ## Run Clippy lints
	cd kernel && cargo clippy $(BUILD_FLAGS) -- -D warnings
	cd runner && cargo clippy --all-targets -- -D warnings

test: ## Run unit + integration tests (host-side, no QEMU)
	cd kernel && cargo test --lib

stress-test: ## Run deterministic mutation fuzzing and subsystem stress tests
	cd kernel && cargo test --lib fuzz_tests
	cd kernel && cargo test --lib stress_tests

# --- Housekeeping ------------------------------------------------------------

clean: ## Remove build artifacts
	cargo clean
	rm -f *.img *.iso *.bin
	rm -rf dist/

# --- Testing -----------------------------------------------------------------

test-image: build-release ## Build the shared release-kernel image used by QEMU tests
	NO_RUN=1 KERNEL_BIN_PATH=$(KERNEL_RELEASE) cargo run --package runner

boot-test: test-image ## Run automated release-kernel boot test in QEMU (60 s timeout)
	python3 tests/boot/test_boot.py --img $(TEST_IMAGE)

pcie-test: test-image ## Boot Q35 and verify ACPI MCFG + PCIe ECAM discovery
	python3 tests/boot/test_boot.py --img $(TEST_IMAGE) --machine q35

smp-test: test-image ## Boot with four vCPUs and verify AP startup and IPI probe
	python3 tests/boot/test_boot.py --img $(TEST_IMAGE) --cpus 4

acpi-test: test-image ## ACPI test: S3 suspend, QMP wake and post-resume shell
	python3 tests/acpi/test_suspend_resume.py --img $(TEST_IMAGE)

security-test: test-image ## Security tests: capability denial + privilege escalation
	python3 tests/security/test_capability_denial.py --img $(TEST_IMAGE)
	python3 tests/security/test_privilege_escalation.py --img $(TEST_IMAGE)

integration-test: test-image ## Integration tests: syscall→service + capability broker
	python3 tests/integration/test_syscall_service.py --img $(TEST_IMAGE)
	python3 tests/integration/test_capability_broker.py --img $(TEST_IMAGE)

e2e-test: test-image ## E2E tests: brsh commands + full boot flow verification
	python3 tests/e2e/test_brsh_commands.py --no-inject --img $(TEST_IMAGE)
	python3 tests/e2e/test_full_boot_flow.py --img $(TEST_IMAGE)

test-all: test stress-test boot-test pcie-test smp-test acpi-test security-test integration-test e2e-test ## Full test suite (unit → stress/fuzz → boot/PCIe → SMP → ACPI → security → integration → e2e)
	@echo ""
	@echo "  \033[32mAll test suites passed ✓\033[0m"

# --- Documentation -----------------------------------------------------------

docs: ## Generate API documentation into target/doc/
	cargo doc -p brane_os_kernel --no-deps --document-private-items
	@echo ""
	@echo "  Docs: target/doc/brane_os_kernel/index.html"

# --- Release / Packaging -----------------------------------------------------

iso: ## Build versioned bootable ISO + checksum + archive (VERSION=dev)
	chmod +x tools/make_iso.sh
	./tools/make_iso.sh $(VERSION)

iso-test: iso ## Build and boot the ISO through QEMU/TCG
	python3 tests/boot/test_boot.py --iso $(RELEASE_ISO)

release-test: iso-test ## Validate release artifacts and UEFI boot catalog
	python3 tests/release/test_release_artifacts.py --dist dist --version $(VERSION)

release: iso ## Full release: ISO + SHA256 checksum + tar.gz archive
	@echo ""
	@echo "Release artifacts in dist/:"
	@ls -lh dist/

# --- Parallels Desktop -------------------------------------------------------

parallels-deploy: iso ## Create/update the dedicated Parallels VM with the ISO
	VERSION=$(VERSION) ./tools/parallels_deploy.sh deploy

parallels-start: ## Start the dedicated Parallels VM
	./tools/parallels_deploy.sh start

parallels-stop: ## Request ACPI shutdown for the dedicated Parallels VM
	./tools/parallels_deploy.sh stop

parallels-status: ## Show detailed status for the dedicated Parallels VM
	./tools/parallels_deploy.sh status

# --- Help --------------------------------------------------------------------

help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | \
		awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-20s\033[0m %s\n", $$1, $$2}'
