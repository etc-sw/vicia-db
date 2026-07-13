use super::matcher::edn_to_entity_id;
use super::types::{AttributeSpec, DatalogQuery, EdnValue, WhereClause};
use crate::graph::FactStorage;
use crate::graph::types::Fact;
use crate::storage::index::encode_value;
use anyhow::Result;
use std::collections::{BTreeSet, HashSet};
use uuid::Uuid;

pub(crate) const MAX_SELECTIVE_LOOKUPS: usize = 4;
/// Exact entity sets stay bounded by caller-supplied identities rather than by
/// attribute cardinality. This larger cap supports batched embedded-ledger
/// reads without turning broad mixed/attribute plans into accidental scans.
pub(crate) const MAX_SELECTIVE_ENTITY_LOOKUPS: usize = 128;

/// One bounded committed-index lookup needed by a Datalog query.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum QueryLookup {
    Entity(Uuid),
    Attribute(String),
}

/// Storage access shape selected before query evaluation begins.
///
/// Planning and I/O outcomes are deliberately separate: `FullScan` means the
/// query shape cannot be served by a bounded set of committed-index lookups.
/// An error while executing a `Selective` plan remains an error and must never
/// be reinterpreted as permission to scan a potentially corrupt store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum QueryAccessPlan {
    FullScan,
    Selective { lookups: Vec<QueryLookup> },
}

impl QueryAccessPlan {
    /// Number of neighboring pages worth speculatively fetching after one
    /// sparse browser miss. Entity lookups favor low over-read; attribute
    /// ranges favor fewer IndexedDB/WASM callback round trips.
    #[cfg(all(target_arch = "wasm32", feature = "browser"))]
    pub(crate) fn browser_demand_batch_pages(&self) -> u64 {
        match self {
            Self::FullScan => 0,
            Self::Selective { lookups }
                if lookups
                    .iter()
                    .any(|lookup| matches!(lookup, QueryLookup::Attribute(_))) =>
            {
                64
            }
            Self::Selective { .. } => 8,
        }
    }

    pub(crate) fn for_query(query: &DatalogQuery) -> Self {
        if query.uses_rules() {
            return Self::FullScan;
        }

        let mut lookups = BTreeSet::new();
        if !collect_lookups(&query.where_clauses, &mut lookups) || lookups.is_empty() {
            return Self::FullScan;
        }

        let lookup_limit = if lookups
            .iter()
            .all(|lookup| matches!(lookup, QueryLookup::Entity(_)))
        {
            MAX_SELECTIVE_ENTITY_LOOKUPS
        } else {
            MAX_SELECTIVE_LOOKUPS
        };
        if lookups.len() > lookup_limit {
            return Self::FullScan;
        }

        Self::Selective {
            lookups: lookups.into_iter().collect(),
        }
    }

    /// Whether executing this query requires the complete committed fact base.
    ///
    /// Browser storage uses this pre-I/O classification to choose between a
    /// bounded demand-read loop and an explicit full-scan path. It must not
    /// infer a full scan from a failed selective lookup.
    pub(crate) fn is_full_scan(&self) -> bool {
        matches!(self, Self::FullScan)
    }

    #[allow(dead_code)]
    pub(crate) fn read_facts(&self, storage: &FactStorage) -> Result<Vec<Fact>> {
        self.read_facts_bounded(storage, usize::MAX)
    }

    /// Execute this access plan while retaining at most `max_facts` complete
    /// ledger records. The first excess record is an error, never truncation.
    pub(crate) fn read_facts_bounded(
        &self,
        storage: &FactStorage,
        max_facts: usize,
    ) -> Result<Vec<Fact>> {
        let Self::Selective { lookups } = self else {
            return storage.get_all_facts_bounded(max_facts);
        };

        // A single index range cannot contain cross-lookup duplicates. Return
        // it directly instead of cloning every fact identity into a HashSet.
        // This is the common browser path for exact-entity and single-attribute
        // reads, including Vetch foreground authority queries.
        if let [lookup] = lookups.as_slice() {
            return match lookup {
                QueryLookup::Entity(entity) => {
                    storage.get_facts_by_entity_bounded(entity, max_facts)
                }
                QueryLookup::Attribute(attribute) => {
                    storage.get_facts_by_attribute_bounded(attribute, max_facts)
                }
            };
        }

        type LedgerIdentity = (Uuid, String, Vec<u8>, i64, i64, u64, u64, bool);

        fn ledger_identity(fact: &Fact) -> LedgerIdentity {
            (
                fact.entity,
                fact.attribute.clone(),
                encode_value(&fact.value),
                fact.valid_from,
                fact.valid_to,
                fact.tx_count,
                fact.tx_id,
                fact.asserted,
            )
        }

        let mut seen: HashSet<LedgerIdentity> = HashSet::new();
        let mut facts = Vec::new();

        for lookup in lookups {
            let candidates = match lookup {
                QueryLookup::Entity(entity) => {
                    storage.get_facts_by_entity_bounded(entity, max_facts)?
                }
                QueryLookup::Attribute(attribute) => {
                    storage.get_facts_by_attribute_bounded(attribute, max_facts)?
                }
            };
            for fact in candidates {
                if seen.insert(ledger_identity(&fact)) {
                    if facts.len() >= max_facts {
                        anyhow::bail!(
                            "query source work exceeds max-results {max_facts}; incomplete result rejected"
                        );
                    }
                    facts.push(fact);
                }
            }
        }

        Ok(facts)
    }
}

fn collect_lookups(clauses: &[WhereClause], lookups: &mut BTreeSet<QueryLookup>) -> bool {
    for clause in clauses {
        match clause {
            WhereClause::Pattern(pattern) => {
                let bound_entity = match &pattern.entity {
                    EdnValue::Uuid(entity) => Some(*entity),
                    EdnValue::Keyword(_) => edn_to_entity_id(&pattern.entity).ok(),
                    _ => None,
                };

                if let Some(entity) = bound_entity {
                    lookups.insert(QueryLookup::Entity(entity));
                } else if let AttributeSpec::Real(EdnValue::Keyword(attribute)) = &pattern.attribute
                {
                    lookups.insert(QueryLookup::Attribute(attribute.clone()));
                } else {
                    return false;
                }
            }
            WhereClause::Not(inner) | WhereClause::NotJoin { clauses: inner, .. } => {
                if !collect_lookups(inner, lookups) {
                    return false;
                }
            }
            WhereClause::Or(branches) | WhereClause::OrJoin { branches, .. } => {
                for branch in branches {
                    if !collect_lookups(branch, lookups) {
                        return false;
                    }
                }
            }
            WhereClause::RuleInvocation { .. } => return false,
            WhereClause::Expr { .. } => {}
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::datalog::parser::parse_datalog_command;
    use crate::query::datalog::types::{DatalogCommand, FindSpec, Pattern};

    fn parse_query(input: &str) -> DatalogQuery {
        match parse_datalog_command(input).unwrap() {
            DatalogCommand::Query(query) => query,
            _ => panic!("expected query command"),
        }
    }

    #[test]
    fn nested_patterns_produce_deterministic_deduplicated_lookups() {
        let query = parse_query(
            "(query [:find ?e :where \
             [?e :name ?name] \
             (not [?e :archived true]) \
             (not-join [?e] [?e :archived true]) \
             (or [?e :kind :note] [?e :kind :task]) \
             (or-join [?e] [?e :kind :note] [?e :kind :task])])",
        );

        assert_eq!(
            QueryAccessPlan::for_query(&query),
            QueryAccessPlan::Selective {
                lookups: vec![
                    QueryLookup::Attribute(":archived".to_string()),
                    QueryLookup::Attribute(":kind".to_string()),
                    QueryLookup::Attribute(":name".to_string()),
                ],
            }
        );
    }

    #[test]
    fn bound_entity_is_preferred_and_deduplicated_across_patterns() {
        let query = parse_query(
            "(query [:find ?name ?age :where \
             [:alice :name ?name] \
             [:alice :age ?age]])",
        );
        let alice = edn_to_entity_id(&EdnValue::Keyword(":alice".to_string())).unwrap();

        assert_eq!(
            QueryAccessPlan::for_query(&query),
            QueryAccessPlan::Selective {
                lookups: vec![QueryLookup::Entity(alice)],
            }
        );
    }

    #[test]
    fn unbounded_pattern_and_lookup_limit_require_full_scan() {
        let unbounded = parse_query("(query [:find ?e :where [?e ?a ?v]])");
        assert_eq!(
            QueryAccessPlan::for_query(&unbounded),
            QueryAccessPlan::FullScan
        );

        let over_limit = parse_query(
            "(query [:find ?e :where \
             [?e :a ?a] [?e :b ?b] [?e :c ?c] [?e :d ?d] [?e :e ?e]])",
        );
        assert_eq!(
            QueryAccessPlan::for_query(&over_limit),
            QueryAccessPlan::FullScan
        );
    }

    #[test]
    fn bounded_exact_entity_set_has_a_separate_selective_limit() {
        let entities: Vec<Uuid> = (1..=MAX_SELECTIVE_ENTITY_LOOKUPS)
            .map(|value| Uuid::from_u128(value as u128))
            .collect();
        let query = DatalogQuery::new(
            vec![FindSpec::Variable("?value".to_string())],
            vec![WhereClause::Or(
                entities
                    .iter()
                    .map(|entity| {
                        vec![WhereClause::Pattern(Pattern::new(
                            EdnValue::Uuid(*entity),
                            EdnValue::Keyword(":bounded/value".to_string()),
                            EdnValue::Symbol("?value".to_string()),
                        ))]
                    })
                    .collect(),
            )],
        );

        assert_eq!(
            QueryAccessPlan::for_query(&query),
            QueryAccessPlan::Selective {
                lookups: entities.into_iter().map(QueryLookup::Entity).collect(),
            }
        );
    }

    #[test]
    fn exact_entity_set_over_its_bound_requires_full_scan() {
        let query = DatalogQuery::new(
            vec![FindSpec::Variable("?value".to_string())],
            vec![WhereClause::Or(
                (1..=MAX_SELECTIVE_ENTITY_LOOKUPS + 1)
                    .map(|value| {
                        vec![WhereClause::Pattern(Pattern::new(
                            EdnValue::Uuid(Uuid::from_u128(value as u128)),
                            EdnValue::Keyword(":bounded/value".to_string()),
                            EdnValue::Symbol("?value".to_string()),
                        ))]
                    })
                    .collect(),
            )],
        );

        assert_eq!(
            QueryAccessPlan::for_query(&query),
            QueryAccessPlan::FullScan
        );
    }

    #[test]
    fn mixed_entity_and_attribute_plans_keep_the_general_lookup_limit() {
        let bounded_entities: Vec<Uuid> = (1..=3).map(Uuid::from_u128).collect();
        let mut bounded_clauses: Vec<WhereClause> = bounded_entities
            .iter()
            .map(|entity| {
                WhereClause::Pattern(Pattern::new(
                    EdnValue::Uuid(*entity),
                    EdnValue::Keyword(":bounded/value".to_string()),
                    EdnValue::Symbol("?value".to_string()),
                ))
            })
            .collect();
        bounded_clauses.push(WhereClause::Pattern(Pattern::new(
            EdnValue::Symbol("?entity".to_string()),
            EdnValue::Keyword(":bounded/kind".to_string()),
            EdnValue::Keyword(":bounded/selected".to_string()),
        )));
        let bounded = DatalogQuery::new(
            vec![FindSpec::Variable("?value".to_string())],
            bounded_clauses,
        );

        let mut over_limit_clauses: Vec<WhereClause> = (1u128..=4)
            .map(|value| {
                WhereClause::Pattern(Pattern::new(
                    EdnValue::Uuid(Uuid::from_u128(value)),
                    EdnValue::Keyword(":bounded/value".to_string()),
                    EdnValue::Symbol("?value".to_string()),
                ))
            })
            .collect();
        over_limit_clauses.push(WhereClause::Pattern(Pattern::new(
            EdnValue::Symbol("?entity".to_string()),
            EdnValue::Keyword(":bounded/kind".to_string()),
            EdnValue::Keyword(":bounded/selected".to_string()),
        )));
        let over_limit = DatalogQuery::new(
            vec![FindSpec::Variable("?value".to_string())],
            over_limit_clauses,
        );

        assert_eq!(
            QueryAccessPlan::for_query(&bounded),
            QueryAccessPlan::Selective {
                lookups: bounded_entities
                    .into_iter()
                    .map(QueryLookup::Entity)
                    .chain(std::iter::once(QueryLookup::Attribute(
                        ":bounded/kind".to_string()
                    )))
                    .collect(),
            }
        );
        assert_eq!(
            QueryAccessPlan::for_query(&over_limit),
            QueryAccessPlan::FullScan
        );
    }

    #[test]
    fn rule_invocation_requires_full_fact_base() {
        let mut query = parse_query("(query [:find ?e :where [?e :name ?name]])");
        query.where_clauses.push(WhereClause::RuleInvocation {
            predicate: "reachable".to_string(),
            args: vec![
                EdnValue::Symbol("?e".to_string()),
                EdnValue::Symbol("?target".to_string()),
            ],
        });

        assert_eq!(
            QueryAccessPlan::for_query(&query),
            QueryAccessPlan::FullScan
        );
    }

    #[test]
    fn full_scan_shape_is_explicitly_inspectable_before_io() {
        let full_scan = QueryAccessPlan::for_query(&parse_query(
            "(query [:find ?e :where [?e ?attribute ?value]])",
        ));
        let selective =
            QueryAccessPlan::for_query(&parse_query("(query [:find ?e :where [?e :name ?name]])"));

        assert!(full_scan.is_full_scan());
        assert!(!selective.is_full_scan());
    }
}
