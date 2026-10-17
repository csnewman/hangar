import { useState } from 'react'

import { formatMemory } from './format'

type Unit = 'GiB' | 'MiB'

// What the server allows a machine.
const minMiB = 512
const maxMiB = 256 * 1024

// unitFor is the unit a size reads best in: GiB when it is a whole number
// of quarter GiB, else MiB.
const unitFor = (mib: number): Unit => (mib % 256 === 0 ? 'GiB' : 'MiB')

const show = (mib: number, unit: Unit) => (unit === 'GiB' ? String(Number((mib / 1024).toFixed(3))) : String(mib))

// parse reads a size as typed: a number, with a decimal point or comma, in
// the unit chosen or one typed after it -- g, gb or gib, m, mb or mib, in
// any case.
function parse(text: string, unit: Unit): { mib: number; unit: Unit } | null {
  const m = /^\s*(\d+(?:[.,]\d*)?|[.,]\d+)\s*([a-z]*)\s*$/i.exec(text)
  if (!m) return null
  const n = Number(m[1].replace(',', '.'))
  const suffix = m[2].toLowerCase()
  let u = unit
  if (['g', 'gb', 'gib'].includes(suffix)) u = 'GiB'
  else if (['m', 'mb', 'mib'].includes(suffix)) u = 'MiB'
  else if (suffix) return null
  return { mib: Math.round(u === 'GiB' ? n * 1024 : n), unit: u }
}

// MemoryField is a machine's memory: a number in GiB or MiB, as chosen
// beside it, so a size is typed as it is thought of -- 6 GiB, or 6144
// MiB exactly. A unit typed after the number is taken too, and switching
// the unit keeps the size. Beside it is the size in the other unit, or
// what is wrong with what was typed. className is given to its input and
// unit, for the form it is in.
export function MemoryField({
  mib,
  onChange,
  id,
  className,
}: {
  mib: number
  onChange: (mib: number) => void
  id?: string
  className?: string
}) {
  const [unit, setUnit] = useState<Unit>(() => unitFor(mib))
  const [text, setText] = useState(() => show(mib, unit))
  // The size last typed or shown; one set from outside, such as a reset,
  // replaces what is in the field.
  const [seen, setSeen] = useState(mib)
  if (mib !== seen) {
    setSeen(mib)
    setText(show(mib, unit))
  }

  const parsed = parse(text, unit)
  const problem = !parsed
    ? 'A number, such as 6 or 1.5'
    : parsed.mib < minMiB || parsed.mib > maxMiB
      ? `Between ${formatMemory(minMiB)} and ${formatMemory(maxMiB)}`
      : null

  return (
    <span className="memory-field">
      <input
        id={id}
        type="text"
        inputMode="decimal"
        autoComplete="off"
        className={className}
        aria-label={id ? undefined : 'Memory'}
        aria-invalid={problem !== null}
        value={text}
        onChange={(e) => {
          setText(e.target.value)
          const p = parse(e.target.value, unit)
          if (!p) return
          setUnit(p.unit)
          setSeen(p.mib)
          onChange(p.mib)
        }}
        onBlur={() => {
          if (parsed && !problem) setText(show(parsed.mib, parsed.unit))
        }}
      />
      <select
        aria-label="Unit"
        className={className}
        value={unit}
        onChange={(e) => {
          const u = e.target.value as Unit
          setUnit(u)
          setText(show(mib, u))
        }}
      >
        <option value="GiB">GiB</option>
        <option value="MiB">MiB</option>
      </select>
      <span className={problem ? 'memory-field-note field-error' : 'memory-field-note'}>
        {problem ?? (unit === 'GiB' ? `${mib} MiB` : mib >= 1024 ? formatMemory(mib) : '')}
      </span>
    </span>
  )
}
