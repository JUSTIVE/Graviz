//! Type/field search — port of the search internals of `TreePanel.tsx`
//! (`fuzzyScore`, `proseMatch`, `makeSnippet`, and the result assembly
//! including the `"Type.field"` dotted-query mode).
//!
//! Indices are character indices (not bytes), so highlights are safe on
//! non-ASCII descriptions.

use crate::graph::{GraphNodeData, NodeKind, ParsedGraph};

pub const MAX_RESULTS: usize = 80;

#[derive(Debug, Clone, PartialEq)]
pub struct FuzzyMatch {
    pub score: i32,
    /// Character indices into the target that matched.
    pub indices: Vec<usize>,
}

/// Subsequence match with streak/boundary bonuses. `None` when the query is
/// not a subsequence of the target. Empty query matches with score 0.
pub fn fuzzy_score(query: &str, target: &str) -> Option<FuzzyMatch> {
    if query.is_empty() {
        return Some(FuzzyMatch { score: 0, indices: Vec::new() });
    }
    let q: Vec<char> = query.chars().flat_map(|c| c.to_lowercase()).collect();
    let t_orig: Vec<char> = target.chars().collect();
    let t: Vec<char> = t_orig.iter().flat_map(|c| c.to_lowercase()).collect();
    // NOTE: to_lowercase can expand some chars; schema names are ASCII in
    // practice, where it is 1:1. Guard by falling back to per-char eq_ignore.
    let (q, t) = if t.len() == t_orig.len() {
        (q, t)
    } else {
        (
            query.chars().collect::<Vec<_>>(),
            t_orig.clone(),
        )
    };

    let mut qi = 0usize;
    let mut indices = Vec::with_capacity(q.len());
    for (i, tc) in t.iter().enumerate() {
        if qi < q.len() && tc.eq_ignore_ascii_case(&q[qi]) {
            indices.push(i);
            qi += 1;
        }
    }
    if qi < q.len() {
        return None;
    }

    let mut score = 0i32;
    let mut streak = 0i32;
    for (i, &idx) in indices.iter().enumerate() {
        let prev_idx = if i > 0 { indices[i - 1] as i64 } else { -2 };
        if idx as i64 == prev_idx + 1 {
            streak += 1;
            score += 4 + streak * 2;
        } else {
            streak = 0;
            score += 1;
        }
        if idx == 0 {
            score += 8;
        } else {
            let prev = t_orig[idx - 1];
            let curr = t_orig[idx];
            if prev == '_' || prev == '-' || prev == '.' {
                score += 7;
            } else if curr.is_ascii_uppercase() {
                score += 5;
            }
        }
    }
    let ql = query.chars().count() as f64;
    let tl = t_orig.len().max(1) as f64;
    score += (ql / tl * 8.0).round() as i32;
    Some(FuzzyMatch { score, indices })
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProseMatch {
    /// Character index of the match start.
    pub index: usize,
    pub score: i32,
}

/// Case-insensitive substring match for free-form prose, preferring earlier
/// hits in shorter text.
pub fn prose_match(query: &str, text: Option<&str>) -> Option<ProseMatch> {
    let text = text?;
    if query.is_empty() {
        return None;
    }
    let hay: String = text.to_lowercase();
    let needle: String = query.to_lowercase();
    let byte_idx = hay.find(&needle)?;
    let index = hay[..byte_idx].chars().count();
    let pos_bonus = (6 - (index as i32) / 4).max(0);
    let ql = query.chars().count() as f64;
    let tl = text.chars().count().max(1) as f64;
    let score = 4 + pos_bonus + (ql / tl * 4.0).round() as i32;
    Some(ProseMatch { index, score })
}

#[derive(Debug, Clone, PartialEq)]
pub struct Snippet {
    pub snippet: String,
    /// Character indices of the highlighted span within `snippet`.
    pub indices: Vec<usize>,
}

/// Short ellipsized excerpt centered on the match.
pub fn make_snippet(text: &str, match_start: usize, query_len: usize) -> Snippet {
    const CONTEXT: usize = 24;
    let chars: Vec<char> = text.chars().collect();
    let start = match_start.saturating_sub(CONTEXT);
    let end = (match_start + query_len + CONTEXT).min(chars.len());
    let prefix = if start > 0 { "…" } else { "" };
    let suffix = if end < chars.len() { "…" } else { "" };
    let body: String = chars[start..end].iter().collect();
    let snippet = format!("{prefix}{body}{suffix}");
    let base = match_start - start + prefix.chars().count();
    let indices = (0..query_len).map(|k| base + k).collect();
    Snippet { snippet, indices }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnippetKind {
    Description,
    DeprecationReason,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SearchResult {
    /// Index into `ParsedGraph::nodes`.
    pub node_index: usize,
    pub type_name: String,
    pub type_kind: NodeKind,
    pub field_name: Option<String>,
    pub field_type: Option<String>,
    /// Index of the field/value row on the node, when the hit is a row.
    pub row_index: Option<usize>,
    pub score: i32,
    pub match_indices: Vec<usize>,
    /// Set in `"Type.field"` mode: highlight on the type name.
    pub type_match_indices: Option<Vec<usize>>,
    pub snippet: Option<Snippet>,
    pub snippet_kind: Option<SnippetKind>,
}

impl SearchResult {
    fn of(node_index: usize, node: &GraphNodeData, score: i32) -> Self {
        SearchResult {
            node_index,
            type_name: node.name.clone(),
            type_kind: node.kind,
            field_name: None,
            field_type: None,
            row_index: None,
            score,
            match_indices: Vec::new(),
            type_match_indices: None,
            snippet: None,
            snippet_kind: None,
        }
    }
}

/// Full search over the graph: fuzzy on names, prose fallback on
/// descriptions / deprecation reasons (only when the name missed, so nothing
/// appears twice), and `"Type.field"` dotted mode. Sorted by score, capped
/// at [`MAX_RESULTS`].
pub fn search_graph(graph: &ParsedGraph, query: &str) -> Vec<SearchResult> {
    let q = query.trim();
    if q.is_empty() {
        return Vec::new();
    }
    let qlen = q.chars().count();
    let mut out: Vec<SearchResult> = Vec::new();

    let dot = q.char_indices().find(|&(_, c)| c == '.').map(|(i, _)| i);
    if let Some(dot_idx) = dot.filter(|&i| i > 0) {
        let type_part = &q[..dot_idx];
        let field_part = &q[dot_idx + 1..];
        for (ni, node) in graph.nodes.iter().enumerate() {
            let Some(tm) = fuzzy_score(type_part, &node.name) else { continue };
            let fields = node.fields.as_deref().unwrap_or(&[]);
            let values = node.values.as_deref().unwrap_or(&[]);
            let rows: Vec<(String, Option<String>)> = fields
                .iter()
                .map(|f| (f.name.clone(), Some(f.type_.clone())))
                .chain(values.iter().map(|v| (v.name.clone(), None)))
                .collect();
            for (ri, (name, ty)) in rows.into_iter().enumerate() {
                let fm = if field_part.is_empty() {
                    Some(FuzzyMatch { score: 0, indices: Vec::new() })
                } else {
                    fuzzy_score(field_part, &name)
                };
                let Some(fm) = fm else { continue };
                let mut r = SearchResult::of(ni, node, tm.score + fm.score);
                r.field_name = Some(name);
                r.field_type = ty;
                r.row_index = Some(ri);
                r.match_indices = fm.indices;
                r.type_match_indices = Some(tm.indices.clone());
                out.push(r);
            }
        }
    } else {
        for (ni, node) in graph.nodes.iter().enumerate() {
            if let Some(tm) = fuzzy_score(q, &node.name) {
                let mut r = SearchResult::of(ni, node, tm.score + 3);
                r.match_indices = tm.indices;
                out.push(r);
            } else if let Some(dm) = prose_match(q, node.description.as_deref()) {
                let mut r = SearchResult::of(ni, node, dm.score);
                r.snippet = Some(make_snippet(
                    node.description.as_deref().unwrap(),
                    dm.index,
                    qlen,
                ));
                r.snippet_kind = Some(SnippetKind::Description);
                out.push(r);
            }
            for (ri, f) in node.fields.as_deref().unwrap_or(&[]).iter().enumerate() {
                if let Some(fm) = fuzzy_score(q, &f.name) {
                    let mut r = SearchResult::of(ni, node, fm.score);
                    r.field_name = Some(f.name.clone());
                    r.field_type = Some(f.type_.clone());
                    r.row_index = Some(ri);
                    r.match_indices = fm.indices;
                    out.push(r);
                } else {
                    let dm = prose_match(q, f.description.as_deref())
                        .map(|m| (f.description.as_deref().unwrap(), m, SnippetKind::Description));
                    let hit = dm.or_else(|| {
                        prose_match(q, f.deprecation_reason.as_deref()).map(|m| {
                            (
                                f.deprecation_reason.as_deref().unwrap(),
                                m,
                                SnippetKind::DeprecationReason,
                            )
                        })
                    });
                    if let Some((text, m, kind)) = hit {
                        let mut r = SearchResult::of(ni, node, m.score);
                        r.field_name = Some(f.name.clone());
                        r.field_type = Some(f.type_.clone());
                        r.row_index = Some(ri);
                        r.snippet = Some(make_snippet(text, m.index, qlen));
                        r.snippet_kind = Some(kind);
                        out.push(r);
                    }
                }
            }
            let field_count = node.fields.as_deref().map(|f| f.len()).unwrap_or(0);
            for (vi, v) in node.values.as_deref().unwrap_or(&[]).iter().enumerate() {
                if let Some(vm) = fuzzy_score(q, &v.name) {
                    let mut r = SearchResult::of(ni, node, vm.score);
                    r.field_name = Some(v.name.clone());
                    r.row_index = Some(field_count + vi);
                    r.match_indices = vm.indices;
                    out.push(r);
                } else {
                    let dm = prose_match(q, v.description.as_deref())
                        .map(|m| (v.description.as_deref().unwrap(), m, SnippetKind::Description));
                    let hit = dm.or_else(|| {
                        prose_match(q, v.deprecation_reason.as_deref()).map(|m| {
                            (
                                v.deprecation_reason.as_deref().unwrap(),
                                m,
                                SnippetKind::DeprecationReason,
                            )
                        })
                    });
                    if let Some((text, m, kind)) = hit {
                        let mut r = SearchResult::of(ni, node, m.score);
                        r.field_name = Some(v.name.clone());
                        r.row_index = Some(field_count + vi);
                        r.snippet = Some(make_snippet(text, m.index, qlen));
                        r.snippet_kind = Some(kind);
                        out.push(r);
                    }
                }
            }
        }
    }

    out.sort_by_key(|r| std::cmp::Reverse(r.score));
    out.truncate(MAX_RESULTS);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{sdl_to_graph, SdlToGraphOptions};

    #[test]
    fn fuzzy_prefers_camel_boundaries_and_prefix() {
        let a = fuzzy_score("ru", "RepositoryUser").unwrap();
        let b = fuzzy_score("ru", "peruser").unwrap();
        assert!(a.score > b.score, "{} vs {}", a.score, b.score);
        assert_eq!(a.indices, vec![0, 10]);
        assert!(fuzzy_score("xyz", "Repository").is_none());
    }

    #[test]
    fn fuzzy_streak_bonus() {
        let streak = fuzzy_score("repo", "Repository").unwrap();
        let scattered = fuzzy_score("rpo", "Repository").unwrap();
        assert!(streak.score > scattered.score);
    }

    #[test]
    fn prose_match_prefers_early_hits() {
        let early = prose_match("commit", Some("commit info")).unwrap();
        let late =
            prose_match("commit", Some("The record of an individual commit somewhere")).unwrap();
        assert!(early.score > late.score);
    }

    #[test]
    fn snippet_windows_and_highlights() {
        let text = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaHITbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let m = prose_match("hit", Some(text)).unwrap();
        let s = make_snippet(text, m.index, 3);
        assert!(s.snippet.starts_with('…'));
        assert!(s.snippet.ends_with('…'));
        let chars: Vec<char> = s.snippet.chars().collect();
        let hl: String = s.indices.iter().map(|&i| chars[i]).collect();
        assert_eq!(hl, "HIT");
    }

    #[test]
    fn dotted_query_matches_type_and_field() {
        let g = sdl_to_graph(
            "type Query { user: User } type User { id: ID!, name: String }",
            &SdlToGraphOptions::default(),
        );
        let results = search_graph(&g, "user.na");
        assert!(!results.is_empty());
        let top = &results[0];
        assert_eq!(top.type_name, "User");
        assert_eq!(top.field_name.as_deref(), Some("name"));
        assert!(top.type_match_indices.is_some());
    }

    #[test]
    fn prose_fallback_only_when_name_misses() {
        let g = sdl_to_graph(
            r#"
            "The queen of types"
            type Majesty { id: ID! }
            type Query { m: Majesty }
            "#,
            &SdlToGraphOptions::default(),
        );
        let results = search_graph(&g, "queen");
        assert_eq!(
            results
                .iter()
                .filter(|r| r.type_name == "Majesty" && r.field_name.is_none())
                .count(),
            1
        );
        assert!(results
            .iter()
            .any(|r| r.snippet.is_some() && r.type_name == "Majesty"));
    }
}
