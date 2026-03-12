//! Query engine that intersects results from path, PID, and type indexes.
//!
//! [`QueryEngine`] holds references to the three index types and accepts
//! a [`QueryFilter`] describing the desired subset of events. Filters
//! are intersected: an event must satisfy **all** specified criteria to
//! appear in the results.

use std::collections::BTreeSet;

use crate::events::Event;

use super::path_index::{IndexEntry, PathIndex};
use super::pid_index::PidIndex;
use super::type_index::TypeIndex;

/// Criteria for filtering events via the indexes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QueryFilter {
    /// Exact path match.
    pub path: Option<String>,
    /// Path prefix match (e.g. `/workspace/src/`).
    pub path_prefix: Option<String>,
    /// Exact PID match.
    pub pid: Option<u32>,
    /// Exact event type match (serde tag).
    pub event_type: Option<String>,
    /// Inclusive lower bound on wall-clock time (RFC 3339).
    pub since: Option<String>,
    /// Inclusive upper bound on wall-clock time (RFC 3339).
    pub until: Option<String>,
    /// Inclusive lower bound on sequence number.
    pub seq_from: Option<u64>,
    /// Inclusive upper bound on sequence number.
    pub seq_to: Option<u64>,
    /// Maximum number of results to return.
    pub limit: Option<usize>,
}

/// Single entry in query results.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryResult {
    /// Event sequence number.
    pub seq: u64,
    /// Event type tag.
    pub event_type: String,
}

/// Intersects index lookups to answer filtered event queries.
#[derive(Debug)]
pub struct QueryEngine<'a> {
    path_index: &'a PathIndex,
    pid_index: &'a PidIndex,
    type_index: &'a TypeIndex,
}

impl<'a> QueryEngine<'a> {
    /// Creates a query engine over the given indexes.
    pub fn new(
        path_index: &'a PathIndex,
        pid_index: &'a PidIndex,
        type_index: &'a TypeIndex,
    ) -> Self {
        Self {
            path_index,
            pid_index,
            type_index,
        }
    }

    /// Executes a query using the provided filter.
    ///
    /// Index-backed filters (path, pid, type) narrow the candidate set.
    /// Sequence range and time range filters are applied afterward.
    /// Results are returned in ascending sequence order.
    pub fn query(&self, filter: &QueryFilter) -> Vec<QueryResult> {
        let candidates = self.gather_candidates(filter);
        let mut results: Vec<QueryResult> = candidates
            .into_iter()
            .filter(|r| self.matches_seq_range(r.seq, filter))
            .collect();

        results.sort_by_key(|r| r.seq);

        if let Some(limit) = filter.limit {
            results.truncate(limit);
        }

        results
    }

    /// Executes a query against raw events for time-range filtering.
    ///
    /// When `since` or `until` are set, the caller must supply the
    /// full event stream so wall-clock times can be checked. Index
    /// filters still narrow the candidate set.
    pub fn query_events(
        &self,
        filter: &QueryFilter,
        events: &[Event],
    ) -> Vec<QueryResult> {
        let candidates = self.gather_candidates(filter);
        let candidate_seqs: BTreeSet<u64> =
            candidates.iter().map(|r| r.seq).collect();

        let mut results: Vec<QueryResult> = events
            .iter()
            .filter(|e| candidate_seqs.contains(&e.seq))
            .filter(|e| self.matches_seq_range(e.seq, filter))
            .filter(|e| matches_time_range(e, filter))
            .map(|e| QueryResult {
                seq: e.seq,
                event_type: e.payload.event_type_tag().to_owned(),
            })
            .collect();

        if let Some(limit) = filter.limit {
            results.truncate(limit);
        }

        results
    }

    /// Collects candidate results by intersecting active index filters.
    fn gather_candidates(
        &self,
        filter: &QueryFilter,
    ) -> Vec<QueryResult> {
        let mut sets: Vec<BTreeSet<u64>> = Vec::new();
        let mut type_map: std::collections::HashMap<u64, String> =
            std::collections::HashMap::new();

        // Path filter
        if let Some(path) = &filter.path {
            let entries = self.path_index.lookup(path);
            let set = collect_entries(entries, &mut type_map);
            sets.push(set);
        } else if let Some(prefix) = &filter.path_prefix {
            let matches = self.path_index.lookup_prefix(prefix);
            let mut set = BTreeSet::new();
            for (_path, entries) in matches {
                for e in entries {
                    type_map.insert(e.seq, e.event_type.clone());
                    set.insert(e.seq);
                }
            }
            sets.push(set);
        }

        // PID filter
        if let Some(pid) = filter.pid {
            let entries = self.pid_index.lookup(pid);
            let set = collect_entries(entries, &mut type_map);
            sets.push(set);
        }

        // Type filter
        if let Some(event_type) = &filter.event_type {
            let seqs = self.type_index.lookup(event_type);
            let set: BTreeSet<u64> = seqs.iter().copied().collect();
            for &seq in &set {
                type_map
                    .entry(seq)
                    .or_insert_with(|| event_type.clone());
            }
            sets.push(set);
        }

        // Intersect all non-empty sets
        let result_seqs = if sets.is_empty() {
            // No index filters: return all known seqs from the type index
            let mut all = BTreeSet::new();
            for (event_type, seqs) in self.type_index.iter() {
                for &seq in seqs {
                    type_map
                        .entry(seq)
                        .or_insert_with(|| event_type.to_owned());
                    all.insert(seq);
                }
            }
            all
        } else {
            intersect_sets(&sets)
        };

        result_seqs
            .into_iter()
            .map(|seq| {
                let event_type = type_map
                    .get(&seq)
                    .cloned()
                    .unwrap_or_default();
                QueryResult { seq, event_type }
            })
            .collect()
    }

    fn matches_seq_range(&self, seq: u64, filter: &QueryFilter) -> bool {
        if let Some(from) = filter.seq_from {
            if seq < from {
                return false;
            }
        }
        if let Some(to) = filter.seq_to {
            if seq > to {
                return false;
            }
        }
        true
    }
}

fn collect_entries(
    entries: &[IndexEntry],
    type_map: &mut std::collections::HashMap<u64, String>,
) -> BTreeSet<u64> {
    let mut set = BTreeSet::new();
    for e in entries {
        type_map.insert(e.seq, e.event_type.clone());
        set.insert(e.seq);
    }
    set
}

fn intersect_sets(sets: &[BTreeSet<u64>]) -> BTreeSet<u64> {
    if sets.is_empty() {
        return BTreeSet::new();
    }
    let mut iter = sets.iter();
    let first = iter.next().expect("non-empty").clone();
    iter.fold(first, |acc, s| acc.intersection(s).copied().collect())
}

fn matches_time_range(event: &Event, filter: &QueryFilter) -> bool {
    if let Some(since) = &filter.since {
        if event.ts_wall.as_str() < since.as_str() {
            return false;
        }
    }
    if let Some(until) = &filter.until {
        if event.ts_wall.as_str() > until.as_str() {
            return false;
        }
    }
    true
}

#[cfg(test)]
#[path = "query_tests.rs"]
mod tests;

// Rust guideline compliant 2026-02-21
