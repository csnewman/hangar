import './pane.css'

import { BellDot, Copy, Info, Search, X } from 'lucide-react'
import { useState, type ReactNode, type Ref } from 'react'
import { NavLink } from 'react-router'

import { copyText } from '../code/ContextMenu'

// PaneRoot is a page that fills the window and scrolls inside: its bar at
// the top, then a list beside the chosen one, or a page of its own.
export function PaneRoot({ children }: { children: ReactNode }) {
  return <div className="pane-root">{children}</div>
}

// PaneBar is a page's title, with what it is searched by and what it makes
// at the far end.
export function PaneBar({ title, children }: { title: string; children?: ReactNode }) {
  return (
    <header className="pane-bar">
      <h1>{title}</h1>
      {children && <div className="pane-bar-actions">{children}</div>}
    </header>
  )
}

// SearchField is a box that narrows a list as it is typed into. keys, if
// given, is the shortcut that focuses it.
export function SearchField({
  value,
  onChange,
  placeholder,
  keys,
  width,
  inputRef,
}: {
  value: string
  onChange: (v: string) => void
  placeholder: string
  keys?: string
  width?: number
  inputRef?: Ref<HTMLInputElement>
}) {
  return (
    <label className="pane-search" style={width ? { width } : undefined}>
      <Search size={13} />
      <input
        ref={inputRef}
        type="search"
        aria-label={placeholder}
        placeholder={placeholder}
        value={value}
        onChange={(e) => onChange(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === 'Escape' && value) {
            e.preventDefault()
            onChange('')
          }
        }}
      />
      {keys && !value && <kbd>{keys}</kbd>}
    </label>
  )
}

// MasterDetail is a list of things on the left and the chosen one beside it.
export function MasterDetail({ list, children }: { list: ReactNode; children: ReactNode }) {
  return (
    <div className="md">
      {list}
      <div className="md-detail">{children}</div>
    </div>
  )
}

// ListPane is the list: what it is searched by and made with at its head,
// then its groups.
export function ListPane({ head, children }: { head?: ReactNode; children: ReactNode }) {
  return (
    <nav className="md-list">
      {head && <div className="md-list-head">{head}</div>}
      <div className="md-list-body">{children}</div>
    </nav>
  )
}

export function ListGroup({ title, children }: { title?: string; children: ReactNode }) {
  return (
    <>
      {title && <div className="md-group">{title}</div>}
      {children}
    </>
  )
}

// ListItem is one thing in the list, a link to it, marked while chosen.
export function ListItem({
  to,
  mark,
  title,
  sub,
  end,
}: {
  to: string
  mark?: ReactNode
  title: ReactNode
  sub?: ReactNode
  end?: ReactNode
}) {
  return (
    <NavLink to={to} className="md-item">
      {mark}
      <span className="md-item-main">
        <span className="md-item-title">{title}</span>
        {sub && <span className="md-item-sub">{sub}</span>}
      </span>
      {end && <span className="md-item-end">{end}</span>}
    </NavLink>
  )
}

// NoneChosen fills the detail while nothing in the list is chosen.
export function NoneChosen({ children }: { children: ReactNode }) {
  return <div className="md-none">{children}</div>
}

// DetailHead is the chosen one's head: its mark, name and what it is, and
// what is done to it.
export function DetailHead({
  mark,
  title,
  sub,
  actions,
}: {
  mark?: ReactNode
  title: ReactNode
  sub?: ReactNode
  actions?: ReactNode
}) {
  return (
    <header className="pane-head">
      {mark}
      <div className="pane-head-main">
        <div className="pane-head-title">
          <h2>{title}</h2>
        </div>
        {sub && <div className="pane-head-sub">{sub}</div>}
      </div>
      {actions && <div className="pane-head-actions">{actions}</div>}
    </header>
  )
}

// DetailPage is the chosen one's sections, scrolling under its head.
export function DetailPage({
  children,
  wide = false,
  scrollRef,
}: {
  children: ReactNode
  wide?: boolean
  scrollRef?: Ref<HTMLDivElement>
}) {
  return (
    <div className="pane-scroll" ref={scrollRef}>
      <div className={wide ? 'pane-page pane-page-wide' : 'pane-page'}>{children}</div>
    </div>
  )
}

// Section is a titled rule over what it holds. extra sits at the rule's
// far end. id names it for an index to jump to.
export function Section({
  id,
  title,
  extra,
  children,
}: {
  id?: string
  title: string
  extra?: ReactNode
  children: ReactNode
}) {
  return (
    <section className="pane-section" data-section={id} aria-label={title}>
      <div className="pane-rule">
        <h3>{title}</h3>
        {extra && <div className="pane-rule-extra">{extra}</div>}
      </div>
      {children}
    </section>
  )
}

// Rows are labels beside their values.
export function Rows({ children }: { children: ReactNode }) {
  return <dl className="pane-rows">{children}</dl>
}

// Row is one label and its value; modified marks a value changed and not
// yet applied.
export function Row({ label, modified, children }: { label: ReactNode; modified?: boolean; children: ReactNode }) {
  return (
    <>
      <dt className={modified ? 'pane-modified' : undefined}>{label}</dt>
      <dd>{children}</dd>
    </>
  )
}

// CopyField is text to copy, in a box with its Copy button.
export function CopyField({ text }: { text: string }) {
  const [copied, setCopied] = useState(false)
  return (
    <span className="pane-copy">
      <code className="mono">{text}</code>
      <button
        type="button"
        aria-label="Copy"
        title={copied ? 'Copied' : 'Copy'}
        onClick={() => {
          copyText(text)
          setCopied(true)
          setTimeout(() => setCopied(false), 1500)
        }}
      >
        <Copy size={12} />
      </button>
    </span>
  )
}

// Segmented is a choice of a few, side by side.
export function Segmented<T extends string | boolean>({
  value,
  options,
  onChange,
  label,
  disabled,
}: {
  value: T
  options: { value: T; label: string }[]
  onChange: (v: T) => void
  label: string
  disabled?: boolean
}) {
  return (
    <span className="pane-segmented" role="radiogroup" aria-label={label}>
      {options.map((o) => (
        <button
          key={String(o.value)}
          type="button"
          role="radio"
          aria-checked={o.value === value}
          className={o.value === value ? 'pane-segment pane-segment-on' : 'pane-segment'}
          disabled={disabled}
          onClick={() => o.value !== value && onChange(o.value)}
        >
          {o.label}
        </button>
      ))}
    </span>
  )
}

// Banner is a notice across the top: what it is about, and what can be
// done.
export function Banner({
  tone,
  children,
  actions,
  onDismiss,
  below,
}: {
  tone: 'warn' | 'info'
  children: ReactNode
  actions?: ReactNode
  onDismiss?: () => void
  below?: ReactNode
}) {
  return (
    <div className={`pane-banner pane-banner-${tone}`} role="status">
      <div className="pane-banner-row">
        {tone === 'warn' ? <BellDot size={14} /> : <Info size={14} />}
        <div className="pane-banner-text">{children}</div>
        {actions && <div className="pane-banner-actions">{actions}</div>}
        {onDismiss && (
          <button type="button" className="pane-icon-btn" aria-label="Dismiss" title="Dismiss" onClick={onDismiss}>
            <X size={13} />
          </button>
        )}
      </div>
      {below}
    </div>
  )
}

// ApplyBar is the bar at the foot that sums up what has been changed and
// applies it: count changes, what they are, and the buttons.
export function ApplyBar({ count, children, actions }: { count: number; children: ReactNode; actions: ReactNode }) {
  return (
    <div className="pane-apply">
      <strong className="pane-modified">
        {count} change{count === 1 ? '' : 's'}
      </strong>
      <span className="pane-apply-what">{children}</span>
      {actions}
    </div>
  )
}

// The colours a thing's mark is drawn on, chosen by its name so it keeps
// one.
const tileColours = ['#0b72b9', '#7c5cc4', '#c4582f', '#2f8f6b', '#9b3c6e', '#4f7a28', '#8a6d3b', '#3a6ea5', '#b0452f']

// Tile is a thing's mark, its initials on a colour of its own, as an IDE
// marks a project.
export function Tile({ name, small = false }: { name: string; small?: boolean }) {
  let h = 0
  for (const c of name) h = (h * 31 + c.charCodeAt(0)) >>> 0
  const words = name.split(/[\s._/-]+/).filter(Boolean)
  const letters = words.length > 1 ? words[0][0] + words[1][0] : name.slice(0, 2)
  return (
    <span
      className={small ? 'pane-tile pane-tile-small' : 'pane-tile'}
      style={{ background: tileColours[h % tileColours.length] }}
      aria-hidden="true"
    >
      {letters.toUpperCase()}
    </span>
  )
}

// Meter is how full something is, from 0 to 1.
export function Meter({ value, width = 80, colour }: { value: number; width?: number; colour?: string }) {
  const v = Math.max(0, Math.min(1, value))
  return (
    <span
      className="pane-meter"
      style={{ width }}
      role="meter"
      aria-valuenow={Math.round(v * 100)}
      aria-valuemin={0}
      aria-valuemax={100}
    >
      <span style={{ width: `${v * 100}%`, background: colour }} />
    </span>
  )
}
