import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { test } from 'node:test'

import {
  InputEditor,
  TerminalInputDecoder,
  extractSelection,
  lineToCells,
  parseConsoleCommand,
} from '../bin/console.mjs'

test('shared console command contract is implemented', () => {
  const contract = JSON.parse(readFileSync(
    new URL('../../../fixtures/terminal-console-contract.json', import.meta.url),
    'utf8',
  ))
  for (const entry of contract.commands) assert.equal(parseConsoleCommand(entry.input), entry.command)
  for (const value of contract.invalid) assert.throws(() => parseConsoleCommand(value))
})

test('input editor preserves graphemes, normalizes paste, and recalls history', () => {
  const editor = new InputEditor()
  editor.insertPaste('/help\n你好👨‍👩‍👧‍👦')
  assert.equal(editor.value, '/help 你好👨‍👩‍👧‍👦')
  editor.backspace()
  assert.equal(editor.value, '/help 你好')
  editor.clear()
  editor.insert('help')
  assert.deepEqual(editor.submit(), { command: 'help' })
  editor.historyPrevious()
  assert.equal(editor.value, 'help')
  editor.historyNext()
  assert.equal(editor.value, '')
})

test('cell selection handles wide characters, reverse drags, and padding', () => {
  const rows = [lineToCells('a你', 6), lineToCells('bc', 6)]
  assert.equal(extractSelection(rows, {
    start: { x: 1, y: 1 },
    end: { x: 0, y: 0 },
  }), 'a你\nbc')
})

test('terminal decoder handles fragmented paste and combined SGR mouse events', () => {
  const decoder = new TerminalInputDecoder()
  assert.deepEqual(decoder.push(Buffer.from('\x1b[20')), [])
  assert.deepEqual(decoder.push(Buffer.from('0~你\n')), [])
  assert.deepEqual(decoder.push(Buffer.from('好\x1b[201~')), [{ type: 'paste', value: '你\n好' }])
  assert.deepEqual(
    decoder.push(Buffer.from('\x1b[<0;2;3M\x1b[<32;5;3M\x1b[<0;5;3m')),
    [
      { type: 'mouse', kind: 'down', button: 'left', x: 1, y: 2 },
      { type: 'mouse', kind: 'drag', button: 'left', x: 4, y: 2 },
      { type: 'mouse', kind: 'up', button: 'left', x: 4, y: 2 },
    ],
  )
})
