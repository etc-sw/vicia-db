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
        row(1)["$ref"], "00000000-0000-0000-0000-000000000001",
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
    let responses = run_session("{\"op\":\"ping\",\"id\":42}\n{\"op\":\"ping\",\"id\":\"abc\"}\n{\"op\":\"ping\"}\n");
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
    assert!(s["last_checkpoint_unix_ms"].is_u64(), "checkpoint time recorded");
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

// ─── Child-process tests: the real binary over a real pipe ──────────────────

mod child_process {
    use serde_json::Value as JVal;
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
            Self { child, stdin, stdout }
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
        assert!(status["result"]["wal_bytes"].is_u64(), "WAL exists before checkpoint");
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
}
