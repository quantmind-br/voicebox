# Voicebox — build and install.
#
# The justfile remains the day-to-day development entry point (`just dev`,
# `just test`, `just lint`). This Makefile covers the other half: producing a
# release build and installing it into a filesystem, with the DESTDIR/PREFIX
# conventions distro packaging expects.
#
# It is also the single definition of the install layout. packaging/arch's
# PKGBUILD calls `make install DESTDIR=...` rather than re-deriving the same
# paths, so the two cannot drift apart.
#
#   make build              release binary + frozen Python sidecars
#   sudo make install       install into /usr/local
#   sudo make uninstall     remove it again
#   make package            build an Arch package instead (pacman-tracked)
#
# Overridable:
#   PREFIX=/usr             install root (packaging uses /usr)
#   DESTDIR=/tmp/stage      staging root, prepended to every path

PREFIX      ?= /usr/local
DESTDIR     ?=
BINDIR      ?= $(PREFIX)/bin
LIBDIR      ?= $(PREFIX)/lib
DATADIR     ?= $(PREFIX)/share
# udev only ever reads /usr/lib and /etc, never /usr/local — so this one path
# deliberately ignores PREFIX. A rule installed under /usr/local/lib/udev
# would be silently inert.
UDEVDIR     ?= /usr/lib/udev/rules.d

APPDIR       = $(LIBDIR)/voicebox

# Tauri names sidecars `<name>-<target triple>` on disk and strips the triple
# when it bundles. Resolving the triple from rustc keeps this working on
# aarch64 without a second code path.
TRIPLE      := $(shell rustc --print host-tuple 2>/dev/null || echo x86_64-unknown-linux-gnu)

TAURI_DIR    = tauri/src-tauri
BIN_SRC      = $(TAURI_DIR)/target/release/voicebox
SIDECAR_DIR  = $(TAURI_DIR)/binaries
SERVER_SRC   = $(SIDECAR_DIR)/voicebox-server-$(TRIPLE)
MCP_SRC      = $(SIDECAR_DIR)/voicebox-mcp-$(TRIPLE)
ICON_DIR     = $(TAURI_DIR)/icons
PACKAGING    = packaging/arch

VENV         = backend/venv
# 3.14 has no wheels for the pinned numpy<2 / numba<0.61, so prefer 3.12.
SYSTEM_PY   := $(shell command -v python3.12 || command -v python3.13 || command -v python3)

INSTALL      = install
# Icon sizes that exist in the source tree and are meaningful to hicolor.
ICON_SIZES   = 32x32 64x64 128x128

.DEFAULT_GOAL := help
.PHONY: help setup build build-server build-app install uninstall package dev test clean distclean

help: ## Show this help
	@echo "Voicebox — make targets:"
	@echo
	@grep -hE '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) \
		| awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-14s\033[0m %s\n", $$1, $$2}'
	@echo
	@echo "  PREFIX=$(PREFIX)  DESTDIR=$(DESTDIR)  TRIPLE=$(TRIPLE)"

# ─── Setup ────────────────────────────────────────────────────────────

setup: $(VENV) ## Install Python and JavaScript dependencies
	bun install

$(VENV):
	@echo "==> Creating the Python environment with $(SYSTEM_PY)"
	$(SYSTEM_PY) -m venv $(VENV)
	$(VENV)/bin/pip install --upgrade pip -q
	@# GPU wheels first, so the generic requirements install does not pull a
	@# CPU-only torch and win. NVIDIA is checked before AMD: a machine with a
	@# discrete NVIDIA card and an AMD iGPU has both /proc/driver/nvidia and
	@# /dev/kfd, and the discrete card is the one worth using.
	@if [ -e /proc/driver/nvidia/version ]; then \
		echo "==> NVIDIA driver detected — installing CUDA PyTorch"; \
		$(VENV)/bin/pip install torch torchaudio --index-url https://download.pytorch.org/whl/cu128; \
	elif [ -e /dev/kfd ]; then \
		echo "==> AMD compute node detected — installing ROCm PyTorch"; \
		$(VENV)/bin/pip install torch torchaudio --index-url "https://download.pytorch.org/whl/rocm$${VOICEBOX_ROCM_VERSION:-6.3}"; \
	fi
	$(VENV)/bin/pip install -r backend/requirements.txt
	@# Both pin dependency ranges that conflict with the rest of the tree.
	$(VENV)/bin/pip install --no-deps chatterbox-tts
	$(VENV)/bin/pip install --no-deps hume-tada
	$(VENV)/bin/pip install git+https://github.com/QwenLM/Qwen3-TTS.git
	$(VENV)/bin/pip install pyinstaller -q

# ─── Build ────────────────────────────────────────────────────────────

build: build-server build-app ## Build everything (sidecars + desktop app)

build-server: $(SERVER_SRC) ## Freeze the Python backend into sidecars

$(SERVER_SRC) $(MCP_SRC): $(VENV) $(shell find backend -name '*.py' -not -path '*/venv/*' 2>/dev/null)
	@echo "==> Freezing the Python sidecars (several GB, this takes a while)"
	./scripts/build-server.sh

build-app: $(BIN_SRC) ## Build the desktop app in release mode

# The sidecars are a hard prerequisite, not a convenience: tauri-build fails
# at configure time if the files named in externalBin are missing.
$(BIN_SRC): $(SERVER_SRC) $(MCP_SRC)
	@echo "==> Building the desktop app"
	bun install --frozen-lockfile
	cd tauri && bun run tauri build --no-bundle

# ─── Install ──────────────────────────────────────────────────────────

install: ## Install into $(DESTDIR)$(PREFIX)
	@test -x "$(BIN_SRC)" || { echo "error: $(BIN_SRC) is missing — run 'make build' first" >&2; exit 1; }
	@test -x "$(SERVER_SRC)" || { echo "error: $(SERVER_SRC) is missing — run 'make build' first" >&2; exit 1; }
	@echo "==> Installing into $(DESTDIR)$(PREFIX)"
	@# Sidecars must land beside the main binary, and keep their bare names:
	@# that is where Tauri's `sidecar()` and our `mcp_shim_path` both look,
	@# both of which resolve relative to the running executable.
	$(INSTALL) -Dm755 $(BIN_SRC)    $(DESTDIR)$(APPDIR)/voicebox
	$(INSTALL) -Dm755 $(SERVER_SRC) $(DESTDIR)$(APPDIR)/voicebox-server
	$(INSTALL) -Dm755 $(MCP_SRC)    $(DESTDIR)$(APPDIR)/voicebox-mcp
	@# /proc/self/exe resolves the symlink, so the sidecar lookup above still
	@# finds $(APPDIR) when launched through this name.
	$(INSTALL) -d $(DESTDIR)$(BINDIR)
	ln -sf $(APPDIR)/voicebox $(DESTDIR)$(BINDIR)/voicebox
	$(INSTALL) -Dm644 $(PACKAGING)/voicebox.desktop \
		$(DESTDIR)$(DATADIR)/applications/voicebox.desktop
	@for size in $(ICON_SIZES); do \
		$(INSTALL) -Dm644 $(ICON_DIR)/$$size.png \
			$(DESTDIR)$(DATADIR)/icons/hicolor/$$size/apps/voicebox.png; \
	done
	$(INSTALL) -Dm644 $(ICON_DIR)/icon.png \
		$(DESTDIR)$(DATADIR)/icons/hicolor/512x512/apps/voicebox.png
	@# Only the uinput fallback needs this; wtype needs no permissions at all.
	@# Installed either way so the fallback works if wtype is later removed.
	$(INSTALL) -Dm644 $(PACKAGING)/99-voicebox-uinput.rules \
		$(DESTDIR)$(UDEVDIR)/99-voicebox-uinput.rules
	$(INSTALL) -Dm644 LICENSE $(DESTDIR)$(DATADIR)/licenses/voicebox/LICENSE
	@echo "==> Installed. Run 'voicebox', or launch it from your application menu."

uninstall: ## Remove an installation made by `make install`
	rm -rf $(DESTDIR)$(APPDIR)
	rm -f  $(DESTDIR)$(BINDIR)/voicebox
	rm -f  $(DESTDIR)$(DATADIR)/applications/voicebox.desktop
	rm -f  $(DESTDIR)$(DATADIR)/icons/hicolor/*/apps/voicebox.png
	rm -f  $(DESTDIR)$(UDEVDIR)/99-voicebox-uinput.rules
	rm -rf $(DESTDIR)$(DATADIR)/licenses/voicebox
	@echo "==> Removed. User data in ~/.config/sh.voicebox.app was left alone."

package: ## Build an Arch package (pacman-tracked, preferred on Arch)
	cd $(PACKAGING) && makepkg -f

# ─── Development ──────────────────────────────────────────────────────

dev: ## Run backend + desktop app for development
	just dev

test: ## Run the Rust and Python test suites
	cd $(TAURI_DIR) && cargo test --bin voicebox
	just test

clean: ## Remove build output, keeping dependencies
	rm -rf $(TAURI_DIR)/target/release backend/build backend/dist
	rm -f  $(SIDECAR_DIR)/voicebox-server-* $(SIDECAR_DIR)/voicebox-mcp-*
	rm -rf $(PACKAGING)/src $(PACKAGING)/pkg $(PACKAGING)/*.pkg.tar.*

distclean: clean ## Also remove the Python venv and node_modules
	rm -rf $(VENV) node_modules */node_modules
