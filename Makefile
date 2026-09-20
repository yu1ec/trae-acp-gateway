# Trae ACP Gateway — build automation
APP_NAME   := Trae ACP Gateway
APP_DIR    := app
TARGET_DIR := target
CARGO      ?= cargo

# OS: macos | linux | windows
# ARCH: arm64 | x86_64 | universal (macos only)
HOST_OS := $(shell uname -s | tr '[:upper:]' '[:lower:]')
ifeq ($(HOST_OS),darwin)
  HOST_OS := macos
endif
ifneq ($(findstring mingw,$(HOST_OS)),)
  HOST_OS := windows
endif
ifneq ($(findstring msys,$(HOST_OS)),)
  HOST_OS := windows
endif

HOST_ARCH_RAW := $(shell uname -m)
ifeq ($(HOST_ARCH_RAW),arm64)
  HOST_ARCH := arm64
else ifeq ($(HOST_ARCH_RAW),aarch64)
  HOST_ARCH := arm64
else ifeq ($(HOST_ARCH_RAW),x86_64)
  HOST_ARCH := x86_64
else ifeq ($(HOST_ARCH_RAW),amd64)
  HOST_ARCH := x86_64
else
  HOST_ARCH := $(HOST_ARCH_RAW)
endif

OS   ?= $(HOST_OS)
ARCH ?= $(HOST_ARCH)

# SIGN=0 (default) passes --no-sign to tauri build
SIGN ?= 0

# INIT_TARGETS=all adds all common rustup targets during init
INIT_TARGETS ?= current

.PHONY: help init init-rust init-tauri init-frontend build-cli build-frontend build-app dev-app run-cli bump-version check-version clean

.DEFAULT_GOAL := help

help:
	@echo "Trae ACP Gateway — Makefile targets"
	@echo ""
	@echo "Targets:"
	@echo "  init        Fetch deps, install tauri-cli, npm packages, add rustup targets"
	@echo "  build-cli   Build CLI binary (trae_acp_gateway)"
	@echo "  build-frontend  Build React UI to app/ui/"
	@echo "  build-app   Build desktop app ($(APP_NAME)) installers"
	@echo "  dev-app     Run Tauri dev server (hot reload)"
	@echo "  run-cli     Build (if needed) and run CLI gateway"
	@echo "  bump-version  Sync version across Cargo/Tauri/frontend files"
	@echo "  check-version Validate version matches tag (CI check)"
	@echo "  clean       Remove build artifacts (cargo clean)"
	@echo ""
	@echo "Variables:"
	@echo "  OS          Target OS: macos | linux | windows  (default: $(HOST_OS))"
	@echo "  ARCH        Target arch: arm64 | x86_64 | universal  (default: $(HOST_ARCH))"
	@echo "  BUNDLES     Installer types (default per OS)"
	@echo "  SIGN        1=enable signing, 0=--no-sign  (default: 0)"
	@echo "  INIT_TARGETS  current | all  (default: current)"
	@echo "  VERSION     Target version for bump-version (e.g. 0.1.2)"
	@echo "  TAG         Tag version for check-version (default: current git tag)"
	@echo ""
	@echo "Examples:"
	@echo "  make init"
	@echo "  make build-cli"
	@echo "  make build-cli OS=linux ARCH=arm64"
	@echo "  make build-app"
	@echo "  make build-app OS=macos ARCH=universal BUNDLES=app,dmg"
	@echo "  make build-app OS=windows ARCH=x86_64 BUNDLES=msi"
	@echo "  make dev-app"
	@echo "  make run-cli"
	@echo "  make bump-version VERSION=0.1.2"
	@echo "  make check-version TAG=0.1.2"
	@echo ""
	@echo "Note: Tauri app builds must run on the target OS (macOS universal is an exception)."

init: init-rust init-tauri init-frontend
	@echo ""
	@echo "==> System dependencies (install manually if missing):"
	@case "$(HOST_OS)" in \
		macos)  echo "    macOS: Xcode Command Line Tools" ;; \
		linux)  echo "    Linux: webkit2gtk, libayatana-appindicator, etc. (see Tauri docs)" ;; \
		*)      echo "    Windows: WebView2, Visual Studio C++ Build Tools" ;; \
	esac

init-rust:
	@echo "==> Fetching Rust dependencies..."
	$(CARGO) fetch
	@echo "==> Adding rustup targets..."
	@targets=""; \
	if [ "$(INIT_TARGETS)" = "all" ]; then \
		targets="aarch64-apple-darwin x86_64-apple-darwin \
		         aarch64-unknown-linux-gnu x86_64-unknown-linux-gnu \
		         x86_64-pc-windows-msvc"; \
	else \
		case "$(OS)" in \
			macos) \
				case "$(ARCH)" in \
					arm64)     targets="aarch64-apple-darwin" ;; \
					x86_64)    targets="x86_64-apple-darwin" ;; \
					universal) targets="aarch64-apple-darwin x86_64-apple-darwin" ;; \
				esac ;; \
			linux) \
				case "$(ARCH)" in \
					arm64)  targets="aarch64-unknown-linux-gnu" ;; \
					x86_64) targets="x86_64-unknown-linux-gnu" ;; \
				esac ;; \
			windows) \
				targets="x86_64-pc-windows-msvc" ;; \
		esac; \
	fi; \
	for t in $$targets; do \
		echo "    rustup target add $$t"; \
		rustup target add $$t; \
	done

init-tauri:
	@echo "==> Checking tauri-cli..."
	@if $(CARGO) tauri --version >/dev/null 2>&1; then \
		echo "    tauri-cli already installed: $$($(CARGO) tauri --version)"; \
	else \
		echo "    Installing tauri-cli..."; \
		$(CARGO) install tauri-cli --locked; \
	fi

init-frontend:
	@echo "==> Installing frontend dependencies..."
	cd $(APP_DIR)/frontend && npm install

build-frontend:
	@echo "==> Building frontend..."
	cd $(APP_DIR)/frontend && npm run build

build-cli:
	@triple=$$( $(MAKE) -s print-triple OS=$(OS) ARCH=$(ARCH) ); \
	native=$$( $(MAKE) -s is-native-triple TRIPLE=$$triple ); \
	echo "==> Building CLI for $(OS)/$(ARCH) ($$triple)..."; \
	if [ "$(OS)" = "macos" ] && [ "$(ARCH)" = "universal" ]; then \
		$(CARGO) build --release --target aarch64-apple-darwin; \
		$(CARGO) build --release --target x86_64-apple-darwin; \
		mkdir -p $(TARGET_DIR)/universal-apple-darwin/release; \
		lipo -create \
			$(TARGET_DIR)/aarch64-apple-darwin/release/trae_acp_gateway \
			$(TARGET_DIR)/x86_64-apple-darwin/release/trae_acp_gateway \
			-output $(TARGET_DIR)/universal-apple-darwin/release/trae_acp_gateway; \
		echo "==> Output: $(TARGET_DIR)/universal-apple-darwin/release/trae_acp_gateway"; \
	elif [ "$$native" = "1" ]; then \
		$(CARGO) build --release; \
		echo "==> Output: $(TARGET_DIR)/release/trae_acp_gateway"; \
	else \
		$(CARGO) build --release --target $$triple; \
		echo "==> Output: $(TARGET_DIR)/$$triple/release/trae_acp_gateway"; \
	fi

build-app: check-app-host
	@triple=$$( $(MAKE) -s print-triple OS=$(OS) ARCH=$(ARCH) ); \
	bundles=$$( $(MAKE) -s print-bundles OS=$(OS) ); \
	if [ -n "$(BUNDLES)" ]; then bundles="$(BUNDLES)"; fi; \
	sign_args=""; \
	if [ "$(SIGN)" != "1" ]; then sign_args="--no-sign"; fi; \
	echo "==> Building $(APP_NAME) for $(OS)/$(ARCH) ($$triple)"; \
	echo "    bundles: $$bundles"; \
	cd $(APP_DIR) && $(CARGO) tauri build --target $$triple --bundles $$bundles $$sign_args; \
	bundle_root="$(CURDIR)/$(TARGET_DIR)/$$triple/release/bundle"; \
	echo ""; \
	echo "==> Build complete. Artifacts:"; \
	if [ -d "$$bundle_root" ]; then \
		find "$$bundle_root" \( \
			-type f \( -name "*.dmg" -o -name "*.deb" -o -name "*.AppImage" -o \
			          -name "*.msi" -o -name "*-setup.exe" \) \
			-o -type d -name "*.app" \
		\) 2>/dev/null | sort | sed 's/^/    /'; \
	else \
		echo "    (bundle directory not found: $$bundle_root)"; \
	fi

dev-app:
	cd $(APP_DIR) && $(CARGO) tauri dev

run-cli:
	@./run.sh

bump-version:
	@test -n "$(VERSION)" || (echo "Usage: make bump-version VERSION=0.1.2" >&2; exit 1)
	@./scripts/bump-version.sh "$(VERSION)"

check-version:
	@./scripts/check-version.sh "$(TAG)"

clean:
	$(CARGO) clean

# --- internal helpers (not .PHONY — safe to call via make -s) ---

print-triple:
	@case "$(OS)" in \
		macos) \
			case "$(ARCH)" in \
				arm64)     echo "aarch64-apple-darwin" ;; \
				x86_64)    echo "x86_64-apple-darwin" ;; \
				universal) echo "universal-apple-darwin" ;; \
				*) echo "ERROR: unsupported ARCH=$(ARCH) for OS=macos (use arm64, x86_64, or universal)" >&2; exit 1 ;; \
			esac ;; \
		linux) \
			case "$(ARCH)" in \
				arm64)  echo "aarch64-unknown-linux-gnu" ;; \
				x86_64) echo "x86_64-unknown-linux-gnu" ;; \
				*) echo "ERROR: unsupported ARCH=$(ARCH) for OS=linux (use arm64 or x86_64)" >&2; exit 1 ;; \
			esac ;; \
		windows) \
			case "$(ARCH)" in \
				x86_64) echo "x86_64-pc-windows-msvc" ;; \
				*) echo "ERROR: unsupported ARCH=$(ARCH) for OS=windows (use x86_64)" >&2; exit 1 ;; \
			esac ;; \
		*) echo "ERROR: unsupported OS=$(OS) (use macos, linux, or windows)" >&2; exit 1 ;; \
	esac

print-bundles:
	@case "$(OS)" in \
		macos)   echo "app,dmg" ;; \
		linux)   echo "deb,appimage" ;; \
		windows) echo "msi,nsis" ;; \
		*)       echo "app" ;; \
	esac

is-native-triple:
	@host_triple=""; \
	case "$(HOST_OS)" in \
		macos) \
			case "$(HOST_ARCH)" in \
				arm64)  host_triple="aarch64-apple-darwin" ;; \
				x86_64) host_triple="x86_64-apple-darwin" ;; \
			esac ;; \
		linux) \
			case "$(HOST_ARCH)" in \
				arm64)  host_triple="aarch64-unknown-linux-gnu" ;; \
				x86_64) host_triple="x86_64-unknown-linux-gnu" ;; \
			esac ;; \
		*) \
			case "$(HOST_ARCH)" in \
				x86_64) host_triple="x86_64-pc-windows-msvc" ;; \
			esac ;; \
	esac; \
	if [ "$(TRIPLE)" = "$$host_triple" ]; then echo "1"; else echo "0"; fi

check-app-host:
	@case "$(OS)" in \
		macos) \
			if [ "$(HOST_OS)" != "macos" ]; then \
				echo "ERROR: build-app OS=macos requires a macOS host." >&2; exit 1; \
			fi ;; \
		linux) \
			if [ "$(HOST_OS)" != "linux" ]; then \
				echo "ERROR: build-app OS=linux requires a Linux host." >&2; exit 1; \
			fi ;; \
		windows) \
			if [ "$(HOST_OS)" != "windows" ]; then \
				echo "ERROR: build-app OS=windows requires a Windows host." >&2; exit 1; \
			fi ;; \
		*) \
			echo "ERROR: unsupported OS=$(OS)" >&2; exit 1 ;; \
	esac
