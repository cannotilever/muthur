#!/bin/sh
# shellcheck disable=SC2031,SC2013 # tests run in subshells on purpose; test names have no spaces
# Plain-sh test harness for install.sh. Run: sh tests/install/run.sh
set -u
HERE=$(cd -P "$(dirname "$0")" && pwd)
REPO=$(cd -P "$HERE/../.." && pwd)
SELF=$HERE/run.sh
FAILS=0
if [ -n "${MUTHUR_TEST_SH:-}" ]; then SHELL_BIN=$MUTHUR_TEST_SH
elif command -v dash >/dev/null 2>&1; then SHELL_BIN=dash
else SHELL_BIN=$(command -v sh); fi

eq() { # expected actual msg
	[ "$1" = "$2" ] || { echo "  expected [$1] got [$2] ($3)" >&2; return 1; }
}
ok() { # cmd... msg (last argument is the message)
	for _msg; do :; done
	"$@" 2>/dev/null || { echo "  failed: $_msg" >&2; return 1; }
}
# check cmd... : fails the test with msg when cmd fails
chk() { _m=$1; shift; "$@" || { echo "  check failed: $_m" >&2; return 1; }; }

# dies pattern cmd... : cmd must exit 1 and stderr must contain pattern
dies() {
	_pat=$1; shift
	_out=$( ("$@") 2>&1 >/dev/null ) && { echo "  did not die" >&2; return 1; }
	_rc=$?
	[ "$_rc" -eq 1 ] || { echo "  status $_rc, want 1" >&2; return 1; }
	case $_out in *"$_pat"*) ;; *) echo "  message [$_out] lacks [$_pat]" >&2; return 1 ;; esac
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
t_parse_args_unknown() {
	dies bogus parse_args --bogus
	parse_args --shell zsh --no-agent
	eq zsh "$OPT_SHELL" shell
	eq 0 "$OPT_AGENT" agent
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
