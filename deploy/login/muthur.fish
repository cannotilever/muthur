if status is-login; and status is-interactive; and not set -q MUTHUR_LOGIN
    if test -x ~/.local/bin/muthur-login
        set -gx MUTHUR_LOGIN 1
        ~/.local/bin/muthur-login
    end
end
