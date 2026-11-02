//! "What's new" after an update, from CHANGELOG.md (built into the exe).

use serde::Serialize;

const TEXT: &str = include_str!("../../CHANGELOG.md");

#[derive(Serialize, Debug, PartialEq)]
pub struct Entry {
    pub version: String,
    /// Markdown list items without the "- ".
    pub items: Vec<String>,
}

fn parse(text: &str) -> Vec<(semver::Version, Entry)> {
    let mut out: Vec<(semver::Version, Entry)> = vec![];
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("## ") {
            if let Ok(ver) = semver::Version::parse(v.trim()) {
                out.push((ver, Entry { version: v.trim().to_string(), items: vec![] }));
            }
        } else if let (Some(item), Some(last)) = (line.strip_prefix("- "), out.last_mut()) {
            last.1.items.push(item.trim().to_string());
        }
    }
    out
}

/// Entries newer than `seen` up to and including `current`, newest first.
pub fn since(seen: &str, current: &str) -> Vec<Entry> {
    // Versions up to 0.4.1 didn't store what was seen: that is where an
    // update from one of them comes from.
    since_in(TEXT, if seen.is_empty() { "0.4.1" } else { seen }, current)
}

fn since_in(text: &str, seen: &str, current: &str) -> Vec<Entry> {
    let Ok(cur) = semver::Version::parse(current) else { return vec![] };
    let seen = semver::Version::parse(seen).ok();
    let mut v: Vec<_> = parse(text)
        .into_iter()
        .filter(|(ver, e)| *ver <= cur && !e.items.is_empty() && match &seen {
            Some(s) => ver > s,
            None => *ver == cur,
        })
        .collect();
    v.sort_by(|a, b| b.0.cmp(&a.0));
    v.into_iter().map(|(_, e)| e).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    const T: &str = "# x\n\n## 0.5.0\n- five\n\n## 0.4.3\n- three a\n- three b\n\n## 0.4.2\n- two\n";

    #[test]
    fn picks_versions_between() {
        let v = since_in(T, "0.4.2", "0.5.0");
        assert_eq!(v.iter().map(|e| e.version.as_str()).collect::<Vec<_>>(), ["0.5.0", "0.4.3"]);
        assert_eq!(v[1].items, ["three a", "three b"]);
        assert!(since_in(T, "0.5.0", "0.5.0").is_empty());
        assert_eq!(since_in(T, "", "0.4.3").len(), 1);
        assert!(since_in(T, "0.4.2", "0.4.2").is_empty());
    }

    #[test]
    fn real_changelog_has_this_version() {
        assert!(!since("0.4.1", env!("CARGO_PKG_VERSION")).is_empty(), "add a ## {} section to CHANGELOG.md", env!("CARGO_PKG_VERSION"));
    }
}
