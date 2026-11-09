// A message delivered as herdr delivers one (#92): when this session's Bash tool runs `mux-agent send` and it answers
// a Claude Code recipient's session, that session is told at once, as its next input, that mail is waiting, instead
// of finding it only when its `read -Wait` next looks. The mailbox stays the message: the wake-up names the sender and
// says to read.
import type { EngineInterface } from 'claude-code'

// The entry as a whole file name, then the verb send (as board.ts finds spawn).
const ENTRY = /(^|[\\/\s"'])mux-agent(\.cmd|\.exe)?["']?\s+["']?send["']?(\s|$)/i
const SESSION = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/
const NAME = /^([a-z][a-z0-9-]{0,23}|root)$/

// The send reply on one line of stdout: ok, verb send, a sender's name and the recipient's session, or none.
export function sendReply(command: string, stdout: string | undefined): { from: string; session: string } | undefined {
  if (!ENTRY.test(command)) return undefined
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

// The wake-up, tagged as herdr tags a child's report: only a fixed sentence and a checked name, never the body.
export function wakeUp(from: string): string {
  return `<mux-agents from="${from}">You have a new mux-agents message from ${from}. Run your read command to get it.</mux-agents>`
}

async function push($: EngineInterface, command: string, stdout: string | undefined) {
  const reply = sendReply(command, stdout)
  if (!reply) return
  await $.session.send({ to: { sessionId: reply.session }, text: wakeUp(reply.from) })
}

// After a Bash or PowerShell call that ran send (register.tsx's one tool.call hook calls it): the push runs on its
// own, never delays or changes the tool result, and a failure (the recipient not running, no such session) is
// dropped: its read -Wait still finds the mail.
export function pushAfterSend($: EngineInterface, command: string, stdout: string | undefined) {
  push($, command, stdout).catch(() => {})
}
