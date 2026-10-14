import { useEffect, useState } from 'react'

// useChangeCount is how many files git says are uncommitted in the
// environment's folder, kept up to date as they change, or null until it
// is known or where there is no repository. It reads it through the Code
// tab's connection, loaded only when asked for, so a terminal's page does
// not carry the editor.
export function useChangeCount(env: string, enabled: boolean): number | null {
  const [count, setCount] = useState<number | null>(null)
  useEffect(() => {
    if (!enabled) return
    let live = true
    let close = () => {}
    import('../code/client').then(({ CodeClient }) => {
      if (!live) return
      const client = new CodeClient(env)
      let timer: ReturnType<typeof setTimeout> | undefined
      const load = () =>
        client.status().then(
          (s) => live && setCount(s.repo ? s.changes.length : null),
          () => {},
        )
      const later = () => {
        clearTimeout(timer)
        timer = setTimeout(load, 300)
      }
      const offs = [client.on('fs.changed', later), client.on('git.changed', later)]
      load()
      close = () => {
        clearTimeout(timer)
        offs.forEach((off) => off())
        client.close()
      }
    })
    return () => {
      live = false
      close()
    }
  }, [env, enabled])
  return enabled ? count : null
}
