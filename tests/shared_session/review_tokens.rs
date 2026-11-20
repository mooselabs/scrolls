use super::{Queryable, Server, json, literal};
use support::{acceptance_fingerprint, legacy_token};

#[path = "../support/review_tokens.rs"]
mod support;

#[test]
fn shared_review_recovers_each_acceptance_boundary_without_origin_files() {
    for boundary in [
        "CALL DOLT_BRANCH(",
        "SELECT * FROM dolt_conflicts",
        "accept-commit",
        "CALL DOLT_MERGE('--ff-only'",
    ] {
        let mut s = Server::handbook();
        let p = s.proposed_page("propose", "Recover me");
        let branch = p["branch"].as_str().unwrap();
        let token = s.token(branch);
        let before = s.main_head();
        let needle = if boundary == "accept-commit" {
            literal(&format!("accept {branch} by alice"))
        } else {
            boundary.into()
        };
        s.interrupted(
            &[
                "--op-id",
                "accept",
                "review",
                "accept",
                branch,
                "--review-token",
                &token,
            ],
            needle,
        );
        let intent = s.intent("accept");
        assert_eq!(intent["plan"]["base"], before);
        let r = s.call("second-machine", &["recover", "accept", "--resume"], true);
        assert_eq!(r["data"]["proposal"]["status"], "accepted");
        assert_ne!(s.main_head(), before);
        assert_eq!(
            r["data"],
            s.call("third-machine", &["recover", "accept", "--resume"], true)["data"]
        );
        s.call("reader", &["doctor"], true);
    }
}

#[test]
fn shared_review_refuses_v1_intent_and_allows_abandonment() {
    let mut s = Server::handbook();
    let proposal = s.proposed_page("propose", "Review after upgrade");
    let branch = proposal["branch"].as_str().unwrap();
    let review = s.call("reviewer", &["review", branch], true);
    let token = review["data"]["review_token"].as_str().unwrap();
    let op = "accept-v1";
    let mut accept = [
        "--op-id",
        op,
        "review",
        "accept",
        branch,
        "--review-token",
        token,
    ];
    s.interrupted(&accept, "CALL DOLT_BRANCH(".into());

    // Model a prepared intent left by the previous writer before applying its changes.
    let old_token = legacy_token(branch, &review["data"]);
    let (_, args_hash) = acceptance_fingerprint(branch, Some(&old_token));
    let mut intent = s.intent(op);
    assert_eq!(intent["stage"]["state"], "prepared");
    intent["result"]["review_token"] = json!(old_token);
    intent["args_hash"] = json!(args_hash);
    let journal = "scrolls-operations";
    let id = format!("intent:{}", scrolls::model::digest(op));
    let sql = scrolls::storage::put("metadata", &id, &intent).unwrap();
    s.raw(journal)
        .query_drop(sql + "CALL DOLT_ADD('.'); CALL DOLT_COMMIT('-m','legacy intent fixture','--author','Test <test@localhost>')")
        .unwrap();

    let heads = || -> Vec<(String, String)> {
        s.raw("main")
            .query("SELECT name, hash FROM dolt_branches ORDER BY name")
            .unwrap()
    };
    let before = heads();
    *accept.last_mut().unwrap() = &old_token;
    for attempt in [accept.as_slice(), &["recover", op, "--resume"]] {
        let error = s.call("after-upgrade", attempt, false);
        assert_eq!(
            error["error"]["message"],
            format!(
                "run scrolls review {branch} again for a v2 token: unsupported review token version v1"
            )
        );
        assert_eq!(heads(), before);
        assert_eq!(s.intent(op), intent);
    }

    s.call(
        "after-upgrade",
        &[
            "--op-id",
            "cancel",
            "recover",
            op,
            "--abandon",
            "--reason",
            "Reinspect with a v2 token",
        ],
        true,
    );
    assert_eq!(s.intent(op)["stage"]["state"], "abandoned");
    let fresh = s.token(branch);
    assert!(fresh.starts_with("v2."));
    assert_eq!(
        s.call(
            "reviewer",
            &[
                "--op-id",
                "accept-v2",
                "review",
                "accept",
                branch,
                "--review-token",
                &fresh
            ],
            true,
        )["data"]["proposal"]["status"],
        "accepted",
    );
}
