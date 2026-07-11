//! A6 framed pipe session protocol tests.
//!
//! In-process tests drive `Session::run` over byte buffers; the child-process
//! tests spawn the real `minigraf --session` binary and hold ONE session for
//! 10k mixed transact/query round-trips (the A6 gate), plus malformed-input
//! determinism over a real pipe.

#![cfg(not(target_arch = "wasm32"))]

use minigraf::Minigraf;
use minigraf::session::Session;
use serde_json::Value as JVal;

fn run_session(requests: &str) -> Vec<JVal> {
    let db = Minigraf::in_memory().unwrap();
    let mut session = Session::new(db);
    let mut out = Vec::new();
    session.run(requests.as_bytes(), &mut out).unwrap();
    String::from_utf8(out)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).expect("response line must be valid JSON"))
        .collect()
}

#[test]
fn ping_pong_and_shutdown() {
    let responses = run_session("{\"op\":\"ping\"}\n{\"op\":\"shutdown\"}\n");
    assert_eq!(responses.len(), 2, "expected 2 responses");
    assert_eq!(responses[0]["ok"], true);
    assert_eq!(responses[0]["result"]["type"], "pong");
    assert_eq!(responses[1]["result"]["type"], "shutdown");
}

#[test]
fn shutdown_stops_reading_further_requests() {
    let responses = run_session("{\"op\":\"shutdown\"}\n{\"op\":\"ping\"}\n");
    assert_eq!(responses.len(), 1, "no frames after shutdown response");
}

#[test]
fn eof_is_graceful_without_shutdown() {
    let responses = run_session("{\"op\":\"ping\"}\n");
    assert_eq!(responses.len(), 1, "expected 1 response");
    assert_eq!(responses[0]["ok"], true);
}

#[test]
fn transact_reports_tx_and_durability() {
    let responses = run_session(concat!(
        "{\"op\":\"execute\",\"datalog\":\"(transact [[:a :name \\\"x\\\"]])\"}\n",
    ));
    let r = &responses[0];
    assert_eq!(r["ok"], true);
    assert_eq!(r["result"]["type"], "transacted");
    assert_eq!(r["result"]["tx_count"], 1);
    assert_eq!(r["result"]["durability"], "applied");
    assert!(r["result"]["tx_id"].is_u64(), "tx_id must be a number");
}

#[test]
fn forget_reports_count_tx_and_durability() {
    let responses = run_session(concat!(
        "{\"op\":\"execute\",\"datalog\":\"(transact [[:a :name \\\"x\\\"]])\"}\n",
        "{\"op\":\"execute\",\"datalog\":\"(forget [[:a :name \\\"x\\\"]])\"}\n",
        "{\"op\":\"execute\",\"datalog\":\"(forget [[:a :name \\\"x\\\"]])\"}\n",
    ));
    let r = &responses[1];
    assert_eq!(r["ok"], true);
    assert_eq!(r["result"]["type"], "forgotten");
    assert_eq!(r["result"]["forgotten"], 1);
    assert_eq!(r["result"]["tx_count"], 2);
    assert_eq!(r["result"]["durability"], "applied");
    assert!(r["result"]["tx_id"].is_u64(), "tx_id must be a number");

    // Idempotent re-forget: nothing matched, no tx_count consumed, null tx_id.
    let r2 = &responses[2];
    assert_eq!(r2["ok"], true);
    assert_eq!(r2["result"]["type"], "forgotten");
    assert_eq!(r2["result"]["forgotten"], 0);
    assert_eq!(r2["result"]["tx_count"], 2);
    assert!(
        r2["result"]["tx_id"].is_null(),
        "no-op forget has null tx_id"
    );
}

#[test]
fn query_uses_tagged_value_encoding() {
    let responses = run_session(concat!(
        "{\"op\":\"execute\",\"datalog\":\"(transact [[:a :ref #uuid \\\"00000000-0000-0000-0000-000000000001\\\"] [:a :state :status/active] [:a :score 1.5] [:a :n 7]])\"}\n",
        "{\"op\":\"execute\",\"datalog\":\"(query [:find ?v :where [:a :ref ?v]])\"}\n",
        "{\"op\":\"execute\",\"datalog\":\"(query [:find ?v :where [:a :state ?v]])\"}\n",
        "{\"op\":\"execute\",\"datalog\":\"(query [:find ?v :where [:a :score ?v]])\"}\n",
        "{\"op\":\"execute\",\"datalog\":\"(query [:find ?v :where [:a :n ?v]])\"}\n",
    ));
    assert_eq!(responses.len(), 5, "expected 5 responses");
    let row = |i: usize| responses[i]["result"]["results"][0][0].clone();
    assert_eq!(
        row(1)["$ref"],
        "00000000-0000-0000-0000-000000000001",
        "Ref must be tagged"
    );
    assert_eq!(row(2)["$kw"], ":status/active", "Keyword must be tagged");
    assert_eq!(row(3), JVal::from(1.5), "finite float is a plain number");
    assert_eq!(row(4), JVal::from(7), "integer is a plain number");
}

#[test]
fn query_response_carries_variables() {
    let responses = run_session(concat!(
        "{\"op\":\"execute\",\"datalog\":\"(transact [[:a :n 7]])\"}\n",
        "{\"op\":\"execute\",\"datalog\":\"(query [:find ?e ?v :where [?e :n ?v]])\"}\n",
    ));
    let vars = responses[1]["result"]["variables"].as_array().unwrap();
    assert_eq!(vars.len(), 2, "expected 2 variables");
    assert_eq!(responses[1]["result"]["type"], "query");
}

#[test]
fn id_is_echoed_verbatim() {
    let responses = run_session(
        "{\"op\":\"ping\",\"id\":42}\n{\"op\":\"ping\",\"id\":\"abc\"}\n{\"op\":\"ping\"}\n",
    );
    assert_eq!(responses[0]["id"], 42);
    assert_eq!(responses[1]["id"], "abc");
    assert!(responses[2].get("id").is_none(), "no id when not sent");
}

#[test]
fn malformed_input_yields_protocol_error_and_session_survives() {
    let responses = run_session(concat!(
        "this is not json\n",
        "[1,2,3]\n",
        "{\"no\":\"op field\"}\n",
        "{\"op\":\"frobnicate\"}\n",
        "{\"op\":\"execute\"}\n",
        "{\"op\":\"ping\"}\n",
    ));
    assert_eq!(responses.len(), 6, "one deterministic frame per line");
    for r in &responses[..5] {
        assert_eq!(r["ok"], false);
        assert_eq!(r["error"]["kind"], "protocol");
    }
    assert_eq!(responses[5]["result"]["type"], "pong", "session survived");
}

#[test]
fn datalog_parse_error_is_kind_parse() {
    let responses = run_session("{\"op\":\"execute\",\"datalog\":\"(query [:find\"}\n");
    assert_eq!(responses[0]["ok"], false);
    assert_eq!(responses[0]["error"]["kind"], "parse");
}

#[test]
fn execution_error_is_kind_execution_and_session_survives() {
    // Parses fine, fails executor validation: pseudo-attributes require
    // `:valid-at :any-valid-time`.
    let responses = run_session(concat!(
        "{\"op\":\"execute\",\"datalog\":\"(query [:find ?vf :where [:a :db/valid-from ?vf]])\"}\n",
        "{\"op\":\"ping\"}\n",
    ));
    assert_eq!(responses[0]["ok"], false);
    assert_eq!(responses[0]["error"]["kind"], "execution");
    assert_eq!(responses[1]["result"]["type"], "pong");
}

#[test]
fn empty_lines_are_skipped_silently() {
    let responses = run_session("\n   \n{\"op\":\"ping\"}\n\n");
    assert_eq!(responses.len(), 1, "blank lines produce no frames");
}

#[test]
fn status_reports_in_memory_shape() {
    let responses = run_session(concat!(
        "{\"op\":\"execute\",\"datalog\":\"(transact [[:a :n 1] [:b :n 2]])\"}\n",
        "{\"op\":\"status\"}\n",
    ));
    let s = &responses[1]["result"];
    assert_eq!(s["type"], "status");
    assert_eq!(s["fact_count"], 2, "in-memory fact_count is exact");
    assert_eq!(s["pending_facts"], 2);
    assert_eq!(s["tx_count"], 1);
    assert!(s["wal_bytes"].is_null(), "no WAL for in-memory");
    assert!(s["delta_segments"].is_null());
    assert!(s["last_checkpoint_unix_ms"].is_null(), "no checkpoint yet");
}

#[test]
fn checkpoint_then_status_reports_outcome() {
    let responses = run_session(concat!(
        "{\"op\":\"execute\",\"datalog\":\"(transact [[:a :n 1]])\"}\n",
        "{\"op\":\"checkpoint\"}\n",
        "{\"op\":\"status\"}\n",
    ));
    assert_eq!(responses[1]["result"]["type"], "checkpoint");
    assert_eq!(responses[1]["result"]["durability"], "published");
    let s = &responses[2]["result"];
    assert_eq!(s["last_checkpoint_outcome"], "published");
    assert!(
        s["last_checkpoint_unix_ms"].is_u64(),
        "checkpoint time recorded"
    );
}

#[test]
fn maintenance_reports_effects() {
    let responses = run_session("{\"op\":\"maintenance\"}\n");
    let r = &responses[0]["result"];
    assert_eq!(r["type"], "maintenance");
    assert!(r["checkpoint"].is_string());
    assert!(r["delta"].is_string());
    assert!(r["advice"].is_string());
}

// ─── A2: export_since op (frame shape proposed pending caller-lane ACK) ─────

#[test]
fn export_since_returns_tail_records_with_tagged_values() {
    let responses = run_session(concat!(
        "{\"op\":\"execute\",\"datalog\":\"(transact [[:a :name \\\"x\\\"]])\"}\n",
        "{\"op\":\"execute\",\"datalog\":\"(transact [[:a :state :status/active]])\"}\n",
        "{\"op\":\"execute\",\"datalog\":\"(retract [[:a :name \\\"x\\\"]])\"}\n",
        "{\"op\":\"export_since\",\"since_tx_count\":1}\n",
    ));
    let r = &responses[3];
    assert_eq!(r["ok"], true);
    let result = &r["result"];
    assert_eq!(result["type"], "fact_log");
    assert_eq!(result["since_tx_count"], 1);
    assert_eq!(result["head_tx_count"], 3);

    let records = result["records"].as_array().expect("records array");
    assert_eq!(
        records.len(),
        2,
        "tail past tx 1 has keyword tx + retraction"
    );

    let keyword_record = &records[0];
    assert_eq!(keyword_record["tx_count"], 2);
    assert_eq!(keyword_record["asserted"], true);
    assert_eq!(
        keyword_record["value"]["$kw"], ":status/active",
        "keyword values must use the tagged encoding"
    );
    assert!(
        keyword_record["entity"].is_string(),
        "entity is a plain uuid string"
    );
    assert!(
        keyword_record["valid_time"]["valid_to"].is_null(),
        "open-ended valid_to must encode as null, not the i64 sentinel"
    );

    let retraction = &records[1];
    assert_eq!(retraction["tx_count"], 3);
    assert_eq!(retraction["asserted"], false);
    assert_eq!(
        retraction["valid_time"], "all",
        "legacy unscoped retraction is the all-valid-time marker"
    );
}

#[test]
fn export_since_empty_tail_still_reports_head_cursor() {
    let responses = run_session(concat!(
        "{\"op\":\"execute\",\"datalog\":\"(transact [[:a :n 1]])\"}\n",
        "{\"op\":\"export_since\",\"since_tx_count\":99}\n",
    ));
    let result = &responses[1]["result"];
    assert_eq!(result["type"], "fact_log");
    assert_eq!(
        result["records"].as_array().map(Vec::len),
        Some(0),
        "cursor past head yields an empty tail"
    );
    assert_eq!(
        result["head_tx_count"], 1,
        "empty tail must still let the caller advance its cursor"
    );
}

#[test]
fn export_since_missing_field_is_protocol_error_and_session_survives() {
    let responses = run_session(concat!(
        "{\"op\":\"export_since\"}\n",
        "{\"op\":\"export_since\",\"since_tx_count\":-3}\n",
        "{\"op\":\"ping\"}\n",
    ));
    assert_eq!(responses[0]["ok"], false);
    assert_eq!(responses[0]["error"]["kind"], "protocol");
    assert_eq!(
        responses[1]["error"]["kind"], "protocol",
        "negative cursor is rejected as protocol error"
    );
    assert_eq!(responses[2]["result"]["type"], "pong");
}

#[test]
fn backup_field_errors_and_in_memory_rejection_keep_session_alive() {
    let responses = run_session(concat!(
        "{\"op\":\"backup\"}\n",
        "{\"op\":\"backup\",\"destination\":7}\n",
        "{\"op\":\"backup\",\"destination\":\"\"}\n",
        "{\"op\":\"backup\",\"destination\":\"unused.graph\"}\n",
        "{\"op\":\"ping\"}\n",
    ));
    for response in &responses[..3] {
        assert_eq!(response["ok"], false);
        assert_eq!(response["error"]["kind"], "protocol");
    }
    assert_eq!(responses[3]["ok"], false);
    assert_eq!(responses[3]["error"]["kind"], "storage");
    assert_eq!(responses[4]["result"]["type"], "pong");
}

// ─── Child-process tests: the real binary over a real pipe ──────────────────

mod child_process {
    use minigraf::Minigraf;
    use serde_json::Value as JVal;
    use serde_json::json;
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

    struct ChildSession {
        child: Child,
        stdin: ChildStdin,
        stdout: BufReader<ChildStdout>,
    }

    impl ChildSession {
        fn spawn(extra_args: &[&str]) -> Self {
            let mut cmd = Command::new(env!("CARGO_BIN_EXE_minigraf"));
            cmd.arg("--session");
            cmd.args(extra_args);
            let mut child = cmd
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .expect("spawn minigraf --session");
            let stdin = child.stdin.take().unwrap();
            let stdout = BufReader::new(child.stdout.take().unwrap());
            Self {
                child,
                stdin,
                stdout,
            }
        }

        fn round_trip(&mut self, request: &str) -> JVal {
            writeln!(self.stdin, "{request}").expect("write request");
            let mut line = String::new();
            self.stdout.read_line(&mut line).expect("read response");
            serde_json::from_str(line.trim()).expect("response must be valid JSON")
        }

        fn close(mut self) {
            drop(self.stdin); // EOF — graceful close
            let status = self.child.wait().expect("child exit");
            assert!(status.success(), "session child must exit 0 on EOF");
        }
    }

    /// The A6 gate: one external process holds one session open and runs 10k
    /// mixed transact/query round-trips without respawn.
    #[test]
    fn gate_10k_mixed_round_trips_single_session() {
        let mut session = ChildSession::spawn(&[]);
        for i in 0..10_000u32 {
            let response = if i % 2 == 0 {
                session.round_trip(&format!(
                    "{{\"op\":\"execute\",\"datalog\":\"(transact [[:e{i} :n {i}]])\"}}"
                ))
            } else {
                let target = i - 1;
                session.round_trip(&format!(
                    "{{\"op\":\"execute\",\"datalog\":\"(query [:find ?v :where [:e{target} :n ?v]])\"}}"
                ))
            };
            assert_eq!(response["ok"], true, "round trip failed");
            if i % 2 == 1 {
                let rows = response["result"]["results"].as_array().unwrap();
                assert_eq!(rows.len(), 1, "query must see the preceding write");
            }
        }
        let status = session.round_trip("{\"op\":\"status\"}");
        assert_eq!(status["result"]["tx_count"], 5_000);
        session.close();
    }

    /// Deterministic framing under malformed input over a real pipe.
    #[test]
    fn malformed_input_over_real_pipe() {
        let mut session = ChildSession::spawn(&[]);
        let garbage = session.round_trip("}{ definitely not json");
        assert_eq!(garbage["ok"], false);
        assert_eq!(garbage["error"]["kind"], "protocol");
        let pong = session.round_trip("{\"op\":\"ping\"}");
        assert_eq!(pong["result"]["type"], "pong");
        session.close();
    }

    /// File-backed session: durable writes survive the session and the status
    /// op reports file-only fields.
    #[test]
    fn file_backed_session_durability_and_status() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("session.graph");
        let path_str = db_path.to_str().unwrap();

        let mut session = ChildSession::spawn(&["--file", path_str]);
        let write = session.round_trip(
            "{\"op\":\"execute\",\"datalog\":\"(transact [[:a :name \\\"durable\\\"]])\"}",
        );
        assert_eq!(write["ok"], true);
        assert_eq!(write["result"]["durability"], "applied");
        let status = session.round_trip("{\"op\":\"status\"}");
        // Fresh file, nothing committed yet: everything is in memory, so the
        // exact total is still knowable.
        assert_eq!(status["result"]["fact_count"], 1);
        assert!(
            status["result"]["wal_bytes"].is_u64(),
            "WAL exists before checkpoint"
        );
        assert!(status["result"]["delta_segments"].is_u64());
        let checkpoint = session.round_trip("{\"op\":\"checkpoint\"}");
        assert_eq!(checkpoint["result"]["durability"], "published");
        session.close();

        // A second session on the same file sees the write; with committed
        // data on disk the exact total is no longer cheaply knowable.
        let mut reopened = ChildSession::spawn(&["--file", path_str]);
        let read = reopened.round_trip(
            "{\"op\":\"execute\",\"datalog\":\"(query [:find ?v :where [:a :name ?v]])\"}",
        );
        assert_eq!(read["result"]["results"][0][0], "durable");
        let status = reopened.round_trip("{\"op\":\"status\"}");
        assert!(
            status["result"]["fact_count"].is_null(),
            "committed data on disk: total unknown without full scan"
        );
        assert_eq!(status["result"]["pending_facts"], 0);
        reopened.close();
    }

    #[test]
    fn live_session_backup_is_published_at_exact_watermark() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("live source.graph");
        let backup_path = dir.path().join("rollback point.graph");
        let mut session = ChildSession::spawn(&["--file", db_path.to_str().unwrap()]);

        let first = session
            .round_trip("{\"op\":\"execute\",\"datalog\":\"(transact [[:before :value 1]])\"}");
        assert_eq!(first["result"]["tx_count"], 1);
        let request_id = json!({"kind": "rollback", "sequence": 1});
        let backup_request = json!({
            "op": "backup",
            "destination": backup_path,
            "id": request_id.clone(),
        });
        let backup = session.round_trip(&backup_request.to_string());
        assert_eq!(backup["ok"], true);
        assert_eq!(backup["id"], request_id);
        assert_eq!(backup["result"]["type"], "backup");
        assert_eq!(
            backup["result"]["destination"],
            backup_path.to_str().unwrap()
        );
        assert_eq!(backup["result"]["tx_count"], 1);
        assert_eq!(backup["result"]["durability"], "published");
        assert!(
            backup["result"]["bytes"].as_u64().unwrap() >= 4096,
            "backup response must report a complete header page"
        );

        {
            let snapshot = Minigraf::open(&backup_path).unwrap();
            assert_eq!(snapshot.current_tx_count(), 1);
            assert_eq!(snapshot.export_fact_log().unwrap().len(), 1);
        }

        let second = session
            .round_trip("{\"op\":\"execute\",\"datalog\":\"(transact [[:after :value 2]])\"}");
        assert_eq!(second["result"]["tx_count"], 2);
        let source_read = session.round_trip(
            "{\"op\":\"execute\",\"datalog\":\"(query [:find ?v :where [:after :value ?v]])\"}",
        );
        assert_eq!(
            source_read["result"]["results"].as_array().unwrap().len(),
            1
        );
        let status = session.round_trip("{\"op\":\"status\"}");
        assert_eq!(status["result"]["last_checkpoint_outcome"], "published");
        assert!(status["result"]["last_checkpoint_unix_ms"].is_u64());

        {
            let snapshot = Minigraf::open(&backup_path).unwrap();
            assert_eq!(snapshot.current_tx_count(), 1);
            assert_eq!(snapshot.export_fact_log().unwrap().len(), 1);
        }
        session.close();
    }

    #[test]
    fn backup_storage_errors_preserve_targets_and_session() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("source.graph");
        let mut session = ChildSession::spawn(&["--file", db_path.to_str().unwrap()]);
        let write = session
            .round_trip("{\"op\":\"execute\",\"datalog\":\"(transact [[:safe :value 1]])\"}");
        assert_eq!(write["ok"], true);

        let existing = dir.path().join("existing.graph");
        std::fs::write(&existing, b"sentinel").unwrap();
        let existing_response =
            session.round_trip(&json!({"op": "backup", "destination": existing}).to_string());
        assert_eq!(existing_response["error"]["kind"], "storage");
        assert_eq!(std::fs::read(&existing).unwrap(), b"sentinel");

        let stale_target = dir.path().join("stale.graph");
        let mut stale_wal_name = stale_target.file_name().unwrap().to_os_string();
        stale_wal_name.push(".wal");
        let stale_wal = stale_target.with_file_name(stale_wal_name);
        std::fs::write(&stale_wal, b"unrelated").unwrap();
        let stale_response =
            session.round_trip(&json!({"op": "backup", "destination": stale_target}).to_string());
        assert_eq!(stale_response["error"]["kind"], "storage");
        assert!(!stale_target.exists());
        assert_eq!(std::fs::read(&stale_wal).unwrap(), b"unrelated");

        let missing = dir.path().join("missing-parent").join("backup.graph");
        let missing_response =
            session.round_trip(&json!({"op": "backup", "destination": missing}).to_string());
        assert_eq!(missing_response["error"]["kind"], "storage");
        assert!(!missing.exists());

        let source_response =
            session.round_trip(&json!({"op": "backup", "destination": db_path}).to_string());
        assert_eq!(source_response["error"]["kind"], "storage");
        let read = session.round_trip(
            "{\"op\":\"execute\",\"datalog\":\"(query [:find ?v :where [:safe :value ?v]])\"}",
        );
        assert_eq!(read["result"]["results"].as_array().unwrap().len(), 1);
        let pong = session.round_trip("{\"op\":\"ping\"}");
        assert_eq!(pong["result"]["type"], "pong");
        session.close();
    }
}
