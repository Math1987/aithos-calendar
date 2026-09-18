//! The scenario lab, run through the public routes exactly as a maintainer
//! would: each scenario catalog is fetched, validated with the AI Catalog
//! SDK, and the report is compared with the documented expectations.
use serde_json::Value;
use std::sync::Arc;

mod common;

struct Server {
    base: String,
    job: tokio::task::JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.job.abort();
    }
}

async fn start(account_linked: bool) -> Server {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let mut agents = vec![
        common::mock_agent("alice", &[("09:00", "10:00")]),
        common::mock_agent("bob", &[("09:30", "10:30")]),
    ];
    if account_linked {
        for agent in &mut agents {
            agent.google_account = true;
        }
    }
    let fixture = common::Fixture::new(&base, agents).await;
    let (base, job) = fixture.serve(fixture.config()).await;
    Server { base, job }
}

async fn get(url: &str) -> (reqwest::StatusCode, Value) {
    let response = reqwest::get(url).await.unwrap();
    let status = response.status();
    let body = response.json().await.unwrap_or(Value::Null);
    (status, body)
}

#[tokio::test]
async fn every_scenario_behaves_as_documented_under_each_policy() {
    let server = start(false).await;
    let (status, index) = get(&format!("{}/lab", server.base)).await;
    assert_eq!(status, 200);
    let scenarios = index["scenarios"].as_array().unwrap();
    assert_eq!(scenarios.len(), calendar::lab::SCENARIOS.len());
    for policy in ["guaranteed", "integrity"] {
        let (status, report) = get(&format!("{}/lab/report?policy={policy}", server.base)).await;
        assert_eq!(status, 200, "{report}");
        assert_eq!(report["policy"], policy);
        let failures: Vec<&Value> = report["results"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["passed"] != true)
            .collect();
        assert!(failures.is_empty(), "{policy}: {failures:?}");
        assert_eq!(report["passed"], true);
        // Inbound direction: four caller cases with the documented outcomes.
        let callers = report["callers"].as_array().unwrap();
        assert_eq!(callers.len(), 4, "{report}");
        for case in callers {
            assert_eq!(case["passed"], true, "{case}");
        }
        assert_eq!(callers[2]["actual"], "caller_unguaranteed");
        assert_eq!(callers[3]["actual"], "caller_key_mismatch");
    }
    // Mock agents carry no account attestation: only the booking policy
    // rejects the otherwise valid scenarios, with its own code.
    let (_, report) = get(&format!(
        "{}/lab/report?policy=verified-account",
        server.base
    ))
    .await;
    assert_eq!(report["passed"], true, "{report}");
    let baseline = &report["results"][0];
    assert_eq!(baseline["scenario"], "baseline");
    assert_eq!(baseline["actual"], "attestation_missing");
    assert_eq!(
        get(&format!("{}/lab/report?policy=open", server.base))
            .await
            .0,
        400
    );
}

#[tokio::test]
async fn account_linked_agents_pass_the_booking_policy() {
    let server = start(true).await;
    let (_, report) = get(&format!(
        "{}/lab/report?policy=verified-account",
        server.base
    ))
    .await;
    for result in report["results"].as_array().unwrap() {
        assert_eq!(result["passed"], true, "{result}");
    }
    assert_eq!(report["results"][0]["actual"], "accepted");
    // Without the calendar connector the signed call ends in a protocol
    // error instead of the capability check; the three refusals hold.
    let callers = report["callers"].as_array().unwrap();
    assert_eq!(callers[0]["actual"], "caller_signature_missing");
    assert_eq!(callers[2]["actual"], "caller_unguaranteed");
    assert_eq!(callers[3]["actual"], "caller_key_mismatch");
    let (_, catalog) = get(&format!("{}/.well-known/ai-catalog.json", server.base)).await;
    let attestation = &catalog["entries"][0]["trustManifest"]["attestations"][0];
    assert_eq!(attestation["type"], "account-verified");
    assert!(
        attestation["uri"]
            .as_str()
            .unwrap()
            .starts_with("data:application/json;base64,")
    );
}

#[tokio::test]
async fn scenario_catalogs_are_well_formed_documents_the_sdk_can_read() {
    let server = start(false).await;
    for scenario in calendar::lab::SCENARIOS {
        let url = format!(
            "{}/lab/{}/.well-known/ai-catalog.json",
            server.base, scenario.name
        );
        let response = reqwest::get(&url).await.unwrap();
        assert_eq!(response.status(), 200, "{}", scenario.name);
        assert_eq!(
            response.headers()["content-type"],
            "application/ai-catalog+json"
        );
        let document: Value = response.json().await.unwrap();
        let typed: ai_catalog::AiCatalog = serde_json::from_value(document.clone()).unwrap();
        let validation = ai_catalog_validate::validate(&typed);
        if scenario.name == "replayed-entry" {
            // The one attack a structural validator catches by itself:
            // subject.url must restate the entry url.
            assert!(!validation.is_valid);
            assert!(
                validation
                    .errors
                    .iter()
                    .any(|e| e.path.ends_with("subject.url")),
                "{:?}",
                validation.errors
            );
        } else {
            assert!(
                validation.is_valid,
                "{}: {:?}",
                scenario.name, validation.errors
            );
            // Every other scenario still *looks* Trusted to a structural
            // validator, including `downgraded`: level detection only looks
            // at the manifests that are present (the host's, here), so one
            // entry losing its manifest is invisible to it. Recorded in
            // docs/trust-manifest-evaluation.md.
            assert_eq!(
                validation.conformance_level,
                ai_catalog_validate::ConformanceLevel::Trusted,
                "{}",
                scenario.name
            );
        }
        let report = ai_catalog_trust::analyze_catalog(&typed);
        let errors: Vec<_> = report
            .findings
            .iter()
            .filter(|f| f.severity == ai_catalog_trust::Severity::Error)
            .collect();
        assert!(errors.is_empty(), "{}: {errors:?}", scenario.name);
        // Each entry's card is served at the scenario URL.
        for entry in typed.entries {
            let card = reqwest::get(entry.url.unwrap()).await.unwrap();
            assert_eq!(card.status(), 200, "{}", scenario.name);
        }
    }
    let (status, _) = get(&format!(
        "{}/lab/unknown/.well-known/ai-catalog.json",
        server.base
    ))
    .await;
    assert_eq!(status, 404);
    let _ = Arc::new(());
}
