# shellcheck shell=bash
# shellcheck disable=SC2329 # cc and gg are called by the person, after this file is sourced
# Shortcuts for bash 4+ and zsh 5+, sourced from a profile: `. "<plugin folder>/scripts/shell/mux-agents.sh"`.
# cc runs `<entry> up -Client claude -- <args>`, gg the same with copilot; the arguments pass as given after a literal
# `--`, and the exit code is the entry's. The entry is $MUX_AGENTS_HOME/scripts/mux-agent when MUX_AGENTS_HOME is set,
# otherwise the one two folders above this file, as sourced (symlinks unresolved); it is made absolute once, here.
# Sourcing defines cc, gg and __mux_agents_entry only, changes no shell option, and may be repeated. cc shadows the C
# compiler; type `claude` or `copilot` for the bare client.

__mux_agents_init() {
  local source="$1" home="${MUX_AGENTS_HOME-}" entry=''
  if [ -n "$home" ]; then
    entry="$home/scripts/mux-agent"
  else
    case $source in */*) entry="${source%/*}/../../scripts/mux-agent" ;; *) entry="../../scripts/mux-agent" ;; esac
  fi
  case $entry in /* | [A-Za-z]:[/\\]*) ;; *) entry="$PWD/$entry" ;; esac
  if [ ! -f "$entry" ]; then
    printf '%s\n' "mux-agents: no entry at $entry; source scripts/shell/mux-agents.sh from the plugin folder or set MUX_AGENTS_HOME" >&2
    return 0
  fi
  __mux_agents_entry=$entry
  # Git Bash, MSYS and Cygwin rewrite path-like arguments for a native program unless told not to; the two variables
  # are set for the call only.
  case $(uname -s) in
    MINGW* | MSYS* | CYGWIN*)
      function cc { MSYS2_ARG_CONV_EXCL='*' MSYS_NO_PATHCONV=1 "$__mux_agents_entry" up -Client claude -- "$@"; }
      function gg { MSYS2_ARG_CONV_EXCL='*' MSYS_NO_PATHCONV=1 "$__mux_agents_entry" up -Client copilot -- "$@"; }
      ;;
    *)
      function cc { "$__mux_agents_entry" up -Client claude -- "$@"; }
      function gg { "$__mux_agents_entry" up -Client copilot -- "$@"; }
      ;;
  esac
}

if [ -n "${ZSH_VERSION-}" ]; then
  # zsh names the file being sourced with the prompt escape %x; bash never parses this line.
  eval '__mux_agents_init "${(%):-%x}"'
else
  __mux_agents_init "${BASH_SOURCE[0]}"
fi
unset -f __mux_agents_init
