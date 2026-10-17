import { Link, useSearchParams } from 'react-router'

import { NoneChosen } from '../components/Pane'
import { PhoneHead } from '../components/Phone'
import { usePhone } from '../phone'
import { NewPack, PackList, PacksLayout } from './Packs'

// FilesPage is the packs the user may use, the built-in profile first. A
// pack is a list of paths shared between environments; each is used
// through a copy of its files, the user's own or a shared one. On a
// computer the packs are a list, with a new pack's form beside it while one
// is being made.
export function FilesPage() {
  const [params] = useSearchParams()
  if (usePhone()) {
    return (
      <div className="page phone-page">
        <PhoneHead back={{ to: '/more', label: 'More' }} title="Files">
          <p className="phone-note">
            Packs share files between environments: each is a list of paths, and an environment uses a copy of its
            files, your own or one shared with a team.
          </p>
        </PhoneHead>
        <PackList />
      </div>
    )
  }
  return (
    <PacksLayout>
      {params.has('new') ? (
        <NewPack />
      ) : (
        <NoneChosen>
          Packs share files between environments: each is a list of paths, and an environment uses a copy of its files
          -- your own, or one shared with a team. Choose one, or <Link to="/files?new">make a new pack</Link>.
        </NoneChosen>
      )}
    </PacksLayout>
  )
}
