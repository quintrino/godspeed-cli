//! The cached name -> id maps for lists and labels, and looking names up in them.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::Path;

pub type NameMap = HashMap<String, String>;

pub fn load(path: &Path) -> NameMap {
    if let Ok(content) = fs::read_to_string(path) {
        if let Ok(table) = content.parse::<toml::Table>() {
            let mut map = HashMap::new();
            for (key, value) in table {
                if let Some(val_str) = value.as_str() {
                    map.insert(key.to_lowercase(), val_str.to_string());
                }
            }
            return map;
        }
    }
    HashMap::new()
}

pub fn save(path: &Path, cache: &NameMap) -> io::Result<()> {
    let mut table = toml::Table::new();
    for (key, value) in cache {
        table.insert(key.clone(), toml::Value::String(value.clone()));
    }
    let toml_string = toml::to_string(&table).unwrap();
    fs::write(path, toml_string)
}

/// Looks a name up by exact match, then by prefix, then by the whole name ignoring spaces, slashes
/// and dashes. Used for labels.
pub fn find_matching_key(cache: &NameMap, search: &str) -> Option<String> {
    let search_lower = search.to_lowercase();

    // First try exact match
    if let Some(id) = cache.get(&search_lower) {
        return Some(id.clone());
    }

    // Then try prefix match
    if let Some(id) = best_match(cache, |key| key.starts_with(&search_lower)) {
        return Some(id);
    }

    // Then the same name with the spaces, slashes and dashes left out, as shell completion offers
    // it (`WorkProjects` for "Work Projects"). The whole name has to match, not part of it.
    let search_squashed = squash(&search_lower);
    if search_squashed.is_empty() {
        return None;
    }
    best_match(cache, |key| squash(key) == search_squashed)
}

/// The id of the shortest matching key (ties broken alphabetically), so the result doesn't
/// depend on hash-map ordering.
fn best_match(cache: &NameMap, matches: impl Fn(&str) -> bool) -> Option<String> {
    cache
        .iter()
        .filter(|(key, _)| matches(key))
        .min_by_key(|(key, _)| (key.len(), key.as_str()))
        .map(|(_, id)| id.clone())
}

fn squash(value: &str) -> String {
    value.chars().filter(|c| !matches!(c, ' ' | '/' | '-')).collect()
}

/// Looks a list up by exact name, then prefix, then substring, then substring ignoring
/// spaces, slashes and dashes. Labels keep using the stricter `find_matching_key`.
pub fn find_list_key(cache: &NameMap, search: &str) -> Option<String> {
    if let Some(id) = find_matching_key(cache, search) {
        return Some(id);
    }

    let search_lower = search.to_lowercase();
    if let Some(id) = best_match(cache, |key| key.contains(&search_lower)) {
        return Some(id);
    }

    let search_squashed = squash(&search_lower);
    if search_squashed.is_empty() {
        return None;
    }
    best_match(cache, |key| squash(key).contains(&search_squashed))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lists(names: &[(&str, &str)]) -> NameMap {
        names.iter().map(|(n, id)| (n.to_string(), id.to_string())).collect()
    }

    #[test]
    fn list_lookup_prefers_exact_then_prefix_then_substring() {
        let cache = lists(&[("work", "1"), ("work projects", "2"), ("homework", "3")]);
        assert_eq!(find_list_key(&cache, "Work").as_deref(), Some("1"));
        assert_eq!(find_list_key(&cache, "work p").as_deref(), Some("2"));
        assert_eq!(find_list_key(&cache, "wor").as_deref(), Some("1"));
        assert_eq!(find_list_key(&cache, "mew").as_deref(), Some("3"));
        assert_eq!(find_list_key(&cache, "nothing"), None);
    }

    #[test]
    fn list_lookup_ignores_spaces_slashes_and_dashes() {
        let cache = lists(&[("side projects", "1"), ("home/garden", "2")]);
        assert_eq!(find_list_key(&cache, "sideproj").as_deref(), Some("1"));
        assert_eq!(find_list_key(&cache, "homegarden").as_deref(), Some("2"));
        assert_eq!(find_list_key(&cache, "-"), None);
    }

    #[test]
    fn ambiguous_prefixes_resolve_deterministically_to_the_shortest_name() {
        let cache = lists(&[("work later", "1"), ("work now", "2"), ("work", "3x"), ("workshop", "4")]);
        assert_eq!(find_matching_key(&cache, "wor").as_deref(), Some("3x"));
        let cache = lists(&[("work later", "1"), ("work now", "2")]);
        assert_eq!(find_matching_key(&cache, "wor").as_deref(), Some("2"));
    }

    #[test]
    fn a_name_can_be_given_without_its_spaces_as_completion_offers_it() {
        let cache = lists(&[("work projects", "1"), ("high-priority", "2"), ("home", "3")]);
        assert_eq!(find_matching_key(&cache, "WorkProjects").as_deref(), Some("1"));
        assert_eq!(find_matching_key(&cache, "highpriority").as_deref(), Some("2"));
        assert_eq!(find_list_key(&cache, "workprojects").as_deref(), Some("1"));
        // It has to be the whole name: part of it still doesn't match a label.
        assert_eq!(find_matching_key(&cache, "projects"), None);
        assert_eq!(find_matching_key(&cache, "-"), None);
        assert_eq!(find_matching_key(&cache, "workproj"), None, "part of the squashed name is not enough");
        assert_eq!(find_matching_key(&cache, "work p").as_deref(), Some("1"), "a real prefix still works");
    }

    #[test]
    fn labels_keep_the_stricter_matching() {
        let cache = lists(&[("homework", "1")]);
        assert_eq!(find_matching_key(&cache, "work"), None);
        assert_eq!(find_list_key(&cache, "work").as_deref(), Some("1"));
    }
}
