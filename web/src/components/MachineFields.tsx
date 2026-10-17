import { Minus, Plus } from 'lucide-react'
import { useState } from 'react'

import { machineFit, useWorkerSizes } from '../environments'
import { formatMemory } from './format'

// What the server allows a machine.
const minCPUs = 1
const maxCPUs = 64
const minMiB = 512
const maxMiB = 256 * 1024

// Note is what is said beside a field: what its value comes to, or why it
// may not fit, or what is wrong with it.
function Note({ text, tone }: { text: string; tone?: 'warn' | 'error' }) {
  if (!text) return null
  return <span className={tone ? `machine-note machine-note-${tone}` : 'machine-note'}>{text}</span>
}

// CPUField is a machine's vCPUs: a whole number, typed or stepped with its
// buttons or the arrow keys. Beside it is how many the largest worker
// online has, or that none has as many. memory, if given, is the machine's
// memory, which a worker must have as well. className is given to its
// input, for the form it is in.
export function CPUField({
  cpus,
  onChange,
  memory,
  id,
  className,
}: {
  cpus: number
  onChange: (cpus: number) => void
  memory?: number
  id?: string
  className?: string
}) {
  const sizes = useWorkerSizes()
  const [text, setText] = useState(String(cpus))
  const [seen, setSeen] = useState(cpus)
  if (cpus !== seen) {
    setSeen(cpus)
    setText(String(cpus))
  }
  const set = (n: number) => {
    const v = Math.max(minCPUs, Math.min(maxCPUs, n))
    setText(String(v))
    setSeen(v)
    onChange(v)
  }

  const valid = /^\s*\d+\s*$/.test(text) && Number(text) >= minCPUs && Number(text) <= maxCPUs
  const fit = machineFit(sizes.data, cpus, memory ?? 0)
  let note = { text: `${minCPUs} to ${maxCPUs}`, tone: undefined as 'warn' | 'error' | undefined }
  if (!valid) note = { text: `A whole number, ${minCPUs} to ${maxCPUs}`, tone: 'error' }
  else if (fit && cpus > fit.cpus) note = { text: `No worker has ${cpus}; the most is ${fit.cpus}`, tone: 'warn' }
  else if (fit) note = { text: `of ${fit.cpus} on the largest worker`, tone: undefined }

  return (
    <span className="machine-field">
      <span className="cpu-field">
        <button
          type="button"
          aria-label="Fewer vCPUs"
          disabled={cpus <= minCPUs}
          tabIndex={-1}
          onClick={() => set(cpus - 1)}
        >
          <Minus size={12} />
        </button>
        <input
          id={id}
          type="text"
          inputMode="numeric"
          autoComplete="off"
          className={className}
          aria-label={id ? undefined : 'vCPUs'}
          aria-invalid={!valid}
          value={text}
          onChange={(e) => {
            setText(e.target.value)
            if (/^\s*\d+\s*$/.test(e.target.value)) {
              const n = Number(e.target.value)
              setSeen(n)
              onChange(n)
            }
          }}
          onKeyDown={(e) => {
            if (e.key === 'ArrowUp' || e.key === 'ArrowDown') {
              e.preventDefault()
              set(cpus + (e.key === 'ArrowUp' ? 1 : -1))
            }
          }}
          onBlur={() => valid && setText(String(cpus))}
        />
        <button
          type="button"
          aria-label="More vCPUs"
          disabled={cpus >= maxCPUs}
          tabIndex={-1}
          onClick={() => set(cpus + 1)}
        >
          <Plus size={12} />
        </button>
      </span>
      <Note {...note} />
    </span>
  )
}

type Unit = 'GiB' | 'MiB'

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
// the unit keeps the size. Beside it is the size in the other unit and how
// much the largest worker online has, or that none has as much -- with
// cpus, if given, as well -- or what is wrong with what was typed.
// className is given to its input and unit, for the form it is in.
// running is what a running machine's memory can be resized between, for
// one being resized as it runs.
export function MemoryField({
  mib,
  onChange,
  cpus,
  id,
  className,
  running,
}: {
  mib: number
  onChange: (mib: number) => void
  cpus?: number
  id?: string
  className?: string
  running?: { min: number; max: number }
}) {
  const sizes = useWorkerSizes()
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
  const other = unit === 'GiB' ? `${mib} MiB` : mib >= 1024 ? formatMemory(mib) : ''
  const fit = machineFit(sizes.data, cpus ?? 0, mib)
  let note: { text: string; tone?: 'warn' | 'error' } = { text: other }
  if (!parsed) note = { text: 'A number, such as 6 or 1.5', tone: 'error' }
  else if (parsed.mib < minMiB || parsed.mib > maxMiB)
    note = { text: `Between ${formatMemory(minMiB)} and ${formatMemory(maxMiB)}`, tone: 'error' }
  else if (running && mib < running.min)
    note = { text: `While it runs, it can go down to ${formatMemory(running.min)}`, tone: 'warn' }
  else if (running && mib > running.max)
    note = { text: `While it runs, it can go up to ${formatMemory(running.max)}`, tone: 'warn' }
  else if (running) note = { text: `${formatMemory(running.min)} to ${formatMemory(running.max)} while it runs` }
  else if (fit && mib > fit.memory)
    note = { text: `No worker has ${formatMemory(mib)}; the most is ${formatMemory(fit.memory)}`, tone: 'warn' }
  else if (fit && !fit.fits && cpus !== undefined && cpus <= fit.cpus)
    note = { text: `No worker has ${cpus} vCPUs and ${formatMemory(mib)} together`, tone: 'warn' }
  else if (fit)
    note = { text: [other, `of ${formatMemory(fit.memory)} on the largest worker`].filter(Boolean).join(' · ') }

  return (
    <span className="machine-field">
      <input
        id={id}
        type="text"
        inputMode="decimal"
        autoComplete="off"
        className={className}
        aria-label={id ? undefined : 'Memory'}
        aria-invalid={note.tone === 'error'}
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
          if (parsed && note.tone !== 'error') setText(show(parsed.mib, parsed.unit))
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
      <Note {...note} />
    </span>
  )
}
