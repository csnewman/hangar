import './templates.css'

import { Globe, Lock, Plus } from 'lucide-react'
import { useState, type ReactNode } from 'react'
import { Link } from 'react-router'

import type { Template } from '../api'
import {
  ListGroup,
  ListItem,
  ListPane,
  MasterDetail,
  NoneChosen,
  PaneRoot,
  SearchField,
  Tile,
} from '../components/Pane'
import { PhoneHead, PhoneRow, PhoneSection } from '../components/Phone'
import { usePhone } from '../phone'
import { useMe } from '../session'
import { groupTemplates, specLine, useTemplates } from '../templates'
import { useTeams } from '../teams'

// TemplatesPage is the templates: on a computer the list, with nothing yet
// chosen beside it; on a phone a screen of them, grouped as the list is.
export function TemplatesPage() {
  const me = useMe()
  const templates = useTemplates()
  const teams = useTeams()
  const { mine, teams: teamGroups, collaborating, others } = groupTemplates(templates.data ?? [], me, teams.data)
  const phone = usePhone()

  if (!phone) {
    return (
      <TemplatesLayout>
        <NoneChosen>
          <p>Choose a template to see and change it, or make a new one.</p>
          <Link to="/templates/new" className="btn btn-primary btn-sm">
            <Plus size={14} />
            New template
          </Link>
        </NoneChosen>
      </TemplatesLayout>
    )
  }

  return (
    <div className="page phone-page">
      <PhoneHead
        back={{ to: '/more', label: 'More' }}
        title="Templates"
        actions={
          <Link to="/templates/new" className="phone-icon-btn" aria-label="New template">
            <Plus size={22} />
          </Link>
        }
      />

      {templates.isError && <div className="alert">Could not load templates: {templates.error.message}</div>}

      <TemplateSection
        title="Yours"
        templates={mine}
        loading={templates.isPending}
        empty={
          <>
            You have no templates. <Link to="/templates/new">Make one</Link> to stop starting from scratch.
          </>
        }
      />
      {teamGroups.map(({ team, templates: owned }) => (
        <TemplateSection
          key={team.id}
          title={
            <>
              Team <Link to={`/teams/${team.id}`}>{team.name}</Link>
            </>
          }
          templates={owned}
          empty={
            team.role === 'viewer' ? (
              'The team has no templates yet.'
            ) : (
              <>
                The team has no templates yet. <Link to={`/templates/new?team=${team.id}`}>Make one</Link>.
              </>
            )
          }
        />
      ))}
      {collaborating.length > 0 && <TemplateSection title="Shared with you to edit" templates={collaborating} />}
      <TemplateSection
        title={me.admin ? 'Everyone else’s' : 'Shared by others'}
        templates={others}
        loading={templates.isPending}
        empty="Nobody else has shared a template."
      />
    </div>
  )
}

// TemplatesLayout is the templates down the left, grouped by whose they
// are, and beside them the one chosen (children).
export function TemplatesLayout({ children }: { children?: ReactNode }) {
  return (
    <PaneRoot>
      <MasterDetail list={<TemplateList />}>{children}</MasterDetail>
    </PaneRoot>
  )
}

function TemplateList() {
  const me = useMe()
  const templates = useTemplates()
  const teams = useTeams()
  const [query, setQuery] = useState('')
  const q = query.trim().toLowerCase()
  const matches = (t: Template) =>
    !q || [t.name, t.description ?? '', t.spec.image].some((s) => s.toLowerCase().includes(q))
  const {
    mine,
    teams: teamGroups,
    collaborating,
    others,
  } = groupTemplates((templates.data ?? []).filter(matches), me, teams.data)
  const item = (t: Template) => (
    <ListItem
      key={t.id}
      to={`/templates/${t.id}`}
      mark={<Tile name={t.name} />}
      title={t.name}
      sub={specLine(t.spec, t.name_pattern)}
      end={t.environments > 0 ? `${t.environments} env${t.environments === 1 ? '' : 's'}` : undefined}
    />
  )
  const none = mine.length + collaborating.length + others.length + teamGroups.flatMap((g) => g.templates).length === 0

  return (
    <ListPane
      head={
        <>
          <SearchField value={query} onChange={setQuery} placeholder="Templates" />
          <Link to="/templates/new" className="pane-icon-btn tpl-new" aria-label="New template" title="New template">
            <Plus size={15} />
          </Link>
        </>
      }
    >
      {templates.isError && <div className="md-empty-list">Could not load templates: {templates.error.message}</div>}
      {mine.length > 0 && <ListGroup title="Yours">{mine.map(item)}</ListGroup>}
      {teamGroups
        .filter((g) => g.templates.length > 0)
        .map(({ team, templates: owned }) => (
          <ListGroup key={team.id} title={`Team ${team.name}`}>
            {owned.map(item)}
          </ListGroup>
        ))}
      {collaborating.length > 0 && <ListGroup title="Shared with you to edit">{collaborating.map(item)}</ListGroup>}
      {others.length > 0 && (
        <ListGroup title={me.admin ? 'Everyone else’s' : 'Shared by others'}>{others.map(item)}</ListGroup>
      )}
      {templates.isSuccess && none && (
        <div className="md-empty-list">{q ? 'No template matches.' : 'No templates yet.'}</div>
      )}
    </ListPane>
  )
}

// TemplateSection is one group of templates on a phone's screen of them.
function TemplateSection({
  title,
  templates,
  loading = false,
  empty,
}: {
  title: ReactNode
  templates: Template[]
  loading?: boolean
  empty?: ReactNode
}) {
  const me = useMe()
  return (
    <PhoneSection
      title={
        <>
          {title} · {templates.length}
        </>
      }
      note={!loading && templates.length === 0 ? empty : undefined}
    >
      {templates.map((t) => (
        <PhoneRow
          key={t.id}
          to={`/templates/${t.id}`}
          label={t.name}
          sub={`${specLine(t.spec, t.name_pattern)} · ${t.team ? `team ${t.team.name}` : t.owner.id === me.id ? 'yours' : `by ${t.owner.display_name || t.owner.username}`}`}
          end={<VisibilityBadge template={t} />}
        />
      ))}
    </PhoneSection>
  )
}

export function VisibilityBadge({ template: t }: { template: Template }) {
  if (t.visibility === 'shared') {
    return (
      <span className="badge badge-good" title="Everyone can see and use it">
        <Globe size={11} />
        shared
      </span>
    )
  }
  return (
    <span
      className="badge badge-idle"
      title={t.team ? 'Only its team and collaborators can see it' : 'Only its owner and collaborators can see it'}
    >
      <Lock size={11} />
      private
    </span>
  )
}
