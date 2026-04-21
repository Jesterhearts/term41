use std::collections::HashMap;
use std::num::NonZeroU32;

/// Identifier for a hyperlink interned in [`HyperlinkRegistry`]. Stored
/// per-cell in [`Row`](super::Row); `NonZeroU32` lets `Option<HyperlinkId>`
/// fit in 4 bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HyperlinkId(NonZeroU32);

/// Interns OSC 8 hyperlink targets so each cell only has to carry a 4-byte
/// id. Two OSC 8 spans coalesce to the same id only when both their `id=…`
/// parameter and their URI match — that's the rule apps use to indicate "this
/// is the same link reflowed across cells", which avoids visually splitting a
/// long URL into several adjacent-but-distinct underlines.
///
/// Entries are never freed. Cells in scrollback can keep an id alive
/// indefinitely, and there is no callback when those cells fall out of
/// history. In practice the registry stays tiny (a session typically emits a
/// few dozen unique URLs); a long-running shell that prints millions of
/// distinct hyperlinks would need GC, which can be added later without
/// touching the per-cell representation.
#[derive(Debug, Default)]
pub struct HyperlinkRegistry {
    entries: Vec<String>,
    by_key: HashMap<HyperlinkKey, HyperlinkId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct HyperlinkKey {
    id_param: Option<String>,
    uri: String,
}

impl HyperlinkRegistry {
    /// Create an empty hyperlink registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Return the id for `(id_param, uri)`, allocating a new one if this
    /// pair hasn't been seen. The mapping is stable across calls so a
    /// re-emitted span lands on the same id.
    pub fn intern(
        &mut self,
        id_param: Option<&str>,
        uri: &str,
    ) -> HyperlinkId {
        let key = HyperlinkKey {
            id_param: id_param.map(str::to_owned),
            uri: uri.to_owned(),
        };
        if let Some(&existing) = self.by_key.get(&key) {
            return existing;
        }
        // Indices are 1-based so 0 stays available as the niche for `Option`.
        let raw = u32::try_from(self.entries.len() + 1).expect("hyperlink id overflow");
        let id = HyperlinkId(NonZeroU32::new(raw).expect("non-zero by construction"));
        self.entries.push(uri.to_owned());
        self.by_key.insert(key, id);
        id
    }

    /// Resolve a hyperlink id to its URI.
    pub fn get(
        &self,
        id: HyperlinkId,
    ) -> Option<&str> {
        let idx = id.0.get() as usize - 1;
        self.entries.get(idx).map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intern_same_pair_returns_same_id() {
        let mut r = HyperlinkRegistry::new();
        let a = r.intern(None, "https://example.com");
        let b = r.intern(None, "https://example.com");
        assert_eq!(a, b);
    }

    #[test]
    fn intern_distinct_uri_returns_distinct_id() {
        let mut r = HyperlinkRegistry::new();
        let a = r.intern(None, "https://a.example");
        let b = r.intern(None, "https://b.example");
        assert_ne!(a, b);
    }

    #[test]
    fn intern_distinct_id_param_returns_distinct_id() {
        let mut r = HyperlinkRegistry::new();
        let a = r.intern(Some("a"), "https://example.com");
        let b = r.intern(Some("b"), "https://example.com");
        assert_ne!(a, b);
    }

    #[test]
    fn get_returns_uri() {
        let mut r = HyperlinkRegistry::new();
        let id = r.intern(None, "https://example.com");
        assert_eq!(r.get(id), Some("https://example.com"));
    }
}
