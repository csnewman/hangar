import { Check, ChevronLeft, ChevronRight, Copy, ExternalLink, type LucideIcon } from 'lucide-react'
import { useState, type ReactNode } from 'react'
import { Link } from 'react-router'

// The pieces a phone's screens are made of, as the iPhone design draws
// them: a bar for a screen another leads to, a large title for one the
// tabs lead to, and grouped lists of rows.

// Back is the way off a screen: a page to go to, or what to do instead.
export type Back = { to: string; label: string } | { onBack: () => void; label: string }

function BackControl({ back, wide }: { back: Back; wide?: boolean }) {
  const content = wide ? (
    <>
      <ChevronLeft size={22} />
      <span>{back.label}</span>
    </>
  ) : (
    <ChevronLeft size={24} />
  )
  const cls = wide ? 'phone-back' : 'phone-icon-btn'
  const label = wide ? undefined : `Back to ${back.label}`
  if ('onBack' in back) {
    return (
      <button type="button" className={cls} aria-label={label} onClick={back.onBack}>
        {content}
      </button>
    )
  }
  return (
    <Link to={back.to} className={cls} aria-label={label}>
      {content}
    </Link>
  )
}

// PhoneNav heads a screen another leads to: the way back, its title and
// what it is of, and its own buttons.
export function PhoneNav({
  back,
  title,
  sub,
  children,
}: {
  back: Back
  title: ReactNode
  sub?: ReactNode
  children?: ReactNode
}) {
  return (
    <div className="phone-nav">
      <BackControl back={back} />
      <div className="phone-nav-title">
        <span>{title}</span>
        {sub && <span className="phone-row-sub">{sub}</span>}
      </div>
      <div className="phone-nav-end">{children}</div>
    </div>
  )
}

// PhoneHead heads a list: the way back with where it goes, buttons of its
// own, and a large title.
export function PhoneHead({
  back,
  title,
  actions,
  children,
}: {
  back?: Back
  title: ReactNode
  actions?: ReactNode
  children?: ReactNode
}) {
  return (
    <div className="phone-screen-head">
      {(back || actions) && (
        <div className="phone-screen-head-top">
          {back ? <BackControl back={back} wide /> : <span />}
          {actions && <div className="phone-screen-head-actions">{actions}</div>}
        </div>
      )}
      <h1 className="phone-title">{title}</h1>
      {children}
    </div>
  )
}

// PhoneSection is a heading, a grouped list of rows and a note under it.
export function PhoneSection({ title, note, children }: { title?: ReactNode; note?: ReactNode; children: ReactNode }) {
  return (
    <section className="phone-section">
      {title && <h2 className="phone-heading">{title}</h2>}
      <div className="phone-list">{children}</div>
      {note && <p className="phone-note">{note}</p>}
    </section>
  )
}

// PhoneRow is a row of a list: an icon, its label with a line under it,
// something at its end, and, leading somewhere, a chevron.
export function PhoneRow({
  to,
  state,
  href,
  onClick,
  icon: Icon,
  label,
  sub,
  mono,
  end,
  tone,
  disabled,
}: {
  to?: string
  state?: unknown
  href?: string
  onClick?: () => void
  icon?: LucideIcon
  label: ReactNode
  sub?: ReactNode
  // mono sets the line under the label as code.
  mono?: boolean
  end?: ReactNode
  tone?: 'accent' | 'danger'
  disabled?: boolean
}) {
  const cls = ['phone-row', tone && `phone-row-${tone}`].filter(Boolean).join(' ')
  const content = (
    <>
      {Icon && <Icon size={18} />}
      <span className="phone-row-main">
        <span>{label}</span>
        {sub && <span className={mono ? 'phone-row-sub mono' : 'phone-row-sub'}>{sub}</span>}
      </span>
      {end}
      {to && !tone && <ChevronRight size={16} className="phone-row-chev" />}
      {href && <ExternalLink size={16} className="phone-row-chev" />}
    </>
  )
  if (to) {
    return (
      <Link to={to} state={state} className={cls}>
        {content}
      </Link>
    )
  }
  if (href) {
    return (
      <a href={href} target="_blank" rel="noreferrer" className={cls}>
        {content}
      </a>
    )
  }
  if (onClick) {
    return (
      <button type="button" className={cls} onClick={onClick} disabled={disabled}>
        {content}
      </button>
    )
  }
  return <div className={cls}>{content}</div>
}

// PhoneValue is a row's value at its end.
export function PhoneValue({ children }: { children: ReactNode }) {
  return <span className="phone-row-value">{children}</span>
}

// PhoneCopy is a command or a name as a row, with a button that copies it.
export function PhoneCopy({ text, sub }: { text: string; sub?: ReactNode }) {
  const [copied, setCopied] = useState(false)
  const copy = () => {
    navigator.clipboard.writeText(text).then(() => {
      setCopied(true)
      setTimeout(() => setCopied(false), 1500)
    })
  }
  return (
    <div className="phone-row phone-copy">
      <span className="phone-row-main">
        <code className="mono">{text}</code>
        {sub && <span className="phone-row-sub">{sub}</span>}
      </span>
      <button type="button" className="phone-icon-btn" aria-label={copied ? 'Copied' : 'Copy'} onClick={copy}>
        {copied ? <Check size={18} /> : <Copy size={18} />}
      </button>
    </div>
  )
}

// PhoneFoot keeps a screen's buttons at its foot, over the home indicator.
export function PhoneFoot({ children }: { children: ReactNode }) {
  return <div className="phone-foot">{children}</div>
}

// PhoneChoice is a few choices side by side, one of them chosen.
export function PhoneChoice<T extends string>({
  label,
  value,
  options,
  onChange,
  disabled,
}: {
  label: string
  value: T
  options: { value: T; label: string }[]
  onChange: (v: T) => void
  disabled?: boolean
}) {
  return (
    <div className="phone-choice" role="radiogroup" aria-label={label}>
      {options.map((o) => (
        <button
          key={o.value}
          type="button"
          role="radio"
          aria-checked={o.value === value}
          className={o.value === value ? 'phone-choice-on' : undefined}
          disabled={disabled}
          onClick={() => onChange(o.value)}
        >
          {o.label}
        </button>
      ))}
    </div>
  )
}
