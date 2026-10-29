use url::Url;

/// Turn address-bar input into a URL: explicit URLs pass through, host-like text gets https://,
/// anything else becomes a search query.
pub fn resolve_input(input: &str, search_template: &str) -> String {
    let t = input.trim();
    if let Ok(u) = Url::parse(t) {
        if matches!(u.scheme(), "http" | "https" | "file" | "about" | "data") {
            return t.to_string();
        }
    }
    let looks_host = !t.contains(char::is_whitespace)
        && (t.starts_with("localhost") || (t.contains('.') && !t.ends_with('.')));
    if looks_host {
        if let Ok(u) = Url::parse(&format!("https://{t}")) {
            if u.host().is_some() {
                return u.to_string();
            }
        }
    }
    let q: String = url::form_urlencoded::byte_serialize(t.as_bytes()).collect();
    search_template.replace("%s", &q)
}

#[cfg(test)]
mod tests {
    use super::*;
    const S: &str = "https://duckduckgo.com/?q=%s";

    #[test]
    fn resolves() {
        assert_eq!(resolve_input("https://a.com/x", S), "https://a.com/x");
        assert_eq!(resolve_input("example.com", S), "https://example.com/");
        assert_eq!(resolve_input("rust lang", S), "https://duckduckgo.com/?q=rust+lang");
        assert_eq!(resolve_input("hello", S), "https://duckduckgo.com/?q=hello");
    }
}
