.DEFAULT_GOAL := check
.NOTPARALLEL:
CARGO_DRIVER ?= cargo
BUILD_JOBS ?= 2
TEST_THREADS ?= 2
PACKAGE ?= reviewq
ifeq ($(CARGO_DRIVER),mbx)
# Cached build-script artifacts can be read-only when plain Cargo recompiles them.
RUST_COMMAND = CARGO_TARGET_DIR="$(or $(CARGO_TARGET_DIR),target)/mbx" mbx
else
RUST_COMMAND = $(CARGO_DRIVER)
endif

.PHONY: check crate-deps rust-check fmt-check lint test doc-test web-check web-build web-install web-test fmt check-low check-cached test-package

check: crate-deps rust-check web-check

crate-deps:
	node tools/check-crate-deps.mjs
rust-check: fmt-check lint test doc-test

fmt-check:
	cargo fmt --all -- --check

lint:
	$(RUST_COMMAND) clippy --locked --workspace --all-targets -j $(BUILD_JOBS) -- -D warnings

test:
	$(RUST_COMMAND) nextest run --locked --workspace --build-jobs $(BUILD_JOBS) --test-threads $(TEST_THREADS)

doc-test:
	$(RUST_COMMAND) test --locked --workspace --doc -j $(BUILD_JOBS)

web-check:
	cd web && pnpm run check

web-build:
	cd web && pnpm run build

check-low:
	$(MAKE) check BUILD_JOBS=1 TEST_THREADS=1

check-cached:
	$(MAKE) check CARGO_DRIVER=mbx

test-package:
	$(RUST_COMMAND) nextest run --locked -p $(PACKAGE) --build-jobs $(BUILD_JOBS) --test-threads $(TEST_THREADS)

fmt:
	cargo fmt --all
	cd web && pnpm exec prettier --write .

web-install:
	cd web && pnpm install --frozen-lockfile

web-test:
	cd web && pnpm run test:browser
