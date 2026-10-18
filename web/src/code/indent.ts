import { indentUnit } from '@codemirror/language'
import { EditorState, type Extension } from '@codemirror/state'

import type { EditorConfig } from './editorconfig'

// Files indented with tabs whatever their content: Go's, by gofmt, and
// make's, which needs them.
const tabbed = /(\.go|(^|\/)(GNUmakefile|[Mm]akefile)|\.mk)$/

// Files whose convention is four spaces, when nothing in them says.
const four = /\.(py|pyi|rs|java|kt|kts|scala|c|h|cc|cpp|cxx|hpp|hh|cs|php|swift|m|mm|sql|ps1)$/i

// Indent is how a file is indented: the unit a level is (a tab, or so many
// spaces), how wide a tab is shown, and where that was learnt.
export type Indent = { unit: string; tabSize: number; from: 'editorconfig' | 'file' | 'default' }

// indentFor is a file's indentation: what its .editorconfig says, else as
// the file itself has it (tabs if more lines start with a tab than with
// spaces, else the step most often taken between one indented line and
// the next), else its kind's convention. A tab is four columns unless the
// .editorconfig says.
export function indentFor(path: string, text: string, ec: EditorConfig | null): Indent {
  const tabSize = ec?.tabWidth ?? (typeof ec?.indentSize === 'number' ? ec.indentSize : 4)
  if (ec?.indentStyle === 'tab') return { unit: '\t', tabSize, from: 'editorconfig' }
  if (ec?.indentStyle === 'space' || ec?.indentSize) {
    const size = ec.indentSize === 'tab' || ec.indentSize === undefined ? (ec.tabWidth ?? 4) : ec.indentSize
    if (ec.indentStyle === 'space') return { unit: ' '.repeat(size), tabSize, from: 'editorconfig' }
    // A size alone says how wide a level is, not what it is made of.
    const found = detect(path, text)
    return { unit: found.unit === '\t' ? '\t' : ' '.repeat(size), tabSize, from: 'editorconfig' }
  }
  return { ...detect(path, text), tabSize }
}

// indentation is the editor's configuration for an indentation.
export function indentation(i: Indent): Extension {
  return [indentUnit.of(i.unit), EditorState.tabSize.of(i.tabSize)]
}

// indentName is what the status bar calls an indentation.
export const indentName = (i: Indent) =>
  i.unit === '\t' ? 'Tab' : `${i.unit.length} space${i.unit.length === 1 ? '' : 's'}`

function detect(path: string, text: string): { unit: string; from: 'file' | 'default' } {
  if (tabbed.test(path)) return { unit: '\t', from: 'default' }
  let tabs = 0
  let spaces = 0
  const steps = new Map<number, number>()
  let prev = 0
  let n = 0
  for (const line of text.split('\n')) {
    if (++n > 10000) break
    if (!line.trim()) continue
    if (line[0] === '\t') {
      tabs++
      continue
    }
    const width = line.length - line.trimStart().length
    if (width > 0) spaces++
    const step = width - prev
    // A block comment's continuation, a star one space in, is not a step.
    if (step >= 2 && step <= 8 && line.trimStart()[0] !== '*') steps.set(step, (steps.get(step) ?? 0) + 1)
    prev = width
  }
  if (tabs > spaces) return { unit: '\t', from: 'file' }
  let best = 0
  let count = 0
  for (const [step, c] of steps) {
    if (c > count || (c === count && step < best)) {
      best = step
      count = c
    }
  }
  if (best) return { unit: ' '.repeat(best), from: 'file' }
  return { unit: four.test(path) ? '    ' : '  ', from: 'default' }
}
