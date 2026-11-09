import { expect, test } from 'claude-code/testing'

import { sendReply, wakeUp } from '../../hooks/push'

const SID = '11111111-2222-4333-8444-555555555555'
const reply = (fields: object) => JSON.stringify({ ok: true, verb: 'send', to: 'w1', pane: '%9', id: 'abc', from: 'root', toSessionId: SID, ...fields })
const CMD = '"/p/scripts/mux-agent" send w1 -BodyFile b.md'

test('sendReply takes a successful send\'s sender and Claude recipient session, nothing else', () => {
  expect(sendReply(CMD, reply({}), '/p')).toEqual({ from: 'root', session: SID })
  expect(sendReply('sh /p/scripts/mux-agent send parent -BodyFile b.md', `noise\n${reply({ from: 'w2' })}\r\n`, '/p')).toEqual({ from: 'w2', session: SID })
  // Not send, not the entry, refused, no session (a Copilot recipient), a bad session or a bad name: nothing.
  expect(sendReply('"/p/scripts/mux-agent" list', reply({}), '/p')).toBeUndefined()
  expect(sendReply('echo mux-agents send', reply({}), '/p')).toBeUndefined()
  expect(sendReply(CMD, reply({ ok: false }), '/p')).toBeUndefined()
  expect(sendReply(CMD, reply({ verb: 'read' }), '/p')).toBeUndefined()
  expect(sendReply(CMD, reply({ toSessionId: null }), '/p')).toBeUndefined()
  expect(sendReply(CMD, reply({ toSessionId: 'x; rm -rf /' }), '/p')).toBeUndefined()
  expect(sendReply(CMD, reply({ from: 'Evil"/>' }), '/p')).toBeUndefined()
  expect(sendReply(CMD, undefined, '/p')).toBeUndefined()
  expect(sendReply(CMD, 'not json', '/p')).toBeUndefined()
  // Checked ids in either case, as the entry checks them.
  expect(sendReply(CMD, reply({ toSessionId: SID.toUpperCase() }), '/p')).toEqual({ from: 'root', session: SID.toUpperCase() })
})

test('only a command that is one run of this plugin\'s entry\'s send is trusted with its stdout', () => {
  // The forms the skill and the Reply line give: sh, & and cmd; the shims or a binary the shim runs.
  for (const cmd of [
    CMD,
    '/p/scripts/mux-agent send parent -BodyFile b.md',
    'sh "/p/scripts/mux-agent" send parent -BodyFile b.md',
    "'/p/target/release/mux-agent' send w1 -Body hi",
    '"/p/scripts/bin/darwin-arm64/mux-agent" send w1 -Body hi',
    `"/p/scripts/mux-agent" send parent -Body 'all done, see /tmp/out.md' -Kind x`,
    '"/p/scripts/mux-agent" send w1 -Body "tests pass: 12 of 12"',
  ]) expect(sendReply(cmd, reply({}), '/p')).toEqual({ from: 'root', session: SID })
  for (const cmd of [
    '& "C:/My Plugins/mux/scripts/mux-agent.cmd" send w1 -BodyFile b.md',
    'cmd /d /c "C:\\My Plugins\\mux\\scripts\\mux-agent.cmd" send w1 -BodyFile b.md',
    '& "c:\\my plugins\\MUX\\scripts\\bin\\windows-x64\\mux-agent.exe" send w1 -BodyFile b.md',
  ]) expect(sendReply(cmd, reply({}), 'C:\\My Plugins\\mux')).toEqual({ from: 'root', session: SID })
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
    // Another program by expansion or as the first word, another entry, out of the root, a glob, an expansion.
    'printf${IFS}\\n%s\\n/p/scripts/mux-agent send x',
    'printf "\\n%s\\n" /p/scripts/mux-agent send x',
    '"/other/scripts/mux-agent" send w1 -Body hi',
    'mux-agent send w1 -Body hi',
    '"/p/scripts/../evil/scripts/mux-agent" send w1 -Body hi',
    '"/p/x/scripts/mux-agent" send w1 -Body hi',
    '"/px/scripts/mux-agent" send w1 -Body hi',
    '/p/scripts/mux-agen? send w1 -Body hi',
    '"/p/scripts/mux-agent" send w1 -Body "$HOME"',
    '"$P/scripts/mux-agent" send w1 -Body hi',
    // A word some shell reads: an expression, variable, subexpression, splat or script block, cmd's
    // %VAR% and ^, smart quotes, history, an escaped closing quote, a glob.
    `${CMD} ([Console]::WriteLine('${reply({})}'))`,
    `${CMD} -Body:([Console]::WriteLine('x'))`,
    `${CMD} -Body @args`,
    `${CMD} -Body {x}`,
    `${CMD} -Body %PATH%`,
    `${CMD} -Body a^b`,
    `${CMD} -Body ‘x’`,
    `${CMD} -Body "!!"`,
    `${CMD} -Body "x\\" y`,
    `${CMD} -Body *`,
    `${CMD} -Body 'it''s'`,
    // On POSIX a \ is part of a file name and case matters: another file than the entry.
    '"/p\\scripts\\mux-agent" send w1 -Body hi',
    '"/p/scripts\\mux-agent" send w1 -Body hi',
    '"/P/scripts/mux-agent" send w1 -Body hi',
    '"/p/Scripts/mux-agent" send w1 -Body hi',
  ]) expect(sendReply(cmd, reply({}), '/p')).toBeUndefined()
  // A Windows root still refuses another root, a path out of it, or another file.
  for (const cmd of [
    '& "C:/Other/scripts/mux-agent.cmd" send w1 -Body hi',
    '& "C:/My Plugins/mux/scripts/../x/scripts/mux-agent.cmd" send w1 -Body hi',
    '& "C:/My Plugins/mux/scripts/mux-agent.bat" send w1 -Body hi',
  ]) expect(sendReply(cmd, reply({}), 'C:\\My Plugins\\mux')).toBeUndefined()
  // A POSIX root holding a \: only a quoted path is the file read, since sh removes an unquoted \.
  expect(sendReply('/p\\x/scripts/mux-agent send w1 -Body hi', reply({}), '/p\\x')).toBeUndefined()
  expect(sendReply(`'/p\\x/scripts/mux-agent' send w1 -Body hi`, reply({}), '/p\\x')).toEqual({ from: 'root', session: SID })
  // No root, no trust.
  expect(sendReply(CMD, reply({}), '')).toBeUndefined()
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
  const cmd = entrySend(await pluginRoot($, on))
  const ran = await $.tool.call({ tool: 'Bash', command: cmd } as any)
  expect((ran as any).result.stdout).toBe(stdout)
  await new Promise(r => setTimeout(r, 20))
  // The engine spells a { sessionId } address as the id itself; the origin is this plugin.
  expect(sent).toEqual([{ to: SID, text: wakeUp('root'), origin: { kind: 'plugin', name: 'mux-agents' } }])
  // A send to a Copilot agent (no session) or another verb: no push.
  stdout = reply({ toSessionId: null })
  await $.tool.call({ tool: 'Bash', command: cmd } as any)
  await $.tool.call({ tool: 'Bash', command: cmd.replace(' send w1 -BodyFile b.md', ' list') } as any)
  await new Promise(r => setTimeout(r, 20))
  expect(sent.length).toBe(1)
})

test('a send in the other shell tool wakes the recipient too; a denied or failed one does not', async ($, on) => {
  const sent: object[] = []
  let ran: object = { result: { stdout: reply({}), stderr: '', interrupted: false }, text: 'ok' }
  on('env.get', () => ({ value: undefined }))
  on('session.send', (_: unknown, e: object) => { sent.push({ ...e }); return { isDelivered: true } })
  on('tool.call', () => ran)
  const cmd = entrySend(await pluginRoot($, on))
  await $.tool.call({ tool: 'PowerShell', command: `& ${cmd}` } as any)
  await new Promise(r => setTimeout(r, 20))
  expect(sent.length).toBe(1)
  ran = { deny: reply({}) }
  await $.tool.call({ tool: 'Bash', command: cmd } as any)
  ran = { isError: true, result: { stdout: reply({}), stderr: '', interrupted: false }, text: 'failed' }
  await $.tool.call({ tool: 'Bash', command: cmd } as any)
  await new Promise(r => setTimeout(r, 20))
  expect(sent.length).toBe(1)
})

test('a push that is refused, throws or never answers never delays or changes the tool result', async ($, on) => {
  let send: () => unknown = () => ({ isDelivered: false, reason: 'no such session' })
  on('env.get', () => ({ value: undefined }))
  on('session.send', () => send())
  on('tool.call', () => ({ result: { stdout: reply({}), stderr: '', interrupted: false }, text: 'ok' }))
  const cmd = entrySend(await pluginRoot($, on))
  for (const answer of [
    () => ({ isDelivered: false, reason: 'no such session' }),
    () => { throw new Error('gone') },
    () => new Promise(() => {}),
  ]) {
    send = answer
    const ran = await $.tool.call({ tool: 'Bash', command: cmd } as any)
    expect((ran as any).text).toBe('ok')
  }
})

// The plugin root as the engine reports it, read through a classic SessionStart's cwd; the entry's send under it.
async function pluginRoot($: any, on: any) {
  let root = ''
  on('classic.SessionStart', (_: unknown, e: { cwd: string }) => { root = e.cwd; return {} })
  await $.classic.SessionStart({ source: 'startup' })
  return root
}
const entrySend = (root: string) => `"${root}/scripts/mux-agent" send w1 -BodyFile b.md`
