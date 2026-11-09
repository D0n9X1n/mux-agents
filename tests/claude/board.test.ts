import { expect, test } from 'claude-code/testing'

import { clean, formatAge, isSpawn, look, mail, parseList, sections } from '../../hooks/board'
import {
  SPAWN_ERROR_JSON, SPAWN_ERROR_TEXT, SPAWN_JSON, SPAWN_MODEL_JSON, SPAWN_MODEL_TEXT, SPAWN_OFF_JSON, SPAWN_OFF_TEXT, SPAWN_TEXT,
  TAGGED_JSON, TAGGED_TEXT,
} from './spawn-replies'

const E = 'sh "/p/scripts/mux-agent"'
const LISTED = '{"ok":true,"verb":"list","running":0,"agents":[]}'

test('isSpawn accepts every spawn form with a real spawn reply, JSON or -Text', () => {
  expect(isSpawn(`${E} spawn -Client claude`, SPAWN_JSON)).toBe(true)
  expect(isSpawn("pwsh -File '/p/scripts/mux-agent.ps1' spawn", SPAWN_JSON)).toBe(true)
  expect(isSpawn('pwsh -File /p/scripts/mux-agent.ps1 "spawn" -Client claude', SPAWN_JSON)).toBe(true)
  expect(isSpawn('PWSH -FILE C:/P/SCRIPTS/MUX-AGENT.PS1 SPAWN', SPAWN_JSON)).toBe(true)
  expect(isSpawn('& "C:/p/scripts/mux-agent.ps1" spawn -Client copilot', SPAWN_JSON)).toBe(true)
  expect(isSpawn('pwsh -File mux-agent.ps1 spawn', SPAWN_JSON)).toBe(true)
  expect(isSpawn(`${E} spawn`, `${SPAWN_JSON}\r\n`)).toBe(true)
  expect(isSpawn(`${E} spawn -Text`, SPAWN_TEXT)).toBe(true)
  expect(isSpawn(`${E} spawn -Text`, `${SPAWN_TEXT.replace(/\n/g, '\r\n')}\r\n`)).toBe(true)
})

test('isSpawn accepts a spawn whose side bar is off or failed after the commit, JSON or -Text', () => {
  for (const reply of [SPAWN_OFF_JSON, SPAWN_ERROR_JSON, SPAWN_OFF_TEXT, SPAWN_ERROR_TEXT]) {
    expect(isSpawn(`${E} spawn`, reply)).toBe(true)
    expect(isSpawn(`${E} spawn -Text`, `${reply.replace(/\n/g, '\r\n')}\r\n`)).toBe(true)
  }
})

test('isSpawn accepts a spawn reply naming its model, inherited or chosen, JSON or -Text', () => {
  expect(SPAWN_TEXT).toContain('\nreason: human\nmodel: inherited\n')
  expect(SPAWN_MODEL_TEXT).toContain('\nreason: human\nmodel: sonnet\n')
  for (const reply of [SPAWN_JSON, SPAWN_MODEL_JSON, SPAWN_TEXT, SPAWN_MODEL_TEXT]) {
    expect(isSpawn(`${E} spawn -Model sonnet`, reply)).toBe(true)
    expect(isSpawn(`${E} spawn -Model sonnet -Text`, `${reply.replace(/\n/g, '\r\n')}\r\n`)).toBe(true)
  }
})

test('isSpawn accepts a Windows path with backslashes, quoted or not', () => {
  expect(isSpawn('pwsh -NoProfile -File "C:\\p\\scripts\\mux-agent.ps1" "spawn"', SPAWN_JSON)).toBe(true)
  expect(isSpawn('& C:\\p\\scripts\\mux-agent.ps1 spawn -Client claude', SPAWN_JSON)).toBe(true)
})

test('isSpawn accepts mux-agent, mux-agent.cmd and mux-agent.exe spawn', () => {
  expect(isSpawn('"/p/plug in/scripts/mux-agent" spawn -Client claude', SPAWN_JSON)).toBe(true)
  expect(isSpawn('/p/scripts/mux-agent spawn', SPAWN_JSON)).toBe(true)
  expect(isSpawn('sh /p/scripts/mux-agent "spawn"', SPAWN_JSON)).toBe(true)
  expect(isSpawn('& "C:\\p\\scripts\\mux-agent.cmd" spawn -Client copilot', SPAWN_JSON)).toBe(true)
  expect(isSpawn('cmd.exe /d /c C:/p/scripts/MUX-AGENT.CMD spawn', SPAWN_JSON)).toBe(true)
  expect(isSpawn("'C:/p/target/release/mux-agent.exe' spawn", SPAWN_JSON)).toBe(true)
  expect(isSpawn('mux-agent.exe spawn -Text', SPAWN_TEXT)).toBe(true)
})

test('isSpawn still accepts mux-agent.ps1 spawn', () => {
  expect(isSpawn('pwsh -NoProfile -File "C:/p/scripts/mux-agent.ps1" spawn', SPAWN_JSON)).toBe(true)
})

test('isSpawn rejects mux-agents spawn and mux-agent list', () => {
  expect(isSpawn('/p/scripts/mux-agents spawn', SPAWN_JSON)).toBe(false)
  expect(isSpawn('mux-agents spawn', SPAWN_JSON)).toBe(false)
  expect(isSpawn('/p/scripts/mux-agent list', LISTED)).toBe(false)
  expect(isSpawn('/p/scripts/mux-agent.cmd list', SPAWN_JSON)).toBe(false)
  expect(isSpawn('/p/scripts/xmux-agent spawn', SPAWN_JSON)).toBe(false)
  expect(isSpawn('/p/scripts/mux-agent.sh spawn', SPAWN_JSON)).toBe(false)
  expect(isSpawn('/p/scripts/mux-agent spawn', TAGGED_JSON)).toBe(false)
})

test('isSpawn rejects a bare mention, another verb or file, and every refused, mixed or missing reply', () => {
  expect(isSpawn('grep -n "mux-agent.ps1 spawn" README.md', 'README.md:27:pwsh -File "<entry>" spawn')).toBe(false)
  expect(isSpawn(`${E} spawner`, SPAWN_JSON)).toBe(false)
  expect(isSpawn(`${E} list`, LISTED)).toBe(false)
  expect(isSpawn('pwsh -File C:/p/xmux-agent.ps1 spawn', SPAWN_JSON)).toBe(false)
  expect(isSpawn('pwsh -File C:/p/mux-agent.ps1.bak spawn', SPAWN_JSON)).toBe(false)
  expect(isSpawn(`${E} spawn`, TAGGED_JSON)).toBe(false)
  expect(isSpawn(`${E} spawn -Text`, TAGGED_TEXT)).toBe(false)
  expect(isSpawn(`${E} spawn`, '')).toBe(false)
  expect(isSpawn(`${E} spawn`, undefined)).toBe(false)
})

test('ok and verb must come from one reply: a refused spawn then a successful list does not count', () => {
  expect(isSpawn(`${E} spawn`, `${TAGGED_JSON}${LISTED}`)).toBe(false)
  expect(isSpawn(`${E} spawn`, `${TAGGED_JSON}\n${LISTED}`)).toBe(false)
})

test('the accepted false positive: a command that mentions spawn and prints a spawn reply', () => {
  expect(isSpawn(`echo 'mux-agent.ps1 spawn' '${SPAWN_JSON}'`, SPAWN_JSON)).toBe(true)
})

test('clean drops C0, DEL and C1 and turns tabs into spaces', () => {
  expect(clean('a\u0000b\u0007c\u001b[1md\te\u007ff\u0080g\u009bh\u009fi\u00a0j')).toBe('abc[1md efghi\u00a0j')
})

test('parseList maps every failure to undefined and cleans every string', () => {
  const good = '{"ok":true,"verb":"list","running":0,"agents":[]}'
  expect(parseList(0, good)).toEqual({ running: 0, agents: [] })
  expect(parseList(1, good)).toBe(undefined)
  expect(parseList(0, 'not json')).toBe(undefined)
  expect(parseList(0, '{"ok":false,"verb":"list","code":1,"check":"endpoint"}')).toBe(undefined)
  expect(parseList(0, '{"ok":false,"verb":"list","running":1,"agents":[]}')).toBe(undefined)
  expect(parseList(0, '{"ok":true,"verb":"list","agents":[]}')).toBe(undefined)
  expect(parseList(0, '{"ok":true,"verb":"list","running":0}')).toBe(undefined)
  const dirty = JSON.stringify({ ok: true, running: 1, agents: [{ name: 'a\u001b', client: 'c\u0007', state: 's\u009b', doing: 'd\te', age: 1 }] })
  expect(parseList(0, dirty)).toEqual({ running: 1, agents: [{ name: 'a', client: 'c', state: 's', doing: 'd e', age: 1, model: null, unread: 0, exitCode: null, activity: null, since: null }] })
})

test('parseList keeps a chosen model, cleaned, and null for an inherited one', () => {
  const rows = [
    { name: 'a', client: 'claude', state: 'running', doing: null, age: 1, model: 'son\u001bnet' },
    { name: 'b', client: 'claude', state: 'running', doing: null, age: 1, model: null },
    { name: 'c', client: 'claude', state: 'running', doing: null, age: 1 },
  ]
  const parsed = parseList(0, JSON.stringify({ ok: true, verb: 'list', running: 3, agents: rows }))
  expect(parsed?.agents.map(a => a.model)).toEqual(['sonnet', null, null])
})

test('parseList keeps unread and an ended row\'s exit code: 0 and null when missing or not a number', () => {
  const rows = [
    { name: 'a', client: 'claude', state: 'running', doing: null, age: 1, unread: 2 },
    { name: 'b', client: 'claude', state: 'exited', doing: null, age: 1, unread: 0, exitCode: 0 },
    { name: 'c', client: 'claude', state: 'exited', doing: null, age: 1, exitCode: 101 },
    { name: 'd', client: 'claude', state: 'stopped', doing: null, age: 1, unread: '3', exitCode: null },
    { name: 'e', client: 'claude', state: 'exited', doing: null, age: 1, exitCode: '1' },
  ]
  const parsed = parseList(0, JSON.stringify({ ok: true, verb: 'list', running: 1, agents: rows }))
  expect(parsed?.agents.map(a => [a.name, a.unread, a.exitCode])).toEqual([['a', 2, null], ['b', 0, 0], ['c', 0, 101], ['d', 0, null], ['e', 0, null]])
})

const agent = (state: string, exitCode?: unknown) => ({ name: 'n', client: 'claude', state, doing: null, age: 1, model: null, unread: 0, exitCode }) as never

test('look draws each state with the side bar\'s glyph, word and color, Unicode or ASCII', () => {
  const both = (state: string, exitCode?: unknown) => [look(agent(state, exitCode), false), look(agent(state, exitCode), true)].map(l => l && `${l.glyph} ${l.word} ${l.color ?? 'dim'} ${l.ended}`)
  expect(both('running')).toEqual(['\u25c9 working green false', '* working green false'])
  expect(both('exited', 0)).toEqual(['\u2713 done dim true', '+ done dim true'])
  expect(both('exited', 101)).toEqual(['\u2717 exit 101 red true', 'x exit 101 red true'])
  expect(both('exited', -1)).toEqual(['\u2717 exit -1 red true', 'x exit -1 red true'])
  expect(both('stopped', null)).toEqual(['- stopped yellow true', '- stopped yellow true'])
})

test('look draws a live agent by its activity as the side bar does: needs you, done, working, idle', () => {
  const live = (activity: unknown) => ({ ...agent('running'), activity })
  const both = (activity: unknown) => [look(live(activity), false), look(live(activity), true)].map(l => l && `${l.glyph} ${l.word} ${l.color ?? 'dim'} ${l.ended}${l.muted ? ' muted' : ''}${l.pill ? ' pill' : ''}`)
  expect(both('blocked')).toEqual(['! needs you yellow false pill', '! needs you yellow false pill'])
  expect(both('done')).toEqual(['\u2726 done cyan false', '@ done cyan false'])
  expect(both('working')).toEqual(['\u25c9 working green false', '* working green false'])
  expect(both('idle')).toEqual(['\u25cc idle dim false muted', 'o idle dim false muted'])
  // A list without activity (an older binary), or an unknown word: working.
  expect(both(undefined)).toEqual(['\u25c9 working green false', '* working green false'])
  expect(both('dreaming')).toEqual(['\u25c9 working green false', '* working green false'])
})

test('parseList keeps a live row\'s activity and since, null when missing or not a word or a number', () => {
  const one = (fields: object) => parseList(0, JSON.stringify({ ok: true, running: 1, agents: [{ name: 'w', state: 'running', ...fields }] }))!.agents[0]
  expect([one({ activity: 'done', since: 30 }).activity, one({ activity: 'done', since: 30 }).since]).toEqual(['done', 30])
  expect([one({}).activity, one({}).since]).toEqual([null, null])
  expect([one({ activity: 5, since: '30' }).activity, one({ activity: 5, since: '30' }).since]).toEqual([null, null])
  expect(one({ activity: 'do\u001bne' }).activity).toBe('done')
})

test('look draws an exit with no code (an older list) as a dim exited, another state dim, and no lost agent', () => {
  for (const code of [null, undefined, '0']) expect(look(agent('exited', code), false)).toEqual({ glyph: '-', word: 'exited', color: undefined, ended: true })
  expect(look(agent('paused'), true)).toEqual({ glyph: '-', word: 'paused', color: undefined, ended: true })
  expect([look(agent('lost'), false), look(agent('lost'), true)]).toEqual([undefined, undefined])
})

test('mail is the side bar\'s unread badge, only for a count above 0', () => {
  expect([mail(3, false), mail(3, true), mail(0, false), mail(undefined, false), mail('2', false)]).toEqual(['\u2709 3', 'm 3', undefined, undefined, undefined])
})

test('formatAge uses s, m and h, floored', () => {
  expect([0, 59, 60, 3599, 3600, 7199].map(formatAge)).toEqual(['0s', '59s', '1m', '59m', '1h', '1h'])
})

test('sections heads each state\'s first live row with its count, only when the live agents are in more than one state', () => {
  const a = (state: string, activity: string | null) => ({ ...agent(state), activity })
  expect(sections([a('running', 'blocked'), a('running', 'done'), a('running', 'done'), a('running', null), a('exited', null)]))
    .toEqual(['Needs you \u00b7 1', 'Done \u00b7 2', undefined, 'Working \u00b7 1', undefined])
  expect(sections([a('running', 'working'), a('running', 'working'), a('stopped', null)])).toEqual([undefined, undefined, undefined])
  expect(sections([])).toEqual([])
  // ASCII: `-` between the name and the count, as the side bar draws it.
  expect(sections([a('running', 'blocked'), a('running', 'working')], true)).toEqual(['Needs you - 1', 'Working - 1'])
})
