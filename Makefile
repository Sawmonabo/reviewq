.DEFAULT_GOAL := check
.NOTPARALLEL:
CARGO_DRIVER ?= cargo
BUILD_JOBS ?= 2
TEST_THREADS ?= 2
PACKAGE ?= reviewq
override CONTRACT_SCHEMA := contracts/v1.schema.json
override CONTRACT_OUTPUT := web/src/api/generated
override LEDGER_CONTRACT_SCHEMA := contracts/ledger-tools.schema.json
override LEDGER_CONTRACT_EXAMPLE := fixtures/contracts/ledger-tools.json
override CLAUDE_LEDGER_CONTRACT := hosts/claude/plugin/hooks/ledger-schema.js
ifeq ($(CARGO_DRIVER),mbx)
# Cached build-script artifacts can be read-only when plain Cargo recompiles them.
RUST_COMMAND = CARGO_TARGET_DIR="$(or $(CARGO_TARGET_DIR),target)/mbx" mbx
else
RUST_COMMAND = $(CARGO_DRIVER)
endif

.PHONY: check crate-deps rust-check fmt-check lint test doc-test web-check web-build web-install web-test fmt check-low check-cached test-package package
.PHONY: contracts contracts-check contracts-verify contracts-verify-browser

check: crate-deps rust-check web-check contracts-check contracts-verify

contracts contracts-check:
	@set -eu; \
		if [ "$@" = contracts ]; then \
			if [ -L contracts ] || [ -L "$(CONTRACT_SCHEMA)" ] || [ -L "$(CONTRACT_OUTPUT)" ] || [ -L "$(LEDGER_CONTRACT_SCHEMA)" ] || [ -L "$(LEDGER_CONTRACT_EXAMPLE)" ] || [ -L "$(CLAUDE_LEDGER_CONTRACT)" ]; then \
				printf '%s\n' 'Refusing to write symlinked contract outputs' >&2; exit 1; \
			fi; \
			mkdir -p contracts; \
			scratch=$$(mktemp -d contracts/.generate.XXXXXX); \
		else \
			unexpected_schemas=$$(find "$(dir $(CONTRACT_SCHEMA))" -maxdepth 1 -name '*.schema.json' ! -path "$(CONTRACT_SCHEMA)" ! -path "$(LEDGER_CONTRACT_SCHEMA)" -print); \
			if [ -n "$$unexpected_schemas" ]; then \
				printf 'Unexpected contract schemas\n%s\n' "$$unexpected_schemas" >&2; exit 1; \
			fi; \
			scratch=$$(mktemp -d); \
		fi; \
		trap 'find "$$scratch" -depth -delete' EXIT; \
		cargo run --locked --quiet -p reviewq-protocol --features schema --bin export-schema > "$$scratch/v1.schema.json"; \
		cargo run --locked --quiet -p reviewq-protocol --features schema --bin export-schema -- --ledger-tools-schema > "$$scratch/ledger-tools.schema.json"; \
		cargo run --locked --quiet -p reviewq-protocol --features schema --bin export-schema -- --ledger-tools-example > "$$scratch/ledger-tools.json"; \
		cargo run --locked --quiet -p reviewq-protocol --features schema --bin export-schema -- --claude-ledger-tools > "$$scratch/ledger-schema.js"; \
		node web/tools/generate-contracts.mjs --schema "$$scratch/v1.schema.json" --out "$$scratch/generated"; \
		if [ "$@" = contracts-check ]; then \
			diff -u "$(CONTRACT_SCHEMA)" "$$scratch/v1.schema.json"; \
			diff -u "$(LEDGER_CONTRACT_SCHEMA)" "$$scratch/ledger-tools.schema.json"; \
			diff -u "$(LEDGER_CONTRACT_EXAMPLE)" "$$scratch/ledger-tools.json"; \
			diff -u "$(CLAUDE_LEDGER_CONTRACT)" "$$scratch/ledger-schema.js"; \
			diff -ru "$(CONTRACT_OUTPUT)" "$$scratch/generated"; \
		else \
			mkdir -p "$(dir $(CONTRACT_OUTPUT))"; \
			if [ -d "$(CONTRACT_OUTPUT)" ]; then \
				mv -T -- "$(CONTRACT_OUTPUT)" "$$scratch/previous-generated"; \
			fi; \
			if mv -T -- "$$scratch/generated" "$(CONTRACT_OUTPUT)"; then \
				if mv -T -- "$$scratch/v1.schema.json" "$(CONTRACT_SCHEMA)"; then \
					mv -T -- "$$scratch/ledger-tools.schema.json" "$(LEDGER_CONTRACT_SCHEMA)"; \
					mv -T -- "$$scratch/ledger-tools.json" "$(LEDGER_CONTRACT_EXAMPLE)"; \
					mv -T -- "$$scratch/ledger-schema.js" "$(CLAUDE_LEDGER_CONTRACT)"; \
					exit 0; \
				fi; \
				mv -T -- "$(CONTRACT_OUTPUT)" "$$scratch/generated"; \
			fi; \
			if [ -d "$$scratch/previous-generated" ]; then \
				mv -T -- "$$scratch/previous-generated" "$(CONTRACT_OUTPUT)"; \
			fi; \
			exit 1; \
		fi

contracts-verify contracts-verify-browser:
	@set -eu; \
		scratch=$$(mktemp -d); trap 'find "$$scratch" -depth -delete' EXIT; \
		cargo run --locked --quiet -p reviewq-protocol --example delivery-receipt > "$$scratch/receipt.ndjson"; \
		cargo run --locked --quiet -p reviewq-protocol --example first-review-contracts -- "$$scratch/fixtures"; \
		node web/tools/contracts/verify.mjs "$$scratch" $(if $(filter contracts-verify-browser,$@),--browser)

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

package: web-build
	cargo build --locked --release -p reviewq -j $(BUILD_JOBS)
	node tools/package/assemble.mjs $(if $(PACKAGE_OUT),"$(PACKAGE_OUT)")

check-low:
	$(MAKE) check BUILD_JOBS=1 TEST_THREADS=1

check-cached:
	$(MAKE) check CARGO_DRIVER=mbx

test-package:
	$(RUST_COMMAND) nextest run --locked -p $(PACKAGE) --no-tests=pass --build-jobs $(BUILD_JOBS) --test-threads $(TEST_THREADS)

fmt:
	cargo fmt --all
	cd web && pnpm exec prettier --write .

web-install:
	cd web && pnpm install --frozen-lockfile

web-test:
	cd web && pnpm run test:browser
