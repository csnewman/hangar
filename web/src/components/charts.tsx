import { useId, useRef, useState, type PointerEvent, type ReactNode } from 'react'

// Charts for live usage. Built to the house specs: 2px lines, a light area
// wash for a single series, hairline recessive grid, a legend whenever there
// are two series, the latest reading once in the header, and a crosshair that
// finds the nearest sample and lists every series there. Series colours come from CSS tokens
// (--series-1, --series-2), validated for both themes; text never wears them.

export interface Series {
  name: string
  values: number[]
}

// StatTile is a single current value with, optionally, its recent trend.
export function StatTile({
  label,
  value,
  detail,
  trend,
  max,
}: {
  label: string
  value: ReactNode
  detail?: ReactNode
  trend?: number[]
  max?: number
}) {
  return (
    <div className="tile">
      <div className="tile-label">{label}</div>
      <div className="tile-value">{value}</div>
      {detail && <div className="tile-detail">{detail}</div>}
      {trend && trend.length > 1 && <Sparkline values={trend} max={max} />}
    </div>
  )
}

function Sparkline({ values, max }: { values: number[]; max?: number }) {
  const w = 120
  const h = 28
  const top = max ?? Math.max(...values, 1e-9)
  const x = (i: number) => (i / (values.length - 1)) * w
  const y = (v: number) => h - 2 - (Math.min(v, top) / top) * (h - 4)
  const d = values.map((v, i) => `${i ? 'L' : 'M'}${x(i).toFixed(1)},${y(v).toFixed(1)}`).join('')
  return (
    <svg className="sparkline" viewBox={`0 0 ${w} ${h}`} preserveAspectRatio="none" aria-hidden="true">
      <path d={d} fill="none" stroke="var(--series-1)" strokeWidth="1.5" strokeLinejoin="round" strokeLinecap="round" vectorEffect="non-scaling-stroke" />
    </svg>
  )
}

// LineChart plots one or two series over the same samples.
export function LineChart({
  title,
  current,
  series,
  times,
  format,
  max,
  floor = 0,
}: {
  title: string
  // current is the latest reading, shown beside the title, so the chart
  // needs no separate tile to say where things stand.
  current?: ReactNode
  series: Series[]
  times: number[]
  format: (v: number) => string
  // max fixes the top of the scale, for a quantity with a ceiling.
  max?: number
  // floor is the least the scale shows, so an idle quantity draws a flat
  // line at the bottom of a sensible scale rather than filling a tiny one.
  floor?: number
}) {
  const id = useId()
  const ref = useRef<SVGSVGElement>(null)
  const [hover, setHover] = useState<number | null>(null)
  const n = times.length

  const width = 560
  const height = 180
  const pad = { top: 10, right: 12, bottom: 22, left: 56 }
  const plotW = width - pad.left - pad.right
  const plotH = height - pad.top - pad.bottom

  const peak = Math.max(...series.flatMap((s) => s.values), 0)
  const top = max ?? niceCeiling(Math.max(peak, floor))
  const ticks = [0, top / 2, top]
  const x = (i: number) => pad.left + (n > 1 ? (i / (n - 1)) * plotW : plotW)
  const y = (v: number) => pad.top + plotH - (Math.min(Math.max(v, 0), top) / top) * plotH

  const onMove = (e: PointerEvent<SVGSVGElement>) => {
    const svg = ref.current
    if (!svg || n === 0) return
    const box = svg.getBoundingClientRect()
    const px = ((e.clientX - box.left) / box.width) * width
    const i = Math.round(((px - pad.left) / plotW) * (n - 1))
    setHover(Math.min(Math.max(i, 0), n - 1))
  }

  const colours = ['var(--series-1)', 'var(--series-2)']
  const single = series.length === 1

  return (
    <figure className="chart">
      <figcaption className="chart-head">
        <span className="chart-heading">
          <span className="chart-title">{title}</span>
          {current && <span className="chart-current">{current}</span>}
        </span>
        {!single && (
          <span className="legend">
            {series.map((s, i) => (
              <span key={s.name} className="legend-item">
                <span className="legend-key" style={{ background: colours[i] }} />
                {s.name}
              </span>
            ))}
          </span>
        )}
      </figcaption>
      {n < 2 ? (
        <div className="chart-empty">Collecting samples…</div>
      ) : (
        <div className="chart-plot">
          <svg
            ref={ref}
            viewBox={`0 0 ${width} ${height}`}
            role="img"
            aria-labelledby={`${id}-desc`}
            onPointerMove={onMove}
            onPointerLeave={() => setHover(null)}
          >
            <desc id={`${id}-desc`}>
              {title}: {series.map((s) => `${s.name} ${format(s.values[n - 1])}`).join(', ')}
            </desc>
            {ticks.map((t) => (
              <g key={t}>
                <line className="grid" x1={pad.left} x2={pad.left + plotW} y1={y(t)} y2={y(t)} />
                <text className="tick" x={pad.left - 8} y={y(t)} dy="0.35em" textAnchor="end">
                  {format(t)}
                </text>
              </g>
            ))}
            <text className="tick" x={pad.left} y={height - 4}>
              {ago(times[0], times[n - 1])}
            </text>
            <text className="tick" x={pad.left + plotW} y={height - 4} textAnchor="end">
              now
            </text>
            {series.map((s, si) => {
              const d = s.values.map((v, i) => `${i ? 'L' : 'M'}${x(i).toFixed(1)},${y(v).toFixed(1)}`).join('')
              return (
                <g key={s.name}>
                  {single && (
                    <path
                      d={`${d}L${x(n - 1)},${y(0)}L${x(0)},${y(0)}Z`}
                      fill={colours[si]}
                      opacity="0.1"
                    />
                  )}
                  <path d={d} fill="none" stroke={colours[si]} strokeWidth="2" strokeLinejoin="round" strokeLinecap="round" />
                </g>
              )
            })}
            {hover !== null && (
              <g>
                <line className="crosshair" x1={x(hover)} x2={x(hover)} y1={pad.top} y2={pad.top + plotH} />
                {series.map((s, si) => (
                  <circle
                    key={s.name}
                    cx={x(hover)}
                    cy={y(s.values[hover])}
                    r="4"
                    fill={colours[si]}
                    stroke="var(--surface)"
                    strokeWidth="2"
                  />
                ))}
              </g>
            )}
            <rect x={pad.left} y={pad.top} width={plotW} height={plotH} fill="transparent" />
          </svg>
          {hover !== null && (
            <div
              className="chart-tip"
              style={{ left: `${(x(hover) / width) * 100}%` }}
              role="status"
            >
              <div className="tip-time">{new Date(times[hover]).toLocaleTimeString()}</div>
              {series.map((s, si) => (
                <div key={s.name} className="tip-row">
                  <span className="tip-key" style={{ background: colours[si] }} />
                  <span className="tip-value">{format(s.values[hover])}</span>
                  {!single && <span className="tip-name">{s.name}</span>}
                </div>
              ))}
            </div>
          )}
        </div>
      )}
    </figure>
  )
}

// niceCeiling rounds a peak up to 1, 2 or 5 times a power of ten, so the
// scale's ticks are round numbers.
function niceCeiling(v: number): number {
  if (v <= 0) return 1
  const p = 10 ** Math.floor(Math.log10(v))
  for (const m of [1, 2, 5, 10]) {
    if (v <= m * p) return m * p
  }
  return 10 * p
}

function ago(from: number, to: number): string {
  const s = Math.round((to - from) / 1000)
  return s < 60 ? `${s}s ago` : `${Math.round(s / 60)}m ago`
}

// Meter is a share of a limit.
export function Meter({ used, total, label }: { used: number; total: number; label: string }) {
  const pct = total > 0 ? Math.min(100, (used / total) * 100) : 0
  return (
    <div className="meter" title={label}>
      <div className="meter-label nowrap">{label}</div>
      <div className="meter-track">
        <div className={`meter-fill${pct > 90 ? ' meter-hot' : ''}`} style={{ width: `${pct}%` }} />
      </div>
    </div>
  )
}
