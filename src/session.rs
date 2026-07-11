//! A6 framed pipe session mode — NDJSON request/response protocol for a
//! caller-owned child process.
//!
//! One JSON object per line (UTF-8, LF) in each direction. The caller owns
//! the child's lifecycle: stdin EOF (or the `shutdown` op) ends the session
//! gracefully with no implicit checkpoint — WAL replay on next open is the
//! durability story. Malformed input produces an error frame and the session
//! continues; framing resynchronizes at the next newline.
//!
//! Design frozen 2026-07-11 by dual caller-lane ACK — see
//! `docs/A6_SESSION_PROTOCOL_QUESTIONS.md` (status block) and
//! `docs/SESSION_PROTOCOL.md` for the frame reference.

#![cfg(not(target_arch = "wasm32"))]

use crate::db::Minigraf;
use crate::graph::types::{FactRecord, FactValidTime, VALID_TIME_FOREVER};
use crate::json_value::to_tagged_json;
use crate::query::datalog::executor::QueryResult;
use crate::query::datalog::parser::parse_datalog_command;
use serde_json::{Value as JVal, json};
use std::io::{BufRead, Write};

/// A single caller-owned protocol session over any line-based transport.
pub struct Session {
    db: Minigraf,
    last_checkpoint_unix_ms: Option<i64>,
    last_checkpoint_outcome: Option<&'static str>,
}

impl Session {
    /// Wrap an open database in a protocol session.
    pub fn new(db: Minigraf) -> Self {
        Self {
            db,
            last_checkpoint_unix_ms: None,
            last_checkpoint_outcome: None,
        }
    }

    /// Run the request/response loop until stdin EOF, a `shutdown` op, or an
    /// unrecoverable storage/transport error.
    ///
    /// # Errors
    ///
    /// Returns an error only for transport failures (broken pipe) — protocol
    /// and execution errors are reported in-band as error frames.
    pub fn run(&mut self, mut reader: impl BufRead, mut writer: impl Write) -> anyhow::Result<()> {
        let mut line = String::new();
        loop {
            line.clear();
            if reader.read_line(&mut line)? == 0 {
                return Ok(()); // EOF — graceful close, no implicit checkpoint
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            let request: JVal = match serde_json::from_str(trimmed) {
                Ok(v) => v,
                Err(e) => {
                    write_error(
                        &mut writer,
                        JVal::Null,
                        "protocol",
                        &format!("invalid JSON: {e}"),
                    )?;
                    continue;
                }
            };
            let Some(obj) = request.as_object() else {
                write_error(
                    &mut writer,
                    JVal::Null,
                    "protocol",
                    "request must be a JSON object",
                )?;
                continue;
            };
            let id = obj.get("id").cloned().unwrap_or(JVal::Null);
            let Some(op) = obj.get("op").and_then(JVal::as_str) else {
                write_error(&mut writer, id, "protocol", "missing string field \"op\"")?;
                continue;
            };

            match op {
                "execute" => self.op_execute(obj, id, &mut writer)?,
                "status" => self.op_status(id, &mut writer)?,
                "checkpoint" => self.op_checkpoint(id, &mut writer)?,
                "maintenance" => self.op_maintenance(id, &mut writer)?,
                "backup" => self.op_backup(obj, id, &mut writer)?,
                "export_since" => self.op_export_since(obj, id, &mut writer)?,
                "ping" => write_ok(&mut writer, id, json!({"type": "pong"}))?,
                "shutdown" => {
                    write_ok(&mut writer, id, json!({"type": "shutdown"}))?;
                    return Ok(());
                }
                other => {
                    write_error(
                        &mut writer,
                        id,
                        "protocol",
                        &format!("unknown op {other:?}"),
                    )?;
                }
            }
        }
    }

    fn op_execute(
        &mut self,
        obj: &serde_json::Map<String, JVal>,
        id: JVal,
        writer: &mut impl Write,
    ) -> anyhow::Result<()> {
        let Some(datalog) = obj.get("datalog").and_then(JVal::as_str) else {
            return write_error(
                writer,
                id,
                "protocol",
                "execute requires string field \"datalog\"",
            );
        };
        // Classify parse failures separately from execution failures. The
        // command is re-parsed inside `execute`; the duplicate parse is
        // microseconds against millisecond-scale ops.
        if let Err(e) = parse_datalog_command(datalog) {
            return write_error(writer, id, "parse", &e);
        }
        match self.db.execute(datalog) {
            Ok(QueryResult::Transacted(tx_id)) => {
                let body = self.write_result_body("transacted", tx_id);
                write_ok(writer, id, body)
            }
            Ok(QueryResult::Retracted(tx_id)) => {
                let body = self.write_result_body("retracted", tx_id);
                write_ok(writer, id, body)
            }
            Ok(QueryResult::Forgotten { tx_id, count }) => {
                let mut body = match tx_id {
                    Some(tx_id) => self.write_result_body("forgotten", tx_id),
                    // Nothing matched: no tx_count consumed, no WAL entry —
                    // vacuously durable.
                    None => json!({
                        "type": "forgotten",
                        "tx_id": JVal::Null,
                        "tx_count": self.db.current_tx_count(),
                        "durability": "applied",
                    }),
                };
                if let Some(obj) = body.as_object_mut() {
                    obj.insert("forgotten".to_string(), json!(count));
                }
                write_ok(writer, id, body)
            }
            Ok(QueryResult::QueryResults { vars, results }) => {
                let rows: Vec<Vec<JVal>> = results
                    .iter()
                    .map(|row| row.iter().map(to_tagged_json).collect())
                    .collect();
                write_ok(
                    writer,
                    id,
                    json!({"type": "query", "variables": vars, "results": rows}),
                )
            }
            Ok(QueryResult::Ok) => write_ok(writer, id, json!({"type": "ok"})),
            Err(e) => write_error(writer, id, "execution", &e.to_string()),
        }
    }

    fn write_result_body(&self, kind: &str, tx_id: u64) -> JVal {
        let durability = if self.db.maintenance_advised() {
            "maintenance_pending"
        } else {
            "applied"
        };
        json!({
            "type": kind,
            "tx_id": tx_id,
            "tx_count": self.db.current_tx_count(),
            "durability": durability,
        })
    }

    fn op_status(&self, id: JVal, writer: &mut impl Write) -> anyhow::Result<()> {
        match self.db.session_status() {
            Ok(s) => write_ok(
                writer,
                id,
                json!({
                    "type": "status",
                    "fact_count": s.fact_count,
                    "pending_facts": s.pending_facts,
                    "tx_count": s.tx_count,
                    "wal_bytes": s.wal_bytes,
                    "delta_segments": s.delta_segments,
                    "delta_pages": s.delta_pages,
                    "last_checkpoint_unix_ms": self.last_checkpoint_unix_ms,
                    "last_checkpoint_outcome": self.last_checkpoint_outcome,
                }),
            ),
            Err(e) => write_error(writer, id, "storage", &e.to_string()),
        }
    }

    fn op_checkpoint(&mut self, id: JVal, writer: &mut impl Write) -> anyhow::Result<()> {
        match self.db.checkpoint() {
            Ok(()) => {
                self.record_checkpoint("published");
                write_ok(
                    writer,
                    id,
                    json!({"type": "checkpoint", "durability": "published"}),
                )
            }
            Err(e) => write_error(writer, id, "storage", &e.to_string()),
        }
    }

    fn op_maintenance(&mut self, id: JVal, writer: &mut impl Write) -> anyhow::Result<()> {
        use crate::db::{MaintenanceAdvice, MaintenanceCheckpointEffect, MaintenanceDeltaEffect};
        match self.db.run_idle_maintenance() {
            Ok(outcome) => {
                let checkpoint = match outcome.checkpoint {
                    MaintenanceCheckpointEffect::Published => "published",
                    _ => "noop",
                };
                if checkpoint == "published" {
                    self.record_checkpoint("published");
                }
                let delta = match outcome.delta {
                    MaintenanceDeltaEffect::Recompacted => "recompacted",
                    _ => "noop",
                };
                let advice = match outcome.advice {
                    MaintenanceAdvice::ReduceCheckpointCadence => "reduce_checkpoint_cadence",
                    _ => "none",
                };
                write_ok(
                    writer,
                    id,
                    json!({"type": "maintenance", "checkpoint": checkpoint, "delta": delta, "advice": advice}),
                )
            }
            Err(e) => write_error(writer, id, "storage", &e.to_string()),
        }
    }

    fn op_backup(
        &mut self,
        obj: &serde_json::Map<String, JVal>,
        id: JVal,
        writer: &mut impl Write,
    ) -> anyhow::Result<()> {
        let Some(destination) = obj.get("destination").and_then(JVal::as_str) else {
            return write_error(
                writer,
                id,
                "protocol",
                "backup requires non-empty string field \"destination\"",
            );
        };
        if destination.is_empty() {
            return write_error(
                writer,
                id,
                "protocol",
                "backup requires non-empty string field \"destination\"",
            );
        }

        match self.db.backup_to(destination) {
            Ok(outcome) => {
                self.record_checkpoint("published");
                write_ok(
                    writer,
                    id,
                    json!({
                        "type": "backup",
                        "destination": destination,
                        "tx_count": outcome.tx_count,
                        "bytes": outcome.bytes,
                        "durability": "published",
                    }),
                )
            }
            Err(error) => write_error(writer, id, "storage", &error.to_string()),
        }
    }

    /// A2 incremental fact-log read: every record with `tx_count > since`.
    ///
    /// Frame shape is proposed pending caller-lane ACK (A6 precedent) — see
    /// `docs/SESSION_PROTOCOL.md` "export_since". `head_tx_count` is returned
    /// so an empty tail still advances the caller's stored cursor.
    fn op_export_since(
        &self,
        obj: &serde_json::Map<String, JVal>,
        id: JVal,
        writer: &mut impl Write,
    ) -> anyhow::Result<()> {
        let Some(since) = obj.get("since_tx_count").and_then(JVal::as_u64) else {
            return write_error(
                writer,
                id,
                "protocol",
                "export_since requires unsigned integer field \"since_tx_count\"",
            );
        };
        match self.db.export_fact_log_since(since) {
            Ok(records) => {
                let rows: Vec<JVal> = records.iter().map(fact_record_to_json).collect();
                write_ok(
                    writer,
                    id,
                    json!({
                        "type": "fact_log",
                        "since_tx_count": since,
                        "head_tx_count": self.db.current_tx_count(),
                        "records": rows,
                    }),
                )
            }
            Err(e) => write_error(writer, id, "storage", &e.to_string()),
        }
    }

    fn record_checkpoint(&mut self, outcome: &'static str) {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
            .unwrap_or(0);
        self.last_checkpoint_unix_ms = Some(now_ms);
        self.last_checkpoint_outcome = Some(outcome);
    }
}

/// Encode one fact-log record for the `export_since` response.
///
/// `entity` is a plain UUID string (an `EntityId` is always a UUID — no type
/// ambiguity, so the `$ref` tag is unnecessary); `value` uses the tagged
/// encoding. `valid_to: null` means open-ended (`VALID_TIME_FOREVER` does not
/// survive an f64 round-trip, so the sentinel never crosses the wire);
/// `"valid_time": "all"` is the legacy all-valid-time retraction marker.
fn fact_record_to_json(record: &FactRecord) -> JVal {
    let valid_time = match record.valid_time {
        FactValidTime::AllValidTime => json!("all"),
        FactValidTime::Window {
            valid_from,
            valid_to,
        } => json!({
            "valid_from": valid_from,
            "valid_to": if valid_to == VALID_TIME_FOREVER {
                JVal::Null
            } else {
                json!(valid_to)
            },
        }),
    };
    json!({
        "entity": record.entity.to_string(),
        "attribute": record.attribute,
        "value": to_tagged_json(&record.value),
        "tx_id": record.tx_id,
        "tx_count": record.tx_count,
        "valid_time": valid_time,
        "asserted": record.asserted,
    })
}

fn write_ok(writer: &mut impl Write, id: JVal, result: JVal) -> anyhow::Result<()> {
    let mut frame = json!({"ok": true, "result": result});
    if !id.is_null() {
        frame
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("internal error: response frame is not an object"))?
            .insert("id".to_string(), id);
    }
    writeln!(writer, "{frame}")?;
    writer.flush()?;
    Ok(())
}

fn write_error(writer: &mut impl Write, id: JVal, kind: &str, message: &str) -> anyhow::Result<()> {
    let mut frame = json!({"ok": false, "error": {"kind": kind, "message": message}});
    if !id.is_null() {
        frame
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("internal error: error frame is not an object"))?
            .insert("id".to_string(), id);
    }
    writeln!(writer, "{frame}")?;
    writer.flush()?;
    Ok(())
}
