HOST   ?= omicron
TARGET := x86_64-unknown-linux-musl
BIN    := target/$(TARGET)/release/muthur
# AGENT=0 builds without the technical agent (`--no-default-features`).
AGENT  ?= 1
FEATURES := $(if $(filter 0,$(AGENT)),--no-default-features,)

UDEV   := deploy/udev/99-wy60.rules
DROPIN := deploy/systemd/serial-getty@wy60.service.d/override.conf
SYSUSERS := deploy/sysusers.d/muthur-agent.conf
TMPFILES := deploy/tmpfiles.d/muthur-agent.conf
SUDOERS  := deploy/sudoers.d/muthur-agent
NFT      := deploy/nftables/muthur-agent.nft

# Agent account files (skipped with AGENT=0). sysusers and tmpfiles share a
# basename, so they are staged under distinct names. OPERATOR is replaced on
# the host with the user the ssh session logs in as. The reminder text avoids
# characters the nested quoting would eat.
ifeq ($(AGENT),0)
AGENT_SCP :=
AGENT_STEPS :=
else
AGENT_SCP := scp $(SYSUSERS) $(HOST):$$tmp/sysusers.conf && scp $(TMPFILES) $(HOST):$$tmp/tmpfiles.conf && scp $(SUDOERS) $(HOST):$$tmp/sudoers && scp $(NFT) $(HOST):$$tmp/agent.nft &&
AGENT_STEPS := \
	echo \"==> agent account\"; sudo install -d -m 755 /etc/sysusers.d /etc/tmpfiles.d; sudo install -m 644 $$tmp/sysusers.conf /etc/sysusers.d/muthur-agent.conf; sudo install -m 644 $$tmp/tmpfiles.conf /etc/tmpfiles.d/muthur-agent.conf; sudo systemd-sysusers; sudo systemd-tmpfiles --create; \
	echo \"==> agent sudoers\"; if [ \"\$$(whoami)\" = root ]; then echo \"ABORT: deploy as a normal user, not root\"; exit 1; fi; sed \"s/OPERATOR/\$$(whoami)/\" $$tmp/sudoers > $$tmp/sudoers.out; sudo visudo -cf $$tmp/sudoers.out; sudo install -m 440 $$tmp/sudoers.out /etc/sudoers.d/muthur-agent; \
	echo \"==> agent network isolation\"; sudo install -d /etc/nftables.d; sudo install -m 644 $$tmp/agent.nft /etc/nftables.d/muthur-agent.nft; sudo nft -f /etc/nftables.d/muthur-agent.nft; \
	echo \"==> REMINDER: include /etc/nftables.d/*.nft from /etc/nftables.conf so the rule survives a reboot\";
endif

.PHONY: build deploy install-system check-deploy check

build:
	cargo build --release --target $(TARGET) $(FEATURES)

deploy: build
	ssh $(HOST) 'mkdir -p ~/.local/bin ~/.config/fish/conf.d'
	scp $(BIN) $(HOST):.local/bin/muthur.new
	ssh $(HOST) "bash -c 'cd ~/.local/bin && (mv -f muthur muthur.old 2>/dev/null || true) && mv muthur.new muthur'"
	scp deploy/fish/muthur.fish $(HOST):.config/fish/conf.d/muthur.fish

# The remote login shell is fish, so the script runs under bash -c. $$tmp is
# expanded locally (inside the double quotes); the script has no other $.
install-system:
	tmp=$$(ssh $(HOST) mktemp -d) && \
	scp $(UDEV) $(DROPIN) $(HOST):$$tmp/ && \
	$(AGENT_SCP) \
	ssh -t $(HOST) "bash -c 'set -e; \
	echo \"==> install udev rule\"; sudo install -d -m 755 /etc/udev/rules.d; sudo install -m 644 $$tmp/99-wy60.rules /etc/udev/rules.d/99-wy60.rules; \
	echo \"==> install getty drop-in\"; sudo install -d /etc/systemd/system/serial-getty@wy60.service.d; sudo install -m 644 $$tmp/override.conf /etc/systemd/system/serial-getty@wy60.service.d/override.conf; \
	echo \"==> reload systemd\"; sudo systemctl daemon-reload; \
	echo \"==> reload udev\"; sudo udevadm control --reload; \
	echo \"==> migrate old getty\"; if systemctl is-enabled --quiet serial-getty@wy60; then sudo systemctl disable serial-getty@wy60; fi; \
	echo \"==> disable old getty\"; if systemctl is-enabled --quiet serial-getty@ttyUSB0; then sudo systemctl disable --now serial-getty@ttyUSB0; fi; \
	echo \"==> trigger udev (starts the getty)\"; sudo udevadm trigger --subsystem-match=tty --action=add; \
	$(AGENT_STEPS) \
	echo \"==> clean up\"; rm -rf $$tmp'"

# Local checks. udevadm verify needs udev >= 255, hence the guard. The unit is
# the system serial-getty@.service plus the drop-in's [Service] lines, since
# systemd-analyze cannot verify a drop-in alone. Warnings about the base unit
# or the not-yet-existing dev-wy60.device are filtered; the filter is narrow
# and anything else (e.g. a syntax error in the drop-in) fails the target.
check-deploy:
	if udevadm verify --help >/dev/null 2>&1; then udevadm verify $(UDEV); else echo 'udevadm verify unavailable, skipping'; fi
	d=$$(mktemp -d) && trap 'rm -rf $$d' EXIT && \
	{ cat /usr/lib/systemd/system/serial-getty@.service; echo; cat $(DROPIN); } > $$d/serial-getty@wy60.service && \
	out=$$(systemd-analyze verify --man=no $$d/serial-getty@wy60.service 2>&1); \
	bad=$$(printf '%s\n' "$$out" | grep -v -e '^$$' -e 'dev-wy60.device' || true); \
	if [ -n "$$bad" ]; then printf '%s\n' "$$bad"; exit 1; fi
	if command -v visudo >/dev/null 2>&1; then d=$$(mktemp -d) && trap 'rm -rf $$d' EXIT && sed s/OPERATOR/nobody/ $(SUDOERS) > $$d/sudoers && visudo -cf $$d/sudoers; else echo 'visudo unavailable, skipping'; fi
	if command -v nft >/dev/null 2>&1; then out=$$(nft -c -f $(NFT) 2>&1) || { case "$$out" in *'Operation not permitted'*|*'Permission denied'*) echo 'nft -c needs privileges, skipping';; *) printf '%s\n' "$$out"; exit 1;; esac; }; else echo 'nft unavailable, skipping'; fi
	if command -v systemd-sysusers >/dev/null 2>&1; then systemd-sysusers --dry-run $(SYSUSERS); else echo 'systemd-sysusers unavailable, skipping'; fi

# Lints and tests in both feature modes.
check:
	cargo clippy --all-targets -- -D warnings
	cargo test
	cargo clippy --all-targets --no-default-features -- -D warnings
	cargo test --no-default-features
