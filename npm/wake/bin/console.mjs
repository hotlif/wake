import { StringDecoder } from 'node:string_decoder'
import stringWidth from 'string-width'

const segmenter = new Intl.Segmenter(undefined, { granularity: 'grapheme' })
const MAX_INPUT = 4096
const MAX_HISTORY = 50

export function graphemes(value) {
  return [...segmenter.segment(String(value))].map((part) => part.segment)
}

export function parseConsoleCommand(value) {
  const original = String(value).trim()
  if (!original) throw new Error('Enter a command. Type help to list available commands.')
  const normalized = (original.startsWith('/') ? original.slice(1) : original).toLowerCase()
  if (normalized === 'help') return 'help'
  if (normalized === 'clear') return 'clear'
  if (normalized === 'open') return 'open'
  if (normalized === 'quit' || normalized === 'q') return 'quit'
  throw new Error(`Unknown command: ${original}. Type help for available commands.`)
}

export class InputEditor {
  constructor() {
    this.value = ''
    this.cursor = 0
    this.history = []
    this.historyIndex = undefined
    this.draft = ''
  }

  clear() {
    this.value = ''
    this.cursor = 0
    this.historyIndex = undefined
    this.draft = ''
  }

  insert(value) {
    const current = graphemes(this.value)
    const addition = graphemes(value).slice(0, Math.max(0, MAX_INPUT - current.length))
    current.splice(this.cursor, 0, ...addition)
    this.value = current.join('')
    this.cursor += addition.length
    this.historyIndex = undefined
  }

  insertPaste(value) {
    this.insert(String(value).replace(/[\r\n\t\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f]/g, ' '))
  }

  moveLeft() { this.cursor = Math.max(0, this.cursor - 1) }
  moveRight() { this.cursor = Math.min(graphemes(this.value).length, this.cursor + 1) }
  moveHome() { this.cursor = 0 }
  moveEnd() { this.cursor = graphemes(this.value).length }

  backspace() {
    if (this.cursor === 0) return
    const values = graphemes(this.value)
    values.splice(this.cursor - 1, 1)
    this.value = values.join('')
    this.cursor -= 1
  }

  delete() {
    const values = graphemes(this.value)
    if (this.cursor >= values.length) return
    values.splice(this.cursor, 1)
    this.value = values.join('')
  }

  historyPrevious() {
    if (this.history.length === 0) return
    if (this.historyIndex === undefined) {
      this.draft = this.value
      this.historyIndex = this.history.length - 1
    } else {
      this.historyIndex = Math.max(0, this.historyIndex - 1)
    }
    this.value = this.history[this.historyIndex]
    this.moveEnd()
  }

  historyNext() {
    if (this.historyIndex === undefined) return
    if (this.historyIndex + 1 < this.history.length) {
      this.historyIndex += 1
      this.value = this.history[this.historyIndex]
    } else {
      this.historyIndex = undefined
      this.value = this.draft
      this.draft = ''
    }
    this.moveEnd()
  }

  submit() {
    const submitted = this.value.trim()
    let result
    try {
      result = { command: parseConsoleCommand(submitted) }
    } catch (error) {
      result = { error: error.message }
    }
    if (submitted && this.history.at(-1) !== submitted) {
      this.history.push(submitted)
      if (this.history.length > MAX_HISTORY) this.history.shift()
    }
    this.clear()
    return result
  }

  cursorCell() {
    return graphemes(this.value).slice(0, this.cursor).reduce((sum, value) => sum + stringWidth(value), 0)
  }

  setCursorFromCell(cell) {
    let width = 0
    let cursor = 0
    for (const value of graphemes(this.value)) {
      const next = width + stringWidth(value)
      if (cell < next) break
      width = next
      cursor += 1
    }
    this.cursor = cursor
  }

  visible(width) {
    if (width <= 0) return { text: '', cursor: 0 }
    const cursor = this.cursorCell()
    const start = cursor >= width ? cursor + 1 - width : 0
    let position = 0
    let text = ''
    for (const value of graphemes(this.value)) {
      const next = position + stringWidth(value)
      if (next > start && position < start + width) text += value
      position = next
      if (position >= start + width) break
    }
    return { text, cursor: Math.min(width, Math.max(0, cursor - start)) }
  }
}

export function lineToCells(value, width) {
  const cells = []
  for (const valuePart of graphemes(value)) {
    const cellWidth = Math.max(0, stringWidth(valuePart))
    if (cells.length + cellWidth > width) break
    cells.push(valuePart)
    for (let index = 1; index < cellWidth; index += 1) cells.push('')
  }
  while (cells.length < width) cells.push(' ')
  return cells.slice(0, width)
}

function orderedSelection(selection) {
  const forward = selection.start.y < selection.end.y
    || (selection.start.y === selection.end.y && selection.start.x <= selection.end.x)
  return forward ? [selection.start, selection.end] : [selection.end, selection.start]
}

export function selectionContains(selection, x, y) {
  if (!selection) return false
  const [start, end] = orderedSelection(selection)
  if (y < start.y || y > end.y) return false
  return (y !== start.y || x >= start.x) && (y !== end.y || x <= end.x)
}

export function extractSelection(rows, selection) {
  if (!selection || (selection.start.x === selection.end.x && selection.start.y === selection.end.y)) return ''
  const [start, end] = orderedSelection(selection)
  const lines = []
  for (let y = start.y; y <= Math.min(end.y, rows.length - 1); y += 1) {
    const row = rows[y] || []
    const from = y === start.y ? start.x : 0
    const to = y === end.y ? Math.min(end.x, row.length - 1) : row.length - 1
    lines.push(row.slice(from, to + 1).join('').replace(/ +$/u, ''))
  }
  return lines.join('\n').replace(/\n+$/u, '')
}

const KEY_SEQUENCES = new Map([
  ['\x1b[A', 'up'], ['\x1b[B', 'down'], ['\x1b[C', 'right'], ['\x1b[D', 'left'],
  ['\x1bOA', 'up'], ['\x1bOB', 'down'], ['\x1bOC', 'right'], ['\x1bOD', 'left'],
  ['\x1b[H', 'home'], ['\x1b[F', 'end'], ['\x1b[1~', 'home'], ['\x1b[4~', 'end'],
  ['\x1bOH', 'home'], ['\x1bOF', 'end'],
  ['\x1b[3~', 'delete'], ['\x1b[5~', 'pageup'], ['\x1b[6~', 'pagedown'],
  ['\x1b[1;5F', 'ctrl-end'],
])

export class TerminalInputDecoder {
  constructor() {
    this.decoder = new StringDecoder('utf8')
    this.buffer = ''
    this.paste = false
  }

  push(chunk) {
    this.buffer += this.decoder.write(Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk))
    const events = []
    while (this.buffer) {
      if (this.paste) {
        const end = this.buffer.indexOf('\x1b[201~')
        if (end === -1) break
        events.push({ type: 'paste', value: this.buffer.slice(0, end) })
        this.buffer = this.buffer.slice(end + 6)
        this.paste = false
        continue
      }
      if (this.buffer.startsWith('\x1b[200~')) {
        this.buffer = this.buffer.slice(6)
        this.paste = true
        continue
      }
      const mouse = /^\x1b\[<(\d+);(\d+);(\d+)([mM])/.exec(this.buffer)
      if (mouse) {
        const code = Number(mouse[1])
        const suffix = mouse[4]
        let kind = 'down'
        let button = ['left', 'middle', 'right'][code & 3] || 'left'
        if ((code & 64) !== 0) kind = (code & 1) === 0 ? 'scroll-up' : 'scroll-down'
        else if ((code & 32) !== 0) kind = 'drag'
        else if (suffix === 'm') kind = 'up'
        events.push({ type: 'mouse', kind, button, x: Number(mouse[2]) - 1, y: Number(mouse[3]) - 1 })
        this.buffer = this.buffer.slice(mouse[0].length)
        continue
      }
      const matched = [...KEY_SEQUENCES].find(([sequence]) => this.buffer.startsWith(sequence))
      if (matched) {
        events.push({ type: 'key', key: matched[1] })
        this.buffer = this.buffer.slice(matched[0].length)
        continue
      }
      if (this.buffer.startsWith('\x1b') && [...KEY_SEQUENCES.keys(), '\x1b[200~', '\x1b[<'].some((value) => value.startsWith(this.buffer))) break
      const first = this.buffer.codePointAt(0)
      const value = String.fromCodePoint(first)
      this.buffer = this.buffer.slice(value.length)
      const control = {
        '\u0003': 'ctrl-c', '\u0019': 'ctrl-y', '\u0016': 'ctrl-v', '\r': 'enter', '\n': 'enter',
        '\u007f': 'backspace', '\u0008': 'backspace', '\u001b': 'escape',
      }[value]
      events.push(control ? { type: 'key', key: control } : { type: 'text', value })
    }
    return events
  }
}
