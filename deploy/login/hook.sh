case $- in *i*) if [ -z "${MUTHUR_LOGIN:-}" ] && [ -x "$HOME/.local/bin/muthur-login" ]; then MUTHUR_LOGIN=1; export MUTHUR_LOGIN; "$HOME/.local/bin/muthur-login"; fi ;; esac
