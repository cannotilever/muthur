if status is-login; and status is-interactive
    and test (tty) = (realpath /dev/wy60 2>/dev/null)
    stty rows 43 cols 80 erase ^H ixon ixoff
    ~/.local/bin/muthur
end
