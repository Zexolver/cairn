//! Browsing history and bookmarks (with folders), persisted as JSON.
use serde::{Deserialize, Serialize};
use std::path::Path;

pub type FolderId = u32;
const MAX_HISTORY: usize = 5000;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct HistoryEntry {
    pub url: String,
    pub title: String,
    pub visited: u64,
    pub visits: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Folder {
    pub id: FolderId,
    pub name: String,
    pub parent: Option<FolderId>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Bookmark {
    pub url: String,
    pub title: String,
    pub folder: Option<FolderId>,
}

#[derive(Serialize, Deserialize, Default, Debug)]
#[serde(default)]
pub struct Library {
    pub history: Vec<HistoryEntry>,
    pub bookmarks: Vec<Bookmark>,
    pub folders: Vec<Folder>,
    next_folder: FolderId,
}

impl Library {
    /// Record a visit; the newest entry is kept at the end. Internal pages are not recorded.
    pub fn visit(&mut self, url: &str, title: &str, now: u64) {
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return;
        }
        if let Some(pos) = self.history.iter().position(|h| h.url == url) {
            let mut e = self.history.remove(pos);
            e.visited = now;
            e.visits += 1;
            if !title.is_empty() {
                e.title = title.into();
            }
            self.history.push(e);
        } else {
            self.history.push(HistoryEntry { url: url.into(), title: title.into(), visited: now, visits: 1 });
            if self.history.len() > MAX_HISTORY {
                self.history.remove(0);
            }
        }
    }

    /// Update the title of the most recent entry for `url` (titles arrive after the visit).
    pub fn set_title(&mut self, url: &str, title: &str) {
        if let Some(e) = self.history.iter_mut().rev().find(|h| h.url == url) {
            e.title = title.into();
        }
    }

    pub fn clear_history(&mut self) {
        self.history.clear();
    }

    pub fn recent(&self, query: &str, limit: usize) -> Vec<&HistoryEntry> {
        let q = query.to_lowercase();
        self.history
            .iter()
            .rev()
            .filter(|h| q.is_empty() || h.url.to_lowercase().contains(&q) || h.title.to_lowercase().contains(&q))
            .take(limit)
            .collect()
    }

    pub fn add_folder(&mut self, name: &str, parent: Option<FolderId>) -> Option<FolderId> {
        let name = name.trim();
        if name.is_empty() || parent.is_some_and(|p| !self.folders.iter().any(|f| f.id == p)) {
            return None;
        }
        let id = self.next_folder + 1;
        self.next_folder = id;
        self.folders.push(Folder { id, name: name.into(), parent });
        Some(id)
    }

    /// Deletes the folder, its subfolders and their bookmarks.
    pub fn remove_folder(&mut self, id: FolderId) {
        let mut doomed = vec![id];
        let mut i = 0;
        while i < doomed.len() {
            let cur = doomed[i];
            doomed.extend(self.folders.iter().filter(|f| f.parent == Some(cur)).map(|f| f.id));
            i += 1;
        }
        self.folders.retain(|f| !doomed.contains(&f.id));
        self.bookmarks.retain(|b| !b.folder.is_some_and(|f| doomed.contains(&f)));
    }

    pub fn add_bookmark(&mut self, url: &str, title: &str, folder: Option<FolderId>) {
        if self.bookmarks.iter().any(|b| b.url == url && b.folder == folder) {
            return;
        }
        let title = if title.is_empty() { url } else { title };
        self.bookmarks.push(Bookmark { url: url.into(), title: title.into(), folder });
    }

    pub fn remove_bookmark(&mut self, url: &str, folder: Option<FolderId>) {
        self.bookmarks.retain(|b| !(b.url == url && b.folder == folder));
    }

    pub fn is_bookmarked(&self, url: &str) -> bool {
        self.bookmarks.iter().any(|b| b.url == url)
    }

    pub fn bookmarks_in(&self, folder: Option<FolderId>) -> Vec<&Bookmark> {
        self.bookmarks.iter().filter(|b| b.folder == folder).collect()
    }

    pub fn subfolders(&self, parent: Option<FolderId>) -> Vec<&Folder> {
        self.folders.iter().filter(|f| f.parent == parent).collect()
    }

    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::write(path, serde_json::to_string(self).unwrap())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_dedups_and_skips_internal() {
        let mut l = Library::default();
        l.visit("about:blank", "", 1);
        l.visit("https://a.com/", "A", 1);
        l.visit("https://b.com/", "B", 2);
        l.visit("https://a.com/", "", 3);
        assert_eq!(l.history.len(), 2);
        assert_eq!(l.recent("", 10)[0].url, "https://a.com/");
        assert_eq!(l.recent("", 10)[0].visits, 2);
        assert_eq!(l.recent("b", 10).len(), 1);
        l.set_title("https://b.com/", "Bee");
        assert_eq!(l.recent("bee", 10).len(), 1);
    }

    #[test]
    fn folders_nest_and_delete_recursively() {
        let mut l = Library::default();
        let a = l.add_folder("A", None).unwrap();
        let b = l.add_folder("B", Some(a)).unwrap();
        assert!(l.add_folder("X", Some(99)).is_none());
        l.add_bookmark("https://x.com", "X", Some(b));
        l.add_bookmark("https://x.com", "X", Some(b));
        l.add_bookmark("https://y.com", "", None);
        assert_eq!(l.bookmarks.len(), 2);
        assert!(l.is_bookmarked("https://y.com"));
        l.remove_folder(a);
        assert!(l.folders.is_empty());
        assert_eq!(l.bookmarks.len(), 1);
    }
}
