import { Share, SquarePlus, X } from 'lucide-react'
import { useState } from 'react'

import { installable } from '../phone'

const hintKey = 'hangar.install.dismissed'

// InstallHint says how to put Hangar on the home screen, in Safari on an
// iPhone, until it is dismissed.
export function InstallHint() {
  const [shown, setShown] = useState(() => {
    try {
      return installable() && localStorage.getItem(hintKey) !== 'true'
    } catch {
      return installable()
    }
  })
  if (!shown) return null
  const dismiss = () => {
    setShown(false)
    try {
      localStorage.setItem(hintKey, 'true')
    } catch {
      // Shown again next time.
    }
  }
  return (
    <div className="phone-card install-hint">
      <SquarePlus size={22} className="install-hint-icon" />
      <div className="install-hint-text">
        <span className="strong">Add Hangar to your Home Screen</span>
        <span className="muted">
          In Safari's Share menu <Share size={14} className="install-hint-share" aria-hidden="true" />, choose Add to
          Home Screen. Hangar then opens full screen, like an app, and asks you to sign in once more.
        </span>
      </div>
      <button type="button" className="phone-icon-btn" aria-label="Dismiss" onClick={dismiss}>
        <X size={18} />
      </button>
    </div>
  )
}
