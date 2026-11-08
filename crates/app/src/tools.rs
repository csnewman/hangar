//! The tool windows and where they dock. As IntelliJ's, each side of an
//! environment's tab has a stripe of icons in three groups: the top group
//! opens on that side, the group below the line opens beneath it, and the
//! foot opens along the bottom. Dragging an icon moves its tool between
//! groups; the arrangement is kept with the panels' sizes.

use serde::{Deserialize, Serialize};

/// A tool window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tool {
    Project,
    Filesystem,
    Commit,
    Structure,
    Terminal,
    Problems,
    Git,
    Docker,
    Find,
    Log,
    Agents,
    Ports,
}

impl Tool {
    pub const ALL: [Tool; 12] = [
        Tool::Project,
        Tool::Filesystem,
        Tool::Commit,
        Tool::Structure,
        Tool::Terminal,
        Tool::Problems,
        Tool::Git,
        Tool::Docker,
        Tool::Find,
        Tool::Log,
        Tool::Agents,
        Tool::Ports,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Tool::Project => "Project",
            Tool::Filesystem => "Filesystem",
            Tool::Commit => "Commit",
            Tool::Structure => "Structure",
            Tool::Terminal => "Terminal",
            Tool::Problems => "Problems",
            Tool::Git => "Git",
            Tool::Docker => "Docker",
            Tool::Find => "Find",
            Tool::Log => "Log",
            Tool::Agents => "Agents",
            Tool::Ports => "Ports",
        }
    }

    /// Its icon's name in the app's assets.
    pub fn icon(self) -> &'static str {
        match self {
            Tool::Project => "project",
            Tool::Filesystem => "filesystem",
            Tool::Commit => "commit",
            Tool::Structure => "structure",
            Tool::Terminal => "terminal",
            Tool::Problems => "problems",
            Tool::Git => "git",
            Tool::Docker => "docker",
            Tool::Find => "search",
            Tool::Log => "log",
            Tool::Agents => "agents",
            Tool::Ports => "ports",
        }
    }

    /// The tool window with a name, ignoring case.
    pub fn by_name(name: &str) -> Option<Tool> {
        Tool::ALL
            .into_iter()
            .find(|t| t.name().eq_ignore_ascii_case(name))
    }
}

/// Where a tool window opens: on a side, beneath that side's window, or
/// along the bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Slot {
    Left,
    LeftBelow,
    Bottom,
    Right,
    RightBelow,
}

/// A group of icons on a stripe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Group {
    LeftTop,
    LeftBelow,
    LeftFoot,
    RightTop,
    RightBelow,
    RightFoot,
}

impl Group {
    pub const ALL: [Group; 6] = [
        Group::LeftTop,
        Group::LeftBelow,
        Group::LeftFoot,
        Group::RightTop,
        Group::RightBelow,
        Group::RightFoot,
    ];

    /// Where its tools open; both feet open along the bottom.
    pub fn slot(self) -> Slot {
        match self {
            Group::LeftTop => Slot::Left,
            Group::LeftBelow => Slot::LeftBelow,
            Group::RightTop => Slot::Right,
            Group::RightBelow => Slot::RightBelow,
            Group::LeftFoot | Group::RightFoot => Slot::Bottom,
        }
    }

    pub fn left(self) -> bool {
        matches!(self, Group::LeftTop | Group::LeftBelow | Group::LeftFoot)
    }

    /// A side's groups: top, below the line, foot.
    pub fn of_side(left: bool) -> [Group; 3] {
        if left {
            [Group::LeftTop, Group::LeftBelow, Group::LeftFoot]
        } else {
            [Group::RightTop, Group::RightBelow, Group::RightFoot]
        }
    }

    fn index(self) -> usize {
        self as usize
    }
}

/// Which tools each group holds, in order. Every tool is in exactly one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "Kept", into = "Kept")]
pub struct Stripes {
    groups: [Vec<Tool>; 6],
}

impl Default for Stripes {
    fn default() -> Self {
        let mut s = Stripes {
            groups: Default::default(),
        };
        for t in Tool::ALL {
            s.groups[first_home(t).index()].push(t);
        }
        s
    }
}

/// Where a tool starts.
fn first_home(t: Tool) -> Group {
    match t {
        Tool::Project | Tool::Filesystem | Tool::Commit => Group::LeftTop,
        Tool::Structure => Group::LeftBelow,
        Tool::Agents => Group::RightTop,
        Tool::Ports => Group::RightBelow,
        _ => Group::LeftFoot,
    }
}

impl Stripes {
    pub fn group(&self, g: Group) -> &[Tool] {
        &self.groups[g.index()]
    }

    /// The group holding a tool.
    pub fn group_of(&self, t: Tool) -> Group {
        Group::ALL
            .into_iter()
            .find(|g| self.groups[g.index()].contains(&t))
            .unwrap_or_else(|| first_home(t))
    }

    /// Where a tool opens.
    pub fn slot(&self, t: Tool) -> Slot {
        self.group_of(t).slot()
    }

    /// Whether a side's stripe has no tools.
    pub fn side_empty(&self, left: bool) -> bool {
        Group::of_side(left)
            .iter()
            .all(|g| self.groups[g.index()].is_empty())
    }

    /// Moves a tool into a group, before another of its tools or at its
    /// end; says whether anything moved.
    pub fn moved(&mut self, t: Tool, to: Group, before: Option<Tool>) -> bool {
        if before == Some(t) {
            return false;
        }
        let was = self.clone();
        for g in &mut self.groups {
            g.retain(|x| *x != t);
        }
        let group = &mut self.groups[to.index()];
        match before.and_then(|b| group.iter().position(|x| *x == b)) {
            Some(i) => group.insert(i, t),
            None => group.push(t),
        }
        *self != was
    }
}

/// The arrangement as kept: each group's tools by name. A name no tool has
/// is dropped, and a tool not named goes back where it starts.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct Kept {
    left: Vec<String>,
    left_below: Vec<String>,
    left_foot: Vec<String>,
    right: Vec<String>,
    right_below: Vec<String>,
    right_foot: Vec<String>,
}

impl From<Kept> for Stripes {
    fn from(k: Kept) -> Self {
        let named = [
            k.left,
            k.left_below,
            k.left_foot,
            k.right,
            k.right_below,
            k.right_foot,
        ];
        let mut s = Stripes {
            groups: Default::default(),
        };
        let mut placed = vec![];
        for (i, names) in named.iter().enumerate() {
            for t in names.iter().filter_map(|n| Tool::by_name(n)) {
                if !placed.contains(&t) {
                    placed.push(t);
                    s.groups[i].push(t);
                }
            }
        }
        for t in Tool::ALL {
            if !placed.contains(&t) {
                s.groups[first_home(t).index()].push(t);
            }
        }
        s
    }
}

impl From<Stripes> for Kept {
    fn from(s: Stripes) -> Self {
        let names =
            |g: Group| -> Vec<String> { s.group(g).iter().map(|t| t.name().to_string()).collect() };
        Kept {
            left: names(Group::LeftTop),
            left_below: names(Group::LeftBelow),
            left_foot: names(Group::LeftFoot),
            right: names(Group::RightTop),
            right_below: names(Group::RightBelow),
            right_foot: names(Group::RightFoot),
        }
    }
}
