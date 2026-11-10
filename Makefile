# LanShare build commands. Run `make` to list them.
#
# Installers are built on their own operating system, except that Linux can cross-build the Windows NSIS installer (run
# `make deps-windows-cross` once first). To get all three at once, push a `v*` tag and let
# .github/workflows/release.yml build them.
#
# Windows needs GNU make and Git Bash on the PATH.

SHELL := bash

ifeq ($(OS),Windows_NT)
  HOST := windows
else ifeq ($(shell uname -s),Darwin)
  HOST := mac
else
  HOST := linux
endif

# Signs the update bundles. `TAURI_SIGNING_PRIVATE_KEY` in the environment takes priority over this file.
UPDATER_KEY ?= $$HOME/.tauri/lanshare-updater.key
SIGNED = TAURI_SIGNING_PRIVATE_KEY="$${TAURI_SIGNING_PRIVATE_KEY:-$$(cat "$(UPDATER_KEY)")}"
export TAURI_SIGNING_PRIVATE_KEY_PASSWORD ?=

NODE_MODULES := app/node_modules/.package-lock.json

.DEFAULT_GOAL := help
.PHONY: help install deps-linux dev build fmt lint test check clean \
	release release-mac release-linux release-windows signing-key \
	deps-windows-cross windows-cross-tools

help:
	@echo "Development (this computer is: $(HOST))"
	@echo "  make install          Install frontend packages"
	@echo "  make deps-linux       Install the system libraries Debian and Ubuntu need"
	@echo "  make deps-windows-cross  Install the tools Linux needs to build the Windows installer"
	@echo "  make dev              Run the desktop app with live reload"
	@echo "  make build            Build the CLI and the app, without installers"
	@echo "  make fmt              Format Rust and frontend code"
	@echo "  make lint             Run clippy and the frontend lint and type checks"
	@echo "  make test             Run all Rust and frontend tests"
	@echo "  make check            Run every check that CI runs"
	@echo "  make clean            Delete build output"
	@echo
	@echo "Release"
	@echo "  make release          Installers for this computer's operating system"
	@echo "  make release-mac      Universal .app and .dmg (macOS only)"
	@echo "  make release-linux    .deb and .AppImage (Linux only)"
	@echo "  make release-windows  .exe and .msi on Windows; .exe only when cross-built on Linux"

$(NODE_MODULES): app/package-lock.json
	cd app && npm ci
	@touch $@

install: $(NODE_MODULES)

# `lanshare-app` embeds app/dist, so it must exist before anything compiles that crate.
app/dist/index.html: $(NODE_MODULES)
	cd app && npm run build

deps-linux:
	sudo apt-get update
	sudo apt-get install -y build-essential curl wget file patchelf libssl-dev libxdo-dev \
		libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev

# The MSVC CRT and Windows SDK headers are downloaded by cargo-xwin on its first build, which accepts Microsoft's license.
deps-windows-cross:
	sudo apt-get update
	sudo apt-get install -y nsis lld llvm clang
	rustup target add x86_64-pc-windows-msvc
	cargo install --locked cargo-xwin

dev: $(NODE_MODULES)
	cd app && npx tauri dev

build: $(NODE_MODULES)
	cargo build --release -p lanshare-cli
	cd app && npx tauri build --no-bundle

fmt: $(NODE_MODULES)
	cargo +nightly fmt --all
	cd app && npm run format

lint: app/dist/index.html
	cargo clippy --workspace --all-targets --all-features -- -D warnings
	cd app && npm run lint && npm run check

test: app/dist/index.html
	cargo test --workspace
	cd app && npm run test

check: app/dist/index.html
	cargo +nightly fmt --all -- --check
	cd app && npm run format:check
	$(MAKE) lint test

clean:
	cargo clean
	rm -rf app/dist

release: release-$(HOST)

release-mac: on-mac signing-key $(NODE_MODULES)
	rustup target add aarch64-apple-darwin x86_64-apple-darwin
	cd app && $(SIGNED) npx tauri build \
		--target universal-apple-darwin --bundles app,dmg
	@echo "Installers: target/universal-apple-darwin/release/bundle"

release-linux: on-linux signing-key $(NODE_MODULES)
	cd app && $(SIGNED) npx tauri build --bundles deb,appimage
	@echo "Installers: target/release/bundle"

ifeq ($(HOST),linux)
# WiX only runs on Windows, so a Linux cross-build makes the NSIS installer but no .msi.
release-windows: windows-cross-tools signing-key $(NODE_MODULES)
	cd app && $(SIGNED) npx tauri build --runner cargo-xwin --target x86_64-pc-windows-msvc --bundles nsis
	@echo "Installer: target/x86_64-pc-windows-msvc/release/bundle/nsis"
else
release-windows: on-windows signing-key $(NODE_MODULES)
	cd app && $(SIGNED) npx tauri build --bundles nsis,msi
	@echo "Installers: target/release/bundle"
endif

on-%:
	@[ "$(HOST)" = $* ] || { echo "This target needs $*, but this computer is $(HOST). Push a v* tag to build all three." >&2; exit 1; }

windows-cross-tools:
	@missing=; for t in makensis clang lld-link llvm-rc cargo-xwin; do command -v $$t >/dev/null || missing="$$missing $$t"; done; \
	rustup target list --installed | grep -qx x86_64-pc-windows-msvc || missing="$$missing x86_64-pc-windows-msvc"; \
	[ -z "$$missing" ] || { echo "Missing:$$missing. Run make deps-windows-cross." >&2; exit 1; }

signing-key:
	@[ -n "$$TAURI_SIGNING_PRIVATE_KEY" ] || [ -f "$(UPDATER_KEY)" ] || { \
		echo "No updater key at $(UPDATER_KEY). Set TAURI_SIGNING_PRIVATE_KEY or UPDATER_KEY." >&2; exit 1; }
