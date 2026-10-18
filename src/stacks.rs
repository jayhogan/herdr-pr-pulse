//! Groups PRs into native stacks, branch chains and standalones.

use std::collections::HashMap;

use crate::model::{Pr, PrState};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupKind {
    Native,
    Chain,
    Standalone,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Group {
    pub key: String,
    pub repo: String,
    pub name: String,
    pub kind: GroupKind,
    /// Base branch of the bottom open layer (the bottom layer when none is open).
    pub base: String,
    /// Merged/closed layers first, then open layers bottom-first.
    pub layers: Vec<Pr>,
}

impl Group {
    pub fn open_layers(&self) -> impl Iterator<Item = &Pr> {
        self.layers.iter().filter(|p| p.state == PrState::Open)
    }

    /// Number of the open layer directly below `number`, if any.
    pub fn waiting_on(&self, number: u32) -> Option<u32> {
        let open: Vec<u32> = self.open_layers().map(|p| p.number).collect();
        let i = open.iter().position(|&n| n == number)?;
        i.checked_sub(1).map(|j| open[j])
    }
}

/// Common branch-name prefix cut at a separator; the bottom ref when too short.
pub fn stack_name(head_refs: &[&str]) -> String {
    let Some(&first) = head_refs.first() else {
        return String::new();
    };
    let mut prefix: &str = first;
    for r in &head_refs[1..] {
        let end = prefix
            .char_indices()
            .zip(r.chars())
            .find(|((_, a), b)| a != b)
            .map_or_else(|| prefix.len().min(r.len()), |((i, _), _)| i);
        prefix = &prefix[..end];
    }
    let is_whole = head_refs.contains(&prefix);
    if !is_whole {
        let cut = prefix.rfind(['-', '/', '_']).map_or(0, |i| i + 1);
        prefix = prefix[..cut].trim_end_matches(['-', '/', '_']);
    }
    if prefix.chars().count() < 3 {
        first.to_string()
    } else {
        prefix.to_string()
    }
}

/// Merged/closed layers first, then open ones; order within each part is kept.
fn merged_first(mut layers: Vec<Pr>) -> Vec<Pr> {
    layers.sort_by_key(|p| p.state == PrState::Open);
    layers
}

fn build(key: String, kind: GroupKind, layers: Vec<Pr>) -> Group {
    let layers = merged_first(layers);
    let name = match kind {
        GroupKind::Standalone => "standalone".to_string(),
        _ => {
            let refs: Vec<&str> = layers.iter().map(|p| p.head_ref.as_str()).collect();
            stack_name(&refs)
        }
    };
    let bottom = layers
        .iter()
        .find(|p| p.state == PrState::Open)
        .unwrap_or(&layers[0]);
    Group {
        key,
        repo: bottom.repo.clone(),
        name,
        kind,
        base: bottom.base_ref.clone(),
        layers,
    }
}

pub fn group(prs: &[Pr]) -> Vec<Group> {
    let mut out = Vec::new();

    // Native stacks, in order of first appearance.
    let mut native: Vec<(String, Vec<Pr>)> = Vec::new();
    for p in prs.iter().filter(|p| p.stack.is_some()) {
        let id = &p.stack.as_ref().expect("filtered").id;
        match native.iter_mut().find(|(k, _)| k == id) {
            Some((_, v)) => v.push(p.clone()),
            None => native.push((id.clone(), vec![p.clone()])),
        }
    }
    for (id, mut layers) in native {
        layers.sort_by_key(|p| p.stack.as_ref().map_or(0, |s| s.position));
        out.push(build(id, GroupKind::Native, layers));
    }

    // Branch chains among PRs without a native stack.
    let loose: Vec<&Pr> = prs.iter().filter(|p| p.stack.is_none()).collect();
    let mut parent: HashMap<usize, usize> = HashMap::new();
    let mut child: HashMap<usize, usize> = HashMap::new();
    for (i, p) in loose.iter().enumerate() {
        let below = loose
            .iter()
            .position(|q| q.repo == p.repo && q.head_ref == p.base_ref && q.id != p.id);
        if let Some(j) = below
            && !child.contains_key(&j)
        {
            parent.insert(i, j);
            child.insert(j, i);
        }
    }
    let mut used = vec![false; loose.len()];
    for root in (0..loose.len()).filter(|i| !parent.contains_key(i) && child.contains_key(i)) {
        let mut chain = vec![root];
        while let Some(&next) = child.get(chain.last().expect("non-empty")) {
            chain.push(next);
        }
        let layers: Vec<Pr> = chain.iter().map(|&i| loose[i].clone()).collect();
        for &i in &chain {
            used[i] = true;
        }
        let key = format!("chain:{}:{}", layers[0].repo, layers[0].head_ref);
        out.push(build(key, GroupKind::Chain, layers));
    }

    // Everything else, including cycle members, stands alone.
    for (_, p) in loose.iter().enumerate().filter(|(i, _)| !used[*i]) {
        out.push(build(
            format!("pr:{}", p.id),
            GroupKind::Standalone,
            vec![(*p).clone()],
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{self, Pr, PrState};

    fn fixture() -> Vec<Pr> {
        model::parse_detail(include_str!("../tests/fixtures/detail.json")).unwrap()
    }

    fn find(groups: &[Group], n: u32) -> &Group {
        groups
            .iter()
            .find(|g| g.layers.iter().any(|p| p.number == n))
            .unwrap()
    }

    fn numbers(g: &Group) -> Vec<u32> {
        g.layers.iter().map(|p| p.number).collect()
    }

    #[test]
    fn native_stack_ordered_bottom_first() {
        let mut prs = fixture();
        prs.reverse();
        let groups = group(&prs);
        let g = find(&groups, 1291);
        assert_eq!(numbers(g), [1284, 1287, 1291, 1295]);
        assert_eq!(g.kind, GroupKind::Native);
        assert_eq!(g.key, "PRS_audit");
    }

    #[test]
    fn chain_fallback_links_base_to_head() {
        let groups = group(&fixture());
        let g = find(&groups, 412);
        assert_eq!(numbers(g), [412, 415]);
        assert_eq!(g.kind, GroupKind::Chain);
        assert_eq!(g.key, format!("chain:{}:{}", g.repo, g.layers[0].head_ref));
    }

    #[test]
    fn standalone_alone() {
        let groups = group(&fixture());
        let g = find(&groups, 1279);
        assert_eq!(g.kind, GroupKind::Standalone);
        assert_eq!(g.name, "standalone");
        assert_eq!(numbers(g), [1279]);
        assert_eq!(g.key, format!("pr:{}", g.layers[0].id));
    }

    #[test]
    fn merged_layers_sink_to_bottom() {
        let mut prs = fixture();
        let mut merged = prs.iter().find(|p| p.number == 1284).unwrap().clone();
        merged.state = PrState::Merged;
        prs.retain(|p| p.number != 1284);
        prs.push(merged);
        let groups = group(&prs);
        let g = find(&groups, 1284);
        assert_eq!(g.layers[0].number, 1284);
        assert_eq!(g.open_layers().next().unwrap().number, 1287);
        assert_eq!(g.open_layers().count(), 3);
    }

    #[test]
    fn stack_name_common_prefix() {
        assert_eq!(
            stack_name(&["audit/schema", "audit/kafka-producer"]),
            "audit"
        );
        assert_eq!(
            stack_name(&["CORE-8953-large-events", "CORE-8953-large-events-readers"]),
            "CORE-8953-large-events"
        );
        assert_eq!(stack_name(&["a/x", "b/y"]), "a/x");
    }

    #[test]
    fn waiting_on_lower_layer() {
        let groups = group(&fixture());
        let g = find(&groups, 1291);
        assert_eq!(g.waiting_on(1291), Some(1287));
        assert_eq!(g.waiting_on(1284), None);
        assert_eq!(g.waiting_on(9999), None);
    }

    #[test]
    fn cycles_are_standalone() {
        let mut a = fixture().into_iter().find(|p| p.number == 1279).unwrap();
        let mut b = fixture().into_iter().find(|p| p.number == 1302).unwrap();
        b.repo = a.repo.clone();
        (a.head_ref, a.base_ref) = ("x/a".into(), "x/b".into());
        (b.head_ref, b.base_ref) = ("x/b".into(), "x/a".into());
        let groups = group(&[a, b]);
        assert_eq!(groups.len(), 2);
        assert!(groups.iter().all(|g| g.kind == GroupKind::Standalone));
    }
}
