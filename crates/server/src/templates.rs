//! Templates: recipes for environments, so nobody starts from an empty
//! machine. Who may see, edit and manage each, their specs' rules, and the
//! variables a name fills in, as internal/templates has them.

use std::collections::{BTreeMap, HashMap};

use chrono::Utc;
use hangar_api::{Display, Gpu, PackRef, Repo, Spec, TemplateSpec};
use regex::Regex;
use uuid::Uuid;

use crate::audit::{self, Event};
use crate::fail::{self, Fail};
use crate::store::rows::{Collaborator, Team, TeamMember, Template, User};
use crate::store::{Result as StoreResult, Store, Tx};
use crate::users::Principal;

pub const MAX_CPUS: i64 = 64;
pub const MIN_MEMORY_MIB: i64 = 512;
pub const MAX_MEMORY_MIB: i64 = 256 * 1024;
pub const MAX_SWAP_MIB: i64 = 256 * 1024;
const MAX_REPOS: usize = 16;
const MAX_PROJECTS: usize = 16;
const MAX_WEB_NAMES: usize = 16;
const MAX_PACKS: usize = 16;
/// A web server name and -env<short> make one DNS label.
const MAX_WEB_NAME: usize = 63 - "-env000000".len();

/// Someone, as a template shows them.
#[derive(Debug, Clone)]
pub struct Person {
    pub id: Uuid,
    pub username: String,
    pub display_name: String,
}

impl Person {
    fn of(u: &User) -> Person {
        Person {
            id: u.id,
            username: u.username.clone(),
            display_name: u.display_name.clone(),
        }
    }
}

/// A team, as a template names it.
#[derive(Debug, Clone)]
pub struct TeamRef {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
}

/// A template as someone sees it.
#[derive(Debug, Clone)]
pub struct View {
    pub row: Template,
    pub spec: TemplateSpec,
    pub owner: Person,
    pub team: Option<TeamRef>,
    pub collaborators: Vec<Person>,
    pub collaborator_teams: Vec<TeamRef>,
    pub environments: i64,
    pub can_edit: bool,
    pub can_manage: bool,
}

/// What a template is made or changed with.
pub struct Input {
    pub name: String,
    pub description: String,
    pub visibility: String,
    pub spec: TemplateSpec,
    /// The team to own it; Some("") gives it back to whoever asks.
    pub team: Option<String>,
}

/// Everything who may do what with templates is worked out from.
struct World {
    templates: Vec<Template>,
    users: HashMap<Uuid, User>,
    teams: HashMap<Uuid, Team>,
    members: Vec<TeamMember>,
    collaborators: Vec<Collaborator>,
    team_collaborators: Vec<Collaborator>,
    environments: HashMap<Uuid, i64>,
}

impl World {
    async fn load(tx: &mut Tx) -> StoreResult<World> {
        let templates = tx.templates().await?;
        let users = tx.people().await?.into_iter().map(|u| (u.id, u)).collect();
        let teams = tx.teams().await?.into_iter().map(|t| (t.id, t)).collect();
        let members = tx.team_members().await?;
        let (collaborators, team_collaborators) = tx.collaborators().await?;
        let environments = tx.template_environments().await?;
        Ok(World {
            templates,
            users,
            teams,
            members,
            collaborators,
            team_collaborators,
            environments,
        })
    }

    fn role(&self, team: Uuid, user: Uuid) -> Option<&str> {
        self.members
            .iter()
            .find(|m| m.team_id == team && m.user_id == user)
            .map(|m| m.role.as_str())
    }

    /// Whether p sees, edits and manages t.
    fn access(&self, p: &Principal, t: &Template) -> (bool, bool, bool) {
        let personal = t.team_id.is_none() && t.owner_id == p.user_id;
        let collaborator = self
            .collaborators
            .iter()
            .any(|c| c.template_id == t.id && c.who == p.user_id);
        let role = t.team_id.and_then(|team| self.role(team, p.user_id));
        let via_team = |editors: bool| {
            self.team_collaborators.iter().any(|c| {
                c.template_id == t.id
                    && self
                        .role(c.who, p.user_id)
                        .is_some_and(|r| !editors || r == "member" || r == "admin")
            })
        };
        let see = t.visibility == "shared"
            || p.admin
            || personal
            || collaborator
            || role.is_some()
            || via_team(false);
        let edit = p.admin
            || personal
            || collaborator
            || matches!(role, Some("member" | "admin"))
            || via_team(true);
        let manage = p.admin || personal || role == Some("admin");
        (see, edit, manage)
    }

    fn view(&self, p: &Principal, t: &Template) -> Option<View> {
        let (see, edit, manage) = self.access(p, t);
        if !see {
            return None;
        }
        Some(self.view_as(t, edit, manage))
    }

    fn view_as(&self, t: &Template, can_edit: bool, can_manage: bool) -> View {
        let team_ref = |id: &Uuid| {
            self.teams.get(id).map(|tm| TeamRef {
                id: tm.id,
                slug: tm.slug.clone(),
                name: tm.name.clone(),
            })
        };
        let mut collaborators: Vec<Person> = self
            .collaborators
            .iter()
            .filter(|c| c.template_id == t.id)
            .filter_map(|c| self.users.get(&c.who).map(Person::of))
            .collect();
        collaborators.sort_by_key(|p| p.username.to_lowercase());
        let mut collaborator_teams: Vec<TeamRef> = self
            .team_collaborators
            .iter()
            .filter(|c| c.template_id == t.id)
            .filter_map(|c| team_ref(&c.who))
            .collect();
        collaborator_teams.sort_by_key(|t| t.name.to_lowercase());
        View {
            row: t.clone(),
            spec: serde_json::from_value(t.spec.clone()).unwrap_or_default(),
            owner: self
                .users
                .get(&t.owner_id)
                .map(Person::of)
                .unwrap_or(Person {
                    id: t.owner_id,
                    username: String::new(),
                    display_name: String::new(),
                }),
            team: t.team_id.as_ref().and_then(team_ref),
            collaborators,
            collaborator_teams,
            environments: self.environments.get(&t.id).copied().unwrap_or(0),
            can_edit,
            can_manage,
        }
    }
}

/// Every template p may see, by name, then owner.
pub async fn list(store: &Store, p: &Principal) -> StoreResult<Vec<View>> {
    let mut tx = store.begin().await?;
    let w = World::load(&mut tx).await?;
    let mut out: Vec<View> = w.templates.iter().filter_map(|t| w.view(p, t)).collect();
    out.sort_by(|a, b| {
        (a.row.name.to_lowercase(), &a.owner.username)
            .cmp(&(b.row.name.to_lowercase(), &b.owner.username))
    });
    Ok(out)
}

/// The template with id, if p may see it.
pub async fn get(store: &Store, p: &Principal, id: Uuid) -> StoreResult<Option<View>> {
    let mut tx = store.begin().await?;
    let w = World::load(&mut tx).await?;
    Ok(w.templates
        .iter()
        .find(|t| t.id == id)
        .and_then(|t| w.view(p, t)))
}

/// A template p may use: its name, spec and revision.
pub async fn for_use(
    tx: &mut Tx,
    p: &Principal,
    id: Uuid,
) -> StoreResult<Option<(String, TemplateSpec, i64)>> {
    let w = World::load(tx).await?;
    Ok(w.templates
        .iter()
        .find(|t| t.id == id)
        .filter(|t| w.access(p, t).0)
        .map(|t| {
            (
                t.name.clone(),
                serde_json::from_value(t.spec.clone()).unwrap_or_default(),
                t.revision,
            )
        }))
}

fn not_found<T>() -> fail::Result<T> {
    Err(Fail::NotFound("template not found".into()))
}

/// Whether p may give a template to team: one of its members or admins.
fn team_editor(w: &World, p: &Principal, team: &str) -> fail::Result<Uuid> {
    let Some(id) = Uuid::parse_str(team)
        .ok()
        .filter(|id| w.teams.contains_key(id))
    else {
        return fail::invalid("unknown team");
    };
    if p.admin || matches!(w.role(id, p.user_id), Some("member" | "admin")) {
        return Ok(id);
    }
    fail::forbidden("only the team's members and admins may give it a template")
}

fn related(v: &View, e: Event) -> Event {
    let e = e.related("owner", v.owner.id);
    match &v.team {
        Some(t) => e.related("team", t.id),
        None => e,
    }
}

pub async fn create(store: &Store, p: &Principal, ip: &str, input: Input) -> fail::Result<View> {
    let input = normalise(input)?;
    let mut tx = store.begin().await?;
    let w = World::load(&mut tx).await?;
    let team = match input.team.as_deref().filter(|t| !t.is_empty()) {
        Some(t) => Some(team_editor(&w, p, t)?),
        None => None,
    };
    let now = Utc::now();
    let row = Template {
        id: Uuid::new_v4(),
        owner_id: p.user_id,
        team_id: team,
        name: input.name.clone(),
        description: input.description,
        visibility: input.visibility,
        spec: serde_json::to_value(&input.spec).unwrap_or_default(),
        revision: 1,
        created_at: now,
        updated_at: now,
    };
    if !tx.insert_template(&row).await? {
        return match team {
            Some(_) => fail::conflict(format!(
                "the team already has a template named {}",
                input.name
            )),
            None => fail::conflict(format!("you already have a template named {}", input.name)),
        };
    }
    let w = World::load(&mut tx).await?;
    let (_, edit, manage) = w.access(p, &row);
    let v = w.view_as(&row, edit, manage);
    let e = related(
        &v,
        Event::new("template.create", "template", row.id, &row.name)
            .related("image", &v.spec.spec.image)
            .details(serde_json::json!({ "visibility": row.visibility })),
    );
    audit::record(&mut tx, &p.actor(ip), e).await?;
    tx.commit().await?;
    Ok(v)
}

pub async fn update(
    store: &Store,
    p: &Principal,
    ip: &str,
    id: Uuid,
    input: Input,
) -> fail::Result<View> {
    let input = normalise(input)?;
    let mut tx = store.begin().await?;
    let Some(cur) = tx.template(id).await? else {
        return not_found();
    };
    let w = World::load(&mut tx).await?;
    let Some(before) = w.view(p, &cur) else {
        return not_found();
    };
    if !before.can_edit {
        return fail::forbidden("only the owner and collaborators may edit this template");
    }
    if input.visibility != cur.visibility && !before.can_manage {
        return fail::forbidden("only the owner may change who can see this template");
    }
    let cur_team = cur.team_id.map(|t| t.to_string()).unwrap_or_default();
    let (mut owner, mut team) = (cur.owner_id, cur.team_id);
    if let Some(t) = input.team.as_deref().filter(|t| *t != cur_team) {
        if !before.can_manage {
            return fail::forbidden("only the owner may give this template to another");
        }
        if t.is_empty() {
            team = None;
            owner = p.user_id;
        } else {
            team = Some(team_editor(&w, p, t)?);
        }
    }
    let spec = serde_json::to_value(&input.spec).unwrap_or_default();
    let row = Template {
        owner_id: owner,
        team_id: team,
        name: input.name,
        description: input.description,
        visibility: input.visibility,
        revision: cur.revision + if spec == cur.spec { 0 } else { 1 },
        spec,
        updated_at: Utc::now(),
        ..cur.clone()
    };
    if !tx.update_template(&row).await? {
        return fail::conflict(format!(
            "the owner already has a template named {}",
            row.name
        ));
    }
    if let Some(team) = team {
        let (users, teams) = tx.collaborators().await?;
        let users: Vec<Uuid> = users
            .iter()
            .filter(|c| c.template_id == id)
            .map(|c| c.who)
            .collect();
        let teams: Vec<Uuid> = teams
            .iter()
            .filter(|c| c.template_id == id && c.who != team)
            .map(|c| c.who)
            .collect();
        tx.set_collaborators(id, &users, &teams).await?;
    }
    let w = World::load(&mut tx).await?;
    let (_, edit, manage) = w.access(p, &row);
    let v = w.view_as(&row, edit, manage);
    let mut details = serde_json::Map::new();
    let change = |from: &str, to: &str| serde_json::json!({ "from": from, "to": to });
    if cur.name != row.name {
        details.insert("name".into(), change(&cur.name, &row.name));
    }
    if cur.visibility != row.visibility {
        details.insert(
            "visibility".into(),
            change(&cur.visibility, &row.visibility),
        );
    }
    if before.spec.spec.image != v.spec.spec.image {
        details.insert(
            "image".into(),
            change(&before.spec.spec.image, &v.spec.spec.image),
        );
    }
    if before.spec.spec.access != v.spec.spec.access {
        details.insert(
            "access".into(),
            serde_json::to_value(v.spec.spec.access).unwrap_or_default(),
        );
    }
    let new_team = team.map(|t| t.to_string()).unwrap_or_default();
    if new_team != cur_team {
        details.insert("team".into(), change(&cur_team, &new_team));
    }
    let e = related(
        &v,
        Event::new("template.update", "template", id, &row.name)
            .related("image", &v.spec.spec.image)
            .details(details.into()),
    );
    audit::record(&mut tx, &p.actor(ip), e).await?;
    tx.commit().await?;
    Ok(v)
}

pub async fn delete(store: &Store, p: &Principal, ip: &str, id: Uuid) -> fail::Result<()> {
    let mut tx = store.begin().await?;
    let Some(cur) = tx.template(id).await? else {
        return not_found();
    };
    let w = World::load(&mut tx).await?;
    let Some(v) = w.view(p, &cur) else {
        return not_found();
    };
    if !v.can_manage {
        return fail::forbidden("only the owner may delete this template");
    }
    tx.delete_template(id).await?;
    let e = related(&v, Event::new("template.delete", "template", id, &cur.name));
    audit::record(&mut tx, &p.actor(ip), e).await?;
    tx.commit().await?;
    Ok(())
}

pub async fn set_collaborators(
    store: &Store,
    p: &Principal,
    ip: &str,
    id: Uuid,
    user_ids: &[String],
    team_ids: &[String],
) -> fail::Result<View> {
    let parse = |ids: &[String], what: &str| -> fail::Result<Vec<Uuid>> {
        ids.iter()
            .map(|s| Uuid::parse_str(s).or_else(|_| fail::invalid(format!("unknown {what} {s:?}"))))
            .collect()
    };
    let (mut users, mut teams) = (parse(user_ids, "user")?, parse(team_ids, "team")?);
    let mut tx = store.begin().await?;
    let Some(cur) = tx.template(id).await? else {
        return not_found();
    };
    let w = World::load(&mut tx).await?;
    let Some(before) = w.view(p, &cur) else {
        return not_found();
    };
    if !before.can_edit {
        return fail::forbidden("only the owner and collaborators may choose collaborators");
    }
    users.retain(|u| *u != cur.owner_id);
    users.sort();
    users.dedup();
    if users.iter().any(|u| !w.users.contains_key(u)) {
        return fail::invalid("unknown user among collaborators");
    }
    teams.retain(|t| Some(*t) != cur.team_id);
    teams.sort();
    teams.dedup();
    if teams.iter().any(|t| !w.teams.contains_key(t)) {
        return fail::invalid("unknown team among collaborators");
    }
    tx.set_collaborators(id, &users, &teams).await?;
    let row = Template {
        updated_at: Utc::now(),
        ..cur.clone()
    };
    tx.update_template(&row).await?;
    let w = World::load(&mut tx).await?;
    let v = match w.view(p, &row) {
        Some(v) => v,
        // Taking themselves off leaves them without it.
        None => w.view_as(&row, false, false),
    };
    let e = related(
        &v,
        Event::new("template.set_collaborators", "template", id, &row.name)
            .details(serde_json::json!({ "collaborators": users, "teams": teams })),
    );
    audit::record(&mut tx, &p.actor(ip), e).await?;
    tx.commit().await?;
    Ok(v)
}

fn normalise(mut input: Input) -> fail::Result<Input> {
    input.name = input.name.trim().to_string();
    input.description = input.description.trim().to_string();
    if input.name.is_empty() || input.name.len() > 100 {
        return fail::invalid("a template needs a name of at most 100 characters");
    }
    match input.visibility.as_str() {
        "private" | "shared" => {}
        "" => input.visibility = "private".into(),
        _ => return fail::invalid("visibility must be private or shared"),
    }
    input.spec = validate(input.spec)?;
    Ok(input)
}

/// The builtin variables every template has.
const BUILTINS: [&str; 4] = ["name", "owner", "short_id", "template"];

/// What a name fills a template's variables with.
pub struct Vars<'a> {
    pub name: &'a str,
    pub owner: &'a str,
    pub short_id: &'a str,
    pub template: &'a str,
}

fn name_pattern(p: &str) -> Result<Regex, regex::Error> {
    Regex::new(&format!("^(?:{p})$"))
}

fn pattern_groups(pattern: &str) -> Vec<String> {
    if pattern.is_empty() {
        return Vec::new();
    }
    name_pattern(pattern)
        .map(|re| re.capture_names().flatten().map(str::to_string).collect())
        .unwrap_or_default()
}

fn values(pattern: &str, v: &Vars) -> BTreeMap<String, String> {
    let mut vals: BTreeMap<String, String> = [
        ("name", v.name),
        ("owner", v.owner),
        ("short_id", v.short_id),
        ("template", v.template),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    if pattern.is_empty() {
        return vals;
    }
    let Ok(re) = name_pattern(pattern) else {
        return vals;
    };
    let caps = re.captures(v.name);
    for g in re.capture_names().flatten() {
        let got = caps
            .as_ref()
            .and_then(|c| c.name(g))
            .map(|m| m.as_str().to_string())
            .unwrap_or_default();
        vals.insert(g.to_string(), got);
    }
    vals
}

fn stand_ins(pattern: &str) -> BTreeMap<String, String> {
    BTreeMap::from_iter(
        BUILTINS
            .iter()
            .map(|b| b.to_string())
            .chain(pattern_groups(pattern))
            .map(|n| (n.clone(), n)),
    )
}

/// s with each {variable} (or {variable:lower}, {variable:upper}) filled in
/// from vals.
fn expand(s: &str, vals: &BTreeMap<String, String>) -> Result<String, String> {
    static PLACEHOLDER: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let placeholder = PLACEHOLDER.get_or_init(|| {
        Regex::new(r"\{([A-Za-z_][A-Za-z0-9_]*)(?::([A-Za-z]+))?\}").expect("a regex")
    });
    let mut bad = None;
    let out = placeholder.replace_all(s, |c: &regex::Captures| {
        let whole = c.get(0).map(|m| m.as_str()).unwrap_or_default();
        let Some(val) = vals.get(&c[1]) else {
            if bad.is_none() {
                let names: Vec<String> = vals.keys().map(|n| format!("{{{n}}}")).collect();
                bad = Some(format!(
                    "{{{}}} is not a variable: there are {}",
                    &c[1],
                    names.join(", ")
                ));
            }
            return whole.to_string();
        };
        match c.get(2).map(|m| m.as_str()) {
            None => val.clone(),
            Some("lower") => val.to_lowercase(),
            Some("upper") => val.to_uppercase(),
            Some(m) => {
                if bad.is_none() {
                    bad = Some(format!("{m:?} in {whole} is not :lower or :upper"));
                }
                whole.to_string()
            }
        }
    });
    if let Some(b) = bad {
        return Err(b);
    }
    if placeholder.replace_all(s, "").contains(['{', '}']) {
        return Err(format!(
            "{s:?} has a brace that is not part of a {{variable}}"
        ));
    }
    Ok(out.into_owned())
}

fn abs_path(p: &str) -> bool {
    p.starts_with('/') && !p.contains(['\0', '\n'])
}

/// A path cleaned as Go's path.Clean cleans it.
pub fn clean(p: &str) -> String {
    let rooted = p.starts_with('/');
    let mut out: Vec<&str> = Vec::new();
    for part in p.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if out.last().is_some_and(|l| *l != "..") {
                    out.pop();
                } else if !rooted {
                    out.push("..");
                }
            }
            x => out.push(x),
        }
    }
    let joined = out.join("/");
    match (rooted, joined.is_empty()) {
        (true, _) => format!("/{joined}"),
        (false, true) => ".".into(),
        (false, false) => joined,
    }
}

fn git_url(u: &str) -> bool {
    if u.contains([' ', '\t', '\n', '\0']) {
        return false;
    }
    for scheme in ["https://", "http://", "ssh://", "git://"] {
        if let Some(rest) = u.strip_prefix(scheme) {
            return !rest.is_empty() && !rest.starts_with('/');
        }
    }
    static SCP: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    SCP.get_or_init(|| Regex::new(r"^[A-Za-z0-9._-]+@[A-Za-z0-9.-]+:[^\s]+$").expect("a regex"))
        .is_match(u)
}

fn ref_name(r: &str) -> bool {
    if r.is_empty() || r == "@" || r.len() > 255 {
        return false;
    }
    if r.starts_with('/')
        || r.ends_with('/')
        || r.ends_with('.')
        || r.ends_with(".lock")
        || r.starts_with('-')
    {
        return false;
    }
    if ["..", "//", "@{", "/."].iter().any(|b| r.contains(b)) {
        return false;
    }
    if r.chars()
        .any(|c| (c as u32) < 0x20 || c as u32 == 0x7f || " ~^:?*[\\".contains(c))
    {
        return false;
    }
    !r.starts_with('.')
}

fn label_key(k: &str) -> bool {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    k.len() <= 253
        && RE
            .get_or_init(|| {
                Regex::new(r"^([a-z0-9]([-a-z0-9.]*[a-z0-9])?/)?[A-Za-z0-9]([-A-Za-z0-9_.]{0,61}[A-Za-z0-9])?$")
                    .expect("a regex")
            })
            .is_match(k)
}

fn label_value(v: &str) -> bool {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^([A-Za-z0-9]([-A-Za-z0-9_.]{0,61}[A-Za-z0-9])?)?$").expect("a regex")
    })
    .is_match(v)
}

fn dns_label(n: &str) -> bool {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$").expect("a regex"))
        .is_match(n)
}

/// A template's spec as the server keeps it, or why it is refused.
pub fn validate(mut t: TemplateSpec) -> fail::Result<TemplateSpec> {
    let s = &mut t.spec;
    s.image = s.image.trim().to_string();
    if s.image.is_empty() {
        return fail::invalid("an image is required");
    }
    if s.cpus < 1 || s.cpus > MAX_CPUS {
        return fail::invalid(format!("vCPUs must be between 1 and {MAX_CPUS}"));
    }
    if s.memory_mib < MIN_MEMORY_MIB || s.memory_mib > MAX_MEMORY_MIB {
        return fail::invalid(format!(
            "memory must be between {MIN_MEMORY_MIB} and {MAX_MEMORY_MIB} MiB"
        ));
    }
    if s.swap_mib.is_some_and(|m| !(0..=MAX_SWAP_MIB).contains(&m)) {
        return fail::invalid(format!("swap must be between 0 and {MAX_SWAP_MIB} MiB"));
    }
    if !matches!(s.display, Display::None | Display::Desktop) {
        return fail::invalid("display must be none or desktop");
    }
    if !matches!(s.gpu, Gpu::None | Gpu::Virtual | Gpu::Passthrough) {
        return fail::invalid("gpu must be none, virtual or passthrough");
    }
    t.name_pattern = t.name_pattern.trim().to_string();
    t.name_hint = t.name_hint.trim().to_string();
    if !t.name_pattern.is_empty() {
        if let Err(e) = name_pattern(&t.name_pattern) {
            return fail::invalid(format!(
                "the name pattern is not a valid regular expression: {e}"
            ));
        }
        if let Some(g) = pattern_groups(&t.name_pattern)
            .iter()
            .find(|g| BUILTINS.contains(&g.as_str()))
        {
            return fail::invalid(format!(
                "the name pattern's group {g:?} has the name of a variable every template has"
            ));
        }
    }
    let stand = stand_ins(&t.name_pattern);
    let check = |what: &str, v: &str, ok: fn(&str) -> bool, want: &str| -> fail::Result<()> {
        let x = expand(v, &stand).or_else(|e| fail::invalid(format!("{what}: {e}")))?;
        if !ok(&x) {
            return fail::invalid(format!("{what}: {v:?} is not {want}"));
        }
        Ok(())
    };
    let s = &mut t.spec;
    if s.repos.len() > MAX_REPOS {
        return fail::invalid(format!("at most {MAX_REPOS} repositories"));
    }
    let mut paths = Vec::new();
    for (i, r) in s.repos.iter_mut().enumerate() {
        let n = i + 1;
        r.url = r.url.trim().to_string();
        r.r#ref = r.r#ref.trim().to_string();
        r.path = r.path.trim().to_string();
        r.branch = r.branch.trim().to_string();
        if !git_url(&r.url) {
            return fail::invalid(format!(
                "repository {n}: {:?} is not a git URL (https://, ssh://, git@host:path or git://)",
                r.url
            ));
        }
        check(
            &format!("repository {n}'s path"),
            &r.path,
            abs_path,
            "an absolute path",
        )?;
        r.path = clean(&r.path);
        if paths.contains(&r.path) {
            return fail::invalid(format!("two repositories are cloned to {}", r.path));
        }
        paths.push(r.path.clone());
        if !r.r#ref.is_empty() {
            check(
                &format!("repository {n}'s ref"),
                &r.r#ref,
                ref_name,
                "a valid ref",
            )?;
        }
        if !r.branch.is_empty() {
            check(
                &format!("repository {n}'s branch"),
                &r.branch,
                ref_name,
                "a valid branch name",
            )?;
        }
    }
    let from = s.project_folders();
    if from.len() > MAX_PROJECTS {
        return fail::invalid(format!("at most {MAX_PROJECTS} projects"));
    }
    let mut projects: Vec<String> = Vec::new();
    for (i, p) in from.iter().enumerate() {
        let p = p.trim();
        check(
            &format!("project {}", i + 1),
            p,
            abs_path,
            "an absolute path",
        )?;
        let p = clean(p);
        if !projects.contains(&p) {
            projects.push(p);
        }
    }
    s.editor_path = projects.first().cloned().unwrap_or_default();
    s.projects = projects;
    for (i, f) in s.trusted_folders.iter_mut().enumerate() {
        let x = f.trim();
        if !abs_path(x) {
            return fail::invalid(format!("trusted folder {} must be an absolute path", i + 1));
        }
        *f = clean(x);
    }
    if s.packs.len() > MAX_PACKS {
        return fail::invalid(format!("at most {MAX_PACKS} packs"));
    }
    let mut packs: Vec<PackRef> = Vec::new();
    for r in &s.packs {
        let pack = r.pack.trim().to_lowercase();
        let copy = r.copy.trim().to_lowercase();
        if Uuid::parse_str(&pack).is_err() {
            return fail::invalid(format!("pack {pack:?} is not a pack's ID"));
        }
        if !copy.is_empty() && Uuid::parse_str(&copy).is_err() {
            return fail::invalid(format!("copy {copy:?} is not a copy's ID"));
        }
        if !packs.iter().any(|x| x.pack == pack) {
            packs.push(PackRef { pack, copy });
        }
    }
    s.packs = packs;
    if s.web_names.len() > MAX_WEB_NAMES {
        return fail::invalid(format!("at most {MAX_WEB_NAMES} web server names"));
    }
    let mut seen = Vec::new();
    for n in s.web_names.iter_mut() {
        let given = n.clone();
        let x = n.trim().to_lowercase();
        let x = x.strip_suffix('-').unwrap_or(&x).to_string();
        if !dns_label(&x) || x.len() > MAX_WEB_NAME {
            return fail::invalid(format!(
                "web server name {given:?} must be lower-case letters, digits and hyphens, at most {MAX_WEB_NAME} long"
            ));
        }
        if seen.contains(&x) {
            return fail::invalid(format!("web server name {x:?} is given twice"));
        }
        seen.push(x.clone());
        *n = x;
    }
    for (k, v) in &s.placement {
        if !label_key(k) {
            return fail::invalid(format!("placement: {k:?} is not a valid label key"));
        }
        if !label_value(v) {
            return fail::invalid(format!("placement: {v:?} is not a valid label value"));
        }
    }
    Ok(t)
}

/// The spec an environment named as vars says gets from template t.
pub fn resolve(t: &TemplateSpec, v: &Vars) -> fail::Result<Spec> {
    let name = v.name;
    static ENV_NAME: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let env_name = ENV_NAME.get_or_init(|| {
        Regex::new(r"^[A-Za-z0-9]([A-Za-z0-9-]{0,61}[A-Za-z0-9])?$").expect("a regex")
    });
    if !env_name.is_match(name) {
        return fail::invalid(
            "a name is letters, digits and hyphens, not starting or ending with one, at most 63 characters",
        );
    }
    if !t.name_pattern.is_empty() {
        let re = name_pattern(&t.name_pattern).or_else(|e| fail::invalid(e.to_string()))?;
        if !re.is_match(name) {
            let hint = if t.name_hint.is_empty() {
                format!("it must match {}", t.name_pattern)
            } else {
                t.name_hint.clone()
            };
            return fail::invalid(format!(
                "this template does not accept the name {name:?}: {hint}"
            ));
        }
    }
    let vals = values(&t.name_pattern, v);
    let fill = |what: &str, s: &str, ok: fn(&str) -> bool| -> fail::Result<String> {
        let x = expand(s, &vals).or_else(|e| fail::invalid(format!("{what}: {e}")))?;
        if !x.is_empty() && !ok(&x) {
            return fail::invalid(format!(
                "the name {name:?} makes {what} {x:?}, which is not valid"
            ));
        }
        Ok(x)
    };
    let mut s = t.spec.clone();
    let mut paths: Vec<String> = Vec::new();
    s.repos = t
        .spec
        .repos
        .iter()
        .map(|r| {
            let path = clean(&fill("the path", &r.path, abs_path)?);
            if paths.contains(&path) {
                return fail::invalid(format!(
                    "the name {name:?} clones two repositories to {path}"
                ));
            }
            paths.push(path.clone());
            Ok(Repo {
                branch: fill("the branch", &r.branch, ref_name)?,
                r#ref: fill("the ref", &r.r#ref, ref_name)?,
                path,
                ..r.clone()
            })
        })
        .collect::<fail::Result<_>>()?;
    s.projects = Vec::new();
    for (i, p) in t.spec.project_folders().iter().enumerate() {
        let x = clean(&fill(&format!("project {}", i + 1), p, abs_path)?);
        if x != "." && !s.projects.contains(&x) {
            s.projects.push(x);
        }
    }
    if s.projects.is_empty() {
        s.projects = s.repos.iter().map(|r| r.path.clone()).collect();
    }
    s.editor_path = s.projects.first().cloned().unwrap_or_default();
    s.placement = t.spec.placement.clone();
    Ok(s)
}
