#!/bin/sh
# shellcheck disable=SC2031,SC2013,SC2329,SC2016,SC2032,SC2033 # tests run in subshells on purpose; test names have no spaces
# Plain-sh test harness for install.sh. Run: sh tests/install/run.sh
set -u
HERE=$(cd -P "$(dirname "$0")" && pwd)
REPO=$(cd -P "$HERE/../.." && pwd)
SELF=$HERE/$(basename "$0")
FAILS=0
if [ -n "${MUTHUR_TEST_SH:-}" ]; then SHELL_BIN=$MUTHUR_TEST_SH
elif command -v dash >/dev/null 2>&1; then SHELL_BIN=dash
else SHELL_BIN=$(command -v sh); fi

# Assertions exit 1 on failure: each test runs in its own process.
eq() { # expected actual msg
	[ "$1" = "$2" ] || { echo "  expected [$1] got [$2] ($3)" >&2; exit 1; }
}
ok() { # cmd... msg (last argument is the message)
	for _msg; do :; done
	_n=$#; _i=1
	for _a; do
		[ "$_i" -lt "$_n" ] && set -- "$@" "$_a"
		_i=$((_i + 1))
	done
	shift "$_n"
	"$@" 2>/dev/null || { echo "  failed: $_msg" >&2; exit 1; }
}
chk() { _m=$1; shift; "$@" || { echo "  check failed: $_m" >&2; exit 1; }; }

# dies pattern cmd... : cmd must exit 1 and stderr must contain pattern
dies() {
	_pat=$1; shift
	_rc=0
	_out=$( ("$@") 2>&1 >/dev/null ) || _rc=$?
	[ "$_rc" -eq 1 ] || { echo "  status $_rc, want 1" >&2; exit 1; }
	case $_out in *"$_pat"*) ;; *) echo "  message [$_out] lacks [$_pat]" >&2; exit 1 ;; esac
}

setup() {
	T=$(mktemp -d)
	HOME=$T/home ROOT=$T/root
	mkdir -p "$HOME" "$ROOT"
	export HOME ROOT
	PATH=$HERE/stubs:$PATH
	STUBLOG=$T/stub.log; : > "$STUBLOG"
	export PATH STUBLOG MUTHUR_INSTALL_LIB=1 MUTHUR_INSTALL_SRC="$REPO/install.sh"
	cd "$T" || exit 1
	set -eu
	. "$REPO/install.sh"
	set +e
}

BLOCK_OPEN='# >>> muthur >>>'
BLOCK_CLOSE='# <<< muthur <<<'

t_block_insert_empty() {
	printf 'X\n' | block_write "$T/f"
	eq "$(printf '%s\nX\n%s\n_' "$BLOCK_OPEN" "$BLOCK_CLOSE")" "$(cat "$T/f"; echo _)" content
}
t_block_idempotent() {
	printf 'a\n' > "$T/f"
	printf 'X\n' | block_write "$T/f"
	cp "$T/f" "$T/g"
	printf 'X\n' | block_write "$T/f"
	chk identical cmp "$T/f" "$T/g"
	eq 1 "$(grep -c '^# >>> muthur >>>$' "$T/f")" pairs
}
t_block_replaces_stale() {
	printf 'a\n%s\nOLD\n%s\nb\n' "$BLOCK_OPEN" "$BLOCK_CLOSE" > "$T/f"
	printf 'NEW\n' | block_write "$T/f"
	printf 'a\n%s\nNEW\n%s\nb\n' "$BLOCK_OPEN" "$BLOCK_CLOSE" > "$T/want"
	chk content cmp "$T/f" "$T/want"
}
t_block_remove_exact() {
	printf 'a\n%s\nX\n%s\nb\n' "$BLOCK_OPEN" "$BLOCK_CLOSE" > "$T/f"
	block_remove "$T/f"
	printf 'a\nb\n' > "$T/want"
	chk content cmp "$T/f" "$T/want"
}
t_block_no_trailing_newline() {
	printf 'a' > "$T/f"
	printf 'X\n' | block_write "$T/f"
	printf 'a\n%s\nX\n%s\n' "$BLOCK_OPEN" "$BLOCK_CLOSE" > "$T/want"
	chk written cmp "$T/f" "$T/want"
	block_remove "$T/f"
	printf 'a\n' > "$T/want"
	chk removed cmp "$T/f" "$T/want"
}
t_block_follows_symlink() {
	printf 'a\n' > "$T/real"
	ln -s real "$T/f"
	printf 'X\n' | block_write "$T/f"
	chk symlink test -L "$T/f"
	chk block grep -q '^X$' "$T/real"
	block_remove "$T/f"
	chk symlink-after test -L "$T/f"
	eq a "$(cat "$T/real")" removed
}
t_block_keeps_mode() {
	printf 'a\n' > "$T/f"; chmod 600 "$T/f"
	printf 'X\n' | block_write "$T/f"
	eq 600 "$(stat -c %a "$T/f")" write-mode
	chmod 400 "$T/f"
	printf 'Y\n' | block_write "$T/f"
	eq 400 "$(stat -c %a "$T/f")" ro-mode
	chk ro-content grep -q '^Y$' "$T/f"
	block_remove "$T/f"
	eq 400 "$(stat -c %a "$T/f")" ro-remove-mode
}
t_block_new_file_mode() {
	printf 'X\n' | block_write "$T/sub/dir/f"
	eq 644 "$(stat -c %a "$T/sub/dir/f")" mode
}
t_render_ok() {
	render "$REPO/deploy/udev/99-muthur-tty.rules.in" "$T/out" MATCH='ENV{ID_VENDOR_ID}=="1"'
	if grep -Eq '@[A-Z_]+@' "$T/out"; then echo "  leftover @" >&2; return 1; fi
	chk match grep -q 'ID_VENDOR_ID' "$T/out"
}
t_render_leftover() {
	dies MATCH render "$REPO/deploy/udev/99-muthur-tty.rules.in" "$T/out"
}
t_preflight_root() {
	STUB_UID=0; export STUB_UID
	dies "not as root" preflight
}
t_preflight_no_systemd() {
	SYSTEMD_RUN=$HOME/nope
	dies manual-install preflight
}
t_script_dir_independent_of_cwd() {
	( cd / && . "$REPO/install.sh" && eq "$REPO" "$SCRIPT_DIR" dir && [ -f "$SCRIPT_DIR/deploy/login/muthur-login" ] )
}
t_ask_no_tty_names_flag() {
	YES=0
	dies --shell ask "Shell?" "" shell </dev/null
}
t_ask_yes_default() {
	YES=1
	eq zsh "$(ask 'Shell?' zsh shell)" default
}
t_ask_reads_stdin() {
	YES=0 MUTHUR_ASSUME_TTY=1
	eq fish "$(echo fish | ask 'Shell?' zsh shell)" answer
	eq zsh "$(echo | ask 'Shell?' zsh shell)" empty-default
}
bw_n() { printf "N\n" | block_write "$1"; }
t_block_unbalanced() {
	printf 'a\n%s\nkeep1\nkeep2\n' "$BLOCK_OPEN" > "$T/f"
	cp "$T/f" "$T/orig"
	dies unbalanced block_remove "$T/f"
	chk untouched1 cmp "$T/f" "$T/orig"
	dies unbalanced bw_n "$T/f"
	chk untouched2 cmp "$T/f" "$T/orig"
}
t_block_failed_producer_keeps_file() {
	printf 'a\n' > "$T/f"; cp "$T/f" "$T/orig"
	cat() { return 1; }
	dies cannot block_write "$T/f" </dev/null
	unset -f cat
	chk untouched cmp "$T/f" "$T/orig"
	for _f in "$T"/*muthur.tmp*; do [ ! -e "$_f" ] || { echo "  leftover $_f" >&2; exit 1; }; done
}
t_confirm_no_tty_dies() {
	YES=0
	_rc=0
	( if confirm "Go?" </dev/null; then exit 0; fi; exit 3 ) 2>/dev/null || _rc=$?
	eq 1 "$_rc" "confirm must die, not answer yes or no"
}
t_render_missing_template() {
	dies "missing template" render "$T/nope.in" "$T/out"
	chk noout test ! -e "$T/out"
}
t_script_dir_from_dollar0() {
	_d=$(cd / && MUTHUR_INSTALL_SRC='' MUTHUR_INSTALL_LIB=1 sh -c '. "$0"; printf %s "$SCRIPT_DIR"' "$REPO/install.sh")
	eq "$REPO" "$_d" dollar0
	_h=$(cd / && MUTHUR_INSTALL_LIB='' sh "$REPO/install.sh" --help | head -1)
	eq "Usage: install.sh [options]" "$_h" help
}
t_parse_args_unknown() {
	dies bogus parse_args --bogus
	parse_args --shell zsh --no-agent
	eq zsh "$OPT_SHELL" shell
	eq 0 "$OPT_AGENT" agent
}

FX=$REPO/tests/install/fixtures
mkdev() { mkdir -p "$ROOT/dev"; : > "$ROOT/dev/$1"; }
t_parse_ftdi() {
	parse_udev_props < "$FX/udev-ftdi.txt"
	eq 0403 "$ADP_VID" vid; eq 6001 "$ADP_PID" pid; eq A9E91BA3 "$ADP_SERIAL" serial
	case $ADP_BYID in /dev/serial/by-id/*) ;; *) echo "  byid [$ADP_BYID]" >&2; exit 1 ;; esac
	eq 'ATTRS{idVendor}=="0403", ATTRS{idProduct}=="6001", ENV{ID_SERIAL_SHORT}=="A9E91BA3"' "$(match_line)" match
}
t_parse_ch340_no_serial() {
	parse_udev_props < "$FX/udev-ch340.txt"
	eq 'ATTRS{idVendor}=="1a86", ATTRS{idProduct}=="7523", ENV{ID_PATH}=="pci-0000:00:14.0-usb-0:3:1.0"' "$(match_line)" match
	mkdev ttyUSB1; STUB_UDEV_ttyUSB1=$FX/udev-ch340.txt; export STUB_UDEV_ttyUSB1
	YES=1; step_adapter 2>"$T/err" >/dev/null
	chk warns grep -q port "$T/err"
}
t_adapter_none() { dies --vid step_adapter; }
t_adapter_flags_skip_detection() {
	mkdev ttyUSB0
	OPT_VID=0403 OPT_PID=6001 OPT_SERIAL=S; step_adapter >/dev/null
	if grep -q 'udevadm info' "$STUBLOG"; then echo "  detection ran" >&2; exit 1; fi
	eq 'ATTRS{idVendor}=="0403", ATTRS{idProduct}=="6001", ENV{ID_SERIAL_SHORT}=="S"' "$ADAPTER_MATCH" match
}
t_adapter_vid_without_serial_dies() {
	OPT_VID=0403 OPT_PID=6001; dies --serial step_adapter
}
t_adapter_several_yes_dies() {
	mkdev ttyUSB0; mkdev ttyUSB1
	STUB_UDEV_ttyUSB0=$FX/udev-ftdi.txt STUB_UDEV_ttyUSB1=$FX/udev-ch340.txt
	export STUB_UDEV_ttyUSB0 STUB_UDEV_ttyUSB1
	YES=1; dies several step_adapter
}
t_adapter_pick_by_number() {
	mkdev ttyUSB0; mkdev ttyUSB1
	STUB_UDEV_ttyUSB0=$FX/udev-ftdi.txt STUB_UDEV_ttyUSB1=$FX/udev-ch340.txt
	export STUB_UDEV_ttyUSB0 STUB_UDEV_ttyUSB1
	YES=0 MUTHUR_ASSUME_TTY=1
	echo 1 > "$T/in"
	step_adapter < "$T/in" >/dev/null 2>&1
	case $ADAPTER_MATCH in *A9E91BA3*) ;; *) echo "  [$ADAPTER_MATCH]" >&2; exit 1 ;; esac
}
sys_env() {
	mkdir -p "$ROOT/usr/sbin"; : > "$ROOT/usr/sbin/nologin"
	ADAPTER_MATCH='ATTRS{idVendor}=="0403"'
}
RULE=etc/udev/rules.d/99-muthur-tty.rules
DROP=etc/systemd/system/serial-getty@muthur-tty.service.d/override.conf
t_system_installs() {
	sys_env; step_system >/dev/null
	chk rule test -f "$ROOT/$RULE"
	eq 644 "$(stat -c %a "$ROOT/$DROP")" mode
	chk baud grep -q ' 38400 ' "$ROOT/$DROP"
	_a=$(grep -n 'systemctl daemon-reload' "$STUBLOG" | tail -1 | cut -d: -f1)
	_b=$(grep -n 'udevadm control --reload' "$STUBLOG" | tail -1 | cut -d: -f1)
	_c=$(grep -n 'udevadm trigger --subsystem-match=tty --action=add' "$STUBLOG" | tail -1 | cut -d: -f1)
	chk order test -n "$_a" -a "$_a" -lt "$_b" -a "$_b" -lt "$_c"
}
t_system_migrates_wy60() {
	sys_env
	mkdir -p "$ROOT/etc/udev/rules.d" "$ROOT/etc/systemd/system/serial-getty@wy60.service.d"
	: > "$ROOT/etc/udev/rules.d/99-wy60.rules"
	: > "$ROOT/etc/systemd/system/serial-getty@wy60.service.d/override.conf"
	step_system >/dev/null
	chk rule-gone test ! -e "$ROOT/etc/udev/rules.d/99-wy60.rules"
	chk dir-gone test ! -e "$ROOT/etc/systemd/system/serial-getty@wy60.service.d"
	_s=$(grep -n '^systemctl stop serial-getty@wy60' "$STUBLOG" | head -1 | cut -d: -f1)
	_r=$(grep -n '^sudo rm .*wy60' "$STUBLOG" | head -1 | cut -d: -f1)
	chk order test -n "$_s" -a -n "$_r" -a "$_s" -lt "$_r"
}
t_system_disables_ttyusb0() {
	sys_env; step_system >/dev/null
	chk disabled grep -q '^systemctl disable.*serial-getty@ttyUSB0' "$STUBLOG"
	: > "$STUBLOG"; STUB_SYSTEMCTL_IS_ENABLED_RC=1; export STUB_SYSTEMCTL_IS_ENABLED_RC
	step_system >/dev/null
	if grep -q 'systemctl disable' "$STUBLOG"; then echo "  disabled anyway" >&2; exit 1; fi
}
t_system_render_failure_writes_nothing() {
	sys_env; mkdir -p "$T/empty"; SCRIPT_DIR=$T/empty
	( step_system ) >/dev/null 2>&1 && exit 1
	chk none test ! -e "$ROOT/etc"
}
t_agent_files() {
	sys_env
	step_agent >/dev/null 2>&1
	if grep -q '@' "$ROOT/etc/sysusers.d/muthur-agent.conf"; then echo "  @ left" >&2; exit 1; fi
	chk nologin grep -q '/usr/sbin/nologin' "$ROOT/etc/sysusers.d/muthur-agent.conf"
	chk tmpfiles test -f "$ROOT/etc/tmpfiles.d/muthur-agent.conf"
	chk operator grep -q '^tester ALL' "$ROOT/etc/sudoers.d/muthur-agent"
	eq 440 "$(stat -c %a "$ROOT/etc/sudoers.d/muthur-agent")" mode
	chk nft test -f "$ROOT/etc/nftables.d/muthur-agent.nft"
	_v=$(grep -n 'visudo -cf' "$STUBLOG" | head -1 | cut -d: -f1)
	_i=$(grep -n 'install .*sudoers.d' "$STUBLOG" | head -1 | cut -d: -f1)
	chk order test -n "$_v" -a -n "$_i" -a "$_v" -lt "$_i"
	chk nftlog grep -q 'nft -f' "$STUBLOG"
	chk sysusers grep -q 'systemd-sysusers' "$STUBLOG"
	chk tmpfiles-run grep -q 'systemd-tmpfiles --create' "$STUBLOG"
}
t_agent_visudo_failure_installs_nothing() {
	sys_env; STUB_VISUDO_RC=1; export STUB_VISUDO_RC
	( step_agent ) >/dev/null 2>&1 && exit 1
	chk none test ! -e "$ROOT/etc"
}
t_agent_nft_include_once() {
	sys_env; YES=1
	mkdir -p "$ROOT/etc"; printf 'flush ruleset' > "$ROOT/etc/nftables.conf"
	step_agent >/dev/null 2>&1
	step_agent >/dev/null 2>&1
	eq 1 "$(grep -c 'include "/etc/nftables.d/\*.nft"' "$ROOT/etc/nftables.conf")" once
	chk kept grep -q '^flush ruleset$' "$ROOT/etc/nftables.conf"
}
t_agent_nsswitch_warns() {
	sys_env; mkdir -p "$ROOT/etc"; echo 'hosts: files resolve dns' > "$ROOT/etc/nsswitch.conf"
	step_agent 2>&1 >/dev/null | grep -q resolve || { echo "  no warning" >&2; exit 1; }
}
t_rerun_is_noop() {
	sys_env; YES=1
	mkdir -p "$ROOT/etc"; echo 'flush ruleset' > "$ROOT/etc/nftables.conf"
	step_system >/dev/null 2>&1; step_agent >/dev/null 2>&1
	_a=$(cd "$ROOT/etc" && find . -type f -exec cksum {} + | sort)
	step_system >/dev/null 2>&1; step_agent >/dev/null 2>&1
	_b=$(cd "$ROOT/etc" && find . -type f -exec cksum {} + | sort)
	eq "$_a" "$_b" etc
}

t_adapter_bad_vid_dies() { OPT_VID='04"3' OPT_PID=6001 OPT_SERIAL=S; dies --vid step_adapter; }
t_adapter_bad_serial_dies() {
	OPT_VID=0403 OPT_PID=6001 OPT_SERIAL='a"b'; dies --serial step_adapter
	OPT_SERIAL=; OPT_PORT='a*b'; dies --port step_adapter
}
t_adapter_serial_and_port_dies() {
	OPT_VID=0403 OPT_PID=6001 OPT_SERIAL=S OPT_PORT=P; dies exclusive step_adapter
}
t_picker_serial_once() {
	mkdev ttyUSB0; mkdev ttyUSB1
	STUB_UDEV_ttyUSB0=$FX/udev-ftdi.txt STUB_UDEV_ttyUSB1=$FX/udev-ch340.txt
	export STUB_UDEV_ttyUSB0 STUB_UDEV_ttyUSB1
	YES=0 MUTHUR_ASSUME_TTY=1; echo 1 > "$T/in"
	step_adapter < "$T/in" 2>&1 >/dev/null | grep -q '^1) /dev/ttyUSB0  0403:6001  serial A9E91BA3  (usb-FTDI' || { echo "  picker line wrong" >&2; exit 1; }
}
t_agent_nft_check_failure() {
	sys_env; STUB_NFT_CHECK_RC=1; export STUB_NFT_CHECK_RC
	( step_agent ) >/dev/null 2>&1 && exit 1
	chk none test ! -e "$ROOT/etc/nftables.d" -a ! -e "$ROOT/etc/sudoers.d"
}
t_agent_unreadable_nftconf_once() {
	sys_env; YES=1
	mkdir -p "$ROOT/etc"; echo 'flush ruleset' > "$ROOT/etc/nftables.conf"
	grep() { case ${*} in *nftables.conf) return 2 ;; esac; command grep "$@"; }
	step_agent >/dev/null 2>&1; step_agent >/dev/null 2>&1
	unset -f grep
	eq 1 "$(grep -c 'include' "$ROOT/etc/nftables.conf")" "include once"
}
t_agent_paths_passed() {
	sys_env; step_agent >/dev/null 2>&1
	chk sysusers grep -q "systemd-sysusers $ROOT/etc/sysusers.d/muthur-agent.conf" "$STUBLOG"
	chk tmpfiles grep -q "systemd-tmpfiles --create $ROOT/etc/tmpfiles.d/muthur-agent.conf" "$STUBLOG"
	chk nftc grep -q '^nft -c -f' "$STUBLOG"
}
t_find_tool_sbin_dirs() {
	mkdir -p "$T/sb"; printf '#!/bin/sh\n' > "$T/sb/zz-fake"; chmod +x "$T/sb/zz-fake"
	MUTHUR_SBIN_DIRS="$T/nope $T/sb"
	eq "$T/sb/zz-fake" "$(find_tool zz-fake)" found
	if find_tool zz-absent >/dev/null; then exit 1; fi
}
t_cleanup_trap_runs_hook() {
	HOOKF=$T/hook MUTHUR_INSTALL_LIB=1 sh -c '. "$0"; stage_new; CLEANUP_HOOK=myhook; myhook() { echo hook > "$HOOKF"; }; echo "$STAGE" > "$2"; exit 0' "$REPO/install.sh" x "$T/stage"
	eq hook "$(cat "$T/hook")" hook
	chk stage-gone test ! -e "$(cat "$T/stage")"
}

t_adapter_newline_in_flags_dies() {
	NL='
'
	OPT_VID="0403${NL}\"; RUN+=\"x" OPT_PID=6001 OPT_SERIAL=S; dies --vid step_adapter
	OPT_VID=0403 OPT_SERIAL="S${NL}x"; dies --serial step_adapter
}
t_system_no_agetty_dies() {
	sys_env; MUTHUR_SBIN_DIRS=$T/none;
	( find_tool() { return 1; }; step_system ) >/dev/null 2>&1 && exit 1
	chk none test ! -e "$ROOT/etc"
}
t_agent_no_nologin_dies() {
	( step_agent ) >/dev/null 2>&1 && exit 1
	chk none test ! -e "$ROOT/etc"
}

fake_bin() { printf '#!/bin/sh\necho %s\n' "$1" > "$2"; chmod +x "$2"; }
bin_env() {
	SCRIPT_DIR=$T/sd; mkdir -p "$SCRIPT_DIR"; ln -s "$REPO/deploy" "$SCRIPT_DIR/deploy"
	PATH=$HOME/.local/bin:$PATH
}
t_binary_install() {
	bin_env
	fake_bin one "$T/b1"; fake_bin two "$T/b2"
	OPT_BIN=$T/b1; step_binary >/dev/null 2>&1
	chk copy cmp "$T/b1" "$HOME/.local/bin/muthur"
	chk launcher test -x "$HOME/.local/bin/muthur-login"
	chk launcher-same cmp "$REPO/deploy/login/muthur-login" "$HOME/.local/bin/muthur-login"
	step_binary >/dev/null 2>&1
	chk noop-no-old test ! -e "$HOME/.local/bin/muthur.old"
	OPT_BIN=$T/b2; step_binary >/dev/null 2>&1
	chk new cmp "$T/b2" "$HOME/.local/bin/muthur"
	chk old cmp "$T/b1" "$HOME/.local/bin/muthur.old"
	step_binary >/dev/null 2>&1
	chk old-kept cmp "$T/b1" "$HOME/.local/bin/muthur.old"
}
t_binary_bad_bin_dies() {
	bin_env
	OPT_BIN=$T/missing; dies --bin step_binary
	: > "$T/plain"; OPT_BIN=$T/plain; dies --bin step_binary
}
t_binary_default_beside_script() {
	bin_env
	fake_bin top "$SCRIPT_DIR/muthur"
	mkdir -p "$SCRIPT_DIR/target/x/release"; fake_bin tgt "$SCRIPT_DIR/target/x/release/muthur"
	step_binary >/dev/null 2>&1
	chk beside cmp "$SCRIPT_DIR/muthur" "$HOME/.local/bin/muthur"
	rm "$SCRIPT_DIR/muthur" "$HOME/.local/bin/muthur"
	step_binary >/dev/null 2>&1
	chk target cmp "$SCRIPT_DIR/target/x/release/muthur" "$HOME/.local/bin/muthur"
	mkdir -p "$SCRIPT_DIR/target/y/release"; fake_bin t2 "$SCRIPT_DIR/target/y/release/muthur"
	dies --bin step_binary
}
t_shell_from_passwd() {
	STUB_PASSWD='tester:x:1000:1000::/home/t:/usr/bin/zsh'; export STUB_PASSWD
	eq zsh "$(login_shell)" zsh
	OPT_SHELL=fish; eq fish "$(login_shell)" override
	OPT_SHELL=
	STUB_PASSWD='tester:x:1000:1000::/home/t:/bin/dash'; export STUB_PASSWD
	dies 'bash, zsh, fish' login_shell
}
t_bash_file_choice() {
	eq "$HOME/.bash_profile" "$(bash_login_file)" none
	: > "$HOME/.profile"; eq "$HOME/.profile" "$(bash_login_file)" profile
	: > "$HOME/.bash_login"; eq "$HOME/.bash_login" "$(bash_login_file)" login
	: > "$HOME/.bash_profile"; eq "$HOME/.bash_profile" "$(bash_login_file)" all
}
t_bash_creates_profile() {
	install_hook bash >/dev/null 2>&1
	f=$HOME/.bash_profile
	eq '[ -f ~/.profile ] && . ~/.profile; [ -f ~/.bashrc ] && . ~/.bashrc' "$(head -n 1 "$f")" source-line
	eq "$BLOCK_OPEN" "$(sed -n 2p "$f")" open
	chk body grep -q 'muthur-login' "$f"
	install_hook bash >/dev/null 2>&1
	eq 1 "$(grep -c "^$BLOCK_OPEN\$" "$f")" once
	eq 1 "$(grep -c '^\[ -f ~/.profile' "$f")" source-once
}
t_bash_existing_no_source_line() {
	printf 'keep\n' > "$HOME/.profile"
	install_hook bash >/dev/null 2>&1
	chk none test ! -e "$HOME/.bash_profile"
	eq keep "$(head -n 1 "$HOME/.profile")" keep
}
t_zsh_zdotdir() {
	ZDOTDIR=$HOME/z; export ZDOTDIR
	install_hook zsh >/dev/null 2>&1
	chk in-z block_present "$HOME/z/.zprofile"
	chk not-home test ! -e "$HOME/.zprofile"
}
t_fish_hook() {
	install_hook fish >/dev/null 2>&1
	chk same cmp "$REPO/deploy/login/muthur.fish" "$HOME/.config/fish/conf.d/muthur.fish"
}
t_switch_shell_removes_old() {
	STUB_PASSWD='t:x:1:1::/h:/x'; export STUB_PASSWD
	OPT_SHELL=fish; step_hook >/dev/null 2>&1
	chk fish test -f "$HOME/.config/fish/conf.d/muthur.fish"
	OPT_SHELL=bash; step_hook >/dev/null 2>&1
	chk fish-gone test ! -e "$HOME/.config/fish/conf.d/muthur.fish"
	chk bash block_present "$HOME/.bash_profile"
	OPT_SHELL=zsh; step_hook >/dev/null 2>&1
	if block_present "$HOME/.bash_profile"; then exit 1; fi
	chk zsh block_present "$HOME/.zprofile"
}
t_remove_hooks_creates_nothing() {
	remove_hooks >/dev/null 2>&1
	eq "" "$(ls -A "$HOME")" empty
}
t_hook_symlinked_dotfile() {
	printf 'a\n' > "$T/real"; ln -s real "$T/link"; ln -s "$T/link" "$HOME/.bash_profile"
	install_hook bash >/dev/null 2>&1
	chk symlink test -L "$HOME/.bash_profile"
	chk target block_present "$T/real"
	remove_hooks >/dev/null 2>&1
	eq a "$(cat "$T/real")" restored
}
t_hook_rerun_is_noop() {
	STUB_PASSWD='t:x:1:1::/h:/x'; export STUB_PASSWD
	printf 'top\n' > "$HOME/.profile"
	OPT_SHELL=bash; step_hook >/dev/null 2>&1
	printf 'tail\n' >> "$HOME/.profile"
	cp "$HOME/.profile" "$T/before"
	step_hook >/dev/null 2>&1
	chk same cmp "$HOME/.profile" "$T/before"
}
t_launcher_off_serial() {
	fake_bin ran "$T/stubbin"; sed -i "s|^echo ran|touch $T/ran|" "$T/stubbin"
	MUTHUR_TTY=$T/nonexistent MUTHUR_BIN=$T/stubbin sh "$REPO/deploy/login/muthur-login"
	eq 0 $? rc
	chk not-run test ! -e "$T/ran"
}

if [ "${1:-}" = "--one" ]; then
	setup
	"$2"
	exit $?
fi
for t in $(sed -n 's/^\(t_[a-z_0-9]*\)().*/\1/p' "$SELF"); do
	if "$SHELL_BIN" "$SELF" --one "$t" 2>"${TMPDIR:-/tmp}/muthur-t.$$"; then
		echo "ok $t"
	else
		echo "FAIL $t:"; cat "${TMPDIR:-/tmp}/muthur-t.$$"
		FAILS=$((FAILS + 1))
	fi
done
rm -f "${TMPDIR:-/tmp}/muthur-t.$$"
[ "$FAILS" -eq 0 ]
