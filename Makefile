HOST   ?= omicron
# ARCH is x86_64 or aarch64. aarch64 needs cargo-zigbuild:
#   make BUILD=zigbuild ARCH=aarch64 build
# `deploy` defaults ARCH to the remote `uname -m` when not given.
ARCH   ?= x86_64
BUILD  ?= build
# AGENT=0 builds without the technical agent and passes --no-agent to install.sh.
AGENT  ?= 1
FEATURES := $(if $(filter 0,$(AGENT)),--no-default-features,)
# Extra install.sh flags, e.g. INSTALL_ARGS="--yes --shell fish".
INSTALL_ARGS ?=
ALL_ARGS := $(INSTALL_ARGS) $(if $(filter 0,$(AGENT)),--no-agent,)

ifeq ($(MAKECMDGOALS),deploy)
ifneq ($(origin ARCH),command line)
ARCH := $(shell ssh $(HOST) uname -m)
endif
endif

TARGET = $(ARCH)-unknown-linux-musl
BIN    = target/$(TARGET)/release/muthur

SHELLCHECK_FILES := install.sh deploy/login/muthur-login deploy/login/hook.sh tests/install/run.sh $(wildcard tests/install/stubs/*)

.PHONY: build deploy install check-deploy check

build:
	cargo $(BUILD) --release --target $(TARGET) $(FEATURES)

# The remote login shell may be fish, so the remote side is plain argv for sh.
# $$tmp is expanded by the local shell; the trap removes it even on failure.
deploy: build
	tmp=$$(ssh $(HOST) mktemp -d) && \
	trap 'ssh $(HOST) rm -rf '"$$tmp" EXIT && \
	scp $(BIN) $(HOST):$$tmp/muthur && \
	scp -r install.sh deploy $(HOST):$$tmp/ && \
	ssh $(HOST) mkdir -p $$tmp/docs && \
	scp docs/config.example.toml $(HOST):$$tmp/docs/config.example.toml && \
	ssh -t $(HOST) sh $$tmp/install.sh --bin $$tmp/muthur $(ALL_ARGS)

install: build
	./install.sh --bin $(BIN) $(ALL_ARGS)

check-deploy:
	cargo test --test deploy
	if command -v shellcheck >/dev/null 2>&1; then shellcheck -s sh $(SHELLCHECK_FILES); else echo 'shellcheck unavailable, skipping'; fi
	if command -v fish >/dev/null 2>&1; then fish -n deploy/login/muthur.fish; else echo 'fish unavailable, skipping'; fi

# Lints and tests in both feature modes, plus the installer tests.
check:
	cargo clippy --all-targets -- -D warnings
	cargo test
	cargo clippy --all-targets --no-default-features -- -D warnings
	cargo test --no-default-features
	sh tests/install/run.sh
