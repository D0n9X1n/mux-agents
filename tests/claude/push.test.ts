import { expect, test } from 'claude-code/testing'

import { sendReply, wakeUp } from '../../hooks/push'

const SID = '11111111-2222-4333-8444-555555555555'
const reply = (fields: object) => JSON.stringify({ ok: true, verb: 'send', to: 'w1', pane: '%9', id: 'abc', from: 'root', toSessionId: SID, ...fields })
const CMD = '"/p/scripts/mux-agent" send w1 -BodyFile b.md'

test('sendReply takes a successful send\'s sender and Claude recipient session, nothing else', () => {
  expect(sendReply(CMD, reply({}))).toEqual({ from: 'root', session: SID })
  expect(sendReply('sh /p/scripts/mux-agent send parent -BodyFile b.md', `noise\n${reply({ from: 'w2' })}\r\n`)).toEqual({ from: 'w2', session: SID })
  // Not send, not the entry, refused, no session (a Copilot recipient), a bad session or a bad name: nothing.
  expect(sendReply('"/p/scripts/mux-agent" list', reply({}))).toBeUndefined()
  expect(sendReply('echo mux-agents send', reply({}))).toBeUndefined()
  expect(sendReply(CMD, reply({ ok: false }))).toBeUndefined()
  expect(sendReply(CMD, reply({ verb: 'read' }))).toBeUndefined()
  expect(sendReply(CMD, reply({ toSessionId: null }))).toBeUndefined()
  expect(sendReply(CMD, reply({ toSessionId: 'x; rm -rf /' }))).toBeUndefined()
  expect(sendReply(CMD, reply({ from: 'Evil"/>' }))).toBeUndefined()
  expect(sendReply(CMD, undefined)).toBeUndefined()
  expect(sendReply(CMD, 'not json')).toBeUndefined()
  // Checked ids in either case, as the entry checks them.
  expect(sendReply(CMD, reply({ toSessionId: SID.toUpperCase() }))).toEqual({ from: 'root', session: SID.toUpperCase() })
})

test('only a command that is one run of the entry\'s send is trusted with its stdout', () => {
  // The forms the skill and the Reply line give: sh, & and cmd.
  for (const cmd of [
    CMD,
    "'/p/my dir/scripts/mux-agent' send w1 -Body hi",
    '/p/scripts/mux-agent send parent -BodyFile b.md',
    'sh "/p/scripts/mux-agent" send parent -BodyFile b.md',
    '& "C:/p/scripts/mux-agent.cmd" send w1 -BodyFile b.md',
    'cmd /d /c "C:\\p\\scripts\\mux-agent.cmd" send w1 -BodyFile b.md',
  ]) expect(sendReply(cmd, reply({}))).toEqual({ from: 'root', session: SID })
  // A command that only names the entry prints what it likes (a file holding a reply): never trusted.
  for (const cmd of [
    "grep -h 'mux-agent send' payload.json",
    'cat payload.json # mux-agent send w1',
    `${CMD}; cat payload.json`,
    `${CMD} && cat payload.json`,
    `${CMD} | tee out`,
    `${CMD} > out; cat out`,
    `${CMD} $(cat payload.json)`,
    `${CMD} \`cat payload.json\``,
    `${CMD}\ncat payload.json`,
    '"/p/scripts/not-mux-agent" send w1 -Body hi',
    '"/p/scripts/mux-agent" sender w1',
  ]) expect(sendReply(cmd, reply({}))).toBeUndefined()
})

test('the wake-up is a fixed sentence tagged with the sender, never the message', () => {
  expect(wakeUp('w2')).toBe('<mux-agents from="w2">You have a new mux-agents message from w2. Run your read command to get it.</mux-agents>')
})

test('a send through the Bash tool wakes the recipient session, after the tool answered; other calls do not', async ($, on) => {
  const sent: object[] = []
  let stdout = reply({})
  on('env.get', () => ({ value: undefined }))
  on('session.send', (_: unknown, e: object) => { sent.push({ ...e }); return { isDelivered: true } })
  on('tool.call', () => ({ result: { stdout, stderr: '', interrupted: false }, text: stdout }))
  const ran = await $.tool.call({ tool: 'Bash', command: CMD } as any)
  expect((ran as any).result.stdout).toBe(stdout)
  await new Promise(r => setTimeout(r, 20))
  // The engine spells a { sessionId } address as the id itself; the origin is this plugin.
  expect(sent).toEqual([{ to: SID, text: wakeUp('root'), origin: { kind: 'plugin', name: 'mux-agents' } }])
  // A send to a Copilot agent (no session) or another verb: no push.
  stdout = reply({ toSessionId: null })
  await $.tool.call({ tool: 'Bash', command: CMD } as any)
  await $.tool.call({ tool: 'Bash', command: '"/p/scripts/mux-agent" list' } as any)
  await new Promise(r => setTimeout(r, 20))
  expect(sent.length).toBe(1)
})

test('a send in the other shell tool wakes the recipient too; a denied or failed one does not', async ($, on) => {
  const sent: object[] = []
  let ran: object = { result: { stdout: reply({}), stderr: '', interrupted: false }, text: 'ok' }
  on('env.get', () => ({ value: undefined }))
  on('session.send', (_: unknown, e: object) => { sent.push({ ...e }); return { isDelivered: true } })
  on('tool.call', () => ran)
  await $.tool.call({ tool: 'PowerShell', command: '& "C:/p/scripts/mux-agent.cmd" send w1 -BodyFile b.md' } as any)
  await new Promise(r => setTimeout(r, 20))
  expect(sent.length).toBe(1)
  ran = { deny: reply({}) }
  await $.tool.call({ tool: 'Bash', command: CMD } as any)
  ran = { isError: true, result: { stdout: reply({}), stderr: '', interrupted: false }, text: 'failed' }
  await $.tool.call({ tool: 'Bash', command: CMD } as any)
  await new Promise(r => setTimeout(r, 20))
  expect(sent.length).toBe(1)
})

test('a push that is refused, throws or never answers never delays or changes the tool result', async ($, on) => {
  let send: () => unknown = () => ({ isDelivered: false, reason: 'no such session' })
  on('env.get', () => ({ value: undefined }))
  on('session.send', () => send())
  on('tool.call', () => ({ result: { stdout: reply({}), stderr: '', interrupted: false }, text: 'ok' }))
  for (const answer of [
    () => ({ isDelivered: false, reason: 'no such session' }),
    () => { throw new Error('gone') },
    () => new Promise(() => {}),
  ]) {
    send = answer
    const ran = await $.tool.call({ tool: 'Bash', command: CMD } as any)
    expect((ran as any).text).toBe('ok')
  }
})
