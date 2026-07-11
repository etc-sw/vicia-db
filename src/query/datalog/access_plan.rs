use super::matcher::edn_to_entity_id;
use super::types::{AttributeSpec, DatalogQuery, EdnValue, WhereClause};
use crate::graph::FactStorage;
use crate::graph::types::Fact;
use crate::storage::index::encode_value;
use anyhow::Result;
use std::collections::{BTreeSet, HashSet};
use uuid::Uuid;

pub(crate) const MAX_SELECTIVE_LOOKUPS: usize = 4;

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
    pub(crate) fn for_query(query: &DatalogQuery) -> Self {
        if query.uses_rules() {
            return Self::FullScan;
        }

        let mut lookups = BTreeSet::new();
        if !collect_lookups(&query.where_clauses, &mut lookups)
            || lookups.is_empty()
            || lookups.len() > MAX_SELECTIVE_LOOKUPS
        {
            return Self::FullScan;
        }

        Self::Selective {
            lookups: lookups.into_iter().collect(),
        }
    }

    pub(crate) fn read_facts(&self, storage: &FactStorage) -> Result<Vec<Fact>> {
        let Self::Selective { lookups } = self else {
            return storage.get_all_facts();
        };

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
                QueryLookup::Entity(entity) => storage.get_facts_by_entity(entity)?,
                QueryLookup::Attribute(attribute) => storage.get_facts_by_attribute(attribute)?,
            };
            for fact in candidates {
                if seen.insert(ledger_identity(&fact)) {
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
    use crate::query::datalog::types::DatalogCommand;

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
}
