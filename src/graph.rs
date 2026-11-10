//! The release graph: the runtime, build and optional dependencies between
//! units. Ignoring the edges between members of one fixed group, it must be
//! acyclic; its topological order is the order of publication.

use std::collections::{BTreeMap, BTreeSet};

use petgraph::algo::tarjan_scc;
use petgraph::graph::DiGraph;

use crate::adapter::Edge;
use crate::units::Units;

/// The units in the order of publication: every unit after the units it
/// depends on, members of a fixed group by their own edges where these
/// allow an order, and otherwise by name. Fails with the edges of the
/// graph's cycles.
pub fn order<'e>(units: &Units, edges: &'e [Edge]) -> Result<Vec<String>, Vec<&'e Edge>> {
    let names: Vec<&str> = units.iter().map(|unit| unit.name.as_str()).collect();
    let same_group = |a: &str, b: &str| {
        units
            .group_of(a)
            .is_some_and(|group| group.members.iter().any(|member| member == b))
    };
    // Edges from the dependent to the dependency; `true` for one between
    // members of a fixed group that only orders them where it can. An edge
    // the ecosystem's tool needs published first holds inside a group too.
    let mut graph_edges: Vec<(&str, &str, bool, &Edge)> = Vec::new();
    for edge in edges {
        let Some(dependent) = edge.dependent_unit.as_deref() else {
            continue;
        };
        if !edge.orders_publication()
            || dependent == edge.dependency
            || !names.contains(&dependent)
            || !names.contains(&edge.dependency.as_str())
        {
            continue;
        }
        let soft = same_group(dependent, &edge.dependency) && !edge.published_first;
        graph_edges.push((dependent, edge.dependency.as_str(), soft, edge));
    }

    let mut graph = DiGraph::<&str, &Edge>::new();
    let index: BTreeMap<&str, _> = names
        .iter()
        .map(|name| (*name, graph.add_node(*name)))
        .collect();
    for (dependent, dependency, soft, edge) in &graph_edges {
        if !soft {
            graph.add_edge(index[dependent], index[dependency], *edge);
        }
    }
    let mut cycle_edges: Vec<&Edge> = Vec::new();
    for component in tarjan_scc(&graph) {
        if component.len() < 2 {
            continue;
        }
        let members: BTreeSet<_> = component.into_iter().collect();
        cycle_edges.extend(
            graph
                .edge_indices()
                .filter(|&edge| {
                    let (from, to) = graph.edge_endpoints(edge).expect("the edge exists");
                    members.contains(&from) && members.contains(&to)
                })
                .map(|edge| graph[edge]),
        );
    }
    if !cycle_edges.is_empty() {
        cycle_edges.sort_by(|a, b| {
            (&a.dependent_unit, &a.dependency, a.kind, &a.location).cmp(&(
                &b.dependent_unit,
                &b.dependency,
                b.kind,
                &b.location,
            ))
        });
        cycle_edges.dedup();
        return Err(cycle_edges);
    }

    // Kahn's algorithm, by name among the ready units. When only edges
    // inside fixed groups block every remaining unit, they form a cycle of
    // their own, and the first unit by name that no other edge blocks goes
    // next.
    let mut blocking: BTreeMap<&str, BTreeSet<(&str, bool)>> =
        names.iter().map(|name| (*name, BTreeSet::new())).collect();
    for (dependent, dependency, soft, _) in &graph_edges {
        blocking
            .get_mut(dependent)
            .expect("a unit")
            .insert((dependency, *soft));
    }
    let mut ordered = Vec::new();
    while !blocking.is_empty() {
        let next = blocking
            .iter()
            .find(|(_, dependencies)| dependencies.is_empty())
            .or_else(|| {
                blocking
                    .iter()
                    .find(|(_, dependencies)| dependencies.iter().all(|(_, soft)| *soft))
            })
            .map(|(name, _)| *name)
            .expect("edges outside fixed groups are acyclic");
        blocking.remove(next);
        for dependencies in blocking.values_mut() {
            dependencies.retain(|(dependency, _)| *dependency != next);
        }
        ordered.push(next.to_string());
    }
    Ok(ordered)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::{EdgeKind, Location};
    use crate::config::Config;

    fn units(names: &[&str], groups: &[&[&str]]) -> (tempfile::TempDir, Units) {
        let root = tempfile::tempdir().unwrap();
        let mut config = String::new();
        for name in names {
            std::fs::create_dir_all(root.path().join(name)).unwrap();
            config.push_str(&format!(
                "[[unit]]\nname = \"{name}\"\npath = \"{name}\"\nadapter = \"npm\"\n"
            ));
        }
        for (index, members) in groups.iter().enumerate() {
            let members: Vec<String> = members.iter().map(|m| format!("\"{m}\"")).collect();
            config.push_str(&format!(
                "[[group]]\nname = \"g{index}\"\nunits = [{}]\n",
                members.join(", ")
            ));
        }
        let units = Units::resolve(root.path(), &Config::parse(&config).unwrap()).unwrap();
        (root, units)
    }

    fn edge(dependent: &str, dependency: &str, kind: EdgeKind) -> Edge {
        Edge {
            dependent_path: dependent.to_string(),
            dependent_unit: Some(dependent.to_string()),
            dependency: dependency.to_string(),
            kind,
            reference: "^1.0.0".to_string(),
            range: Some("^1.0.0".to_string()),
            follows: false,
            published_first: false,
            location: Location {
                file: format!("{dependent}/package.json"),
                keys: vec!["dependencies".to_string(), dependency.to_string()],
            },
        }
    }

    #[test]
    fn dependencies_come_first() {
        let (_root, units) = units(&["a", "b", "c", "d", "e"], &[]);
        let edges = [
            edge("a", "c", EdgeKind::Runtime),
            edge("c", "d", EdgeKind::Optional),
            edge("d", "e", EdgeKind::Build),
            edge("b", "a", EdgeKind::Peer),
            edge("e", "b", EdgeKind::Development),
        ];
        assert_eq!(order(&units, &edges).unwrap(), ["b", "e", "d", "c", "a"]);
    }

    #[test]
    fn cycle_lists_its_edges() {
        let (_root, units) = units(&["a", "b", "c"], &[]);
        let edges = [
            edge("a", "b", EdgeKind::Runtime),
            edge("b", "a", EdgeKind::Runtime),
            edge("b", "c", EdgeKind::Runtime),
            edge("c", "a", EdgeKind::Development),
        ];
        let cycle: Vec<String> = order(&units, &edges)
            .unwrap_err()
            .iter()
            .map(|edge| format!("{:?} -> {}", edge.dependent_unit, edge.dependency))
            .collect();
        assert_eq!(cycle, ["Some(\"a\") -> b", "Some(\"b\") -> a"]);
        // A development dependency the ecosystem's tool needs published
        // first, as cargo needs one that names a version, orders publication.
        let mut needed = edge("c", "a", EdgeKind::Development);
        needed.published_first = true;
        let edges = [edge("a", "c", EdgeKind::Runtime), needed];
        assert_eq!(order(&units, &edges).unwrap_err().len(), 2);
    }

    #[test]
    fn group_members_follow_their_own_edges() {
        let (_root, units) = units(&["web", "ui", "api"], &[&["web", "ui"]]);
        let edges = [
            edge("web", "ui", EdgeKind::Runtime),
            edge("ui", "api", EdgeKind::Runtime),
        ];
        assert_eq!(order(&units, &edges).unwrap(), ["api", "ui", "web"]);
        // A cycle inside a group is allowed and leaves the order to names.
        let edges = [
            edge("web", "ui", EdgeKind::Runtime),
            edge("ui", "web", EdgeKind::Runtime),
            edge("web", "api", EdgeKind::Runtime),
        ];
        assert_eq!(order(&units, &edges).unwrap(), ["api", "ui", "web"]);
        // Not when the ecosystem's tool needs a dependency published first.
        let mut needed = edge("ui", "web", EdgeKind::Development);
        needed.published_first = true;
        let edges = [edge("web", "ui", EdgeKind::Runtime), needed.clone()];
        assert_eq!(order(&units, &edges).unwrap(), ["api", "web", "ui"]);
        let mut first = edge("web", "ui", EdgeKind::Runtime);
        first.published_first = true;
        let edges = [first, needed];
        assert_eq!(order(&units, &edges).unwrap_err().len(), 2);
    }
}
