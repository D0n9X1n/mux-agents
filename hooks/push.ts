// A message delivered as herdr delivers one (#92): when this session's Bash tool runs `mux-agent send` and it answers
// a Claude Code recipient's session, that session is told at once, as its next input, that mail is waiting, instead
// of finding it only when its `read -Wait` next looks. The mailbox stays the message: the wake-up names the sender and
// says to read.
import type { EngineInterface } from 'claude-code'

// The whole command is one run of this plugin's own entry's send, every word a literal no shell reads: in sh, cmd
// and the Windows shell tool alike, nothing in it runs, expands or prints, so the stdout read below is the entry's own
// reply, never a file's or another program's (a grep naming the entry, `printf${IFS}…`, a `(…)` expression). A word
// is plain characters, or plain characters and spaces in '…' or "…" (no \ before the closing ", which bash reads as an
// escape). Optionally after `& `, `sh ` or `cmd /d /c `; the first word is the entry's path.
const PLAIN = String.raw`[A-Za-z0-9_./:\\=+,-]`
const WORD = String.raw`(?:${PLAIN}+|'(?:${PLAIN}| )*'|"(?:[A-Za-z0-9_./:=+, -]|\\(?!"))*")`
// Words part at spaces and tabs only: a line break would start a second command.
const ONE_SEND = new RegExp(String.raw`^[ \t]*(?:&[ \t]*|sh[ \t]+|cmd[ \t]+/d[ \t]+/c[ \t]+)?(${WORD})[ \t]+send((?:[ \t]+${WORD})*)[ \t]*$`)
// The files the entry is under the plugin root: the shims, or a binary the shim runs.
const ENTRY_FILE = /^\/(scripts\/mux-agent(\.cmd)?|scripts\/bin\/[a-z0-9_-]+\/mux-agent(\.exe)?|target\/release\/mux-agent(\.exe)?)$/
// As checks::session_id: either case.
const SESSION = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i
const NAME = /^([a-z][a-z0-9-]{0,23}|root)$/

// The send reply on one line of stdout: ok, verb send, a sender's name and the recipient's session, or none. Several
// sends in one command wake no one: each recipient's read -Wait still finds its mail.
export function sendReply(command: string, stdout: string | undefined, root: string): { from: string; session: string } | undefined {
  if (!isEntrySend(command, root)) return undefined
  for (const line of (stdout ?? '').replace(/\r\n/g, '\n').split('\n')) {
    if (!line.trim().startsWith('{')) continue
    let reply: unknown
    try { reply = JSON.parse(line) } catch { continue }
    const r = reply as { ok?: unknown; verb?: unknown; from?: unknown; toSessionId?: unknown } | null
    if (r?.ok !== true || r.verb !== 'send') continue
    if (typeof r.from !== 'string' || !NAME.test(r.from)) continue
    if (typeof r.toSessionId !== 'string' || !SESSION.test(r.toSessionId)) continue
    return { from: r.from, session: r.toSessionId }
  }
  return undefined
}

// True when the command is one send by the entry at root, as ONE_SEND says. A Windows root (`C:\…`, `\\server\…`)
// compares with / for \ and in any case, as Windows reads paths; a POSIX root (`/…`) compares exactly, as a \ there
// is a character of a file name, never a folder.
export function isEntrySend(command: string, root: string): boolean {
  const m = ONE_SEND.exec(command)
  if (!m || !root) return false
  const windows = !root.startsWith('/')
  // In sh a \ may be an escape the shell removes (unquoted, or before \ in "…"): the file run would not be the path
  // read here. A POSIX entry path holding one is never trusted.
  if (!windows && m[1].includes('\\')) return false
  let path = m[1].replace(/^['"]|['"]$/g, '')
  let base = root
  if (windows) {
    path = path.replace(/\\/g, '/').toLowerCase()
    base = base.replace(/\\/g, '/').toLowerCase()
  }
  base = base.replace(/\/+$/, '')
  return !path.split('/').includes('..') && path.startsWith(base) && ENTRY_FILE.test(path.slice(base.length))
}

// The wake-up, tagged as herdr tags a child's report: only a fixed sentence and a checked name, never the body.
export function wakeUp(from: string): string {
  return `<mux-agents from="${from}">You have a new mux-agents message from ${from}. Run your read command to get it.</mux-agents>`
}

async function push($: EngineInterface, command: string, stdout: string | undefined) {
  const reply = sendReply(command, stdout, $.plugin.root)
  if (!reply) return
  await $.session.send({ to: { sessionId: reply.session }, text: wakeUp(reply.from) })
}

// After a Bash or PowerShell call that ran send (register.tsx's one tool.call hook calls it): the push runs on its
// own, never delays or changes the tool result, and a failure (the recipient not running, no such session) is
// dropped: its read -Wait still finds the mail.
export function pushAfterSend($: EngineInterface, command: string, stdout: string | undefined) {
  push($, command, stdout).catch(() => {})
}
