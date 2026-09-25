import { useEffect } from 'react'

// useTitle names the browser tab after what is on screen, most specific
// first: "ssh-check · Code · Hangar". It is set back to "Hangar" when the
// page goes.
export function useTitle(...parts: (string | undefined | null | false)[]) {
  const title = [...parts.filter(Boolean), 'Hangar'].join(' · ')
  useEffect(() => {
    document.title = title
    return () => {
      document.title = 'Hangar'
    }
  }, [title])
}
