# lig: run this interactive nu inside lig (once per terminal; Linux and macOS).
if $nu.os-info.family == "unix" and $nu.is-interactive and ($env.LIG_DISABLE? | is-empty) and (is-terminal --stdin) and (is-terminal --stdout) and ($env.TERM? != "dumb") and ($env.INSIDE_EMACS? | is-empty) and (@LIG_BIN_NU@ | path exists) and ($env.LIG_TTY? != (^tty | complete | get stdout | str trim)) {
    if $nu.is-login {
        exec @LIG_BIN_NU@ shell --shell $nu.current-exe --login
    } else {
        exec @LIG_BIN_NU@ shell --shell $nu.current-exe
    }
}
