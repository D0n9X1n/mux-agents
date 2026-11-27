import { atom, read, update } from 'claude-code'
import type { EngineInterface, Register, Timer } from 'claude-code'

import { activity, reportSession } from './activity'
import { argv, arm, onSubmit, run } from './deliver'
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

// Per module load: whether this hc terminal is a mux-agents team member, as `read -Count` answers it (deliver.ts
// eligible). A member, a non-member, or a `herdr`, `tagged` or `target` refusal is kept; any other failure asks again.
let hcMember: boolean | undefined

async function teamMember($: EngineInterface): Promise<boolean> {
  if (hcMember !== undefined) return hcMember
  const c = await run($, ['read', '-Count'])
  if (c && c.ok !== false) hcMember = c.member === true
  else if (c && /^(herdr|tagged|target)$/.test(String(c.check))) hcMember = false
  return hcMember === true
}

// Terminal CLI only, read fresh each time and never cached: the session draws on the terminal and nothing else
// ($.ui.open has no surface selector, so with a desktop, mobile or IDE client attached too the pane would show there as
// an empty tab), it runs inside tmux or rmux, or in an hc terminal as a team member (; a plain herdr terminal stays
// without the pane), and it is not a cloud session. A refused read, or no surface at all (a headless or SDK run), is
// not eligible. The surfaces, the reading most likely to change, are read last.
async function eligible($: EngineInterface): Promise<boolean> {
  try {
    if ((await $.env.get('CLAUDE_CODE_REMOTE')) === 'true') return false
    if (!((await $.env.get('TMUX')) || (await $.env.get('RMUX')))) {
      if (!(await $.env.get('HC_TERMINAL_ID')) || !(await teamMember($))) return false
    }
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

// /mux-agents-sidebar's word to the entry's `sidebar` form: none or `toggle`, `open`, `close`; any other is undefined.
const SIDEBAR_FORMS: Record<string, string[]> = { '': ['sidebar', '-Toggle'], toggle: ['sidebar', '-Toggle'], open: ['sidebar'], close: ['sidebar', '-Close'] }
const SIDEBAR_USAGE = 'usage: /mux-agents-sidebar [open|close|toggle]'

// The person's /mux-agents-sidebar: one run of the entry's `sidebar`, with no model turn. The binary decides who may
// (a person's chat or the team's leader; an agent's pane is refused `tagged`) and which bars open or close; its one JSON
// line becomes one line of text. Run through `argv` (deliver.ts), not `run`, which stops mail delivery on a `tagged`
// answer. A close walks every window, so it may take longer than a mail read.
async function sidebarCommand($: EngineInterface, args: string): Promise<string> {
  const word = args.trim().toLowerCase()
  // Own keys only: a word such as `constructor` is no form.
  const form = Object.hasOwn(SIDEBAR_FORMS, word) ? SIDEBAR_FORMS[word] : undefined
  if (!form) return SIDEBAR_USAGE
  let r: Record<string, unknown> | undefined
  try {
    const ran = await $.process.run(await argv($, form), { timeoutMs: 30000 })
    r = JSON.parse(ran.stdout.trim().split('\n').at(-1) ?? '') as Record<string, unknown>
  } catch {
    r = undefined
  }
  const silent = 'mux-agents: the side bar command did not answer.'
  if (!r || typeof r !== 'object') return silent
  if (r.ok === false) return `mux-agents: ${String(r.message ?? r.check ?? 'refused')}`
  // Only a reply that says what it did is shown as done.
  if (typeof r.closed === 'boolean') {
    // Only a list of pane names says which bars closed; closed with none named, or not closed with one named,
    // contradicts itself. Either way nothing is shown as done.
    if (!Array.isArray(r.panes) || !r.panes.every(p => typeof p === 'string' && p !== '')) return silent
    const panes = r.panes as string[]
    if (r.closed !== panes.length > 0) return silent
    return r.closed ? `Side bar closed: ${panes.join(', ')}.` : 'Side bar was already off.'
  }
  if (typeof r.opened !== 'boolean' || typeof r.pane !== 'string' || !r.pane) return silent
  return `${r.opened ? 'Side bar opened' : 'Side bar is on'}: ${r.pane}. Every agent window gets its own.`
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
  // report-2b K3: session.start carries no session id; the classic SessionStart (startup, resume, clear, compact)
  // does. Reported without waiting.
  on('classic.SessionStart', async ($, e, next) => {
    const input = e as { session_id?: unknown; source?: unknown }
    reportSession($, input.session_id, input.source)
    return next(e)
  })
  // The skill skills/mux-agents owns /mux-agents, so the pane command has a name of its own, registered only in an
  // eligible session. A refused registration (another owner of the name) leaves the session and the open-on-spawn
  // path working.
  on('session.start', async ($, e, next) => {
    const here = await eligible($)
    try {
      if (here) await $.command.register({ name: 'mux-agents-pane', description: 'Show the mux-agents board in a pane' })
    } catch {
      // The pane still opens after a spawn; only the command is missing.
    }
    // Its own try: a refused name for one command leaves the other registered.
    try {
      if (here) {
        await $.command.register({
          name: 'mux-agents-sidebar',
          description: 'Open, close or toggle the side bar in every agent window of this session',
          argumentHint: '[open|close|toggle]',
          // Typed while a turn streams, it runs at once: it reads no turn state.
          immediate: true,
        })
      }
    } catch {
      // The binary's `sidebar` still runs from the skill; only the typed command is missing.
    }
    // The wake timer of a member (deliver.ts): armed once per load, never with MUX_AGENTS_WAKE=0.
    await arm($).catch(() => {})
    return next(e)
  })
  // Registered while eligible does not mean eligible now: the command checks again.
  on('command.run', { command: 'mux-agents-pane' }, async $ => {
    if (!(await eligible($)) || !(await openPane($))) return { text: NOT_HERE }
    return { text: 'Agents pane opened.' }
  })
  // Registered while eligible does not mean eligible now: the command checks again, and runs nothing when not.
  on('command.run', { command: 'mux-agents-sidebar' }, async ($, e) => {
    if (!(await eligible($))) return { text: NOT_HERE }
    return { text: await sidebarCommand($, typeof e.args === 'string' ? e.args : '') }
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
    }
    return ran
  })
  // Mail at the turn of an interactive prompt (deliver.ts W2); a plugin wake passes every hook but this one.
  on('prompt.submit', async ($, e, next) => onSubmit($, e, next))
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
