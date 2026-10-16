import { Maximize2, Minimize2, Minus } from 'lucide-react'
import { useContext, type ReactNode } from 'react'

import { Maximise } from './maximise'

// ToolWindow is one of the Code tab's tool windows, as IntelliJ draws them: a
// title bar with the window's own actions and a button to hide it, over its
// content. One in a panel that can take the whole workspace has a button
// for that too.
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
  const max = useContext(Maximise)
  return (
    <div className="code-tool">
      <div className="code-tool-head">
        <span className="code-tool-title">{title}</span>
        {extra}
        <span className="code-tool-spacer" />
        {actions}
        {max && (
          <button
            type="button"
            className="code-tool-btn"
            title={max.maximised ? 'Restore the editor' : `Maximise ${title}`}
            aria-label={max.maximised ? 'Restore the editor' : `Maximise ${title}`}
            aria-pressed={max.maximised}
            onClick={max.toggle}
          >
            {max.maximised ? <Minimize2 size={13} /> : <Maximize2 size={13} />}
          </button>
        )}
        <button type="button" className="code-tool-btn" title={`Hide ${title}`} aria-label={`Hide ${title}`} onClick={onHide}>
          <Minus size={14} />
        </button>
      </div>
      <div className="code-tool-body">{children}</div>
    </div>
  )
}
