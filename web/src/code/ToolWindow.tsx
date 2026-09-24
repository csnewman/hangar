import { Minus } from 'lucide-react'
import type { ReactNode } from 'react'

// ToolWindow is one of the Code tab's tool windows, as IntelliJ draws them: a
// title bar with the window's own actions and a button to hide it, over its
// content.
export function ToolWindow({
  title,
  extra,
  actions,
  onHide,
  children,
}: {
  title: string
  extra?: ReactNode
  actions?: ReactNode
  onHide: () => void
  children: ReactNode
}) {
  return (
    <div className="code-tool">
      <div className="code-tool-head">
        <span className="code-tool-title">{title}</span>
        {extra}
        <span className="code-tool-spacer" />
        {actions}
        <button type="button" className="code-tool-btn" title={`Hide ${title}`} aria-label={`Hide ${title}`} onClick={onHide}>
          <Minus size={14} />
        </button>
      </div>
      <div className="code-tool-body">{children}</div>
    </div>
  )
}
