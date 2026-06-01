use crate::types::InfoItem;

/// Merge results from multiple sources: dedup by url, sort by published desc, truncate.
pub fn merge(mut all_items: Vec<InfoItem>, limit: usize) -> Vec<InfoItem> {
    // Dedup by url, first wins
    let mut seen = std::collections::HashSet::new();
    all_items.retain(|item| seen.insert(item.url.clone()));

    // Sort by published date descending (items without date go last)
    all_items.sort_by(|a, b| match (&a.published, &b.published) {
        (Some(pa), Some(pb)) => pb.cmp(pa),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });

    all_items.truncate(limit);
    all_items
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn item(title: &str, url: &str, published: Option<&str>) -> InfoItem {
        InfoItem {
            title: title.into(),
            url: url.into(),
            summary: String::new(),
            published: published.map(|s| s.into()),
            source: "test".into(),
            raw: json!({}),
        }
    }

    #[test]
    fn dedup_removes_duplicate_urls() {
        let items = vec![
            item("a", "http://1", Some("2024-01-01")),
            item("b", "http://1", Some("2024-01-02")),
        ];
        let merged = merge(items, 10);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].title, "a"); // first wins
    }

    #[test]
    fn sort_newest_first() {
        let items = vec![
            item("old", "http://1", Some("2023-01-01")),
            item("new", "http://2", Some("2024-06-15")),
        ];
        let merged = merge(items, 10);
        assert_eq!(merged[0].title, "new");
        assert_eq!(merged[1].title, "old");
    }

    #[test]
    fn items_without_date_go_last() {
        let items = vec![
            item("nodate", "http://1", None),
            item("dated", "http://2", Some("2024-01-01")),
        ];
        let merged = merge(items, 10);
        assert_eq!(merged[0].title, "dated");
        assert_eq!(merged[1].title, "nodate");
    }

    #[test]
    fn truncate_respects_limit() {
        let items: Vec<_> = (0..5)
            .map(|i| item("x", &format!("http://{i}"), Some("2024-01-01")))
            .collect();
        let merged = merge(items, 3);
        assert_eq!(merged.len(), 3);
    }
}
