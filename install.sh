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
	_c=$(ask "$1 (y/n)" y yes)
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

preflight() {
	[ "$(id -u)" != 0 ] || die "run as your normal user, not as root (sudo is used where needed)"
	[ -d "$SYSTEMD_RUN" ] || die "systemd is required; see docs for the manual-install steps"
	case $(uname -m) in x86_64 | aarch64) ;; *) die "unsupported architecture $(uname -m) (x86_64 and aarch64 only)" ;; esac
	_need="sudo udevadm"
	[ "$OPT_SYSTEM" = 0 ] || _need="$_need agetty"
	[ "$OPT_AGENT" = 0 ] || _need="$_need nft bash visudo"
	for _t in $_need; do
		command -v "$_t" >/dev/null 2>&1 || die "missing required tool: $_t"
	done
}

# render TEMPLATE OUT K=V...: substitute @K@ with V; leftovers are an error.
render() {
	_tpl=$1 _out=$2
	shift 2
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
	cat > "$_tmp"
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
	mv -f "$_tmp" "$_target"
}

block_present() {
	[ -f "$1" ] && grep -qx "$BLOCK_OPEN" "$1"
}

# block_write FILE: stdin is the block body; insert or replace the block.
block_write() {
	_bf=$(resolve_link "$1")
	_body=$(mktemp)
	cat > "$_body"
	if block_present "$_bf"; then
		awk -v o="$BLOCK_OPEN" -v c="$BLOCK_CLOSE" -v bf="$_body" '
			$0 == o { print; while ((getline l < bf) > 0) print l; skip = 1; next }
			skip && $0 == c { skip = 0 }
			!skip { print }
		' "$_bf" | atomic_write "$_bf"
	else
		{
			if [ -s "$_bf" ]; then
				cat "$_bf"
				[ -z "$(tail -c 1 "$_bf")" ] || echo
			fi
			echo "$BLOCK_OPEN"
			cat "$_body"
			echo "$BLOCK_CLOSE"
		} | atomic_write "$_bf"
	fi
	rm -f "$_body"
}

# block_remove FILE: drop the block, leaving every other byte alone.
block_remove() {
	block_present "$1" || return 0
	sed -n "/^$BLOCK_OPEN\$/,/^$BLOCK_CLOSE\$/!p" "$(resolve_link "$1")" | atomic_write "$1"
}

main() {
	parse_args "$@"
	preflight
	# Tasks 6-8 add the install and uninstall steps here.
}

[ -n "${MUTHUR_INSTALL_LIB:-}" ] || main "$@"
