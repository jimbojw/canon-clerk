use canon_clerk::models::{CanonVerdict, Caseload};
use canon_clerk::pipeline::{
    ConfigResolver, Discover, Intake, MockProviderClient, ProbeStatus, ProviderClient,
    ProviderType, Validate,
};
use std::collections::HashMap;

#[tokio::test]
async fn test_end_to_end_caseload_dag() {
    // 1. Intake: parse unified diff
    let diff = r#"diff --git a/src/main.rs b/src/main.rs
--- a/src/main.rs
+++ b/src/main.rs
@@ -1 +1,2 @@
-fn old() {}
+fn new() {}
"#;
    let artifacts = Intake::ingest_diff(diff);
    assert_eq!(artifacts.len(), 1);

    // 2. Validate: parse canon markdown
    let canon_content = r#"---
id: cli-rule
title: CLI Rule
status: active
triggers:
  - "src/**/*.rs"
---

# CLI Rule
Rule details here.
"#;
    let canon_ast = Validate::parse_and_validate(".canons/cli/cli-rule.md", canon_content).unwrap();

    // 3. Assemble Caseload
    let mut caseload = Caseload::new("e2e-test-session");
    for art in artifacts {
        caseload.add_artifact(art);
    }
    caseload.add_canon(canon_ast);

    // 4. Discover active canons against intake artifacts
    let active_ids = Discover::discover_active_canons(&caseload.canons, &caseload.artifacts);
    assert_eq!(active_ids, vec!["cli-rule"]);
    for id in active_ids {
        caseload.activate_canon(id);
    }
    assert_eq!(caseload.active_canon_ids.len(), 1);

    // 5. Environment configuration (Branch B)
    let env_vars = HashMap::new();
    let resolved = ConfigResolver::resolve(Some(ProviderType::Mock), None, None, &env_vars);
    assert_eq!(resolved.model.provider, ProviderType::Mock);

    // 6. Provider probe
    let client = MockProviderClient::new_healthy("mock-provider", "mock-v1");
    let probe = client.probe().await.unwrap();
    assert_eq!(probe.status, ProbeStatus::Healthy);

    // 7. Convergence: record verdict
    caseload.record_verdict(CanonVerdict::admit("cli-rule", 12));
    assert!(caseload.is_passing());

    let summary = caseload.summary();
    assert_eq!(summary.total_artifacts, 1);
    assert_eq!(summary.active_canons, 1);
    assert_eq!(summary.passing_verdicts, 1);
    assert_eq!(summary.failing_verdicts, 0);
    assert!(summary.is_passing);
}

