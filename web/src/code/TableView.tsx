import { ArrowDown, ArrowUp } from 'lucide-react'
import { useMemo, useState } from 'react'

// isTable is whether a file is a table of values, by its name: comma- or
// tab-separated.
export const isTable = (path: string) => /\.(csv|tsv|tab)$/i.test(path)

// The most rows drawn: past these the table says how many more there are.
const maxRows = 5000

type Row = { line: number; cells: string[] }

// delimiter is what separates a file's values: tabs for .tsv, else commas,
// unless the first line has more semicolons (a spreadsheet's export where
// the comma is the decimal point).
function delimiter(path: string, text: string): string {
  if (/\.(tsv|tab)$/i.test(path)) return '\t'
  const first = text.slice(0, text.indexOf('\n') >>> 0)
  const count = (c: string) => first.split(c).length - 1
  return count(';') > count(',') ? ';' : ','
}

// parse reads delimited values as RFC 4180 has them: a value in double
// quotes may hold the delimiter, a line break or a doubled quote. Each row
// keeps the line (from 1) it starts on.
export function parse(text: string, sep: string, limit: number): { rows: Row[]; more: number } {
  const rows: Row[] = []
  let more = 0
  let line = 1
  let i = 0
  const n = text.length
  while (i < n) {
    const start = line
    const cells: string[] = []
    for (;;) {
      let cell = ''
      if (text[i] === '"') {
        i++
        for (;;) {
          if (i >= n) break
          const c = text[i]
          if (c === '"') {
            if (text[i + 1] === '"') {
              cell += '"'
              i += 2
              continue
            }
            i++
            break
          }
          if (c === '\n') line++
          cell += c
          i++
        }
        // Anything between the closing quote and the delimiter is kept.
        while (i < n && text[i] !== sep && text[i] !== '\n' && text[i] !== '\r') cell += text[i++]
      } else {
        while (i < n && text[i] !== sep && text[i] !== '\n' && text[i] !== '\r') cell += text[i++]
      }
      cells.push(cell)
      if (i < n && text[i] === sep) {
        i++
        continue
      }
      if (text[i] === '\r') i++
      if (text[i] === '\n') {
        i++
        line++
      }
      break
    }
    if (cells.length === 1 && cells[0] === '') continue
    if (rows.length < limit) rows.push({ line: start, cells })
    else more++
  }
  return { rows, more }
}

const numeric = (v: string) => v.trim() !== '' && !isNaN(Number(v.replace(/,/g, '')))

// TableView shows a CSV or TSV file as a table, as IntelliJ's data view
// does: its first row the header, kept in sight, and each column sorted by
// a click on its header (numbers as numbers). A click on a row puts the
// editor beside it on the row's line.
export function TableView({ path, text, onLine }: { path: string; text: string; onLine?: (line: number) => void }) {
  const [sort, setSort] = useState<{ col: number; desc: boolean } | null>(null)
  const { head, body, more, numbers, width } = useMemo(() => {
    const { rows, more } = parse(text, delimiter(path, text), maxRows + 1)
    const [head, ...body] = rows
    const width = Math.max(0, ...rows.map((r) => r.cells.length))
    const numbers = Array.from({ length: width }, (_, c) => {
      const vals = body.map((r) => r.cells[c] ?? '').filter((v) => v.trim() !== '')
      return vals.length > 0 && vals.every(numeric)
    })
    return { head, body, more, numbers, width }
  }, [path, text])
  const sorted = useMemo(() => {
    if (!sort) return body
    const { col, desc } = sort
    const num = numbers[col]
    const out = [...body].sort((a, b) => {
      const x = a.cells[col] ?? ''
      const y = b.cells[col] ?? ''
      const r = num
        ? Number(x.replace(/,/g, '')) - Number(y.replace(/,/g, ''))
        : x.localeCompare(y, undefined, { numeric: true })
      return desc ? -r : r
    })
    return out
  }, [body, sort, numbers])

  if (!head) return <div className="code-table-empty muted">The file has no rows.</div>
  const cols = Array.from({ length: width }, (_, c) => c)
  const rows = body.length + more
  return (
    <div className="code-table">
      <div className="code-image-bar">
        <span className="muted small">
          {rows.toLocaleString()} row{rows === 1 ? '' : 's'}, {width} column{width === 1 ? '' : 's'}
          {sort && ` · sorted by ${head.cells[sort.col] || `column ${sort.col + 1}`}${sort.desc ? ', descending' : ''}`}
        </span>
      </div>
      <div className="code-table-body">
        <table>
          <thead>
            <tr>
              <th className="code-table-n">#</th>
              {cols.map((c) => (
                <th
                  key={c}
                  className={numbers[c] ? 'num' : undefined}
                  title="Sort by this column"
                  onClick={() =>
                    setSort((s) => (s?.col !== c ? { col: c, desc: false } : s.desc ? null : { col: c, desc: true }))
                  }
                >
                  {head.cells[c] ?? ''}
                  {sort?.col === c && (sort.desc ? <ArrowDown size={11} /> : <ArrowUp size={11} />)}
                </th>
              ))}
            </tr>
          </thead>
          <tbody>
            {sorted.map((r) => (
              <tr key={r.line} onClick={() => onLine?.(r.line)}>
                <td className="code-table-n">{r.line}</td>
                {cols.map((c) => (
                  <td key={c} className={numbers[c] ? 'num' : undefined}>
                    {r.cells[c] ?? ''}
                  </td>
                ))}
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      {more > 0 && (
        <p className="code-note">
          The first {maxRows.toLocaleString()} rows are shown, of {(body.length + more).toLocaleString()}.
        </p>
      )}
    </div>
  )
}
