# Cache generated Zsh integration so managed shells don't start Lig on every
# startup. The cache is trusted only when every directory is private or owned
# by root and non-writable by other users.
_lig_load_init() {
  emulate -L zsh
  local part=$1 binary=@LIG_BIN_QUOTED@ cache_dir=@LIG_CACHE_DIR_QUOTED@ bin_key=@LIG_BIN_KEY@
  [[ $part == pre || $part == post ]] || return 1
  [[ -n $binary && -x $binary ]] || return 1

  local cache header expected tmp init cached dir
  local -A binary_stat cache_stat dir_stat
  if zmodload -F zsh/stat b:zstat 2>/dev/null && zstat -H binary_stat -- "$binary"; then
    expected="# lig-init-v2 ${ZSH_VERSION} ${binary:A} ${binary_stat[device]} ${binary_stat[inode]} ${binary_stat[mtime]} ${binary_stat[ctime]} ${binary_stat[size]}"

    # Do not create a cache through a symlink or a directory writable by other
    # users. Sticky, root-owned temporary ancestors are safe; no temp fallback
    # is selected when the user's cache directory is unavailable.
    dir=$cache_dir
    while [[ ! -e $dir && ! -L $dir ]]; do dir=${dir:h}; done
    local safe=1
    while [[ -n $dir && $dir != / ]]; do
      if [[ ! -d $dir || -L $dir ]] || ! zstat -H dir_stat -- "$dir" ||
          (( (dir_stat[mode] & 8#022) != 0 &&
             ! ((dir_stat[mode] & 8#1000) != 0 && dir_stat[uid] == 0) )); then
        safe=0
        break
      fi
      dir=${dir:h}
    done

    if (( safe )); then
      command mkdir -m 700 -p "$cache_dir" 2>/dev/null
      # Recheck after creation to catch symlink or permission changes.
      dir=$cache_dir
      while [[ -n $dir && $dir != / ]]; do
        if [[ ! -d $dir || -L $dir ]] || ! zstat -H dir_stat -- "$dir" ||
            (( (dir_stat[mode] & 8#022) != 0 &&
               ! ((dir_stat[mode] & 8#1000) != 0 && dir_stat[uid] == 0) )); then
          safe=0
          break
        fi
        dir=${dir:h}
      done
    fi

    cache="$cache_dir/${bin_key}-${ZSH_VERSION}-$part.zsh"
    if (( safe )) && [[ -d $cache_dir && ! -L $cache_dir && -O $cache_dir ]] &&
        zstat -H dir_stat -- "$cache_dir" && (( (dir_stat[mode] & 8#022) == 0 )) &&
        [[ -f $cache && -r $cache && ! -L $cache && -O $cache ]] &&
        zstat -H cache_stat -- "$cache" && (( (cache_stat[mode] & 8#022) == 0 )); then
      cached=$(< "$cache")
      if [[ $cached == "$expected"$'\n'*$'\n# lig-init-end-v2' ]]; then
        init=${cached#"$expected"$'\n'}
        init=${init%$'\n# lig-init-end-v2'}
        eval "$init"
        return
      fi
    fi

    init=$(command "$binary" init zsh "$part") || return
    [[ -n $init ]] || return 1
    if (( safe )) && [[ -d $cache_dir && ! -L $cache_dir && -O $cache_dir ]]; then
      tmp=$(command mktemp "$cache.XXXXXXXX" 2>/dev/null) || tmp=
      if [[ -n $tmp ]]; then
        {
          if print -rl -- "$expected" "$init" '# lig-init-end-v2' >| "$tmp"; then
            # Run the exact integration generated for this process. Never
            # reopen a shared cache path after publishing it.
            eval "$init"
            command mv -f "$tmp" "$cache" 2>/dev/null
            return
          fi
        } always {
          [[ ! -e $tmp ]] || command rm -f "$tmp"
        }
      fi
    fi
    eval "$init"
    return
  fi

  # No trusted cache location/stat module: keep startup correct without cache.
  init=$(command "$binary" init zsh "$part") || return
  eval "$init"
}
