import { ChevronLeft } from 'lucide-react'
import { type ReactNode } from 'react'
import { Link } from 'react-router'

// The pieces a phone's screens are made of where a computer's pages have
// none: a bar for a screen another leads to, a large title for one the tabs
// lead to, a bar at the foot, and a choice among a few.

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

// PhoneBack is a screen's way back alone, over a page with a head of its
// own.
export function PhoneBack({ back }: { back: Back }) {
  return (
    <div className="phone-screen-head-top phone-back-bar">
      <BackControl back={back} wide />
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
