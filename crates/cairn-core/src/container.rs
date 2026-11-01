//! Containers group tabs. Every tab in a container shares one engine instance (one process), so a
//! container with 17 tabs costs one engine, not 17. Containers are isolated from each other.
use serde::{Deserialize, Serialize};

pub type ContainerId = u32;
pub type TabId = u32;
pub type GroupId = u32;

/// A named, colored set of tabs inside one container.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct TabGroup {
    pub id: GroupId,
    pub container: ContainerId,
    pub name: String,
    pub color: String,
    pub collapsed: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Container {
    pub id: ContainerId,
    pub name: String,
    pub color: String,
    /// Optional per-container proxy (http/https/socks5).
    pub proxy: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Tab {
    pub id: TabId,
    pub container: ContainerId,
    pub title: String,
    pub url: String,
    pub loading: bool,
    pub group: Option<GroupId>,
}

#[derive(thiserror::Error, Debug, PartialEq)]
pub enum ContainerError {
    #[error("container name must not be empty")]
    EmptyName,
    #[error("no such container")]
    NotFound,
    #[error("cannot delete the last container")]
    LastContainer,
}

const PALETTE: &[&str] = &["#7aa2f7", "#9ece6a", "#e0af68", "#f7768e", "#bb9af7", "#7dcfff"];

pub struct Workspace {
    pub containers: Vec<Container>,
    pub tabs: Vec<Tab>,
    pub groups: Vec<TabGroup>,
    pub active: Option<TabId>,
    next_container: ContainerId,
    next_tab: TabId,
    next_group: GroupId,
}

impl Default for Workspace {
    fn default() -> Self {
        Self {
            containers: vec![Container { id: 0, name: "Default".into(), color: PALETTE[0].into(), proxy: None }],
            tabs: vec![],
            groups: vec![],
            active: None,
            next_container: 1,
            next_tab: 1,
            next_group: 1,
        }
    }
}

impl Workspace {
    pub fn add_container(&mut self, name: &str) -> Result<ContainerId, ContainerError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(ContainerError::EmptyName);
        }
        let id = self.next_container;
        self.next_container += 1;
        let color = PALETTE[id as usize % PALETTE.len()].to_string();
        self.containers.push(Container { id, name: name.into(), color, proxy: None });
        Ok(id)
    }

    /// Returns the tabs that were closed so the caller can tear down the container's engine.
    pub fn remove_container(&mut self, id: ContainerId) -> Result<Vec<Tab>, ContainerError> {
        if !self.containers.iter().any(|c| c.id == id) {
            return Err(ContainerError::NotFound);
        }
        if self.containers.len() == 1 {
            return Err(ContainerError::LastContainer);
        }
        self.containers.retain(|c| c.id != id);
        let (gone, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut self.tabs).into_iter().partition(|t| t.container == id);
        self.tabs = kept;
        self.groups.retain(|g| g.container != id);
        if self.active.is_some_and(|a| gone.iter().any(|t| t.id == a)) {
            self.active = self.tabs.first().map(|t| t.id);
        }
        Ok(gone)
    }

    pub fn open_tab(&mut self, container: ContainerId, url: &str) -> Result<TabId, ContainerError> {
        if !self.containers.iter().any(|c| c.id == container) {
            return Err(ContainerError::NotFound);
        }
        let id = self.next_tab;
        self.next_tab += 1;
        self.tabs.push(Tab { id, container, title: "New tab".into(), url: url.into(), loading: false, group: None });
        self.active = Some(id);
        Ok(id)
    }

    pub fn close_tab(&mut self, id: TabId) -> Option<Tab> {
        let pos = self.tabs.iter().position(|t| t.id == id)?;
        let tab = self.tabs.remove(pos);
        self.prune_groups();
        if self.active == Some(id) {
            let near = pos.min(self.tabs.len().saturating_sub(1));
            self.active = self.tabs.get(near).map(|t| t.id);
        }
        Some(tab)
    }

    pub fn add_group(&mut self, container: ContainerId, name: &str) -> Option<GroupId> {
        let name = name.trim();
        if name.is_empty() || !self.containers.iter().any(|c| c.id == container) {
            return None;
        }
        let id = self.next_group;
        self.next_group += 1;
        let color = PALETTE[(id as usize + 2) % PALETTE.len()].to_string();
        self.groups.push(TabGroup { id, container, name: name.into(), color, collapsed: false });
        Some(id)
    }

    /// Puts a tab in a group of the same container, or ungroups it with `None`. Empty groups are dropped.
    pub fn set_tab_group(&mut self, tab: TabId, group: Option<GroupId>) -> bool {
        let Some(container) = self.tabs.iter().find(|t| t.id == tab).map(|t| t.container) else { return false };
        if group.is_some_and(|g| !self.groups.iter().any(|x| x.id == g && x.container == container)) {
            return false;
        }
        self.tab_mut(tab).unwrap().group = group;
        self.prune_groups();
        true
    }

    pub fn toggle_group(&mut self, group: GroupId) {
        if let Some(g) = self.groups.iter_mut().find(|g| g.id == group) {
            g.collapsed = !g.collapsed;
        }
    }

    pub fn prune_groups(&mut self) {
        let used: Vec<GroupId> = self.tabs.iter().filter_map(|t| t.group).collect();
        self.groups.retain(|g| used.contains(&g.id));
    }

    pub fn tab_mut(&mut self, id: TabId) -> Option<&mut Tab> {
        self.tabs.iter_mut().find(|t| t.id == id)
    }

    pub fn tabs_in(&self, container: ContainerId) -> usize {
        self.tabs.iter().filter(|t| t.container == container).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tabs_group_by_container() {
        let mut w = Workspace::default();
        let work = w.add_container("Work").unwrap();
        for _ in 0..17 {
            w.open_tab(work, "about:blank").unwrap();
        }
        w.open_tab(0, "about:blank").unwrap();
        assert_eq!(w.tabs_in(work), 17);
        assert_eq!(w.tabs_in(0), 1);
    }

    #[test]
    fn remove_container_closes_its_tabs() {
        let mut w = Workspace::default();
        let c = w.add_container("X").unwrap();
        let t = w.open_tab(c, "about:blank").unwrap();
        assert_eq!(w.active, Some(t));
        let gone = w.remove_container(c).unwrap();
        assert_eq!(gone.len(), 1);
        assert_eq!(w.active, None);
        assert_eq!(w.remove_container(0), Err(ContainerError::LastContainer));
    }

    #[test]
    fn groups_stay_within_container_and_vanish_when_empty() {
        let mut w = Workspace::default();
        let other = w.add_container("O").unwrap();
        let a = w.open_tab(0, "a").unwrap();
        let b = w.open_tab(other, "b").unwrap();
        let g = w.add_group(0, "Reading").unwrap();
        assert!(w.set_tab_group(a, Some(g)));
        assert!(!w.set_tab_group(b, Some(g)));
        w.toggle_group(g);
        assert!(w.groups[0].collapsed);
        w.close_tab(a);
        assert!(w.groups.is_empty());
        assert!(w.add_group(0, " ").is_none());
    }

    #[test]
    fn closing_active_tab_selects_neighbour() {
        let mut w = Workspace::default();
        let a = w.open_tab(0, "a").unwrap();
        let b = w.open_tab(0, "b").unwrap();
        w.close_tab(b);
        assert_eq!(w.active, Some(a));
    }
}
