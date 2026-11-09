import { expect, test } from 'claude-code/testing'

const ran = (exitCode: number, stdout: string) => ({ value: { exitCode, stdout, stderr: '', isStdoutTruncated: false, isStderrTruncated: false } })
const OK = ran(0, '{"ok":true,"verb":"report"}')
const NOT_AGENT = ran(2, '{"ok":false,"verb":"report","code":2,"check":"target","message":"only an agent reports its activity"}')

// The engine beneath the plugin: process.run answers from `runs` in order (the last repeating) and records each argv;
// a run given as a function answers with what it returns (a held promise, a rejection); the environment is `env`;
// a question tool call answers once `release` is called.
function world(on: any, env: Record<string, string>, runs: unknown[]) {
  const argv: string[][] = []
  const waiters: { n: number; done: () => void }[] = []
  let release: () => void = () => {}
  const answered = new Promise<void>(resolve => { release = resolve })
  on('env.get', (_: unknown, e: { name: string }) => ({ value: env[e.name] }))
  on('process.run', (_: unknown, e: { argv: string[] }) => {
    argv.push([...e.argv])
    for (const w of waiters.filter(w => argv.length >= w.n)) { waiters.splice(waiters.indexOf(w), 1); w.done() }
    const next = runs.length > 1 ? runs.shift() : runs[0]
    return typeof next === 'function' ? next() : next
  })
  on('turn.start', (_: unknown, e: { turnId: string }) => ({ turnId: e.turnId }))
  on('turn.complete', () => ({ text: '' }))
  on('tool.call', async () => { await answered; return { result: { answers: {} } } })
  // The words reported, in order.
  const words = () => argv.map(a => a.slice(a.indexOf('report') + 1).join(' '))
  // Resolves once `n` reports have run.
  const until = (n: number) => new Promise<void>(done => (argv.length >= n ? done() : waiters.push({ n, done })))
  return { argv, words, release, until }
}

const end = (turnId: string, agentId?: string) => ({ turnId, agentId, reason: 'answer', answer: '', durationMs: 1, isAborted: false }) as any

test('a turn reports working, its end idle, through the entry shim', async ($, on) => {
  const w = world(on, { TMUX: '/tmp/tmux-501/default,1,0' }, [OK])
  await $.turn.start({ text: 'go', turnId: 't1' })
  await $.turn.complete(end('t1'))
  await w.until(2)
  expect(w.words()).toEqual(['working', 'idle'])
  expect(w.argv[0][0]).toBe('sh')
  expect(w.argv[0][1]).toMatch(/\/scripts\/mux-agent$/)
})

test('a question blocks until it is answered, then the turn works on', async ($, on) => {
  const w = world(on, { TMUX: 'x' }, [OK])
  await $.turn.start({ text: 'go', turnId: 't1' })
  const asked = $.tool.call({ tool: 'AskUserQuestion', questions: [] } as any)
  await w.until(2)
  expect(w.words()).toEqual(['working', 'blocked -Kind question'])
  w.release()
  await asked
  await w.until(3)
  expect(w.words()).toEqual(['working', 'blocked -Kind question', 'working'])
})

test('a subagent turn reports nothing', async ($, on) => {
  const w = world(on, { TMUX: 'x' }, [OK])
  await $.turn.start({ text: 'go', turnId: 't1', agentId: 'sub-1' })
  await $.turn.complete(end('t1', 'sub-1'))
  // A main-loop turn after them is the first report.
  await $.turn.start({ text: 'go', turnId: 't2' })
  await w.until(1)
  expect(w.words()).toEqual(['working'])
})

test('outside tmux and rmux nothing runs', async ($, on) => {
  const w = world(on, {}, [OK])
  await $.turn.start({ text: 'go', turnId: 't1' })
  await $.turn.complete(end('t1'))
  expect(w.argv).toEqual([])
})

test('a pane that is no agent is asked once, then never again', async ($, on) => {
  const w = world(on, { RMUX: 'x', OS: 'Windows_NT' }, [NOT_AGENT])
  await $.turn.start({ text: 'go', turnId: 't1' })
  await w.until(1)
  await $.turn.complete(end('t1'))
  await $.turn.start({ text: 'again', turnId: 't2' })
  expect(w.argv.length).toBe(1)
  expect(w.argv[0].slice(0, 3)).toEqual(['cmd.exe', '/d', '/c'])
  expect(w.argv[0][3]).toMatch(/\\scripts\\mux-agent\.cmd$/)
})

test('reports run one at a time in event order, and no turn waits for them', async ($, on) => {
  let finish: (v: unknown) => void = () => {}
  const held = () => new Promise(resolve => { finish = resolve })
  const w = world(on, { TMUX: 'x' }, [held, OK])
  // The turn starts and ends while its first report is still running.
  await $.turn.start({ text: 'go', turnId: 't1' })
  await $.turn.complete(end('t1'))
  await w.until(1)
  // Give any report that does not wait its turn every chance to start (it would be the second run).
  const second = await Promise.race([w.until(2).then(() => true), new Promise(r => setTimeout(() => r(false), 200))])
  expect([second, w.words()]).toEqual([false, ['working']])
  // The idle report starts only once working has settled, so it can never land first.
  finish(OK)
  await w.until(2)
  expect(w.words()).toEqual(['working', 'idle'])
})

test('a report that fails is dropped and the next one still runs', async ($, on) => {
  const w = world(on, { TMUX: 'x' }, [() => Promise.reject(new Error('spawn failed')), OK])
  await $.turn.start({ text: 'go', turnId: 't1' })
  await $.turn.complete(end('t1'))
  await w.until(2)
  expect(w.words()).toEqual(['working', 'idle'])
})

test('a pane that stops being an agent\'s stops reporting after its first refusal', async ($, on) => {
  const w = world(on, { TMUX: 'x' }, [OK, NOT_AGENT, OK])
  await $.turn.start({ text: 'go', turnId: 't1' })
  await $.turn.complete(end('t1'))
  await w.until(2)
  await $.turn.start({ text: 'again', turnId: 't2' })
  await $.turn.complete(end('t2'))
  expect(w.argv.length).toBe(2)
})
