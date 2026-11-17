import { expect, mock, test } from 'claude-code/testing'

// report-2b K3: Claude names its session from classic.SessionStart, the event that carries session_id (session.start
// carries none). Wherever the client runs; never awaited.
const SID = '0f11fc2f-2032-48ef-a920-4af2a8e5af50'
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i
const ran = { value: { exitCode: 0, stdout: '', stderr: '', isStdoutTruncated: false, isStderrTruncated: false } }

// The engine beneath the plugin: the environment (a test may change it), every process.run (held `holdMs` on the
// mock clock), and the plugin root, which is the SessionStart event's cwd.
function world(on: any, env: Record<string, string>, holdMs = 0) {
  const seen = { argv: [] as string[][], settled: 0, root: '' }
  const clock = mock.clock(on)
  on('env.get', (_: unknown, e: { name: string }) => ({ value: env[e.name] }))
  on('classic.SessionStart', (_: unknown, e: { cwd: string }) => { seen.root = e.cwd; return {} })
  on('process.run', async (_: unknown, e: { argv: string[] }) => {
    seen.argv.push([...e.argv])
    if (holdMs) await clock.sleep(holdMs)
    seen.settled++
    return ran
  })
  return { seen, clock }
}

// PM call 2: the event carries a session id (the engine supplies one when the caller names none) and a source.
test('classic.SessionStart carries the session id the report needs', async ($, on) => {
  const { seen, clock } = world(on, { HC_TERMINAL_ID: 't1' })
  await ($ as any).classic.SessionStart({ source: 'startup' })
  await clock.settle()
  expect(seen.argv.length).toBe(1)
  const [argv] = seen.argv
  expect(argv.slice(0, 4)).toEqual(['sh', `${seen.root}/scripts/mux-agent`, 'report', '-Session'])
  expect(argv[4]).toMatch(UUID)
  expect(argv.slice(5)).toEqual(['-Source', 'startup', '-Quiet'])
})

// (fix 1, audit 5-9 item 10) SessionStart returns while the report is held; only advancing the clock settles it.
test('an hc terminal reports its session without SessionStart waiting for it', async ($, on) => {
  const { seen, clock } = world(on, { HC_TERMINAL_ID: 't1' }, 60000)
  await ($ as any).classic.SessionStart({ source: 'resume', session_id: SID })
  await clock.settle()
  expect(seen.argv).toEqual([['sh', `${seen.root}/scripts/mux-agent`, 'report', '-Session', SID, '-Source', 'resume', '-Quiet']])
  expect(seen.settled).toBe(0)
  await clock.advance(60000)
  await clock.settle()
  expect(seen.settled).toBe(1)
})

test('on Windows_NT the session report runs cmd.exe /d /c <root>\\scripts\\mux-agent.cmd', async ($, on) => {
  const { seen, clock } = world(on, { RMUX: 'x', OS: 'Windows_NT' })
  await ($ as any).classic.SessionStart({ source: 'startup', session_id: SID })
  await clock.settle()
  expect(seen.argv).toEqual([['cmd.exe', '/d', '/c', `${seen.root}\\scripts\\mux-agent.cmd`, 'report', '-Session', SID, '-Source', 'startup', '-Quiet']])
})

// (fix 2, re-audit 3) One classic.SessionStart per test.
// Outside tmux, rmux and hc the session is still reported: occupancy reads it, so a person's own Claude Code that
// switched into a member's conversation with /resume keeps that member from being resumed into it twice.
test('outside any multiplexer the session is still reported', async ($, on) => {
  const { seen, clock } = world(on, {})
  await ($ as any).classic.SessionStart({ source: 'resume', session_id: SID })
  await clock.settle()
  expect(seen.argv.length).toBe(1)
  expect(seen.argv[0].slice(-6)).toEqual(['report', '-Session', SID, '-Source', 'resume', '-Quiet'])
})

test('a malformed session id runs nothing', async ($, on) => {
  const { seen, clock } = world(on, { HC_TERMINAL_ID: 't1' })
  await ($ as any).classic.SessionStart({ source: 'startup', session_id: 'not-a-uuid' })
  await clock.settle()
  expect(seen.argv).toEqual([])
})

test('an odd source is reported as unknown', async ($, on) => {
  const { seen, clock } = world(on, { HC_TERMINAL_ID: 't1' })
  await ($ as any).classic.SessionStart({ source: 'Re sume', session_id: SID })
  await clock.settle()
  expect(seen.argv).toEqual([['sh', `${seen.root}/scripts/mux-agent`, 'report', '-Session', SID, '-Source', 'unknown', '-Quiet']])
})


// r2b call 25 (QA): session reports are delivered in order. The first report's run is held; a second SessionStart
// (a /clear) comes meanwhile. The second report must not start until the first has finished, so an earlier session
// can never finish after a later one and overwrite its entry; SessionStart still waits for neither.
test('a later session report waits for an earlier one that is still running', async ($, on) => {
  const SID2 = '1e22fc2f-2032-48ef-a920-4af2a8e5af51'
  const started: string[][] = []
  const finished: string[] = []
  const clock = mock.clock(on)
  on('env.get', () => ({ value: undefined }))
  on('classic.SessionStart', () => ({}))
  on('process.run', async (_: unknown, e: { argv: string[] }) => {
    started.push([...e.argv])
    if (started.length === 1) await clock.sleep(60000)
    finished.push(e.argv[e.argv.indexOf('-Session') + 1])
    return ran
  })
  await ($ as any).classic.SessionStart({ source: 'startup', session_id: SID })
  await ($ as any).classic.SessionStart({ source: 'clear', session_id: SID2 })
  await clock.settle()
  expect(started.length).toBe(1)
  await clock.advance(60000)
  await clock.settle()
  expect(started.map(a => [a[4], a[6]])).toEqual([[SID, 'startup'], [SID2, 'clear']])
  expect(finished).toEqual([SID, SID2])
})
