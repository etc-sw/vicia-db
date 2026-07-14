//! Shared native/browser Gate E corpus support.

use serde::Deserialize;
use serde_json::Value as JsonValue;
use uuid::Uuid;

pub(crate) const PAGE_SIZE: usize = crate::storage::PAGE_SIZE;
pub(crate) const NATIVE_FIXTURE: &[u8] = include_bytes!("../tests/fixtures/gate_e/native.graph");
pub(crate) const BROWSER_FIXTURE: &[u8] = include_bytes!("../tests/fixtures/gate_e/browser.graph");

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BoundedSelectiveReadFixture {
    pub schema: String,
    pub visible_source_count: usize,
    pub asserted_source_count: usize,
    pub post_view_source_index: usize,
    pub keyword_attribute: String,
    pub ref_attribute: String,
    pub keyword_target: String,
    pub ref_target: Uuid,
    pub first_valid_from: String,
    pub first_valid_to: String,
    pub second_valid_from: String,
    pub second_valid_to: String,
}

impl BoundedSelectiveReadFixture {
    pub(crate) fn source(&self, index: usize) -> Uuid {
        assert!(index > 0, "fixture source indices are one-based");
        Uuid::from_u128(index as u128)
    }

    pub(crate) fn expected_visible_sources(&self) -> Vec<Uuid> {
        (1..=self.visible_source_count)
            .map(|index| self.source(index))
            .collect()
    }

    pub(crate) fn setup_commands(&self) -> Vec<String> {
        let first_window_rows = (1..=self.asserted_source_count)
            .flat_map(|index| self.rows_for_source(index))
            .collect::<Vec<_>>()
            .join(" ");
        let first_source_rows = self.rows_for_source(1).join(" ");
        let retracted_source_rows = self.rows_for_source(self.asserted_source_count).join(" ");
        vec![
            format!(
                r#"(transact {{:valid-from "{}" :valid-to "{}"}} [{first_window_rows}])"#,
                self.first_valid_from, self.first_valid_to
            ),
            format!(
                r#"(transact {{:valid-from "{}" :valid-to "{}"}} [{first_source_rows}])"#,
                self.second_valid_from, self.second_valid_to
            ),
            format!(
                r#"(retract {{:valid-from "{}" :valid-to "{}"}} [{first_source_rows}])"#,
                self.first_valid_from, self.first_valid_to
            ),
            format!("(retract [{retracted_source_rows}])"),
        ]
    }

    pub(crate) fn post_view_command(&self) -> String {
        format!(
            "(transact [{}])",
            self.rows_for_source(self.post_view_source_index).join(" ")
        )
    }

    pub(crate) fn keyword_query(&self) -> String {
        format!(
            "(query [:find ?source :where [?source {} {}]])",
            self.keyword_attribute, self.keyword_target
        )
    }

    fn rows_for_source(&self, index: usize) -> [String; 2] {
        let source = self.source(index);
        [
            format!(
                r#"[#uuid "{source}" {} {}]"#,
                self.keyword_attribute, self.keyword_target
            ),
            format!(
                r#"[#uuid "{source}" {} #uuid "{}"]"#,
                self.ref_attribute, self.ref_target
            ),
        ]
    }
}

pub(crate) fn bounded_selective_read_fixture() -> BoundedSelectiveReadFixture {
    let fixture: BoundedSelectiveReadFixture = serde_json::from_str(include_str!(
        "../benchmarks/fixtures/vetch-bounded-selective-read.v1.json"
    ))
    .expect("bounded selective-read fixture JSON must parse");
    assert_eq!(
        fixture.schema, "vicia.vetch-bounded-selective-read.v1",
        "bounded selective-read fixture schema"
    );
    assert_eq!(
        fixture.asserted_source_count,
        fixture.visible_source_count + 1,
        "one asserted source must be removed by the unscoped retract"
    );
    assert_eq!(
        fixture.post_view_source_index,
        fixture.asserted_source_count + 1,
        "post-view source must follow the checkpointed fixture"
    );
    fixture
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct Corpus {
    pub queries: Vec<QueryCase>,
    pub corruptions: Vec<CorruptionCase>,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct QueryCase {
    pub id: String,
    pub datalog: String,
    pub variables: Vec<String>,
    pub rows: Vec<Vec<JsonValue>>,
    pub unordered_rows: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct CorruptionCase {
    pub id: String,
    pub mutation: Mutation,
    pub expected: String,
    #[serde(default)]
    pub exportable: bool,
    pub probe: Option<Probe>,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct Probe {
    pub datalog: String,
    pub rows: Vec<Vec<JsonValue>>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum Mutation {
    WriteByte { offset: usize, value: u8 },
    WriteU32Le { offset: usize, value: u32 },
    XorByte { offset: usize, value: u8 },
    Truncate { length: usize },
    CorruptNewestManifestSlot,
    CorruptNewestManifestPayload,
    CorruptNewestDeltaSegment,
    RemoveLastPage,
    TruncateLastPageHalf,
    CorruptOldestDeltaSegment,
    CorruptBothManifestSlots,
    AppendPage { value: u8 },
}

pub(crate) fn corpus() -> Corpus {
    serde_json::from_str(include_str!("../tests/fixtures/gate_e/corpus.json"))
        .expect("Gate E corpus JSON must parse")
}

pub(crate) fn normalize_rows(mut rows: Vec<Vec<JsonValue>>) -> Vec<Vec<JsonValue>> {
    rows.sort_by_key(|row| serde_json::to_string(row).unwrap_or_default());
    rows
}

pub(crate) fn published_byte_len(bytes: &[u8]) -> Result<usize, String> {
    let pages = read_u64(bytes, 8)?;
    let pages = usize::try_from(pages).map_err(|_| "published page count too large".to_string())?;
    pages
        .checked_mul(PAGE_SIZE)
        .ok_or_else(|| "published byte length overflow".to_string())
}

pub(crate) fn apply_mutation(source: &[u8], mutation: &Mutation) -> Result<Vec<u8>, String> {
    let mut bytes = source.to_vec();
    match mutation {
        Mutation::WriteByte { offset, value } => {
            *bytes
                .get_mut(*offset)
                .ok_or_else(|| format!("write_byte offset {offset} out of bounds"))? = *value;
        }
        Mutation::WriteU32Le { offset, value } => {
            let end = offset
                .checked_add(4)
                .ok_or_else(|| "write_u32_le offset overflow".to_string())?;
            let destination = bytes
                .get_mut(*offset..end)
                .ok_or_else(|| format!("write_u32_le offset {offset} out of bounds"))?;
            destination.copy_from_slice(&value.to_le_bytes());
        }
        Mutation::XorByte { offset, value } => {
            let byte = bytes
                .get_mut(*offset)
                .ok_or_else(|| format!("xor_byte offset {offset} out of bounds"))?;
            *byte ^= *value;
        }
        Mutation::Truncate { length } => {
            if *length >= bytes.len() {
                return Err(format!(
                    "truncate length {length} must be shorter than {}",
                    bytes.len()
                ));
            }
            bytes.truncate(*length);
        }
        Mutation::CorruptNewestManifestSlot => {
            let slot = newest_manifest_slot_offset(&bytes)?;
            let checksum = slot
                .checked_add(36)
                .ok_or_else(|| "manifest slot checksum offset overflow".to_string())?;
            let byte = bytes
                .get_mut(checksum)
                .ok_or_else(|| "newest manifest slot checksum outside page 0".to_string())?;
            *byte ^= 0x55;
        }
        Mutation::CorruptNewestManifestPayload => {
            let descriptor = newest_manifest_descriptor(&bytes)?;
            let offset = page_offset(descriptor.manifest_page_start)?;
            let byte = bytes
                .get_mut(offset)
                .ok_or_else(|| "newest manifest payload is outside fixture".to_string())?;
            *byte ^= 1;
        }
        Mutation::CorruptNewestDeltaSegment => {
            let offset = *delta_segment_markers(&bytes)?
                .last()
                .ok_or_else(|| "fixture has no delta segment".to_string())?;
            bytes[offset] ^= 1;
        }
        Mutation::RemoveLastPage => {
            if bytes.len() < PAGE_SIZE * 2 || !bytes.len().is_multiple_of(PAGE_SIZE) {
                return Err("fixture must contain at least two complete pages".to_string());
            }
            bytes.truncate(bytes.len() - PAGE_SIZE);
        }
        Mutation::TruncateLastPageHalf => {
            if bytes.len() < PAGE_SIZE * 2 || !bytes.len().is_multiple_of(PAGE_SIZE) {
                return Err("fixture must contain at least two complete pages".to_string());
            }
            bytes.truncate(bytes.len() - PAGE_SIZE / 2);
        }
        Mutation::CorruptOldestDeltaSegment => {
            let offset = *delta_segment_markers(&bytes)?
                .first()
                .ok_or_else(|| "fixture has no delta segment".to_string())?;
            bytes[offset] ^= 1;
        }
        Mutation::CorruptBothManifestSlots => {
            // v10 header extension: primary/secondary 40-byte slots start at
            // 96/136; slot checksum is the final four bytes.
            for offset in [132usize, 172usize] {
                let byte = bytes
                    .get_mut(offset)
                    .ok_or_else(|| "manifest slot checksum outside page 0".to_string())?;
                *byte ^= 0x55;
            }
        }
        Mutation::AppendPage { value } => bytes.extend(std::iter::repeat_n(*value, PAGE_SIZE)),
    }
    Ok(bytes)
}

#[derive(Clone, Copy)]
struct ManifestDescriptor {
    generation: u64,
    manifest_page_start: u64,
    manifest_page_count: u64,
    manifest_len: u64,
}

fn newest_manifest_descriptor(bytes: &[u8]) -> Result<ManifestDescriptor, String> {
    let published_pages = read_u64(bytes, 8)?;
    let mut descriptors = Vec::new();
    for slot in [96usize, 136usize] {
        let descriptor = ManifestDescriptor {
            generation: read_u64(bytes, slot)?,
            manifest_page_start: read_u64(bytes, slot + 8)?,
            manifest_page_count: read_u64(bytes, slot + 16)?,
            manifest_len: read_u64(bytes, slot + 24)?,
        };
        if descriptor.generation > 0 {
            let end = descriptor
                .manifest_page_start
                .checked_add(descriptor.manifest_page_count)
                .ok_or_else(|| "manifest descriptor page range overflow".to_string())?;
            let capacity = descriptor
                .manifest_page_count
                .checked_mul(PAGE_SIZE as u64)
                .ok_or_else(|| "manifest descriptor capacity overflow".to_string())?;
            if descriptor.manifest_page_start == 0
                || descriptor.manifest_page_count == 0
                || descriptor.manifest_len == 0
                || end > published_pages
                || descriptor.manifest_len > capacity
            {
                return Err("fixture contains an invalid manifest descriptor".to_string());
            }
            descriptors.push(descriptor);
        }
    }
    if descriptors.len() != 2 {
        return Err("Gate E fixture must expose two active manifest slots".to_string());
    }
    descriptors
        .into_iter()
        .max_by_key(|descriptor| descriptor.generation)
        .ok_or_else(|| "Gate E fixture has no manifest descriptor".to_string())
}

fn newest_manifest_slot_offset(bytes: &[u8]) -> Result<usize, String> {
    [96usize, 136usize]
        .into_iter()
        .map(|slot| read_u64(bytes, slot).map(|generation| (slot, generation)))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|(_, generation)| *generation > 0)
        .max_by_key(|(_, generation)| *generation)
        .map(|(slot, _)| slot)
        .ok_or_else(|| "Gate E fixture has no active manifest slot".to_string())
}

fn delta_segment_markers(bytes: &[u8]) -> Result<Vec<usize>, String> {
    const MAGIC: &[u8; 8] = b"MGDSG001";
    let published = published_byte_len(bytes)?;
    let image = bytes
        .get(..published)
        .ok_or_else(|| "fixture is shorter than its published page count".to_string())?;
    let markers: Vec<usize> = image
        .windows(MAGIC.len())
        .enumerate()
        .filter_map(|(offset, window)| (window == MAGIC).then_some(offset))
        .collect();
    if markers.len() < 3 {
        return Err(format!(
            "Gate E fixture must contain at least three delta segments, found {}",
            markers.len()
        ));
    }
    Ok(markers)
}

fn page_offset(page_id: u64) -> Result<usize, String> {
    let page_id = usize::try_from(page_id).map_err(|_| "page id too large".to_string())?;
    page_id
        .checked_mul(PAGE_SIZE)
        .ok_or_else(|| "page offset overflow".to_string())
}

fn read_u64(bytes: &[u8], offset: usize) -> Result<u64, String> {
    let end = offset
        .checked_add(8)
        .ok_or_else(|| "u64 offset overflow".to_string())?;
    let raw: [u8; 8] = bytes
        .get(offset..end)
        .ok_or_else(|| format!("u64 at offset {offset} is outside fixture"))?
        .try_into()
        .map_err(|_| "u64 slice has wrong length".to_string())?;
    Ok(u64::from_le_bytes(raw))
}

#[cfg(not(target_arch = "wasm32"))]
mod native_tests {
    use super::*;
    use crate::json_value::to_tagged_json;
    use crate::{Minigraf, QueryResult, Value};

    fn assert_query(db: &Minigraf, case: &QueryCase) {
        let result = db
            .execute(&case.datalog)
            .expect("Gate E query must execute");
        let QueryResult::QueryResults { vars, results } = result else {
            panic!("Gate E query must return rows");
        };
        assert_eq!(vars, case.variables, "query variables: {}", case.id);
        let mut actual: Vec<Vec<JsonValue>> = results
            .iter()
            .map(|row| row.iter().map(to_tagged_json).collect())
            .collect();
        let mut expected = case.rows.clone();
        if case.unordered_rows {
            actual = normalize_rows(actual);
            expected = normalize_rows(expected);
        }
        assert_eq!(actual, expected, "query rows: {}", case.id);
    }

    fn assert_probe(db: &Minigraf, probe: &Probe, case_id: &str) {
        let result = db
            .execute(&probe.datalog)
            .expect("corruption probe must execute");
        let QueryResult::QueryResults { results, .. } = result else {
            panic!("corruption probe must return rows");
        };
        let actual: Vec<Vec<JsonValue>> = results
            .iter()
            .map(|row| row.iter().map(to_tagged_json).collect())
            .collect();
        assert_eq!(actual, probe.rows, "corruption fallback probe: {case_id}");
    }

    fn with_fixture(bytes: &[u8], test: impl FnOnce(&Minigraf)) {
        let dir = tempfile::tempdir().expect("temporary Gate E directory");
        let path = dir.path().join("fixture.graph");
        std::fs::write(&path, bytes).expect("write Gate E fixture");
        let db = Minigraf::open(&path).expect("open Gate E fixture");
        test(&db);
    }

    #[test]
    fn native_consumer_matches_canonical_queries_for_both_producers() {
        let corpus = corpus();
        for bytes in [NATIVE_FIXTURE, BROWSER_FIXTURE] {
            with_fixture(bytes, |db| {
                for case in &corpus.queries {
                    assert_query(db, case);
                }
            });
        }
    }

    #[test]
    fn browser_produced_fixture_preserves_full_ledger_identity_on_native() {
        with_fixture(BROWSER_FIXTURE, |db| {
            let records = db
                .export_fact_log_since(0)
                .expect("export browser-produced ledger");
            assert_eq!(records.len(), 13, "fixture ledger record count");
            let tx_counts: Vec<u64> = records.iter().map(|record| record.tx_count).collect();
            assert_eq!(
                tx_counts,
                vec![1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 2, 3, 4],
                "browser fixture transaction identity"
            );
            let last = records.last().expect("retraction record");
            assert!(!last.asserted, "last record must preserve the retraction");
            assert!(
                matches!(last.value, Value::Ref(_)),
                "retracted edge must retain its Ref value"
            );
        });
    }

    #[test]
    fn native_corruption_contract_matches_shared_corpus_for_both_producers() {
        let corpus = corpus();
        for (producer, source) in [("native", NATIVE_FIXTURE), ("browser", BROWSER_FIXTURE)] {
            for case in &corpus.corruptions {
                let mutated = apply_mutation(source, &case.mutation)
                    .expect("Gate E corruption mutation must apply");
                let dir = tempfile::tempdir().expect("temporary corruption directory");
                let path = dir.path().join(format!("{producer}-{}.graph", case.id));
                std::fs::write(&path, &mutated).expect("write corrupted fixture");
                let opened = Minigraf::open(&path);
                match case.expected.as_str() {
                    "reject" => {
                        assert!(
                            opened.is_err(),
                            "native must reject corruption: {}",
                            case.id
                        );
                        assert_eq!(
                            std::fs::read(&path).expect("read rejected fixture"),
                            mutated,
                            "rejected open must not rewrite source: {}",
                            case.id
                        );
                    }
                    "recover_previous" => {
                        let db = opened.expect("native must recover previous manifest");
                        for query in &corpus.queries {
                            if query.id != "current_retracted_edge_absent" {
                                assert_query(&db, query);
                            }
                        }
                        let probe = case.probe.as_ref().expect("fallback case must carry probe");
                        assert_probe(&db, probe, &case.id);
                        let backup = dir.path().join("recovered.graph");
                        let backup_result = db.backup_to(&backup);
                        if case.exportable {
                            backup_result.expect("complete fallback image must be exportable");
                            let reopened = Minigraf::open(&backup)
                                .expect("native backup of fallback image must reopen");
                            assert_probe(&reopened, probe, &case.id);
                        } else {
                            assert!(
                                backup_result.is_err(),
                                "physically truncated fallback must not overclaim exportability: {}",
                                case.id
                            );
                        }
                    }
                    "recover_latest" => {
                        let legacy_published =
                            published_byte_len(source).expect("legacy published source length");
                        let db = opened.expect("native must ignore unpublished tail");
                        for query in &corpus.queries {
                            assert_query(&db, query);
                        }
                        let backup = dir.path().join("published.graph");
                        db.backup_to(&backup).expect("backup published prefix");
                        let migrated = std::fs::read(&path).expect("read migrated source");
                        assert_eq!(
                            migrated
                                .get(legacy_published..legacy_published + 8)
                                .expect("migration catalog magic must be published"),
                            b"MGPGC001",
                            "v11 catalog must replace, not publish, the legacy tail page"
                        );
                        assert_eq!(
                            std::fs::metadata(&backup).expect("backup metadata").len() as usize,
                            published_byte_len(&migrated)
                                .expect("migrated published source length"),
                            "native backup must exclude unpublished tail"
                        );
                    }
                    other => panic!("unknown corruption expectation: {other}"),
                }
            }
        }
    }
}
