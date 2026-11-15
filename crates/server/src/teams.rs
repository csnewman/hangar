//! Teams: groups of users that own templates, as a user does, and give each
//! member a role in what the team owns.

use chrono::Utc;
use uuid::Uuid;

use crate::audit::{self, Event};
use crate::fail::{self, Fail};
use crate::store::rows::{Team, TeamMember, User};
use crate::store::{Result as StoreResult, Store, Tx};
use crate::templates::Person;
use crate::users::Principal;

/// A team as someone sees it.
#[derive(Debug, Clone)]
pub struct View {
    pub row: Team,
    /// Its members, admins first, then members, then viewers, each by name.
    pub members: Vec<(Person, String)>,
    pub templates: i64,
    pub images: i64,
    pub packs: i64,
    pub copies: i64,
    /// The asker's role in it.
    pub role: Option<String>,
    pub can_manage: bool,
}

/// What a team is made or changed with.
pub struct Input {
    pub slug: String,
    pub name: String,
    pub description: String,
}

/// A slug: lower-case letters and digits, separated by single . _ or -.
fn valid_slug(s: &str) -> bool {
    if s.is_empty() || s.len() > 63 {
        return false;
    }
    let sep = |c: char| matches!(c, '.' | '_' | '-');
    s.split(sep).all(|part| {
        !part.is_empty()
            && part
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
    })
}

fn rank(role: &str) -> u8 {
    match role {
        "admin" => 0,
        "member" => 1,
        _ => 2,
    }
}

async fn views(tx: &mut Tx, p: &Principal) -> StoreResult<Vec<View>> {
    let teams = tx.teams().await?;
    let members = tx.team_members().await?;
    let people: std::collections::HashMap<Uuid, User> =
        tx.people().await?.into_iter().map(|u| (u.id, u)).collect();
    let templates = tx.templates().await?;
    let holdings = tx.team_holdings().await?;
    Ok(teams
        .into_iter()
        .map(|t| {
            let mut ms: Vec<(Person, String)> = members
                .iter()
                .filter(|m| m.team_id == t.id)
                .filter_map(|m: &TeamMember| {
                    people.get(&m.user_id).map(|u| {
                        (
                            Person {
                                id: u.id,
                                username: u.username.clone(),
                                display_name: u.display_name.clone(),
                            },
                            m.role.clone(),
                        )
                    })
                })
                .collect();
            ms.sort_by(|a, b| {
                (rank(&a.1), a.0.username.to_lowercase())
                    .cmp(&(rank(&b.1), b.0.username.to_lowercase()))
            });
            let role = members
                .iter()
                .find(|m| m.team_id == t.id && m.user_id == p.user_id)
                .map(|m| m.role.clone());
            let (images, packs, copies) = holdings.get(&t.id).copied().unwrap_or_default();
            View {
                templates: templates.iter().filter(|x| x.team_id == Some(t.id)).count() as i64,
                images,
                packs,
                copies,
                can_manage: p.admin || role.as_deref() == Some("admin"),
                role,
                members: ms,
                row: t,
            }
        })
        .collect())
}

/// Every team, by name.
pub async fn list(store: &Store, p: &Principal) -> StoreResult<Vec<View>> {
    let mut out = views(&mut store.begin().await?, p).await?;
    out.sort_by(|a, b| {
        (a.row.name.to_lowercase(), a.row.slug.to_lowercase())
            .cmp(&(b.row.name.to_lowercase(), b.row.slug.to_lowercase()))
    });
    Ok(out)
}

pub async fn get(store: &Store, p: &Principal, id: Uuid) -> StoreResult<Option<View>> {
    Ok(views(&mut store.begin().await?, p)
        .await?
        .into_iter()
        .find(|v| v.row.id == id))
}

/// The team with id, held for a change only its admins may make.
async fn for_change(tx: &mut Tx, p: &Principal, id: Uuid) -> fail::Result<View> {
    tx.lock_team(id).await?;
    let Some(v) = views(tx, p).await?.into_iter().find(|v| v.row.id == id) else {
        return Err(Fail::NotFound("team not found".into()));
    };
    if !v.can_manage {
        return fail::forbidden("only the team's admins run it");
    }
    Ok(v)
}

fn normalise(mut input: Input) -> fail::Result<Input> {
    input.name = input.name.trim().to_string();
    input.description = input.description.trim().to_string();
    if input.name.is_empty() || input.name.len() > 100 {
        return fail::invalid("a team needs a name of at most 100 characters");
    }
    if input.description.len() > 1000 {
        return fail::invalid("a description is at most 1000 characters");
    }
    Ok(input)
}

pub async fn create(store: &Store, p: &Principal, ip: &str, input: Input) -> fail::Result<View> {
    if !p.admin {
        return fail::forbidden("only administrators make teams");
    }
    let slug = input.slug.trim().to_string();
    if !valid_slug(&slug) {
        return fail::invalid(
            "a team's slug is lowercase letters and digits, separated by single . _ or -, at most 63 characters",
        );
    }
    let input = normalise(input)?;
    let mut tx = store.begin().await?;
    if tx.person_by_name(&slug).await?.is_some() {
        return fail::conflict(format!("{slug} is a user's username"));
    }
    let row = Team {
        id: Uuid::new_v4(),
        slug: slug.clone(),
        name: input.name,
        description: input.description,
        created_at: Utc::now(),
    };
    if !tx.insert_team(&row).await? {
        return fail::conflict(format!("there is already a team {slug}"));
    }
    audit::record(
        &mut tx,
        &p.actor(ip),
        Event::new("team.create", "team", row.id, &slug)
            .details(serde_json::json!({ "name": row.name })),
    )
    .await?;
    let v = views(&mut tx, p)
        .await?
        .into_iter()
        .find(|v| v.row.id == row.id);
    tx.commit().await?;
    v.ok_or_else(|| Fail::NotFound("team not found".into()))
}

pub async fn update(
    store: &Store,
    p: &Principal,
    ip: &str,
    id: Uuid,
    input: Input,
) -> fail::Result<View> {
    let slug = input.slug.trim().to_string();
    let input = normalise(input)?;
    let mut tx = store.begin().await?;
    let cur = for_change(&mut tx, p, id).await?;
    if !slug.is_empty() && slug != cur.row.slug {
        return fail::invalid("a team's slug does not change");
    }
    tx.update_team(id, &input.name, &input.description).await?;
    let mut details = serde_json::Map::new();
    if cur.row.name != input.name {
        details.insert(
            "name".into(),
            serde_json::json!({ "from": cur.row.name, "to": input.name }),
        );
    }
    audit::record(
        &mut tx,
        &p.actor(ip),
        Event::new("team.update", "team", id, &cur.row.slug).details(details.into()),
    )
    .await?;
    let v = views(&mut tx, p)
        .await?
        .into_iter()
        .find(|v| v.row.id == id);
    tx.commit().await?;
    v.ok_or_else(|| Fail::NotFound("team not found".into()))
}

pub async fn delete(store: &Store, p: &Principal, ip: &str, id: Uuid) -> fail::Result<()> {
    if !p.admin {
        return fail::forbidden("only administrators delete teams");
    }
    let mut tx = store.begin().await?;
    let cur = for_change(&mut tx, p, id).await?;
    if cur.templates > 0 {
        return fail::conflict(format!(
            "the team owns {} templates; move or delete them first",
            cur.templates
        ));
    }
    if cur.images > 0 {
        return fail::conflict(format!(
            "the team owns {} image repositories; delete them first",
            cur.images
        ));
    }
    if cur.packs > 0 {
        return fail::conflict(format!(
            "the team owns {} packs; delete them first",
            cur.packs
        ));
    }
    if cur.copies > 0 {
        return fail::conflict(format!(
            "the team owns {} shared copies; delete them first",
            cur.copies
        ));
    }
    tx.delete_team(id).await?;
    audit::record(
        &mut tx,
        &p.actor(ip),
        Event::new("team.delete", "team", id, &cur.row.slug),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn set_member(
    store: &Store,
    p: &Principal,
    ip: &str,
    id: Uuid,
    user: &str,
    role: &str,
) -> fail::Result<View> {
    let Ok(user) = Uuid::parse_str(user) else {
        return fail::invalid("unknown user");
    };
    if !matches!(role, "viewer" | "member" | "admin") {
        return fail::invalid("a role is viewer, member or admin");
    }
    let mut tx = store.begin().await?;
    let cur = for_change(&mut tx, p, id).await?;
    let Some(u) = tx.user(user).await?.filter(|u| u.kind == "person") else {
        return fail::invalid("unknown user");
    };
    tx.set_team_member(id, user, role).await?;
    audit::record(
        &mut tx,
        &p.actor(ip),
        Event::new("team.set_member", "team", id, &cur.row.slug)
            .related("user", user)
            .details(serde_json::json!({ "role": role, "user": u.username })),
    )
    .await?;
    let v = views(&mut tx, p)
        .await?
        .into_iter()
        .find(|v| v.row.id == id);
    tx.commit().await?;
    v.ok_or_else(|| Fail::NotFound("team not found".into()))
}

pub async fn remove_member(
    store: &Store,
    p: &Principal,
    ip: &str,
    id: Uuid,
    user: &str,
) -> fail::Result<View> {
    let Ok(user) = Uuid::parse_str(user) else {
        return fail::invalid("unknown user");
    };
    let mut tx = store.begin().await?;
    let cur = for_change(&mut tx, p, id).await?;
    if !tx.remove_team_member(id, user).await? {
        return fail::invalid("not a member of the team");
    }
    audit::record(
        &mut tx,
        &p.actor(ip),
        Event::new("team.remove_member", "team", id, &cur.row.slug).related("user", user),
    )
    .await?;
    let v = views(&mut tx, p)
        .await?
        .into_iter()
        .find(|v| v.row.id == id);
    tx.commit().await?;
    v.ok_or_else(|| Fail::NotFound("team not found".into()))
}
