use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};

pub fn legacy_token(branch: &str, review: &Value) -> String {
    let payload = json!({
        "branch": branch,
        "main_revision": review["main_revision"],
        "proposal_revision": review["proposal_revision"],
    });
    format!(
        "v1.{}",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).unwrap())
    )
}

pub fn acceptance_fingerprint(branch: &str, token: Option<&str>) -> (Value, String) {
    // Explicit historical wire shape, independent of the current Cmd serializer.
    let mut command = json!({"Review":{"action":"accept","branch":branch,"reason":null}});
    if let Some(token) = token {
        command["Review"]["review_token"] = json!(token);
    }
    let hash = scrolls::model::digest(serde_json::to_vec(&json!({"command":command,"body":null,"actor":"alice","role":"Human","proposal":null,"source_commit":null})).unwrap());
    (command, hash)
}
