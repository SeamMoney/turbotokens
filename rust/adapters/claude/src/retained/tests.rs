use super::*;
use turbotokens_test_support::Fixture;

fn args(fixture: &Fixture) -> RetainedClaudeArgs {
    let root = fixture.path("claude");
    fs::create_dir_all(root.join("projects/project")).unwrap();
    RetainedClaudeArgs {
        store: fixture.path("retained"),
        scope: "test-account/publisher".to_string(),
        roots: vec![root],
        initialize: true,
        summary: false,
        since: None,
        until: None,
    }
}

fn row(id: &str, input: u64) -> Value {
    json!({"type":"assistant","timestamp":"2026-10-01T00:00:00Z","sessionId":"session-stable","requestId":format!("request-{id}"),"version":"2.1.0","message":{"id":id,"model":"claude-sonnet-4-5","usage":{"input_tokens":input,"output_tokens":0,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}})
}

fn write(args: &RetainedClaudeArgs, name: &str, values: &[Value]) -> PathBuf {
    let path = args.roots[0].join("projects/project").join(name);
    let bytes = values
        .iter()
        .map(|row| format!("{row}\n"))
        .collect::<String>();
    fs::write(&path, bytes).unwrap();
    path
}

fn count(receipt: &Value) -> u64 {
    receipt["totals"]["totalTokens"].as_u64().unwrap()
}
fn bytes(args: &RetainedClaudeArgs) -> Vec<u8> {
    fs::read(args.store.join("ledger.json")).unwrap()
}
/// A conflicting copy keeps the admitted observation, is counted, and leaves
/// one private diagnostic naming the reason.
fn conflicted(args: &RetainedClaudeArgs, reason: &str) -> Value {
    let receipt = scan(args).unwrap();
    assert_eq!(receipt["conflictingRows"], 1);
    let diagnostic: Value =
        serde_json::from_slice(&fs::read(args.store.join("last-rejection.json")).unwrap()).unwrap();
    assert!(
        diagnostic["reason"].as_str().unwrap().contains(reason),
        "{diagnostic}"
    );
    receipt
}
fn enroll(args: &mut RetainedClaudeArgs) -> Value {
    let value = scan(args).unwrap();
    args.initialize = false;
    value
}

#[test]
fn delete_grow_restore_does_not_replay_old_responses() {
    let fixture = Fixture::new();
    let mut a = args(&fixture);
    let old = write(&a, "old.jsonl", &[row("old", 1000)]);
    write(&a, "current.jsonl", &[row("current", 100)]);
    assert_eq!(count(&enroll(&mut a)), 1100);
    fs::remove_file(&old).unwrap();
    let lower = scan(&a).unwrap();
    assert_eq!(count(&lower), 1100);
    assert_eq!(lower["currentTotals"]["totalTokens"], 100);
    write(&a, "current.jsonl", &[row("current", 100), row("new", 50)]);
    assert_eq!(count(&scan(&a).unwrap()), 1150);
    write(&a, "restored-under-new-name.jsonl", &[row("old", 1000)]);
    assert_eq!(count(&scan(&a).unwrap()), 1150);
}

#[test]
fn offline_deletion_and_new_work_are_counted_on_restart() {
    let fixture = Fixture::new();
    let mut a = args(&fixture);
    let old = write(&a, "old.jsonl", &[row("old", 1000)]);
    write(&a, "current.jsonl", &[row("current", 100)]);
    enroll(&mut a);
    fs::remove_file(old).unwrap();
    write(
        &a,
        "current.jsonl",
        &[row("current", 100), row("offline-new", 50)],
    );
    assert_eq!(count(&scan(&a.clone()).unwrap()), 1150);
    write(&a, "later.jsonl", &[row("later-new", 50)]);
    assert_eq!(count(&scan(&a.clone()).unwrap()), 1200);
}

#[test]
fn equal_counts_with_new_identity_are_real_new_work() {
    let fixture = Fixture::new();
    let mut a = args(&fixture);
    let old = write(&a, "old.jsonl", &[row("old", 1000)]);
    enroll(&mut a);
    fs::remove_file(old).unwrap();
    write(&a, "new.jsonl", &[row("new", 1000)]);
    assert_eq!(count(&scan(&a).unwrap()), 2000);
}

#[test]
fn growing_response_revisions_keep_original_day_and_exact_vector() {
    let fixture = Fixture::new();
    let mut a = args(&fixture);
    write(&a, "one.jsonl", &[row("response", 100)]);
    enroll(&mut a);
    let mut revised = row("response", 150);
    revised["timestamp"] = json!("2026-10-02T00:00:00Z");
    write(&a, "one.jsonl", &[revised]);
    let receipt = scan(&a).unwrap();
    assert_eq!(count(&receipt), 150);
    assert_eq!(receipt["daily"][0]["date"], "2026-10-01");
    assert_eq!(receipt["retainedIdentityCount"], 1);
    write(&a, "one.jsonl", &[row("response", 100)]);
    assert_eq!(count(&scan(&a).unwrap()), 150);
}

#[test]
fn incomparable_and_model_corrections_keep_the_admitted_observation() {
    let fixture = Fixture::new();
    let mut a = args(&fixture);
    write(&a, "one.jsonl", &[row("response", 100)]);
    enroll(&mut a);
    let mut revised = row("response", 90);
    revised["message"]["usage"]["output_tokens"] = json!(20);
    write(&a, "one.jsonl", &[revised]);
    let receipt = conflicted(&a, "incomparable");
    assert_eq!(count(&receipt), 100);
    assert_eq!(receipt["retainedIdentityCount"], 1);
    // A relabelled copy (seen on real history: same response, other model).
    let mut wrong_model = row("response", 100);
    wrong_model["message"]["model"] = json!("claude-opus-4-5");
    write(&a, "one.jsonl", &[wrong_model, row("later", 7)]);
    let receipt = conflicted(&a, "model or speed");
    assert_eq!(count(&receipt), 107);
    let ledger: Ledger = serde_json::from_slice(&bytes(&a)).unwrap();
    let kept = ledger
        .observations
        .iter()
        .find(|o| o.raw.message_id.as_deref() == Some("response"))
        .unwrap();
    assert_eq!(kept.raw.model.as_deref(), Some("claude-sonnet-4-5"));
}

#[test]
fn rename_copy_sidechain_and_advisors_share_original_identity() {
    let fixture = Fixture::new();
    let mut a = args(&fixture);
    let mut parent = row("parent", 100);
    parent["message"]["usage"]["iterations"] = json!([{"type":"advisor_message","model":"claude-sonnet-4-5","input_tokens":20,"output_tokens":5}]);
    let original = write(&a, "one.jsonl", &[parent.clone()]);
    let first = enroll(&mut a);
    assert_eq!(count(&first), 125);
    fs::rename(original, a.roots[0].join("projects/project/renamed.jsonl")).unwrap();
    write(&a, "copy.jsonl", &[parent.clone()]);
    parent["isSidechain"] = json!(true);
    parent["requestId"] = json!("sidechain-request");
    parent["sessionId"] = json!("copied-session");
    parent["timestamp"] = json!("2026-10-03T00:00:00Z");
    write(&a, "fork.jsonl", &[parent]);
    let receipt = scan(&a).unwrap();
    assert_eq!(count(&receipt), 125);
    assert_eq!(receipt["retainedIdentityCount"], 1);
    assert_eq!(receipt["sessions"].as_array().unwrap().len(), 1);
    assert_eq!(receipt["sessions"][0]["sessionId"], "session-stable");
}

#[test]
fn sidechain_parent_lowering_is_an_explicit_conflict() {
    let fixture = Fixture::new();
    let mut a = args(&fixture);
    let mut side = row("response", 100);
    side["isSidechain"] = json!(true);
    write(&a, "one.jsonl", &[side]);
    enroll(&mut a);
    write(&a, "one.jsonl", &[row("response", 80)]);
    assert_eq!(count(&conflicted(&a, "sidechain")), 100);
}

#[test]
fn distinct_parent_request_after_sidechain_copy_is_not_merged() {
    let fixture = Fixture::new();
    let mut a = args(&fixture);
    let parent = row("same-message", 100);
    let mut side = parent.clone();
    side["isSidechain"] = json!(true);
    side["requestId"] = json!("copied-request");
    write(&a, "one.jsonl", &[parent.clone(), side]);
    enroll(&mut a);
    let mut distinct = parent;
    distinct["requestId"] = json!("distinct-parent-request");
    write(&a, "two.jsonl", &[distinct]);
    // A sidechain copy cannot collapse two distinct parent requests.
    assert_eq!(count(&scan(&a).unwrap()), 200);
}

#[test]
fn sidechain_alias_cannot_silently_become_a_second_parent_identity() {
    let fixture = Fixture::new();
    let mut a = args(&fixture);
    let parent = row("same-message", 100);
    let mut side = parent.clone();
    side["isSidechain"] = json!(true);
    side["requestId"] = json!("request-B");
    write(&a, "one.jsonl", &[parent, side.clone()]);
    let enrolled = enroll(&mut a);
    side["isSidechain"] = json!(false);
    write(&a, "one.jsonl", &[side]);
    let receipt = conflicted(&a, "distinct parent request");
    assert_eq!(count(&receipt), count(&enrolled));
    assert_eq!(
        receipt["retainedIdentityCount"],
        enrolled["retainedIdentityCount"]
    );
    let diagnostic: Value =
        serde_json::from_slice(&fs::read(a.store.join("last-rejection.json")).unwrap()).unwrap();
    assert_eq!(diagnostic["candidate"]["request_id"], "request-B");
}

#[test]
fn summary_receipt_omits_identity_bearing_report_sections() {
    let fixture = Fixture::new();
    let mut a = args(&fixture);
    write(&a, "one.jsonl", &[row("private-response", 100)]);
    a.summary = true;
    let receipt = enroll(&mut a);
    assert_eq!(count(&receipt), 100);
    for key in ["daily", "monthly", "sessions"] {
        assert!(receipt.get(key).is_none());
    }
    let output = receipt.to_string();
    assert!(!output.contains("private-response"));
    assert!(!output.contains("session-stable"));
    assert!(!output.contains(a.roots[0].to_string_lossy().as_ref()));
}

#[test]
fn request_metadata_and_cache_ttl_corrections_do_not_create_new_usage() {
    let fixture = Fixture::new();
    let mut a = args(&fixture);
    let mut original = row("response", 100);
    original.as_object_mut().unwrap().remove("requestId");
    write(&a, "one.jsonl", &[original]);
    enroll(&mut a);
    write(&a, "one.jsonl", &[row("response", 100)]);
    let receipt = conflicted(&a, "missing request");
    assert_eq!(count(&receipt), 100);
    assert_eq!(receipt["retainedIdentityCount"], 1);

    let fixture = Fixture::new();
    let mut a = args(&fixture);
    let mut original = row("response", 100);
    original["message"]["usage"]["cache_creation"] =
        json!({"ephemeral_5m_input_tokens":0,"ephemeral_1h_input_tokens":100});
    write(&a, "one.jsonl", &[original.clone()]);
    enroll(&mut a);
    original["message"]["usage"]["cache_creation"] =
        json!({"ephemeral_5m_input_tokens":100,"ephemeral_1h_input_tokens":0});
    write(&a, "one.jsonl", &[original, row("new", 10000)]);
    // The TTL correction is not new usage; the unrelated new response still is.
    assert_eq!(count(&conflicted(&a, "incomparable")), 200 + 10000);
}

#[test]
fn all_report_views_share_the_filtered_set_and_filters_do_not_trim_store() {
    let fixture = Fixture::new();
    let mut a = args(&fixture);
    let mut other_day = row("second", 75);
    other_day["timestamp"] = json!("2026-10-02T12:00:00Z");
    write(&a, "one.jsonl", &[row("first", 100), other_day]);
    a.since = Some("20261002".to_string());
    let receipt = enroll(&mut a);
    assert_eq!(count(&receipt), 75);
    assert_eq!(receipt["allTimeTotals"]["totalTokens"], 175);
    for key in ["daily", "monthly", "sessions"] {
        assert_eq!(
            receipt[key]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r["totalTokens"].as_u64().unwrap())
                .sum::<u64>(),
            75
        );
    }
    fs::remove_dir_all(&a.roots[0]).unwrap();
    a.since = None;
    let absent = scan(&a).unwrap();
    assert_eq!(count(&absent), 175);
    assert_eq!(absent["missingRoots"], 1);
}

#[test]
fn missing_corrupt_and_reinitialized_state_never_reset_silently() {
    let fixture = Fixture::new();
    let mut a = args(&fixture);
    write(&a, "one.jsonl", &[row("first", 100)]);
    enroll(&mut a);
    a.initialize = true;
    assert!(scan(&a).is_err());
    a.initialize = false;
    fs::write(a.store.join("ledger.json"), b"{broken").unwrap();
    assert!(scan(&a).unwrap_err().to_string().contains("corrupt"));
    assert_eq!(bytes(&a), b"{broken");
    fs::remove_file(a.store.join("ledger.json")).unwrap();
    assert!(scan(&a).is_err());
    a.initialize = true;
    assert!(scan(&a).is_err());
}

#[test]
fn initial_source_failure_preserves_enrollment_for_ordinary_retry() {
    let fixture = Fixture::new();
    let mut a = args(&fixture);
    let path = write(&a, "one.jsonl", &[row("first", 100)]);
    // Unreadable rows are counted, so fail the source tree itself.
    let link = path.with_file_name("link.jsonl");
    std::os::unix::fs::symlink(&path, &link).unwrap();
    assert!(scan(&a).is_err());
    let enrolled: Ledger = serde_json::from_slice(&bytes(&a)).unwrap();
    assert_eq!(enrolled.generation, 0);
    assert!(enrolled.observations.is_empty());
    assert!(scan(&a).is_err());
    fs::remove_file(&link).unwrap();
    a.initialize = false;
    let receipt = scan(&a).unwrap();
    assert_eq!(count(&receipt), 100);
    assert_eq!(receipt["generation"], 1);
    assert_eq!(receipt["coverageStart"], enrolled.coverage_start);
}

#[test]
fn roots_scope_and_pricing_changes_fail_without_commit() {
    let fixture = Fixture::new();
    let mut a = args(&fixture);
    write(&a, "one.jsonl", &[row("first", 100)]);
    enroll(&mut a);
    let committed = bytes(&a);
    let mut changed = a.clone();
    changed.roots.push(fixture.path("missing-root"));
    assert!(scan(&changed).is_err());
    changed = a.clone();
    changed.scope.push_str("different-account");
    assert!(scan(&changed).is_err());
    assert_eq!(bytes(&a), committed);
    let mut ledger: Value = serde_json::from_slice(&committed).unwrap();
    ledger["pricingIdentity"][0] = json!("wrong");
    fs::write(
        a.store.join("ledger.json"),
        serde_json::to_vec(&ledger).unwrap(),
    )
    .unwrap();
    assert!(scan(&a).unwrap_err().to_string().contains("pricing"));
}

#[test]
fn partial_tail_waits_for_newline_and_unsupported_complete_rows_are_counted() {
    let fixture = Fixture::new();
    let mut a = args(&fixture);
    let path = write(&a, "one.jsonl", &[row("first", 100)]);
    enroll(&mut a);
    fs::write(&path, format!("{}\n{}", row("first", 100), row("new", 50))).unwrap();
    let pending = scan(&a).unwrap();
    assert_eq!(count(&pending), 100);
    assert_eq!(pending["pendingTailFiles"], 1);
    write(&a, "one.jsonl", &[row("first", 100), row("new", 50)]);
    assert_eq!(count(&scan(&a).unwrap()), 150);
    let mut missing_id = row("third", 20);
    missing_id["message"].as_object_mut().unwrap().remove("id");
    write(&a, "one.jsonl", &[missing_id, row("fourth", 5)]);
    let receipt = scan(&a).unwrap();
    assert_eq!(receipt["unsupportedRows"], 1);
    assert_eq!(count(&receipt), 155);
    assert_eq!(receipt["retainedIdentityCount"], 3);
    fs::write(&path, b"{not-json}\n").unwrap();
    let receipt = scan(&a).unwrap();
    assert_eq!(receipt["unsupportedRows"], 1);
    assert_eq!(count(&receipt), 155);
}

#[test]
fn malformed_iterations_cannot_hide_advisor_usage() {
    let fixture = Fixture::new();
    let mut a = args(&fixture);
    write(&a, "one.jsonl", &[row("first", 100)]);
    enroll(&mut a);
    for iterations in [
        json!({"type":"advisor_message","model":"claude-sonnet-4-5","input_tokens":20,"output_tokens":5}),
        Value::Null,
        json!([{"type":"advisor_message","model":"claude-sonnet-4-5","input_tokens":"bad","output_tokens":5}]),
    ] {
        let mut malformed = row("second", 100);
        malformed["message"]["usage"]["iterations"] = iterations;
        write(&a, "one.jsonl", &[malformed]);
        let receipt = scan(&a).unwrap();
        assert_eq!(receipt["unsupportedRows"], 1);
        assert_eq!(count(&receipt), 100);
        assert_eq!(receipt["retainedIdentityCount"], 1);
    }
}

#[test]
fn observed_synthetic_api_error_with_explicit_zero_usage_is_ignored() {
    let fixture = Fixture::new();
    let mut a = args(&fixture);
    let mut synthetic = row("error-response", 0);
    synthetic["message"]["role"] = json!("assistant");
    synthetic["message"]["model"] = json!("<synthetic>");
    synthetic["version"] = json!("2.1.267");
    synthetic["isApiErrorMessage"] = json!(true);
    synthetic.as_object_mut().unwrap().remove("requestId");
    synthetic["message"]["usage"]["speed"] = Value::Null;
    synthetic["message"]["usage"]["iterations"] = Value::Null;
    synthetic["message"]["usage"]["cache_creation"] =
        json!({"ephemeral_5m_input_tokens":0,"ephemeral_1h_input_tokens":0});
    write(&a, "one.jsonl", &[synthetic.clone(), row("actual", 100)]);
    let receipt = enroll(&mut a);
    assert_eq!(count(&receipt), 100);
    assert_eq!(receipt["retainedIdentityCount"], 1);
    assert_eq!(receipt["unsupportedRows"], 0);
    // Not provably zero any more: never retained, but always reported.
    synthetic["message"]["usage"]["input_tokens"] = json!(1);
    write(&a, "one.jsonl", &[synthetic.clone()]);
    let receipt = scan(&a).unwrap();
    assert_eq!(receipt["unsupportedRows"], 1);
    assert_eq!(count(&receipt), 100);
    synthetic["message"]["usage"]
        .as_object_mut()
        .unwrap()
        .remove("input_tokens");
    write(&a, "one.jsonl", &[synthetic]);
    let receipt = scan(&a).unwrap();
    assert_eq!(receipt["unsupportedRows"], 1);
    assert_eq!(count(&receipt), 100);
}

#[test]
fn iteration_rows_with_null_model_are_retained() {
    let fixture = Fixture::new();
    let mut a = args(&fixture);
    let mut current = row("current", 100);
    current["message"]["usage"]["iterations"] =
        json!([{"type":"message","model":null,"input_tokens":100,"output_tokens":0}]);
    write(&a, "one.jsonl", &[current]);
    let receipt = enroll(&mut a);
    assert_eq!(receipt["unsupportedRows"], 0);
    assert_eq!(count(&receipt), 100);
}

#[test]
fn interrupted_temporary_write_keeps_last_committed_generation() {
    let fixture = Fixture::new();
    let mut a = args(&fixture);
    write(&a, "one.jsonl", &[row("first", 100)]);
    let initial = enroll(&mut a);
    let committed = bytes(&a);
    fs::write(a.store.join("ledger.crashed.tmp"), b"{partial").unwrap();
    assert_eq!(bytes(&a), committed);
    let next = scan(&a).unwrap();
    assert_eq!(count(&next), 100);
    assert_eq!(
        next["generation"].as_u64(),
        initial["generation"].as_u64().map(|v| v + 1)
    );
}

#[test]
fn concurrent_scan_waits_for_lock_then_reads_latest_sources() {
    let fixture = Fixture::new();
    let mut a = args(&fixture);
    write(&a, "one.jsonl", &[row("first", 100)]);
    enroll(&mut a);
    let lock = lock_store(&a.store).unwrap();
    let second = a.clone();
    let (sender, receiver) = std::sync::mpsc::channel();
    let thread = std::thread::spawn(move || {
        sender.send(scan(&second)).unwrap();
    });
    assert!(
        receiver
            .recv_timeout(std::time::Duration::from_millis(100))
            .is_err()
    );
    write(&a, "two.jsonl", &[row("new", 50)]);
    drop(lock);
    let receipt = receiver
        .recv_timeout(std::time::Duration::from_secs(10))
        .unwrap()
        .unwrap();
    thread.join().unwrap();
    assert_eq!(count(&receipt), 150);
    assert_eq!(count(&scan(&a).unwrap()), 150);
}

#[cfg(unix)]
#[test]
fn private_modes_and_symlink_rejection() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
    let fixture = Fixture::new();
    let mut a = args(&fixture);
    write(&a, "one.jsonl", &[row("first", 100)]);
    enroll(&mut a);
    assert_eq!(fs::metadata(&a.store).unwrap().mode() & 0o777, 0o700);
    assert_eq!(
        fs::metadata(a.store.join("ledger.json")).unwrap().mode() & 0o777,
        0o600
    );
    fs::set_permissions(
        a.store.join("ledger.json"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    assert!(scan(&a).is_err());
    fs::set_permissions(
        a.store.join("ledger.json"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    symlink(
        a.roots[0].join("projects/project/one.jsonl"),
        a.roots[0].join("projects/project/copy.jsonl"),
    )
    .unwrap();
    assert!(scan(&a).is_err());
    let mut trailing = a.clone();
    trailing.roots[0] = PathBuf::from(format!("{}/", trailing.roots[0].display()));
    assert!(
        scan(&trailing)
            .unwrap_err()
            .to_string()
            .contains("normalized")
    );
}
