#!/bin/sh
# MUTHUR installer: udev rule, serial getty, login launcher, agent user.
# Run as your normal user; it uses sudo only where it must.
# shellcheck disable=SC2034 # OPT_*, SCRIPT_DIR are used by the step functions in later tasks
set -eu

ROOT=${ROOT:-}
SYSTEMD_RUN=${SYSTEMD_RUN:-/run/systemd/system}
# MUTHUR_INSTALL_SRC lets tests that source this file name the real path.
SCRIPT_DIR=$(cd -P "$(dirname "${MUTHUR_INSTALL_SRC:-$0}")" && pwd)
YES=${YES:-0}

OPT_SHELL=
OPT_VID=
OPT_PID=
OPT_SERIAL=
OPT_PORT=
OPT_BAUD=
OPT_AGENT=1
OPT_SYSTEM=1
OPT_BIN=
OPT_UNINSTALL=0

# One EXIT trap for the whole script: temp paths to remove, plus an optional
# hook (a function name) that restores state such as the terminal's echo.
CLEANUP_PATHS=
CLEANUP_HOOK=
cleanup() {
	_rc=$?
	while IFS= read -r _cp; do
		[ -z "$_cp" ] || rm -rf "$_cp"
	done <<EOF
$CLEANUP_PATHS
EOF
	[ -z "$CLEANUP_HOOK" ] || "$CLEANUP_HOOK" || true
	return "$_rc"
}
trap cleanup EXIT
trap 'exit 130' INT TERM

BLOCK_OPEN='# >>> muthur >>>'
BLOCK_CLOSE='# <<< muthur <<<'

say() { printf '==> %s\n' "$*"; }
warn() { printf 'warning: %s\n' "$*" >&2; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

usage() {
	cat <<'USAGE'
Usage: install.sh [options]
  --yes              accept defaults, ask nothing
  --shell SHELL      bash, zsh or fish
  --vid V --pid P    USB vendor / product id of the serial adapter
  --serial S         USB serial number of the adapter
  --port ID_PATH     match by udev ID_PATH instead
  --baud N           serial speed (default 38400)
  --no-agent         skip the agent user and nftables setup
  --no-system        skip the udev rule, serial getty and agent setup
  --bin PATH         install this muthur binary
  --uninstall        remove what this script installed
  --help             show this text
USAGE
}

# ask PROMPT DEFAULT [FLAG]: echo the answer.
ask() {
	if [ "$YES" = 1 ]; then
		printf '%s\n' "$2"
		return 0
	fi
	if [ "${MUTHUR_ASSUME_TTY:-0}" != 1 ] && [ ! -t 0 ]; then
		die "$1: no terminal to ask on (pass --${3:-yes})"
	fi
	if [ -n "$2" ]; then printf '%s [%s] ' "$1" "$2" >&2; else printf '%s ' "$1" >&2; fi
	_ans=
	read -r _ans || true
	printf '%s\n' "${_ans:-$2}"
}

# confirm PROMPT: status 0 for yes (default yes).
confirm() {
	[ "$YES" = 1 ] && return 0
	_c=$(ask "$1 (y/n)" y yes) || exit 1
	case $_c in [Nn]*) return 1 ;; *) return 0 ;; esac
}

parse_args() {
	while [ $# -gt 0 ]; do
		case $1 in
		--yes) YES=1 ;;
		--no-agent) OPT_AGENT=0 ;;
		--no-system) OPT_SYSTEM=0 OPT_AGENT=0 ;;
		--uninstall) OPT_UNINSTALL=1 ;;
		--help) usage; exit 0 ;;
		--shell | --vid | --pid | --serial | --port | --baud | --bin)
			[ $# -ge 2 ] || die "$1 needs a value"
			case $1 in
			--shell) OPT_SHELL=$2 ;;
			--vid) OPT_VID=$2 ;;
			--pid) OPT_PID=$2 ;;
			--serial) OPT_SERIAL=$2 ;;
			--port) OPT_PORT=$2 ;;
			--baud) OPT_BAUD=$2 ;;
			--bin) OPT_BIN=$2 ;;
			esac
			shift
			;;
		*) die "unknown option: $1 (see --help)" ;;
		esac
		shift
	done
	case $OPT_SHELL in "" | bash | zsh | fish) ;; *) die "--shell must be bash, zsh or fish" ;; esac
}

# find_tool NAME: path of NAME on PATH or in the sbin dirs (Debian keeps several off a user's PATH).
find_tool() {
	command -v "$1" 2>/dev/null && return 0
	for _d in ${MUTHUR_SBIN_DIRS:-/usr/sbin /sbin /usr/bin}; do
		if [ -x "$_d/$1" ]; then printf '%s\n' "$_d/$1"; return 0; fi
	done
	return 1
}

preflight() {
	[ "$(id -u)" != 0 ] || die "run as your normal user, not as root (sudo is used where needed)"
	[ -d "$SYSTEMD_RUN" ] || die "systemd is required; see docs for the manual-install steps"
	case $(uname -m) in x86_64 | aarch64) ;; *) die "unsupported architecture $(uname -m) (x86_64 and aarch64 only)" ;; esac
	_need="sudo udevadm"
	if [ "$OPT_UNINSTALL" != 1 ]; then
		[ "$OPT_SYSTEM" = 0 ] || _need="$_need agetty"
		[ "$OPT_AGENT" = 0 ] || _need="$_need nft bash visudo"
	fi
	for _t in $_need; do
		find_tool "$_t" >/dev/null || die "missing required tool: $_t"
	done
}

# render TEMPLATE OUT K=V...: substitute @K@ with V; leftovers are an error.
render() {
	_tpl=$1 _out=$2
	shift 2
	[ -r "$_tpl" ] || die "missing template $_tpl"
	_content=$(cat "$_tpl"; echo x)
	_content=${_content%x}
	for _kv in "$@"; do
		_k=${_kv%%=*}
		_v=${_kv#*=}
		_pre=
		_rest=$_content
		while :; do
			case $_rest in
			*"@$_k@"*)
				_pre=$_pre${_rest%%"@$_k@"*}$_v
				_rest=${_rest#*"@$_k@"}
				;;
			*) break ;;
			esac
		done
		_content=$_pre$_rest
	done
	if printf '%s' "$_content" | grep -Eq '@[A-Z_]+@'; then
		die "unfilled placeholder in $_tpl: $(printf '%s' "$_content" | grep -Eo '@[A-Z_]+@' | sort -u | tr '\n' ' ')"
	fi
	printf '%s' "$_content" > "$_out"
}

# resolve_link FILE: follow symlinks to the real path (may not exist yet).
resolve_link() {
	_f=$1 _hops=0
	while [ -L "$_f" ]; do
		_hops=$((_hops + 1))
		[ "$_hops" -le 40 ] || die "too many symlink levels (loop?) resolving $1"
		_t=$(readlink "$_f")
		case $_t in /*) _f=$_t ;; *) _f=$(dirname "$_f")/$_t ;; esac
	done
	printf '%s\n' "$_f"
}

# atomic_write FILE: stdin becomes FILE's content; mode kept (644 if new).
atomic_write() {
	_target=$(resolve_link "$1")
	_dir=$(dirname "$_target")
	mkdir -p "$_dir"
	_tmp=$_target.muthur.tmp.$$
	( umask 077; cat > "$_tmp" ) || { rm -f "$_tmp"; return 1; }
	if [ -e "$_target" ]; then
		# shellcheck disable=SC2012
		_p=$(ls -ld -- "$_target" | cut -c2-10)
		_u=$(printf %s "$_p" | cut -c1-3 | tr -d 'sStT-')
		_g=$(printf %s "$_p" | cut -c4-6 | tr -d 'sStT-')
		_o=$(printf %s "$_p" | cut -c7-9 | tr -d 'sStT-')
		chmod "u=$_u,g=$_g,o=$_o" "$_tmp"
	else
		chmod 644 "$_tmp"
	fi
	mv -f "$_tmp" "$_target" || { rm -f "$_tmp"; return 1; }
}

block_present() {
	[ -f "$1" ] && grep -qx "$BLOCK_OPEN" "$1"
}

# block_check FILE: die unless markers are balanced and not nested.
block_check() {
	[ -f "$1" ] || return 0
	awk -v o="$BLOCK_OPEN" -v c="$BLOCK_CLOSE" '
		$0 == o { if (in_b) bad = 1; in_b = 1; next }
		$0 == c { if (!in_b) bad = 1; in_b = 0 }
		END { exit (bad || in_b) ? 1 : 0 }
	' "$1" || die "unbalanced muthur markers in $1; fix by hand"
}

# block_write FILE: stdin is the block body; insert or replace the block.
block_write() {
	_bf=$(resolve_link "$1")
	block_check "$_bf"
	_body=$(mktemp)
	_new=$(mktemp)
	cat > "$_body" || { rm -f "$_body" "$_new"; die "cannot read block body"; }
	if block_present "$_bf"; then
		awk -v o="$BLOCK_OPEN" -v c="$BLOCK_CLOSE" -v bf="$_body" '
			$0 == o { print; while ((getline l < bf) > 0) print l; skip = 1; next }
			skip && $0 == c { skip = 0 }
			!skip { print }
		' "$_bf" > "$_new" || { rm -f "$_body" "$_new"; die "cannot rewrite $_bf"; }
	else
		{
			if [ -s "$_bf" ]; then
				cat "$_bf"
				[ -z "$(tail -c 1 "$_bf")" ] || echo
			fi
			echo "$BLOCK_OPEN"
			cat "$_body"
			echo "$BLOCK_CLOSE"
		} > "$_new" || { rm -f "$_body" "$_new"; die "cannot build $_bf"; }
	fi
	atomic_write "$_bf" < "$_new" || { rm -f "$_body" "$_new"; exit 1; }
	rm -f "$_body" "$_new"
}

# block_remove FILE: drop the block, leaving every other byte alone.
block_remove() {
	_bf=$(resolve_link "$1")
	block_present "$_bf" || return 0
	block_check "$_bf"
	_new=$(mktemp)
	sed -n "/^$BLOCK_OPEN\$/,/^$BLOCK_CLOSE\$/!p" "$_bf" > "$_new" || { rm -f "$_new"; die "cannot rewrite $_bf"; }
	atomic_write "$_bf" < "$_new" || { rm -f "$_new"; exit 1; }
	rm -f "$_new"
}

# parse_udev_props: stdin is `udevadm info --query=property`; sets ADP_*.
parse_udev_props() {
	ADP_VID="" ADP_PID="" ADP_SERIAL="" ADP_PATH="" ADP_BYID=""
	while IFS= read -r _l || [ -n "$_l" ]; do
		case $_l in
		ID_VENDOR_ID=*) ADP_VID=${_l#*=} ;;
		ID_MODEL_ID=*) ADP_PID=${_l#*=} ;;
		ID_SERIAL_SHORT=*) ADP_SERIAL=${_l#*=} ;;
		ID_PATH=*) ADP_PATH=${_l#*=} ;;
		DEVLINKS=*)
			for _d in ${_l#*=}; do
				case $_d in /dev/serial/by-id/*) [ -n "$ADP_BYID" ] || ADP_BYID=$_d ;; esac
			done
			;;
		esac
	done
}

# list_adapters: one tab-separated line per serial device (dev vid pid serial idpath byid).
list_adapters() {
	_udevadm=$(find_tool udevadm) || die "udevadm not found"
	for _dev in "$ROOT"/dev/ttyUSB* "$ROOT"/dev/ttyACM*; do
		[ -e "$_dev" ] || continue
		_props=$("$_udevadm" info --query=property --name="$_dev") || continue
		parse_udev_props <<EOF
$_props
EOF
		printf '%s\t%s\t%s\t%s\t%s\t%s\n' "${_dev#"$ROOT"}" "$ADP_VID" "$ADP_PID" "$ADP_SERIAL" "$ADP_PATH" "$ADP_BYID"
	done
}

# match_line: the udev match for ADP_*; the serial number wins over the port.
match_line() {
	if [ -n "$ADP_SERIAL" ]; then
		printf 'ATTRS{idVendor}=="%s", ATTRS{idProduct}=="%s", ENV{ID_SERIAL_SHORT}=="%s"\n' "$ADP_VID" "$ADP_PID" "$ADP_SERIAL"
	elif [ -n "$ADP_PATH" ]; then
		printf 'ATTRS{idVendor}=="%s", ATTRS{idProduct}=="%s", ENV{ID_PATH}=="%s"\n' "$ADP_VID" "$ADP_PID" "$ADP_PATH"
	else
		die "adapter has neither a serial number nor a port path; pass --serial or --port"
	fi
}

check_hex() {
	case $2 in
	[0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F]) ;;
	*) die "$1 must be four hex digits" ;;
	esac
}
# check_plain FLAG VALUE: no characters that could break out of a udev rule value.
check_plain() {
	# shellcheck disable=SC1003 # the quoted backslash is intended
	case $2 in
	"" | *'"'* | *'\'* | *'*'* | *'?'* | *'['* | *'|'* | *'
'*) die "$1 contains a character not allowed in a udev match (quotes, backslash, newline, * ? [ |)" ;;
	esac
}

# step_adapter: sets ADAPTER_MATCH.
step_adapter() {
	say "serial adapter"
	ADP_VID="" ADP_PID="" ADP_SERIAL="" ADP_PATH="" ADP_BYID=""
	if [ -n "$OPT_VID$OPT_PID" ] || [ -n "$OPT_SERIAL$OPT_PORT" ]; then
		[ -n "$OPT_VID" ] && [ -n "$OPT_PID" ] || die "--serial/--port need --vid and --pid"
		[ -n "$OPT_SERIAL$OPT_PORT" ] || die "--vid/--pid need --serial or --port to name the device"
		[ -z "$OPT_SERIAL" ] || [ -z "$OPT_PORT" ] || die "--serial and --port are mutually exclusive"
		check_hex --vid "$OPT_VID"
		check_hex --pid "$OPT_PID"
		[ -z "$OPT_SERIAL" ] || check_plain --serial "$OPT_SERIAL"
		[ -z "$OPT_PORT" ] || check_plain --port "$OPT_PORT"
		ADP_VID=$OPT_VID ADP_PID=$OPT_PID ADP_SERIAL=$OPT_SERIAL ADP_PATH=$OPT_PORT
		ADAPTER_MATCH=$(match_line) || exit 1
		return 0
	fi
	_list=$(list_adapters) || exit 1
	[ -n "$_list" ] || die "no USB serial adapter found (/dev/ttyUSB*, /dev/ttyACM*); plug it in, or pass --vid, --pid and --serial (or --port)"
	_n=$(printf '%s\n' "$_list" | wc -l)
	_i=0
	while IFS= read -r _row; do
		_i=$((_i + 1))
		_by=$(printf '%s' "$_row" | cut -f6)
		_sn=$(printf '%s' "$_row" | cut -f4)
		printf '%s) %s  %s:%s  %s  (%s)\n' "$_i" "$(printf '%s' "$_row" | cut -f1)" \
			"$(printf '%s' "$_row" | cut -f2)" "$(printf '%s' "$_row" | cut -f3)" \
			"${_sn:+serial }${_sn:-no serial}" "${_by#/dev/serial/by-id/}" >&2
	done <<EOF
$_list
EOF
	if [ "$_n" -eq 1 ]; then
		confirm "Use this adapter?" || die "no adapter chosen; pass --vid, --pid and --serial (or --port)"
		_pick=1
	else
		[ "$YES" != 1 ] || die "several adapters found and --yes gives no default; pass --vid, --pid and --serial (or --port)"
		_pick=$(ask "Which adapter (1-$_n)?" "" vid) || exit 1
		case $_pick in "" | *[!0-9]*) die "not a number: $_pick" ;; esac
		{ [ "$_pick" -ge 1 ] && [ "$_pick" -le "$_n" ]; } || die "choice out of range: $_pick"
	fi
	_row=$(printf '%s\n' "$_list" | sed -n "${_pick}p")
	ADP_VID=$(printf '%s' "$_row" | cut -f2) ADP_PID=$(printf '%s' "$_row" | cut -f3)
	ADP_SERIAL=$(printf '%s' "$_row" | cut -f4) ADP_PATH=$(printf '%s' "$_row" | cut -f5)
	[ -n "$ADP_VID" ] && [ -n "$ADP_PID" ] || die "cannot read the adapter's USB ids; pass --vid, --pid and --serial (or --port)"
	check_hex "adapter vid" "$ADP_VID"
	check_hex "adapter pid" "$ADP_PID"
	[ -z "$ADP_SERIAL" ] || check_plain "adapter serial" "$ADP_SERIAL"
	[ -z "$ADP_PATH" ] || check_plain "adapter port" "$ADP_PATH"
	ADAPTER_MATCH=$(match_line) || exit 1
	[ -n "$ADP_SERIAL" ] || warn "adapter has no serial number; matching its USB port ($ADP_PATH): moving it to another port breaks the match"
}

find_nologin() {
	for _p in /usr/sbin/nologin /usr/bin/nologin /sbin/nologin; do
		if [ -e "$ROOT$_p" ]; then printf '%s\n' "$_p"; return 0; fi
	done
	die "no nologin binary found"
}

find_agetty() {
	find_tool agetty || die "agetty not found"
}

STAGE=
stage_new() {
	STAGE=$(mktemp -d) || die "cannot create a temporary directory"
	CLEANUP_PATHS=$CLEANUP_PATHS$STAGE'
'
}
stage_done() { rm -rf "$STAGE"; }

# sys_install MODE SRC DEST: create DEST's directory, then install.
sys_install() {
	sudo mkdir -p "$(dirname "$3")"
	sudo install -D -m "$1" "$2" "$3"
}

step_system() {
	say "system files (udev rule, serial getty)"
	[ -n "${ADAPTER_MATCH:-}" ] || die "no adapter match; run step_adapter first"
	_agetty=$(find_agetty) || exit 1
	stage_new
	render "$SCRIPT_DIR/deploy/udev/99-muthur-tty.rules.in" "$STAGE/rule" MATCH="$ADAPTER_MATCH"
	render "$SCRIPT_DIR/deploy/systemd/serial-getty@muthur-tty.service.d/override.conf.in" "$STAGE/drop" \
		AGETTY="$_agetty" BAUD="${OPT_BAUD:-38400}"
	_old_rule=$ROOT/etc/udev/rules.d/99-wy60.rules
	_old_dir=$ROOT/etc/systemd/system/serial-getty@wy60.service.d
	if [ -e "$_old_rule" ] || [ -e "$_old_dir" ]; then
		say "migrating the old wy60 names"
		sudo systemctl stop serial-getty@wy60 || true
		sudo rm -f "$_old_rule"
		sudo rm -rf "$_old_dir"
	fi
	if sudo systemctl is-enabled --quiet serial-getty@ttyUSB0 2>/dev/null; then
		sudo systemctl disable serial-getty@ttyUSB0
	fi
	sys_install 644 "$STAGE/rule" "$ROOT/etc/udev/rules.d/99-muthur-tty.rules"
	sys_install 644 "$STAGE/drop" "$ROOT/etc/systemd/system/serial-getty@muthur-tty.service.d/override.conf"
	stage_done
	sudo systemctl daemon-reload
	sudo udevadm control --reload
	sudo udevadm trigger --subsystem-match=tty --action=add
}

step_agent() {
	say "agent account (muthur-agent, sudoers, nftables)"
	_nologin=$(find_nologin) || exit 1
	_op=$(id -un)
	stage_new
	render "$SCRIPT_DIR/deploy/sysusers.d/muthur-agent.conf.in" "$STAGE/sysusers" NOLOGIN="$_nologin"
	sed "s/^OPERATOR /$_op /" "$SCRIPT_DIR/deploy/sudoers.d/muthur-agent" > "$STAGE/sudoers"
	grep -q "^$_op ALL" "$STAGE/sudoers" || die "cannot render sudoers for user $_op"
	_visudo=$(find_tool visudo) || die "visudo not found"
	"$_visudo" -cf "$STAGE/sudoers" >/dev/null || die "rendered sudoers file failed visudo -cf"
	for _g in adm systemd-journal; do
		[ -n "$(getent group "$_g" 2>/dev/null)" ] || warn "group $_g does not exist; the agent will not get its access"
	done
	sys_install 644 "$STAGE/sysusers" "$ROOT/etc/sysusers.d/muthur-agent.conf"
	sys_install 644 "$SCRIPT_DIR/deploy/tmpfiles.d/muthur-agent.conf" "$ROOT/etc/tmpfiles.d/muthur-agent.conf"
	sudo systemd-sysusers "$ROOT/etc/sysusers.d/muthur-agent.conf"
	sudo systemd-tmpfiles --create "$ROOT/etc/tmpfiles.d/muthur-agent.conf"
	sudo nft -c -f "$SCRIPT_DIR/deploy/nftables/muthur-agent.nft" || die "nft rejected the muthur-agent ruleset (nft -c)"
	sys_install 440 "$STAGE/sudoers" "$ROOT/etc/sudoers.d/muthur-agent"
	sys_install 644 "$SCRIPT_DIR/deploy/nftables/muthur-agent.nft" "$ROOT/etc/nftables.d/muthur-agent.nft"
	stage_done
	sudo nft -f "$ROOT/etc/nftables.d/muthur-agent.nft"
	_conf=$ROOT/etc/nftables.conf
	if [ -f "$_conf" ]; then
		_rc=0
		sudo grep -q 'nftables.d' "$_conf" || _rc=$?
		case $_rc in
		0) ;;
		1)
			if confirm "Add an include for /etc/nftables.d/ to /etc/nftables.conf?"; then
				{ [ -z "$(sudo tail -c 1 "$_conf")" ] || echo; echo 'include "/etc/nftables.d/*.nft"'; } | sudo tee -a "$_conf" >/dev/null
			fi
			;;
		*) die "cannot read $_conf" ;;
		esac
	fi
	if ! systemctl is-enabled --quiet nftables 2>/dev/null; then
		if confirm "Enable nftables.service so the rules load at boot?"; then
			sudo systemctl enable nftables
		fi
	fi
	if grep -Eq '^hosts:.*resolve' "$ROOT/etc/nsswitch.conf" 2>/dev/null; then
		warn "nsswitch hosts uses resolve: a local resolver stub may leak DNS lookups from the agent"
	fi
}

# install_exe SRC DEST: mode 755 via a temp file and rename (a running DEST is not overwritten in place).
# Returns 1 if DEST was already identical (nothing done), else 0; keeps the old DEST as DEST.old when KEEP_OLD=1.
install_exe() {
	_dest=$2
	if [ -f "$_dest" ] && [ -x "$_dest" ] && cmp -s "$1" "$_dest"; then return 1; fi
	mkdir -p "$(dirname "$_dest")"
	_xt=$(mktemp "$_dest.XXXXXX") || die "cannot create a temporary file beside $_dest"
	CLEANUP_PATHS=$CLEANUP_PATHS$_xt'
'
	install -m 755 "$1" "$_xt" || die "cannot install $_dest"
	if [ "${KEEP_OLD:-0}" = 1 ] && [ -f "$_dest" ]; then ln -f "$_dest" "$_dest.old" || die "cannot keep $_dest.old"; fi
	mv -f "$_xt" "$_dest" || die "cannot install $_dest"
}

# find_binary: echo the muthur binary to install.
find_binary() {
	if [ -n "$OPT_BIN" ]; then
		{ [ -f "$OPT_BIN" ] && [ -x "$OPT_BIN" ]; } || die "--bin $OPT_BIN is not an executable file"
		printf '%s\n' "$OPT_BIN"
		return 0
	fi
	if [ -f "$SCRIPT_DIR/muthur" ] && [ -x "$SCRIPT_DIR/muthur" ]; then
		printf '%s\n' "$SCRIPT_DIR/muthur"
		return 0
	fi
	_n=0 _found=
	for _c in "$SCRIPT_DIR"/target/*/release/muthur; do
		if [ -f "$_c" ] && [ -x "$_c" ]; then _n=$((_n + 1)); _found=$_c; fi
	done
	[ "$_n" -le 1 ] || die "several release binaries under $SCRIPT_DIR/target; pick one with --bin PATH"
	[ "$_n" -eq 1 ] || die "no muthur binary found; build one or pass --bin PATH"
	printf '%s\n' "$_found"
}

step_binary() {
	say "binary and login launcher"
	_src=$(find_binary) || exit 1
	_bd=$HOME/.local/bin
	_rc=0
	KEEP_OLD=1 install_exe "$_src" "$_bd/muthur" || _rc=$?
	[ "$_rc" -le 1 ] || exit 1
	_rc=0
	KEEP_OLD=0 install_exe "$SCRIPT_DIR/deploy/login/muthur-login" "$_bd/muthur-login" || _rc=$?
	[ "$_rc" -le 1 ] || exit 1
	case ":$PATH:" in *":$_bd:"*) ;; *) warn "$_bd is not on PATH (the login hooks use the absolute path, so login still works)" ;; esac
}

# login_shell: echo bash, zsh or fish.
login_shell() {
	_s=$OPT_SHELL
	if [ -z "$_s" ]; then
		_s=$(getent passwd "$(id -un)" | cut -d: -f7) || _s=
		_s=${_s##*/}
	fi
	[ -n "$_s" ] || die "cannot read the login shell from the passwd database; pass --shell"
	case $_s in
	bash | zsh | fish) printf '%s\n' "$_s" ;;
	*) die "unsupported login shell '$_s'; supported shells: bash, zsh, fish (pass --shell)" ;;
	esac
}

bash_login_file() {
	for _f in "$HOME/.bash_profile" "$HOME/.bash_login" "$HOME/.profile"; do
		if [ -e "$_f" ]; then printf '%s\n' "$_f"; return 0; fi
	done
	printf '%s\n' "$HOME/.bash_profile"
}

zsh_login_file() { printf '%s\n' "${ZDOTDIR:-$HOME}/.zprofile"; }

FISH_HOOK=$HOME/.config/fish/conf.d/muthur.fish

# install_hook SHELL
install_hook() {
	case $1 in
	fish) atomic_write "$FISH_HOOK" < "$SCRIPT_DIR/deploy/login/muthur.fish" || exit 1 ;;
	bash)
		_hf=$(bash_login_file) || exit 1
		if [ ! -e "$_hf" ]; then
			printf '%s\n' '[ -f ~/.profile ] && . ~/.profile; [ -f ~/.bashrc ] && . ~/.bashrc' | atomic_write "$_hf" || exit 1
		fi
		block_write "$_hf" < "$SCRIPT_DIR/deploy/login/hook.sh"
		;;
	zsh) block_write "$(zsh_login_file)" < "$SCRIPT_DIR/deploy/login/hook.sh" ;;
	*) die "no hook for shell $1" ;;
	esac
}

# remove_hooks [KEEP_SHELL]: remove every hook except KEEP_SHELL's, so a re-run rewrites in place.
remove_hooks() {
	_keep=${1:-}
	_kf=
	[ "$_keep" != bash ] || _kf=$(resolve_link "$(bash_login_file)") || exit 1
	for _f in "$HOME/.bash_profile" "$HOME/.bash_login" "$HOME/.profile"; do
		[ -z "$_kf" ] || [ "$(resolve_link "$_f")" != "$_kf" ] || continue
		block_remove "$_f"
	done
	if [ "$_keep" != zsh ]; then block_remove "$(zsh_login_file)"; fi
	_zr=$(resolve_link "$(zsh_login_file)")
	if [ "$_keep" = zsh ] && [ "$_zr" != "$(resolve_link "$HOME/.zprofile")" ]; then block_remove "$HOME/.zprofile"; fi
	if [ "$_keep" != zsh ]; then block_remove "$HOME/.zprofile"; fi
	[ "$_keep" = fish ] || rm -f "$FISH_HOOK"
}

step_hook() {
	say "login hook"
	_sh=$(login_shell) || exit 1
	remove_hooks "$_sh"
	install_hook "$_sh"
}

# set_toml_key FILE KEY VALUE: replace the first `KEY = ...` line of [muthur] with KEY = "VALUE" (TOML-escaped).
# The value travels in the environment, never inside the awk program.
set_toml_key() {
	_tf=$(resolve_link "$1")
	_tn=$(mktemp) || die "cannot create a temporary file"
	CLEANUP_PATHS=$CLEANUP_PATHS$_tn'
'
	TK=$2 TV=$3 awk '
		/^\[/ { in_m = ($0 == "[muthur]") }
		in_m && !done && $0 ~ "^" ENVIRON["TK"] "[ \t]*=" {
			v = ENVIRON["TV"]; out = ""
			while ((i = index(v, "\\")) > 0) { out = out substr(v, 1, i - 1) "\\\\"; v = substr(v, i + 1) }
			print ENVIRON["TK"] " = \"" out v "\""; done = 1; next
		}
		{ print }
		END { exit done ? 0 : 1 }
	' "$_tf" > "$_tn" || { rm -f "$_tn"; die "no $2 line in the [muthur] section of $1"; }
	atomic_write "$_tf" < "$_tn" || { rm -f "$_tn"; exit 1; }
	rm -f "$_tn"
}

# delete_toml_key FILE KEY: drop the first `KEY = ...` line of [muthur] (none is fine).
delete_toml_key() {
	_tf=$(resolve_link "$1")
	_tn=$(mktemp) || die "cannot create a temporary file"
	CLEANUP_PATHS=$CLEANUP_PATHS$_tn'
'
	TK=$2 awk '
		/^\[/ { in_m = ($0 == "[muthur]") }
		in_m && !done && $0 ~ "^" ENVIRON["TK"] "[ \t]*=" { done = 1; next }
		{ print }
	' "$_tf" > "$_tn" || { rm -f "$_tn"; die "cannot rewrite $1"; }
	atomic_write "$_tf" < "$_tn" || { rm -f "$_tn"; exit 1; }
	rm -f "$_tn"
}

restore_echo() { stty echo 2>/dev/null || true; }

# read_secret PROMPT: sets SECRET from one line read with terminal echo off.
# Not run in a subshell, so the EXIT/INT trap in this shell restores echo.
read_secret() {
	[ "$YES" != 1 ] || die "$1: needs an answer (do not pass --yes)"
	if [ "${MUTHUR_ASSUME_TTY:-0}" != 1 ] && [ ! -t 0 ]; then die "$1: no terminal to ask on"; fi
	printf '%s: ' "$1" >&2
	CLEANUP_HOOK=restore_echo
	stty -echo 2>/dev/null || true
	SECRET=
	read -r SECRET || true
	restore_echo
	CLEANUP_HOOK=
	printf '\n' >&2
}

# ask_secret PROMPT: SECRET without a double quote; asks again otherwise.
ask_secret() {
	_tries=0
	while [ "$_tries" -lt 5 ]; do
		_tries=$((_tries + 1))
		read_secret "$1"
		case $SECRET in
		*'"'*) warn "double quotes are not allowed here" ;;
		*) return 0 ;;
		esac
	done
	die "no valid answer for: $1"
}

# ask_value PROMPT: a non-empty answer without a double quote; asks again otherwise.
ask_value() {
	_tries=0
	while [ "$_tries" -lt 5 ]; do
		_tries=$((_tries + 1))
		_v=$(ask "$1" "" yes) || exit 1
		case $_v in
		"") warn "an answer is required" ;;
		*'"'*) warn "double quotes are not allowed here" ;;
		*) printf '%s\n' "$_v"; return 0 ;;
		esac
	done
	die "no valid answer for: $1"
}

find_example_config() {
	for _e in "$SCRIPT_DIR/docs/config.example.toml" "$SCRIPT_DIR/config.example.toml"; do
		if [ -r "$_e" ]; then printf '%s\n' "$_e"; return 0; fi
	done
	die "config.example.toml not found beside the installer"
}

step_config() {
	say "first-run config"
	_cdir=$HOME/.config/muthur
	_cfg=$_cdir/config.toml
	if [ -e "$_cfg" ]; then
		say "config exists, left untouched: $_cfg"
		return 0
	fi
	_ex=$(find_example_config) || exit 1
	mkdir -p "$_cdir" || die "cannot create $_cdir"
	chmod 700 "$_cdir" || die "cannot set mode on $_cdir"
	_new=$(mktemp "$_cdir/config.toml.XXXXXX") || die "cannot create a temporary file in $_cdir"
	CLEANUP_PATHS=$CLEANUP_PATHS$_new'
'
	chmod 600 "$_new"
	cat "$_ex" > "$_new" || die "cannot write $_new"
	if [ "$YES" = 1 ]; then
		mv "$_new" "$_cfg" || die "cannot write $_cfg"
		say "wrote the example config to $_cfg; edit it: set api_key (anthropic), or provider, base_url, model and api_key (openai)"
		return 0
	fi
	_pc=
	_tries=0
	while [ "$_tries" -lt 5 ]; do
		_tries=$((_tries + 1))
		_pc=$(ask "Provider: 1) Anthropic  2) OpenAI-compatible (e.g. vLLM)" 1 yes) || exit 1
		case $_pc in 1 | 2) break ;; *) warn "choose 1 or 2"; _pc= ;; esac
	done
	[ -n "$_pc" ] || die "no provider chosen"
	case $_pc in
	1)
		ask_secret "Anthropic API key (input hidden)"
		[ -n "$SECRET" ] || die "an API key is required for Anthropic"
		set_toml_key "$_new" api_key "$SECRET"
		;;
	2)
		_curl=$(ask_value "base_url (e.g. http://10.0.0.5:8000/v1)") || exit 1
		_cmodel=$(ask_value "model name served") || exit 1
		ask_secret "API key (input hidden; empty for none)"
		set_toml_key "$_new" provider openai
		set_toml_key "$_new" base_url "$_curl"
		set_toml_key "$_new" model "$_cmodel"
		if [ -n "$SECRET" ]; then set_toml_key "$_new" api_key "$SECRET"; else delete_toml_key "$_new" api_key; fi
		;;
	esac
	SECRET=
	mv "$_new" "$_cfg" || die "cannot write $_cfg"
	say "wrote $_cfg (mode 600)"
}

step_verify() {
	say "verify"
	if [ "$OPT_SYSTEM" = 1 ]; then
		if [ -e "$ROOT/dev/muthur-tty" ]; then
			echo "  /dev/muthur-tty: present"
		else
			echo "  /dev/muthur-tty: adapter not plugged in (or the rule has not matched yet)"
		fi
		if systemctl is-active --quiet serial-getty@muthur-tty 2>/dev/null; then
			echo "  serial-getty@muthur-tty: active"
		else
			echo "  serial-getty@muthur-tty: inactive (starts when the adapter is plugged in)"
		fi
	fi
	_hk=no
	[ ! -e "$FISH_HOOK" ] || _hk=yes
	for _f in "$HOME/.bash_profile" "$HOME/.bash_login" "$HOME/.profile" "$(zsh_login_file)" "$HOME/.zprofile"; do
		if block_present "$_f"; then _hk=yes; fi
	done
	echo "  login hook: $_hk"
	if _v=$("$HOME/.local/bin/muthur" --version 2>&1); then
		echo "  muthur --version: $_v"
	else
		echo "  muthur --version: failed to run $HOME/.local/bin/muthur"
	fi
	echo "Next: plug in the adapter, check the config in ~/.config/muthur/config.toml, then log in on the terminal."
	return 0
}

uninstall() {
	say "uninstall"
	remove_hooks
	rm -f "$HOME/.local/bin/muthur-login"
	if [ -e "$HOME/.local/bin/muthur" ] || [ -e "$HOME/.local/bin/muthur.old" ]; then
		if confirm "Remove $HOME/.local/bin/muthur?"; then
			rm -f "$HOME/.local/bin/muthur" "$HOME/.local/bin/muthur.old"
		fi
	fi
	if [ "$OPT_SYSTEM" = 1 ]; then
		sudo systemctl stop serial-getty@muthur-tty || true
		sudo rm -f "$ROOT/etc/udev/rules.d/99-muthur-tty.rules"
		sudo rm -rf "$ROOT/etc/systemd/system/serial-getty@muthur-tty.service.d"
		sudo systemctl daemon-reload
		sudo udevadm control --reload
	fi
	if [ "$OPT_AGENT" = 1 ]; then
		sudo rm -f "$ROOT/etc/sysusers.d/muthur-agent.conf" "$ROOT/etc/tmpfiles.d/muthur-agent.conf" \
			"$ROOT/etc/sudoers.d/muthur-agent" "$ROOT/etc/nftables.d/muthur-agent.nft"
		sudo nft delete table inet muthur_agent || true
	fi
	echo "Left in place: ~/.config/muthur/config.toml, logs, and the include line in /etc/nftables.conf."
	[ "$OPT_AGENT" = 0 ] || echo "To remove the agent account: sudo userdel muthur-agent"
}

main() {
	parse_args "$@"
	preflight
	if [ "$OPT_UNINSTALL" = 1 ]; then
		uninstall
		return 0
	fi
	step_binary
	if [ "$OPT_SYSTEM" = 1 ]; then
		step_adapter
		step_system
	fi
	if [ "$OPT_AGENT" = 1 ]; then step_agent; fi
	step_hook
	step_config
	step_verify
}

[ -n "${MUTHUR_INSTALL_LIB:-}" ] || main "$@"
