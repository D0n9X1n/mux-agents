# Child of bin/claude. Worker: record argv, tty and pids; once the driver has seen the root's read -Wait waiting and
# writes reading, reply to the parent and mark the time; try a spawn; wait for quit. Root: leave a full-bypass Claude's
# evidence, run the driver, wait for quit.
d=$MUX_IT_DIR; sid=; prev=
for a in "$@"; do [ "$prev" = --session-id ] && sid=$a; prev=$a; done
if [ -n "$sid" ]; then
  printf '%s\n' "$@" > "$d/$sid.argv"; echo "$MUX_IT_TTY" > "$d/$sid.tty"; echo "$PPID $$" > "$d/$sid.pids"; echo "$MUX_AGENTS_STATE" > "$d/$sid.state"
  echo 'worker ready'; echo "reply from $sid" > "$d/$sid.reply"; echo nested > "$d/$sid.task"
  while [ ! -e "$d/reading" ]; do sleep 1; done
  "$MUX_IT_PWSH" -NoProfile -File "$MUX_IT_ENTRY" send parent -BodyFile "$d/$sid.reply" > "$d/$sid.send.json" 2>&1
  : > "$d/$sid.sent"
  "$MUX_IT_PWSH" -NoProfile -File "$MUX_IT_ENTRY" spawn -Client claude -TaskFile "$d/$sid.task" > "$d/$sid.spawn.json" 2>&1
  : > "$d/$sid.done"
else
  echo "$MUX_IT_TTY" > "$d/root.tty"
  printf '{"pid":%s,"sessionId":"%s","cwd":"%s"}\n' "$PPID" "$CLAUDE_CODE_SESSION_ID" "$(pwd -P)" > "$HOME/.claude/sessions/$PPID.json"
  echo 'bypass permissions on (shift+tab to cycle)'
  export MUX_AGENTS_STATE="$MUX_IT_STATE"   # set in the caller's shell only, after the server started
  "$MUX_IT_PWSH" -NoProfile -File "$MUX_IT_DRIVER" > "$d/driver.log" 2>&1 < /dev/null
fi
while [ ! -e "$d/quit" ]; do sleep 1; done
