.DEFAULT_GOAL := help

CARGO ?= cargo
SSH ?= ssh
LIMA_INSTANCE ?= herdr-test
LIMA_SSH_PORT ?= 2222
TARGET ?= lima-$(LIMA_INSTANCE)
ARGS ?=
PORT ?=
RELEASE_BIN := target/release/hfwd

ifneq ($(filter lima-destroy,$(MAKECMDGOALS)),)
ifneq ($(words $(MAKECMDGOALS)),1)
$(error usage: make lima-destroy (this destructive target must be run alone))
endif
endif

ifneq ($(filter serve,$(MAKECMDGOALS)),)
ifneq ($(words $(MAKECMDGOALS)),1)
$(error usage: make serve [PORT=number])
endif
endif

ifneq ($(filter lima-up,$(MAKECMDGOALS)),)
ifneq ($(filter run attach,$(MAKECMDGOALS)),)
$(error use 'make lima-run' instead of combining lima-up with run or attach)
endif
endif

ifneq ($(filter run attach lima-run,$(MAKECMDGOALS)),)
ifneq ($(words $(MAKECMDGOALS)),1)
$(error usage: run, attach, and lima-run must be invoked separately)
endif
endif

.PHONY: help fmt fmt-check lint markdown-check markdown-fix deps-check test build demo package package-smoke release-check ci manifest-check scripts-check \
	install uninstall run attach serve lima-up lima-run lima-down lima-destroy \
	lima-status lima-shell

help: ## General|Show the available commands
	@awk 'BEGIN {FS = ":.*## "} \
		/^[a-zA-Z0-9_-]+:.*## / { \
			split($$2, meta, "\\|"); \
			lines[meta[1]] = lines[meta[1]] sprintf("  %-16s %s\n", $$1, meta[2]); \
		} \
		END { \
			sections[1] = "General"; sections[2] = "Core"; sections[3] = "Install"; \
			sections[4] = "Remote development"; sections[5] = "Lima"; sections[6] = "Release"; \
			for (position = 1; position <= 6; position++) { \
				section = sections[position]; \
				if (lines[section] != "") printf "%s%s:\n%s", (position == 1 ? "" : "\n"), section, lines[section]; \
			} \
		}' $(MAKEFILE_LIST)

fmt: ## Core|Format Rust sources
	$(CARGO) fmt --all
	rustfmt --edition 2021 src/bin/local/*.rs src/bin/plugin/*.rs

fmt-check:
	$(CARGO) fmt --all -- --check
	rustfmt --edition 2021 --check src/bin/local/*.rs src/bin/plugin/*.rs

lint:
	$(CARGO) clippy --locked --all-targets --all-features -- -D warnings

markdown-check: ## Core|Check Markdown formatting
	npx --yes markdownlint-cli2@0.23.1 "**/*.md"

markdown-fix: ## Core|Format fixable Markdown violations
	npx --yes markdownlint-cli2@0.23.1 --fix "**/*.md"

deps-check: ## Core|Run dependency policy checks
	$(CARGO) deny check

test: ## Core|Run unit and integration tests
	$(CARGO) test --locked --all-targets

build: ## Core|Build optimized local binaries
	$(CARGO) build --locked --release --bins

demo: build lima-up ## Core|Record the README product GIFs
	vhs .github/assets/demo/overview-dark.tape
	vhs .github/assets/demo/overview-light.tape
	vhs .github/assets/demo/dashboard-dark.tape
	vhs .github/assets/demo/dashboard-light.tape
	vhs .github/assets/demo/settings-dark.tape
	vhs .github/assets/demo/settings-light.tape

manifest-check:
	python3 scripts/check-manifest.py

scripts-check:
	sh -n install.sh scripts/install-plugin-binary.sh scripts/package-release.sh scripts/check-release.sh \
		scripts/render-homebrew-formula.sh scripts/test-homebrew-formula.sh scripts/test-release-package.sh \
		uninstall.sh scripts/test-server.sh .github/assets/demo/herdr
	bash -n .github/assets/demo/setup.sh .github/assets/demo/overview.sh
	sh scripts/test-homebrew-formula.sh
	PYTHONPYCACHEPREFIX="$${TMPDIR:-/tmp}/herdr-fwd-pycache" python3 -m py_compile \
		scripts/check-manifest.py scripts/test-dev-server
	scripts/test-dev-server --self-test

ci: fmt-check lint markdown-check test build manifest-check scripts-check package-smoke ## Core|Run every local CI check

package: build
	scripts/package-release.sh "$$(rustc -vV | sed -n 's/^host: //p')"

package-smoke: package
	scripts/test-release-package.sh

release-check: ## Release|Validate release metadata with VERSION_TAG=vMAJOR.MINOR.PATCH
	scripts/check-release.sh "$${VERSION_TAG:?set VERSION_TAG, for example v0.1.0}"

install: ## Install|Build and install hfwd into ~/.local/bin
	./install.sh --from-source

uninstall: ## Install|Remove hfwd from ~/.local/bin
	./uninstall.sh

run: build ## Remote development|Build, deploy, and attach to TARGET
	$(RELEASE_BIN) $(TARGET) $(ARGS)

attach: build ## Remote development|Attach to TARGET without syncing the plugin
	$(RELEASE_BIN) $(TARGET) $(ARGS)

serve: ## Remote development|Start fixtures; set PORT for one server
	scripts/test-dev-server "$(if $(PORT),$(PORT),--topology)"

lima-up: ## Lima|Create or provision the test VM
	HERDR_FWD_LIMA_INSTANCE=$(LIMA_INSTANCE) HERDR_FWD_LIMA_SSH_PORT=$(LIMA_SSH_PORT) scripts/test-server.sh up

lima-run: ## Lima|Provision the VM and attach with forwarding
	$(MAKE) lima-up LIMA_INSTANCE='$(LIMA_INSTANCE)' LIMA_SSH_PORT='$(LIMA_SSH_PORT)'
	$(MAKE) attach TARGET='lima-$(LIMA_INSTANCE)' ARGS='$(ARGS)'

lima-shell: ## Lima|Open a shell in the test VM
	HERDR_FWD_LIMA_INSTANCE=$(LIMA_INSTANCE) scripts/test-server.sh shell

lima-status: ## Lima|Show the test VM state
	HERDR_FWD_LIMA_INSTANCE=$(LIMA_INSTANCE) scripts/test-server.sh status

lima-down: ## Lima|Stop the test VM and preserve its disk
	HERDR_FWD_LIMA_INSTANCE=$(LIMA_INSTANCE) scripts/test-server.sh down

lima-destroy: ## Lima|Permanently delete the test VM
	HERDR_FWD_LIMA_INSTANCE=$(LIMA_INSTANCE) scripts/test-server.sh destroy
