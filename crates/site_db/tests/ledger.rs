//! Gate (v) unit: the BuildState ledger proves a no-op rebuild. Two identical
//! ledgers => no_op true; a changed output hash => that node dirty, no_op false.

use site_db::ledger::BuildLedger;

#[test]
fn identical_ledger_is_no_op() {
    let mut a = BuildLedger::new();
    a.record("snapshot:StructureDefinition/x", "ih", "oh1");
    a.record("page:index", "", "oh2");
    a.record("config:sushi-config", "", "oh3");

    // Prior = a clone with the same content.
    let prior = a.clone();
    let report = a.finish(Some(&prior));
    assert!(report.no_op, "identical ledgers must be a proven no-op");
    assert_eq!(report.dirty.len(), 0);
    assert_eq!(report.clean.len(), 3);
}

#[test]
fn changed_node_is_dirty() {
    let mut prior = BuildLedger::new();
    prior.record("page:index", "", "old");
    prior.record("config:sushi-config", "", "cfg");

    let mut next = BuildLedger::new();
    next.record("page:index", "", "NEW"); // changed output hash
    next.record("config:sushi-config", "", "cfg");

    let report = next.finish(Some(&prior));
    assert!(!report.no_op);
    assert_eq!(report.dirty, vec!["page:index".to_string()]);
    assert_eq!(report.clean, vec!["config:sushi-config".to_string()]);
}

#[test]
fn no_prior_ledger_is_never_no_op() {
    let mut a = BuildLedger::new();
    a.record("page:index", "", "oh");
    let report = a.finish(None);
    assert!(!report.no_op, "a first build (no prior) is not a no-op");
    assert_eq!(report.dirty.len(), 1);
}

#[test]
fn dropped_node_breaks_no_op() {
    let mut prior = BuildLedger::new();
    prior.record("page:index", "", "oh");
    prior.record("page:gone", "", "oh2");

    let mut next = BuildLedger::new();
    next.record("page:index", "", "oh"); // page:gone dropped

    let report = next.finish(Some(&prior));
    assert!(!report.no_op, "dropping a node must break the no-op");
}
