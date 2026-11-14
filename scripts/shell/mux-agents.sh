# shellcheck shell=bash
# shellcheck disable=SC2329 # cc and gg are called by the person, after this file is sourced
# Shortcuts for bash 4+ and zsh 5+, sourced from a profile: `. "<plugin folder>/scripts/shell/mux-agents.sh"`.
# cc runs `<entry> up -Client claude -- <args>`, gg the same with copilot; the arguments pass as given after a literal
# `--`, and the exit code is the entry's. The entry is $MUX_AGENTS_HOME/scripts/mux-agent when MUX_AGENTS_HOME is set
# and not empty, otherwise the one two folders above this file, as sourced (symlinks unresolved); it is made absolute
# once, here. Sourcing touches cc, gg and __mux_agents_entry only, changes no shell option, and may be repeated. cc
# shadows the C compiler; type `claude` or `copilot` for the bare client.

# Computed in a command substitution, so no other name is set in the caller's shell. A missing entry warns and keeps
# an earlier one, if any.
__mux_agents_entry=$(
  if [ -n "${ZSH_VERSION-}" ]; then
    # zsh names the file being sourced with the prompt escape %x; bash never parses this line.
    # shellcheck disable=SC2016
    eval 'src=${(%):-%x}'
  else
    src=${BASH_SOURCE[0]}
  fi
  if [ -n "${MUX_AGENTS_HOME-}" ]; then
    entry=$MUX_AGENTS_HOME/scripts/mux-agent
  else
    case $src in (*/*) entry=${src%/*}/../../scripts/mux-agent ;; (*) entry=../../scripts/mux-agent ;; esac
  fi
  case $entry in (/* | [A-Za-z]:[/\\]*) ;; (*) entry=$PWD/$entry ;; esac
  if [ -f "$entry" ]; then
    printf '%s\n' "$entry"
  else
    printf '%s\n' "mux-agents: no entry at $entry; source scripts/shell/mux-agents.sh from the plugin folder or set MUX_AGENTS_HOME" >&2
    printf '%s\n' "${__mux_agents_entry-}"
  fi
)
if [ -n "$__mux_agents_entry" ]; then
  function cc { "$__mux_agents_entry" up -Client claude -- "$@"; }
  function gg { "$__mux_agents_entry" up -Client copilot -- "$@"; }
else
  unset __mux_agents_entry
fi
