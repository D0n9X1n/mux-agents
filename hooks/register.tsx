import { atom, read, update } from 'claude-code'
import type { EngineInterface, Register, Timer } from 'claude-code'

import { activity } from './activity'
import { pushAfterSend } from './push'
import { EMPTY, formatAge, isSpawn, look, mail, needYou, parseList, sections } from './board'

const PANE = 'mux-agents'
// Change the shape tag when the board's fields change: it declines what an earlier version stored.
const board = atom({ plugin: 'mux-agents', key: 'board' } as const, EMPTY, { shape: 'board-4' })
// Per module load (a hot reload drops the timer and resets both): the one refresh timer while the pane is open, and at
// most one list run at a time.
let ticking: Timer | undefined
let busy = false
// Bumped by every retire and by a client that is not the terminal attaching: an open begun before the bump is no longer
// current, so it starts no timer and no list and closes the pane it opened.
let epoch = 0

const NOT_HERE = 'mux-agents works only in a terminal CLI inside tmux or rmux.'

// Terminal CLI only (#43), read fresh each time and never cached: the session draws on the terminal and nothing else
// ($.ui.open has no surface selector, so with a desktop, mobile or IDE client attached too the pane would show there as
// an empty tab), it runs inside tmux or rmux, and it is not a cloud session. A refused read, or no surface at all
// (a headless or SDK run), is not eligible. The surfaces, the reading most likely to change, are read last.
async function eligible($: EngineInterface): Promise<boolean> {
  try {
    if ((await $.env.get('CLAUDE_CODE_REMOTE')) === 'true') return false
    if (!((await $.env.get('TMUX')) || (await $.env.get('RMUX')))) return false
    const surfaces = await $.session.surfaces()
    return surfaces.length === 1 && surfaces[0] === 'terminal'
  } catch {
    return false
  }
}

// MUX_AGENTS_ASCII=1 draws ASCII glyphs, as the side bar does (src/paint.rs ascii); an unreadable value draws symbols.
async function asciiGlyphs($: EngineInterface): Promise<boolean> {
  try {
    return (await $.env.get('MUX_AGENTS_ASCII')) === '1'
  } catch {
    return false
  }
}

// Stops the refresh at once, then asks the pane to close. The close is best effort: a hook may refuse it or answer
// without closing, and that pane keeps its last board while nothing polls.
async function retire($: EngineInterface) {
  epoch++
  ticking?.cancel()
  ticking = undefined
  try {
    await $.ui.close({ id: PANE })
  } catch {
    // Kept open below; the refresh is already stopped.
  }
}

// The entry shim's `list -All`. Hooks have no `process`, so the OS comes from the environment; an unreadable OS is
// taken as not Windows.
async function listArgv($: EngineInterface): Promise<string[]> {
  const root = $.plugin.root
  let os
  try { os = await $.env.get('OS') } catch { os = undefined }
  if (os === 'Windows_NT') return ['cmd.exe', '/d', '/c', `${root}\\scripts\\mux-agent.cmd`, 'list', '-All']
  return ['sh', `${root}/scripts/mux-agent`, 'list', '-All']
}

// One list run for `timer`. Eligibility is checked first on every tick, even while the last list still runs, and right
// before list runs; a timer already cancelled runs nothing. `busy` only keeps a second list from overlapping.
async function refresh($: EngineInterface, argv: string[], timer: Timer) {
  const ok = await eligible($)
  if (ticking !== timer) return
  if (!ok) {
    await retire($)
    return
  }
  if (busy) return
  busy = true
  try {
    let next
    try {
      const ran = await $.process.run(argv, { timeoutMs: 10000 })
      next = parseList(ran.exitCode, ran.stdout)
    } catch {
      next = undefined
    }
    if (!next) {
      await update($, board, last => ({ ...last, failed: true }))
      return
    }
    const fresh = next
    await update($, board, () => ({ ...fresh, failed: false }))
    $.ui.status(`agents: ${fresh.running}`)
  } finally {
    busy = false
  }
}

// A refresh started and not awaited: a rejection (a failed state write) is dropped, and the next tick tries again.
function startRefresh($: EngineInterface, argv: string[], timer: Timer) {
  refresh($, argv, timer).catch(() => {})
}

// Opens the pane; the first refresh runs on its own, so neither /mux-agents-pane nor a tool result waits for list.
// Eligibility may be lost while the open waits, so it is checked after every other wait of the open: when it is lost,
// or the open is no longer current, no timer starts and the pane just opened is closed. Each refresh checks again
// before list runs. Answers whether the pane is open and refreshing.
async function openPane($: EngineInterface): Promise<boolean> {
  const since = epoch
  await $.ui.open({ id: PANE, title: 'Agents' })
  const argv = await listArgv($)
  const ok = await eligible($)
  if (since !== epoch || !ok) {
    await retire($)
    return false
  }
  if (!ticking) {
    const timer: Timer = $.clock.every(3000, () => startRefresh($, argv, timer))
    ticking = timer
    startRefresh($, argv, timer)
  }
  return true
}

// After a Bash or PowerShell call: opens the pane when the call was a successful spawn in an eligible session.
async function openAfterSpawn($: EngineInterface, command: string, stdout: string | undefined) {
  if (isSpawn(command, stdout) && (await eligible($))) await openPane($)
}

// After a close of the pane: stop its refresh only when the pane is really gone, so a later open starts one new timer.
// A close refused below, or answered without closing, leaves the pane listed and its refresh running. When the list
// cannot be read the refresh runs on: a spare list every 3 s costs less than a pane that stops updating.
async function stopRefreshIfGone($: EngineInterface) {
  let panes
  try {
    panes = await $.ui.panes()
  } catch {
    return
  }
  if (panes.some(p => p.id === PANE)) return
  ticking?.cancel()
  ticking = undefined
}

export const register: Register = on => {
  // Every session in tmux or rmux reports its own activity; only an agent's pane is answered.
  activity(on)
  // The skill skills/mux-agents owns /mux-agents, so the pane command has a name of its own, registered only in an
  // eligible session. A refused registration (another owner of the name) leaves the session and the open-on-spawn
  // path working.
  on('session.start', async ($, e, next) => {
    try {
      if (await eligible($)) await $.command.register({ name: 'mux-agents-pane', description: 'Show the mux-agents board in a pane' })
    } catch {
      // The pane still opens after a spawn; only the command is missing.
    }
    return next(e)
  })
  // Registered while eligible does not mean eligible now: the command checks again.
  on('command.run', { command: 'mux-agents-pane' }, async $ => {
    if (!(await eligible($)) || !(await openPane($))) return { text: NOT_HERE }
    return { text: 'Agents pane opened.' }
  })
  // A desktop, mobile or IDE client attaching: an open still pending is no longer current, and with the pane open the
  // refresh stops at once and the pane is asked to close, without holding up the attach.
  on('session.attach', async ($, e, next) => {
    if (e.surface !== 'terminal') {
      epoch++
      if (ticking) retire($).catch(() => {})
    }
    return next(e)
  })
  // The pane never delays or changes the tool result: the spawn check and the open run on their own, and any rejection
  // ($.env.get, $.ui.open) is dropped.
  on('tool.call', async ($, e, next) => {
    const ran = await next(e)
    const tool = String(e.tool)
    if ((tool === 'Bash' || tool === 'PowerShell') && ran.deny === undefined && ran.isError !== true) {
      const stdout = (ran.result as { stdout?: unknown } | undefined)?.stdout
      const command = String((e as { command?: unknown }).command ?? '')
      const out = typeof stdout === 'string' ? stdout : undefined
      openAfterSpawn($, command, out).catch(() => {})
      // A send to a Claude Code session wakes it at once, as herdr delivers a message (#92).
      pushAfterSend($, command, out)
    }
    return ran
  })
  // Any origin (the person, a plugin): the close goes on first; its answer is returned unchanged.
  on('ui.close', async ($, e, next) => {
    const closed = await next(e)
    if (e.id === PANE) await stopRefreshIfGone($)
    return closed
  })
  // Every string drawn here comes from parseList, so it is already cleaned. The terminal only: a pane kept open
  // elsewhere draws nothing. Rows look as on the side bar: glyph and state word in the state's color, `✉ N` for a
  // live agent's unread messages; a lost agent is not drawn.
  on('ui.render', { component: 'Pane', requestId: PANE, surface: 'terminal' }, async ($, e) => {
    const { Box, Text } = $.ui.resolve(e)
    const b = await read($, board)
    const ascii = await asciiGlyphs($)
    // Who needs the person leads the header, as on the side bar; tolerant of a stored board without activity.
    const need = needYou(Array.isArray(b.agents) ? b.agents : [])
    const headings = sections(Array.isArray(b.agents) ? b.agents : [], ascii)
    return (
      <Box flexDirection="column">
        <Text>mux-agents · {need > 0 ? <Text color="yellow">{need} need you · </Text> : null}{b.running} running{b.failed ? ' · refresh failed' : ''}</Text>
        {b.agents.map((a, at) => {
          const row = look(a, ascii)
          if (!row) return null
          const heading = headings[at]
          // A live agent's chosen model follows its client, then its unread badge.
          const tag = !row.ended && a.model ? ` [${a.model}]` : ''
          const badge = row.ended ? undefined : mail(a.unread, ascii)
          // A live row's time is since its activity last changed, as on the side bar.
          const age = !row.ended && typeof a.since === 'number' ? a.since : a.age
          const loud = row.pill || row.word === 'done' || undefined
          // Under herdr's section headings a live row shows the compact status, glyph and time: the heading names it.
          const compact = !row.ended && headings.some(Boolean)
          const dot = ascii ? '-' : '\u00b7'
          const [title, count] = heading ? heading.split(` ${dot} `) : []
          return (
            <Box flexDirection="column">
              {heading ? <Text bold color={row.color} dimColor={row.muted || undefined}>{title}<Text dimColor> {dot} {count}</Text></Text> : null}
              <Text dimColor={row.ended || row.muted || undefined}>
                <Text color={row.color} bold={loud}>{row.glyph}</Text> {a.name}  {a.client}{tag}  {badge ? <Text bold color="blue">{badge}</Text> : null}{badge ? '  ' : ''}{compact ? null : <Text color={row.color} bold={loud} inverse={row.pill || undefined}>{row.word}</Text>}{compact ? '' : '  '}{formatAge(age)}
              </Text>
              {a.doing ? <Text dimColor>  {a.doing}</Text> : null}
            </Box>
          )
        })}
      </Box>
    )
  })
}
