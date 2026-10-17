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
import { useMe } from '../session'
import { groupTemplates, specLine, useTemplates } from '../templates'
import { useTeams } from '../teams'

// TemplatesPage is the templates, with nothing yet chosen beside them; on a
// phone the list alone.
export function TemplatesPage() {
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

// TemplatesLayout is the templates down the left, grouped by whose they
// are, and beside them the one chosen (children). chosen says one is, which
// a phone shows on its own, under a way back to the list.
export function TemplatesLayout({ children, chosen = false }: { children?: ReactNode; chosen?: boolean }) {
  return (
    <PaneRoot>
      <MasterDetail list={<TemplateList />} chosen={chosen} back={{ to: '/templates', label: 'Templates' }}>
        {children}
      </MasterDetail>
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
      title="Templates"
      back={{ to: '/more', label: 'More' }}
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
