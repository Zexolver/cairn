//! Filter lists enabled by default in uBlock Origin (Filters, Badware, Privacy, Quick fixes, Unbreak,
//! Resource abuse, EasyList, EasyPrivacy, Peter Lowe's, URLhaus), plus an on-disk cache.
use std::path::{Path, PathBuf};

pub struct ListSource {
    pub id: &'static str,
    pub name: &'static str,
    pub url: &'static str,
}

pub const DEFAULT_LISTS: &[ListSource] = &[
    ListSource { id: "ubo-filters", name: "uBlock filters", url: "https://ublockorigin.github.io/uAssets/filters/filters.min.txt" },
    ListSource { id: "ubo-badware", name: "uBlock filters - Badware risks", url: "https://ublockorigin.github.io/uAssets/filters/badware.min.txt" },
    ListSource { id: "ubo-privacy", name: "uBlock filters - Privacy", url: "https://ublockorigin.github.io/uAssets/filters/privacy.min.txt" },
    ListSource { id: "ubo-quick-fixes", name: "uBlock filters - Quick fixes", url: "https://ublockorigin.github.io/uAssets/filters/quick-fixes.min.txt" },
    ListSource { id: "ubo-unbreak", name: "uBlock filters - Unbreak", url: "https://ublockorigin.github.io/uAssets/filters/unbreak.min.txt" },
    ListSource { id: "ubo-resource-abuse", name: "uBlock filters - Resource abuse", url: "https://ublockorigin.github.io/uAssets/filters/resource-abuse.txt" },
    ListSource { id: "easylist", name: "EasyList", url: "https://easylist.to/easylist/easylist.txt" },
    ListSource { id: "easyprivacy", name: "EasyPrivacy", url: "https://easylist.to/easylist/easyprivacy.txt" },
    ListSource { id: "peter-lowe", name: "Peter Lowe's Ad and tracking server list", url: "https://pgl.yoyo.org/adservers/serverlist.php?hostformat=adblockplus&showintro=1&mimetype=plaintext" },
    ListSource { id: "urlhaus", name: "Online Malicious URL Blocklist", url: "https://malware-filter.gitlab.io/malware-filter/urlhaus-filter-online.txt" },
];

pub fn cache_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.txt"))
}

/// Read every cached default list that exists on disk.
pub fn load_cached(dir: &Path) -> Vec<String> {
    DEFAULT_LISTS.iter().filter_map(|l| std::fs::read_to_string(cache_path(dir, l.id)).ok()).collect()
}

pub fn store(dir: &Path, id: &str, text: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    std::fs::write(cache_path(dir, id), text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_unique_and_https() {
        let mut ids: Vec<_> = DEFAULT_LISTS.iter().map(|l| l.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), DEFAULT_LISTS.len());
        assert!(DEFAULT_LISTS.iter().all(|l| l.url.starts_with("https://")));
    }

    #[test]
    fn cache_roundtrip() {
        let d = std::env::temp_dir().join(format!("cairn-lists-{}", std::process::id()));
        store(&d, "easylist", "||a.com^\n").unwrap();
        assert_eq!(load_cached(&d), vec!["||a.com^\n".to_string()]);
        std::fs::remove_dir_all(d).ok();
    }
}
