import { indentUnit } from '@codemirror/language'
import { EditorState, type Extension } from '@codemirror/state'

// Files indented with tabs whatever their content: Go's, by gofmt, and
// make's, which needs them.
const tabbed = /(\.go|(^|\/)(GNUmakefile|[Mm]akefile)|\.mk)$/

// Files whose convention is four spaces, when nothing in them says.
const four = /\.(py|pyi|rs|java|kt|kts|scala|c|h|cc|cpp|cxx|hpp|hh|cs|php|swift|m|mm|sql|ps1)$/i

// indentation is a file's indentation as the file itself has it: tabs if
// more lines start with a tab than with spaces, else the step most often
// taken between one indented line and the next, else its kind's convention.
// A tab is shown four columns wide.
export function indentation(path: string, text: string): Extension {
  return [indentUnit.of(detect(path, text)), EditorState.tabSize.of(4)]
}

function detect(path: string, text: string): string {
  if (tabbed.test(path)) return '\t'
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
  if (tabs > spaces) return '\t'
  let best = 0
  let count = 0
  for (const [step, c] of steps) {
    if (c > count || (c === count && step < best)) {
      best = step
      count = c
    }
  }
  if (best) return ' '.repeat(best)
  return four.test(path) ? '    ' : '  '
}
