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
  --no-system        skip the udev rule and serial getty
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
	printf '%s [%s] ' "$1" "$2" >&2
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
		--no-system) OPT_SYSTEM=0 ;;
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
	[ "$OPT_SYSTEM" = 0 ] || _need="$_need agetty"
	[ "$OPT_AGENT" = 0 ] || _need="$_need nft bash visudo"
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
	_f=$1
	while [ -L "$_f" ]; do
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

main() {
	parse_args "$@"
	preflight
	if [ "$OPT_SYSTEM" = 1 ]; then
		step_adapter
		step_system
		[ "$OPT_AGENT" = 0 ] || step_agent
	fi
	# Tasks 7-8 add the remaining steps here.
}

[ -n "${MUTHUR_INSTALL_LIB:-}" ] || main "$@"
