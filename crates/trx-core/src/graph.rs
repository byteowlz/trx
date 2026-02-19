//! Dependency graph analysis for trx
//!
//! Uses petgraph for cycle detection, topological sort, and ready-work analysis.

use crate::{DependencyType, Issue};
use petgraph::algo::{is_cyclic_directed, toposort};
use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::visit::EdgeRef;
use std::collections::HashMap;

/// Issue dependency graph
pub struct IssueGraph {
    graph: DiGraph<String, DependencyType>,
    node_map: HashMap<String, NodeIndex>,
}

impl IssueGraph {
    /// Build a graph from a list of issues
    pub fn from_issues(issues: &[&Issue]) -> Self {
        let mut graph = DiGraph::new();
        let mut node_map = HashMap::new();

        // Add all issues as nodes
        for issue in issues {
            let idx = graph.add_node(issue.id.clone());
            node_map.insert(issue.id.clone(), idx);
        }

        // Add dependency edges
        for issue in issues {
            if let Some(&from_idx) = node_map.get(&issue.id) {
                for dep in &issue.dependencies {
                    if let Some(&to_idx) = node_map.get(&dep.depends_on_id) {
                        graph.add_edge(from_idx, to_idx, dep.dep_type);
                    }
                }
            }
        }

        Self { graph, node_map }
    }

    /// Check if the graph has cycles
    pub fn has_cycles(&self) -> bool {
        is_cyclic_directed(&self.graph)
    }

    /// Get all cycles in the graph (returns issue IDs in each cycle)
    pub fn find_cycles(&self) -> Vec<Vec<String>> {
        // Simple cycle detection - find strongly connected components
        let sccs = petgraph::algo::kosaraju_scc(&self.graph);
        sccs.into_iter()
            .filter(|scc| scc.len() > 1 || self.has_self_loop(&scc[0]))
            .map(|scc| scc.into_iter().map(|idx| self.graph[idx].clone()).collect())
            .collect()
    }

    fn has_self_loop(&self, node: &NodeIndex) -> bool {
        self.graph.edges(*node).any(|e| e.target() == *node)
    }

    /// Topological sort (returns None if cycles exist)
    pub fn topological_order(&self) -> Option<Vec<String>> {
        toposort(&self.graph, None).ok().map(|order| {
            order
                .into_iter()
                .map(|idx| self.graph[idx].clone())
                .collect()
        })
    }

    /// Get issues that are ready (no open blocking dependencies)
    pub fn ready_issues<'a>(&self, issues: &'a [&'a Issue]) -> Vec<&'a Issue> {
        let open_ids: std::collections::HashSet<_> = issues
            .iter()
            .filter(|i| i.status.is_open())
            .map(|i| i.id.as_str())
            .collect();

        issues
            .iter()
            .filter(|issue| {
                issue.status.is_open()
                    && !issue.dependencies.iter().any(|dep| {
                        dep.dep_type == DependencyType::Blocks
                            && open_ids.contains(dep.depends_on_id.as_str())
                    })
            })
            .copied()
            .collect()
    }

    /// Get issues blocked by a given issue
    pub fn blocked_by(&self, issue_id: &str) -> Vec<String> {
        if let Some(&idx) = self.node_map.get(issue_id) {
            self.graph
                .neighbors_directed(idx, petgraph::Direction::Incoming)
                .map(|n| self.graph[n].clone())
                .collect()
        } else {
            Vec::new()
        }
    }

    /// Get issues that block a given issue
    pub fn blocking(&self, issue_id: &str) -> Vec<String> {
        if let Some(&idx) = self.node_map.get(issue_id) {
            self.graph
                .neighbors_directed(idx, petgraph::Direction::Outgoing)
                .map(|n| self.graph[n].clone())
                .collect()
        } else {
            Vec::new()
        }
    }

    /// Calculate PageRank-style scores for prioritization
    pub fn pagerank(&self, damping: f64, iterations: usize) -> HashMap<String, f64> {
        let n = self.graph.node_count();
        if n == 0 {
            return HashMap::new();
        }

        let mut scores: HashMap<NodeIndex, f64> = self
            .graph
            .node_indices()
            .map(|idx| (idx, 1.0 / n as f64))
            .collect();

        for _ in 0..iterations {
            let mut new_scores = HashMap::new();

            for node in self.graph.node_indices() {
                let mut score = (1.0 - damping) / n as f64;

                for edge in self
                    .graph
                    .edges_directed(node, petgraph::Direction::Incoming)
                {
                    let source = edge.source();
                    let out_degree = self.graph.edges(source).count() as f64;
                    if out_degree > 0.0 {
                        score += damping * scores[&source] / out_degree;
                    }
                }

                new_scores.insert(node, score);
            }

            scores = new_scores;
        }

        scores
            .into_iter()
            .map(|(idx, score)| (self.graph[idx].clone(), score))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::issue::Issue;

    #[test]
    fn test_ready_issues() {
        let issue1 = Issue::new("trx-001".into(), "First".into());
        let mut issue2 = Issue::new("trx-002".into(), "Second".into());

        // issue2 blocks issue1
        issue2.add_dependency("trx-001".into(), DependencyType::Blocks);

        let issues: Vec<&Issue> = vec![&issue1, &issue2];
        let graph = IssueGraph::from_issues(&issues);

        let ready = graph.ready_issues(&issues);
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].id, "trx-001");
    }
}
