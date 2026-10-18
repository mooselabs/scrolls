//! Human presentation is independent of the versioned JSON protocol.
use serde_json::Value;
fn record(value: &Value) -> String {
    let mut out = format!(
        "# {}\n{} · {} · {} · record revision {}\nOwner: {}\n\n",
        value["title"].as_str().unwrap_or(""),
        value["id"].as_str().unwrap_or(""),
        value["kind"].as_str().unwrap_or(""),
        value["status"].as_str().unwrap_or(""),
        value["revision"],
        value["owner"].as_str().unwrap_or("")
    );
    if let Some(body) = value["body"].as_str() {
        out.push_str(body);
    }
    out.push('\n');
    out
}
fn summary(value: &Value) -> String {
    format!(
        "{}  {}/{}  {}\n",
        value["id"].as_str().unwrap_or(""),
        value["kind"].as_str().unwrap_or(""),
        value["status"].as_str().unwrap_or(""),
        value["title"].as_str().unwrap_or("")
    )
}
pub fn human(envelope: &Value) -> String {
    let data = &envelope["data"];
    let mut text = if data["record"].is_object() {
        let mut out = record(&data["record"]);
        if let Some(uri) = data["uri"].as_str() {
            out.push_str(&format!("\nReference: {uri}\n"));
        }
        if let Some(edges) = data["edges"].as_array() {
            if !edges.is_empty() {
                out.push_str("\nRelationships:\n");
            }
            for edge in edges {
                out.push_str(&format!(
                    "{} --{}--> {} ({})\n",
                    edge["from"].as_str().unwrap_or(""),
                    edge["rel"].as_str().unwrap_or(""),
                    edge["to"].as_str().unwrap_or(""),
                    edge["why"].as_str().unwrap_or("")
                ));
            }
        }
        if let Some(work) = data
            .get("related_work")
            .filter(|v| v.as_array().is_some_and(|a| !a.is_empty()))
        {
            out.push_str(&format!(
                "\nRelated work (cached):\n{}\n",
                serde_json::to_string_pretty(work).unwrap()
            ));
        }
        out
    } else if let Some(items) = data["items"].as_array() {
        let mut out = String::new();
        if let Some(bead) = data.get("bead") {
            out.push_str(&format!("Bead (cached): {}\n\n", bead));
        }
        for item in items {
            out.push_str(&if item["summary_only"] == true {
                summary(item)
            } else {
                record(item)
            });
            out.push('\n');
        }
        out.push_str(&format!(
            "Context: {} / {} bytes; omitted: {}\n",
            data["used_bytes"], data["budget_bytes"], data["omitted"]
        ));
        if data["over_budget_bytes"].as_u64().unwrap_or(0) > 0 {
            out.push_str(&format!("Mandatory doctrine: {} bytes; budget exceeded by {} bytes. All mandatory bodies included.\n", data["mandatory_bytes"], data["over_budget_bytes"]));
        }
        if let Some(clauses) = data.get("undecided_doctrine") {
            out.push_str(&format!(
                "Undecided doctrine: {}\n",
                serde_json::to_string_pretty(clauses).unwrap()
            ));
        }
        out
    } else if data["proposal"].is_object() && data["records"].is_array() {
        let mut out = format!(
            "Proposal {} ({})\n",
            data["proposal"]["branch"].as_str().unwrap_or(""),
            data["proposal"]["status"].as_str().unwrap_or("")
        );
        for change in data["records"].as_array().unwrap() {
            for side in ["before", "after"] {
                let value = &change[side];
                out.push_str(&format!("\n{side}:\n"));
                if value.is_null() {
                    out.push_str("(new record)\n");
                    continue;
                }
                out.push_str(&record(value));
                let mut metadata = value.clone();
                metadata.as_object_mut().unwrap().remove("body");
                metadata.as_object_mut().unwrap().remove("original");
                out.push_str(&serde_json::to_string_pretty(&metadata).unwrap());
                out.push('\n');
            }
        }
        if let Some(diff) = data["graph_diff"].as_str() {
            out.push_str("\nGraph changes:\n");
            out.push_str(diff);
        }
        out.push_str(&format!(
            "\nMain revision: {}\nProposal revision: {}\nReview token: {}\n\nAfter reviewing, accept these revisions with:\nscrolls --role human review accept {} --review-token {}\nAny change to main or this proposal requires another review.\n",
            data["main_revision"].as_str().unwrap_or(""),
            data["proposal_revision"].as_str().unwrap_or(""),
            data["review_token"].as_str().unwrap_or(""),
            shell_words::quote(data["proposal"]["branch"].as_str().unwrap_or("")),
            data["review_token"].as_str().unwrap_or("")
        ));
        out
    } else if data.as_array().is_some_and(|a| {
        a.iter()
            .all(|v| v["id"].is_string() && v["title"].is_string())
    }) {
        data.as_array().unwrap().iter().map(summary).collect()
    } else {
        format!("{}\n", serde_json::to_string_pretty(data).unwrap())
    };
    if let Some(revision) = envelope["revision"].as_str() {
        text.push_str(&format!(
            "\nDolt revision: {revision} ({})\n",
            envelope["branch"].as_str().unwrap_or("main")
        ));
    }
    // Imported bodies are data, not terminal control sequences.
    text.chars()
        .flat_map(|c| {
            if c.is_control() && c != '\n' && c != '\t' {
                format!("\\u{{{:04x}}}", c as u32)
                    .chars()
                    .collect::<Vec<_>>()
            } else {
                vec![c]
            }
        })
        .collect()
}
