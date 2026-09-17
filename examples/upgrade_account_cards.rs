//! Operator-only, repeatable 0.5 -> 0.6 migration. Runtime IAM cannot read keys.
use aws_sdk_dynamodb::types::AttributeValue as A;
use calendar::{registry::Registry, storage::Record};
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), lambda_http::Error> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 4 || !matches!(args[0].as_str(), "check" | "apply") {
        return Err(
            "Usage: upgrade_account_cards check|apply TABLE API_ORIGIN REGISTRY_ORIGIN".into(),
        );
    }
    let apply = args[0] == "apply";
    let config = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
    let db = aws_sdk_dynamodb::Client::new(&config);
    let registry = Registry::new(&args[3])?;
    let mut cursor = None;
    loop {
        let page = db
            .scan()
            .table_name(&args[1])
            .consistent_read(true)
            .projection_expression("id, #r, published")
            .expression_attribute_names("#r", "record")
            .set_exclusive_start_key(cursor)
            .send()
            .await?;
        for item in page.items() {
            let Some(raw) = item.get("record").and_then(|v| v.as_s().ok()) else {
                continue;
            };
            let record: Record = serde_json::from_str(raw)?;
            if !record.agent.google_account
                || serde_json::from_str::<serde_json::Value>(&record.card_bytes)?["version"]
                    != "0.5.0"
            {
                continue;
            }
            println!(
                "{}: {} 0.5.0 -> 0.6.0 (identity preserved)",
                record.agent.id,
                if apply { "upgrading" } else { "would upgrade" }
            );
            if !apply {
                continue;
            }
            // Read only this recovery key, never print it or store it in files.
            let key_item = db
                .get_item()
                .table_name(&args[1])
                .key("id", A::S(record.agent.id.clone()))
                .consistent_read(true)
                .projection_expression("signing_key")
                .send()
                .await?;
            let key = key_item
                .item
                .as_ref()
                .and_then(|i| i.get("signing_key"))
                .and_then(|v| v.as_s().ok())
                .ok_or("Missing recovery key")?;
            let updated = registry.upgrade_account(&record, key, &args[2])?;
            // Deterministic signed bytes: rerun safely if the registry succeeds but
            // this process stops before the conditional local update. No key rotation.
            registry
                .publish(&updated)
                .await
                .map_err(|code| format!("{code}; retry the same migration"))?;
            db.update_item()
                .table_name(&args[1])
                .key("id", A::S(record.agent.id.clone()))
                .condition_expression("#r = :previous")
                .update_expression("SET #r = :next, published = :yes")
                .expression_attribute_names("#r", "record")
                .expression_attribute_values(":previous", A::S(raw.clone()))
                .expression_attribute_values(":next", A::S(serde_json::to_string(&updated)?))
                .expression_attribute_values(":yes", A::Bool(true))
                .send()
                .await?;
            println!("{}: published and stored", record.agent.id);
        }
        cursor = page.last_evaluated_key;
        if cursor.as_ref().is_none_or(|v| v.is_empty()) {
            break;
        }
    }
    Ok(())
}
