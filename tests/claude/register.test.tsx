import { expect, mock, test } from 'claude-code/testing'

import { SPAWN_JSON, SPAWN_TEXT, TAGGED_JSON, TAGGED_TEXT } from './spawn-replies'

const PANE = {
  plugin: 'mux-agents', component: 'Pane', requestId: 'mux-agents',
  props: { title: 'Agents', isFocused: false, bodyColumns: 42, placement: 'dock', scroll: { offset: 0, bodyRows: 20 }, view: {} },
} as const
const SPAWN = 'pwsh -NoProfile -File "C:/p/scripts/mux-agent.ps1" spawn -Client claude -TaskFile t'
const row = (name: string, state: string, doing: string | null, age: number) => ({ name, client: 'claude', state, doing, age })
const list = (running: number, agents: object[]) => JSON.stringify({ ok: true, verb: 'list', running, agents })
const LIST = list(1, [row('claude-1', 'running', 'build it', 240), { ...row('copilot-1', 'exited', null, 900), client: 'copilot' }])
const ran = (exitCode: number, stdout: string) => ({ value: { exitCode, stdout, stderr: '', isStdoutTruncated: false, isStderrTruncated: false } })
const START = { cwd: '/', surface: 'terminal', isInteractive: true } as const
const header = async (ui: any) => (await ui.find({ type: 'Text', text: /running/ }))?.text

// The engine beneath the plugin. Each hook answers a call on $ with { value }.
// process.run answers from `runs` in order, the last repeating; `{ holdMs, answer }` answers after holdMs on the mock clock.
// tool.call answers with `stdout` as the tool's result, `text` as what the model reads and `isError` as the result's mark.
// `answers` replaces the engine's answer to command.register or ui.open, for instance with `{ deny }`; `openHoldMs`
// answers ui.open after that long on the mock clock; `denyWrites` refuses that many state writes first.
type Answers = { register?: object; open?: object; openHoldMs?: number; denyWrites?: number }
function world(on: any, env: Record<string, string>, runs: object[], tool: { stdout: string; text: string; isError?: boolean } = { stdout: SPAWN_JSON, text: SPAWN_JSON }, answers: Answers = {}) {
  const seen = { registered: [] as object[], opened: [] as object[], argv: [] as string[][], init: [] as unknown[], status: [] as (string | undefined)[], settled: 0 }
  mock.env(on, env)
  const clock = mock.clock(on)
  on('command.register', (_: unknown, e: object) => { seen.registered.push({ ...e }); return answers.register ?? { value: { command: 'mux-agents-pane' } } })
  on('ui.open', async (_: unknown, e: object) => {
    seen.opened.push({ ...e })
    if (answers.openHoldMs) await clock.sleep(answers.openHoldMs)
    return answers.open ?? { value: { isPlaced: true } }
  })
  let denyWrites = answers.denyWrites ?? 0
  if (denyWrites) on('state.set', (_: unknown, e: unknown, next: (e: unknown) => unknown) => (denyWrites-- > 0 ? { deny: 'disk full' } : next(e)))
  on('ui.status', (_: unknown, e: { text?: string }) => { seen.status.push(e.text); return { value: undefined } })
  on('process.run', async (_: unknown, e: { argv: string[]; init?: unknown }) => {
    seen.argv.push([...e.argv]); seen.init.push(e.init)
    const next: any = runs.length > 1 ? runs.shift() : runs[0]
    if (next?.holdMs) { await clock.sleep(next.holdMs); seen.settled++; return next.answer }
    seen.settled++
    return next
  })
  on('session.start', (_: unknown, e: { cwd: string }) => ({ cwd: e.cwd }))
  on('tool.call', () => ({ result: { stdout: tool.stdout, stderr: '', interrupted: false }, text: tool.text, ...(tool.isError === undefined ? {} : { isError: tool.isError }) }))
  return { seen, clock }
}

test('session.start registers exactly the mux-agents-pane command', async ($, on) => {
  const { seen } = world(on, {}, [ran(0, LIST)])
  await $.session.start(START)
  expect(seen.registered).toEqual([{ name: 'mux-agents-pane', description: 'Show the mux-agents board in a pane' }])
})

test('a refused command.register still lets session.start finish, and a spawn still opens the pane', async ($, on) => {
  const refused = { deny: '"/mux-agents-pane" refused: it is the plugin\'s /mux-agents:mux-agents' }
  const { seen, clock } = world(on, { TMUX: 'x' }, [ran(0, LIST)], undefined, { register: refused })
  let started: unknown = 'not settled'
  try { started = await $.session.start(START) } catch (err) { started = `threw ${String(err)}` }
  expect(started).toEqual({ cwd: '/' })
  expect(seen.registered.length).toBe(1)
  await $.tool.call({ tool: 'Bash', command: SPAWN })
  await clock.settle()
  expect(seen.opened).toEqual([{ id: 'mux-agents', title: 'Agents' }])
})

test('a Bash spawn inside rmux opens Agents, runs the exact list command and sets the status', async ($, on) => {
  const { seen, clock } = world(on, { RMUX: 'x' }, [ran(0, LIST)])
  let root = ''
  on('classic.SessionStart', (_: unknown, e: { cwd: string }) => { root = e.cwd; return {} })
  await ($ as any).classic.SessionStart({ source: 'startup' })
  await $.session.start(START)
  await $.tool.call({ tool: 'Bash', command: SPAWN })
  await clock.settle()
  expect(seen.opened).toEqual([{ id: 'mux-agents', title: 'Agents' }])
  expect(seen.argv).toEqual([['pwsh', '-NoProfile', '-File', `${root}/scripts/mux-agent.ps1`, 'list', '-All']])
  expect(seen.status).toEqual(['agents: 1'])
})

test('no pane and no refresh without TMUX or RMUX', async ($, on) => {
  const { seen, clock } = world(on, {}, [ran(0, LIST)])
  await $.session.start(START)
  await $.tool.call({ tool: 'Bash', command: SPAWN })
  await clock.settle()
  expect(seen.opened).toEqual([])
  expect(seen.argv).toEqual([])
})

test('inside tmux, a Bash call that is not a spawn opens nothing', async ($, on) => {
  const { seen, clock } = world(on, { TMUX: 'x' }, [ran(0, LIST)], { stdout: 'a.txt\n', text: 'a.txt\n' })
  await $.session.start(START)
  await $.tool.call({ tool: 'Bash', command: 'ls' })
  await clock.settle()
  expect(seen.opened).toEqual([])
})

test('inside tmux, a tagged child refused by spawn opens nothing', async ($, on) => {
  const { seen, clock } = world(on, { TMUX: 'x' }, [ran(0, LIST)], { stdout: TAGGED_JSON, text: TAGGED_JSON })
  await $.session.start(START)
  await $.tool.call({ tool: 'Bash', command: SPAWN })
  await clock.settle()
  expect(seen.opened).toEqual([])
})

test('a real spawn -Text success opens the pane', async ($, on) => {
  const { seen, clock } = world(on, { TMUX: 'x' }, [ran(0, LIST)], { stdout: `${SPAWN_TEXT}\n`, text: `${SPAWN_TEXT}\n` })
  await $.session.start(START)
  await $.tool.call({ tool: 'Bash', command: `${SPAWN} -Text` })
  await clock.settle()
  expect(seen.opened).toEqual([{ id: 'mux-agents', title: 'Agents' }])
})

test('a real spawn -Text tagged refusal opens nothing', async ($, on) => {
  const { seen, clock } = world(on, { TMUX: 'x' }, [ran(0, LIST)], { stdout: `${TAGGED_TEXT}\n`, text: `${TAGGED_TEXT}\n` })
  await $.session.start(START)
  await $.tool.call({ tool: 'Bash', command: `${SPAWN} -Text` })
  await clock.settle()
  expect(seen.opened).toEqual([])
})

test('a spawn whose tool result is an error opens nothing, even with a spawn reply in stdout', async ($, on) => {
  const { seen, clock } = world(on, { TMUX: 'x' }, [ran(0, LIST)], { stdout: SPAWN_JSON, text: SPAWN_JSON, isError: true })
  await $.session.start(START)
  await $.tool.call({ tool: 'Bash', command: SPAWN })
  await clock.settle()
  expect(seen.opened).toEqual([])
})

test('the spawn reply is read from the tool result, not the text the model reads', async ($, on) => {
  const { seen, clock } = world(on, { TMUX: 'x' }, [ran(0, LIST)], { stdout: 'nothing here', text: SPAWN_JSON })
  await $.session.start(START)
  await $.tool.call({ tool: 'Bash', command: SPAWN })
  await clock.settle()
  expect(seen.opened).toEqual([])
})

test('a PowerShell spawn opens the pane; a second spawn starts no second refresh', async ($, on) => {
  const { seen, clock } = world(on, { TMUX: 'x' }, [ran(0, LIST)])
  await $.session.start(START)
  await $.tool.call({ tool: 'PowerShell', command: `& "C:/p/scripts/mux-agent.ps1" spawn -Client copilot` })
  await clock.settle()
  await $.tool.call({ tool: 'PowerShell', command: `& "C:/p/scripts/mux-agent.ps1" spawn -Client copilot` })
  await clock.settle()
  expect(seen.opened.length).toBe(2)
  expect(seen.argv.length).toBe(1)
})

test('list runs once on open, then once at each 3000 ms boundary', async ($, on) => {
  const { seen, clock } = world(on, { TMUX: 'x' }, [ran(0, LIST)])
  await $.session.start(START)
  await $.command.run({ command: 'mux-agents-pane' })
  const counts = [seen.argv.length]
  for (const step of [2999, 1, 2999, 1]) { await clock.advance(step); counts.push(seen.argv.length) }
  expect(counts).toEqual([1, 1, 2, 2, 3])
})

test('a refresh never overlaps: a run held 4000 ms delays the next until it settles', async ($, on) => {
  const { seen, clock } = world(on, { TMUX: 'x' }, [{ holdMs: 4000, answer: ran(0, LIST) }, ran(0, LIST)])
  await $.session.start(START)
  const opened = $.command.run({ command: 'mux-agents-pane' })
  await clock.settle()
  await clock.advance(3500)
  expect([seen.argv.length, seen.settled]).toEqual([1, 0])
  await clock.advance(500)
  await opened
  expect([seen.argv.length, seen.settled]).toEqual([1, 1])
  await clock.advance(2000)
  expect([seen.argv.length, seen.settled]).toEqual([2, 2])
})

test('a spawn\'s tool result does not wait for the first refresh', async ($, on) => {
  const { seen, clock } = world(on, { TMUX: 'x' }, [{ holdMs: 4000, answer: ran(0, LIST) }, ran(0, LIST)])
  await $.session.start(START)
  let done = false
  const call = $.tool.call({ tool: 'Bash', command: SPAWN }).then(r => { done = true; return r })
  await clock.settle()
  expect([done, seen.opened.length, seen.argv.length, seen.settled]).toEqual([true, 1, 1, 0])
  const result: any = await call
  expect([result?.result?.stdout, result?.text]).toEqual([SPAWN_JSON, SPAWN_JSON])
  await clock.advance(4000)
  expect(seen.settled).toBe(1)
})

test('a spawn whose pane cannot open still returns the tool result unchanged', async ($, on) => {
  const { seen, clock } = world(on, { TMUX: 'x' }, [ran(0, LIST)], undefined, { open: { deny: 'no pane here' } })
  await $.session.start(START)
  let outcome: any
  try { outcome = await $.tool.call({ tool: 'Bash', command: SPAWN }) } catch (err) { outcome = `threw ${String(err)}` }
  await clock.settle()
  expect([outcome?.result?.stdout, outcome?.text, outcome?.isError]).toEqual([SPAWN_JSON, SPAWN_JSON, undefined])
  expect([seen.opened.length, seen.argv.length]).toEqual([1, 0])
})

test('a spawn\'s tool result does not wait for the pane to open', async ($, on) => {
  const { seen, clock } = world(on, { TMUX: 'x' }, [ran(0, LIST)], undefined, { openHoldMs: 4000 })
  await $.session.start(START)
  let done = false
  const call = $.tool.call({ tool: 'Bash', command: SPAWN }).then(r => { done = true; return r })
  await clock.settle()
  expect([done, seen.opened.length, seen.argv.length]).toEqual([true, 1, 0])
  await call
  await clock.advance(4000)
  expect(seen.argv.length).toBe(1)
})

test('a refresh whose board write is refused leaves the pane working, and the next refresh draws', async ($, on) => {
  const { seen, clock } = world(on, { TMUX: 'x' }, [ran(0, LIST)], undefined, { denyWrites: 1 })
  await $.session.start(START)
  await $.command.run({ command: 'mux-agents-pane' })
  await clock.settle()
  expect([seen.argv.length, seen.status]).toEqual([1, []])
  await clock.advance(3000)
  const ui = await $.ui.mount({ ...PANE, surface: 'terminal' })
  expect([seen.argv.length, seen.status, await header(ui)]).toEqual([2, ['agents: 1'], 'mux-agents · 1 running'])
  await ui.unmount()
})

test('/mux-agents-pane answers before the first refresh settles, and the board appears once it does', async ($, on) => {
  const { seen, clock } = world(on, { TMUX: 'x' }, [{ holdMs: 4000, answer: ran(0, LIST) }, ran(0, LIST)])
  await $.session.start(START)
  let done = false
  const run = $.command.run({ command: 'mux-agents-pane' }).then(r => { done = true; return r })
  await clock.settle()
  expect([done, seen.opened]).toEqual([true, [{ id: 'mux-agents', title: 'Agents' }]])
  const ui = await $.ui.mount({ ...PANE, surface: 'terminal' })
  expect(await header(ui)).toBe('mux-agents · 0 running')
  await clock.advance(4000)
  expect(await header(ui)).toBe('mux-agents · 1 running')
  await run
  await ui.unmount()
})

test('/mux-agents-pane opens the pane and a stubbed list draws the count and rows', async ($, on) => {
  const { seen, clock } = world(on, { TMUX: 'x' }, [ran(0, LIST)])
  await $.session.start(START)
  await $.command.run({ command: 'mux-agents-pane' })
  await clock.settle()
  expect(seen.opened).toEqual([{ id: 'mux-agents', title: 'Agents' }])
  for (const surface of ['terminal', 'desktop'] as const) {
    const ui = await $.ui.mount({ ...PANE, surface })
    expect(await header(ui)).toBe('mux-agents · 1 running')
    const live = await ui.find({ type: 'Text', text: /claude-1/ })
    expect([live?.text, live?.props.dimColor]).toEqual(['● claude-1  claude  4m', undefined])
    const doing = await ui.find({ type: 'Text', text: /build it/ })
    expect([doing?.text, doing?.props.dimColor]).toEqual(['  build it', true])
    const copilot = await ui.find({ type: 'Text', text: /copilot-1/ })
    expect([copilot?.text, copilot?.props.dimColor]).toEqual(['○ copilot-1  copilot  exited  15m', true])
    await ui.unmount()
  }
})

test('ended rows are dimmed and named by state: exited, stopped and lost', async ($, on) => {
  const { clock } = world(on, { TMUX: 'x' }, [ran(0, list(0, [row('a', 'exited', null, 60), row('b', 'stopped', null, 60), row('c', 'lost', null, 60)]))])
  await $.session.start(START)
  await $.command.run({ command: 'mux-agents-pane' })
  await clock.settle()
  const ui = await $.ui.mount({ ...PANE, surface: 'terminal' })
  const rows = []
  for (const name of ['a', 'b', 'c']) { const t = await ui.find({ type: 'Text', text: new RegExp(`^○ ${name} `) }); rows.push(`${t?.text}|${t?.props.dimColor}`) }
  expect(rows).toEqual(['○ a  claude  exited  1m|true', '○ b  claude  stopped  1m|true', '○ c  claude  lost  1m|true'])
  await ui.unmount()
})

test('the pane draws a board whose name and doing hold ESC, BEL and C1, with none of them drawn', async ($, on) => {
  const { clock } = world(on, { TMUX: 'x' }, [ran(0, list(1, [row('ev\u001b[31mil\u0007', 'running', 'do\u009bne\tnow\u007f', 5)]))])
  await $.session.start(START)
  await $.command.run({ command: 'mux-agents-pane' })
  await clock.settle()
  const ui = await $.ui.mount({ ...PANE, surface: 'terminal' })
  const texts = (await ui.findAll({ type: 'Text' })).map((t: { text: string }) => t.text)
  const controls = texts.flatMap((t: string) => [...t].map(c => c.codePointAt(0)!).filter(c => c < 0x20 || (c >= 0x7f && c <= 0x9f)))
  expect(controls).toEqual([])
  expect(texts).toContain('● ev[31mil  claude  5s')
  expect(texts).toContain('  done now')
  await ui.unmount()
})

test('successful refreshes update rows, count and status; success after a failure clears refresh failed', async ($, on) => {
  const two = list(2, [row('claude-1', 'running', 'build it', 240), row('claude-2', 'running', 'test it', 3)])
  const { seen, clock } = world(on, { TMUX: 'x' }, [ran(0, LIST), ran(0, two), ran(1, ''), ran(0, LIST)])
  await $.session.start(START)
  await $.command.run({ command: 'mux-agents-pane' })
  const ui = await $.ui.mount({ ...PANE, surface: 'terminal' })
  await clock.advance(3000)
  expect([await header(ui), (await ui.find({ type: 'Text', text: /claude-2/ }))?.text]).toEqual(['mux-agents · 2 running', '● claude-2  claude  3s'])
  await clock.advance(3000)
  expect(await header(ui)).toBe('mux-agents · 2 running · refresh failed')
  await clock.advance(3000)
  expect(await header(ui)).toBe('mux-agents · 1 running')
  expect(seen.status).toEqual(['agents: 1', 'agents: 2', 'agents: 1'])
  await ui.unmount()
})

test('a non-zero exit keeps the last board, marked refresh failed, and leaves the status', async ($, on) => {
  const { seen, clock } = world(on, { TMUX: 'x' }, [ran(0, LIST), ran(1, '{"ok":false,"verb":"list","code":1}')])
  await $.session.start(START)
  await $.command.run({ command: 'mux-agents-pane' })
  await clock.advance(3000)
  const ui = await $.ui.mount({ ...PANE, surface: 'terminal' })
  expect(await header(ui)).toBe('mux-agents · 1 running · refresh failed')
  expect((await ui.find({ type: 'Text', text: /claude-1/ }))?.text).toBe('● claude-1  claude  4m')
  expect(seen.status).toEqual(['agents: 1'])
  await ui.unmount()
})

test('output that is not JSON with exit 0 after a good board keeps that board, marked refresh failed', async ($, on) => {
  const { seen, clock } = world(on, { TMUX: 'x' }, [ran(0, LIST), ran(0, 'WARNING: profile loaded\n')])
  await $.session.start(START)
  await $.command.run({ command: 'mux-agents-pane' })
  await clock.advance(3000)
  const ui = await $.ui.mount({ ...PANE, surface: 'terminal' })
  expect(await header(ui)).toBe('mux-agents · 1 running · refresh failed')
  expect((await ui.find({ type: 'Text', text: /claude-1/ }))?.text).toBe('● claude-1  claude  4m')
  expect((await ui.find({ type: 'Text', text: /copilot-1/ }))?.text).toBe('○ copilot-1  copilot  exited  15m')
  expect(seen.status).toEqual(['agents: 1'])
  await ui.unmount()
})

test('an otherwise valid board with ok false after a good board keeps that board, marked refresh failed', async ($, on) => {
  const notOk = JSON.stringify({ ok: false, verb: 'list', running: 2, agents: [row('claude-9', 'running', 'other', 5)] })
  const { seen, clock } = world(on, { TMUX: 'x' }, [ran(0, LIST), ran(0, notOk)])
  await $.session.start(START)
  await $.command.run({ command: 'mux-agents-pane' })
  await clock.advance(3000)
  const ui = await $.ui.mount({ ...PANE, surface: 'terminal' })
  expect(await header(ui)).toBe('mux-agents · 1 running · refresh failed')
  expect((await ui.find({ type: 'Text', text: /claude-1/ }))?.text).toBe('● claude-1  claude  4m')
  expect(await ui.find({ type: 'Text', text: /claude-9/ })).toBe(undefined)
  expect(seen.status).toEqual(['agents: 1'])
  await ui.unmount()
})

test('a process rejection right after a good board keeps that board, marked refresh failed', async ($, on) => {
  const { clock } = world(on, { TMUX: 'x' }, [ran(0, LIST), { deny: 'cannot start' }])
  await $.session.start(START)
  await $.command.run({ command: 'mux-agents-pane' })
  await clock.advance(3000)
  const ui = await $.ui.mount({ ...PANE, surface: 'terminal' })
  expect(await header(ui)).toBe('mux-agents · 1 running · refresh failed')
  expect((await ui.find({ type: 'Text', text: /claude-1/ }))?.text).toBe('● claude-1  claude  4m')
  await ui.unmount()
})

test('list runs with a 10000 ms timeout, and a timeout fails like any rejection', async ($, on) => {
  const { seen, clock } = world(on, { TMUX: 'x' }, [ran(0, LIST), { deny: 'timed out after 10000 ms' }])
  await $.session.start(START)
  await $.command.run({ command: 'mux-agents-pane' })
  await clock.advance(3000)
  expect(seen.init).toEqual([{ timeoutMs: 10000 }, { timeoutMs: 10000 }])
  const ui = await $.ui.mount({ ...PANE, surface: 'terminal' })
  expect(await header(ui)).toBe('mux-agents · 1 running · refresh failed')
  await ui.unmount()
})
