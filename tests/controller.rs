//! Run the same command adapter an embedding host calls, with a pinned store.
use clap::Parser;
use scrolls::{
    app,
    cli::Cli,
    credential::{CredentialRefusal, CredentialVerifier, VerificationRequest},
    model::{DEFAULT_TENANT, Role, TenantId},
    storage::Store,
};
use serde_json::{Value, json};
use std::{cell::Cell, fs, path::PathBuf};

const ACCEPTED: &[u8] = b"test-controller-credential-first";
const RETRY: &[u8] = b"test-controller-credential-second";
const REFUSED: &[u8] = b"test-controller-credential-denied";

struct Fixture {
    dir: tempfile::TempDir,
    dolt: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let fixture = Self {
            dir: tempfile::tempdir().unwrap(),
            dolt: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".tools/dolt"),
        };
        fixture.run(&["init"]);
        fixture
    }

    fn root(&self) -> PathBuf {
        self.dir.path().join("store")
    }

    fn cli(&self, args: &[&str]) -> Cli {
        let root = self.root();
        Cli::try_parse_from(
            [
                "scrolls",
                "--dolt",
                self.dolt.to_str().unwrap(),
                "--store",
                root.to_str().unwrap(),
                "--actor",
                "alice",
            ]
            .into_iter()
            .chain(args.iter().copied()),
        )
        .unwrap()
    }

    fn run(&self, args: &[&str]) -> Value {
        app::run(&self.cli(args)).unwrap()
    }

    fn store(&self) -> Store {
        Store::open_readonly(&self.root(), &self.dolt).unwrap()
    }

    fn state(&self) -> Value {
        let store = self.store();
        let branches: serde_json::Map<String, Value> = store
            .branches()
            .unwrap()
            .into_iter()
            .map(|branch| {
                let head = store.head(&branch).unwrap();
                (branch, json!(head))
            })
            .collect();
        json!({
            "branches": branches,
            "active": store.revision().unwrap(),
            "audit": fs::read(self.root().join("consumed.jsonl")).ok(),
            "review_journal": fs::read(self.root().join("review-journal.json")).ok(),
        })
    }
}

struct TestVerifier<'a> {
    check: &'a dyn Fn(&VerificationRequest<'_>),
    calls: Cell<usize>,
}

impl CredentialVerifier for TestVerifier<'_> {
    fn verify(
        &self,
        request: &VerificationRequest<'_>,
        credential: &[u8],
    ) -> Result<(), CredentialRefusal> {
        self.calls.set(self.calls.get() + 1);
        (self.check)(request);
        match credential {
            ACCEPTED | RETRY => Ok(()),
            _ => Err(CredentialRefusal::InvalidCredential),
        }
    }
}

fn refusal(cli: &Cli, verifier: &TestVerifier<'_>, credential: Option<&[u8]>, error: &str) {
    let message = app::run_with_verifier(cli, &DEFAULT_TENANT, Some(verifier), credential)
        .unwrap_err()
        .to_string();
    assert!(message.starts_with(error), "{message}");
    for bytes in [ACCEPTED, RETRY, REFUSED] {
        assert!(!message.contains(std::str::from_utf8(bytes).unwrap()));
    }
}

#[test]
fn verified_controller_moves_an_exact_label_and_reauthenticates_receipt_retries() {
    let f = Fixture::new();
    let first = f.run(&["new", "note", "--title", "Deployment", "--body", "A"]);
    let id = first["data"]["id"].as_str().unwrap();
    let a = first["revision"].as_str().unwrap();
    f.run(&[
        "--role",
        "human",
        "label",
        "move",
        id,
        "stable",
        "--to",
        a,
        "--expect-absent",
    ]);
    let second = f.run(&["edit", id, "--expect", "1", "--body", "B"]);
    let b = second["revision"].as_str().unwrap();
    f.run(&["show", id]);
    let audit = fs::read(f.root().join("consumed.jsonl")).unwrap();
    let before = f.state();

    let cli = f.cli(&[
        "--role",
        "controller",
        "--op-id",
        "promote",
        "label",
        "move",
        "Deployment",
        "stable",
        "--to",
        b,
        "--expect",
        a,
    ]);
    let expected_fingerprint = scrolls::model::digest(
        serde_json::to_vec(&json!({
            "command":cli.command,"body":null,"actor":"alice","role":"Controller",
            "proposal":null,"source_commit":null
        }))
        .unwrap(),
    );
    let verifier = TestVerifier {
        calls: Cell::new(0),
        check: &|request| {
            assert_eq!(request.actor, "alice");
            assert_eq!(request.claimed_role, Role::Controller);
            assert_eq!(request.op_id, "promote");
            assert_eq!(request.request_fingerprint, expected_fingerprint);
            assert!(!request.dry_run);
            let label = request.label_move.as_ref().unwrap();
            assert_eq!(
                (label.record_id, label.label, label.target_revision),
                (id, "stable", b)
            );
        },
    };
    refusal(&cli, &verifier, None, "credential_required:");
    refusal(&cli, &verifier, Some(REFUSED), "credential_refused:");
    assert_eq!(f.state(), before);

    let moved =
        app::run_with_verifier(&cli, &DEFAULT_TENANT, Some(&verifier), Some(ACCEPTED)).unwrap();
    assert_eq!(
        moved["data"],
        json!({
            "op_id":"promote","actor":"alice","role":"controller","id":id,
            "label":"stable","previous_target":a,"new_target":b
        })
    );
    assert_ne!(moved["revision"], b);
    let after = f.state();
    refusal(&cli, &verifier, Some(REFUSED), "credential_refused:");
    let replay =
        app::run_with_verifier(&cli, &DEFAULT_TENANT, Some(&verifier), Some(RETRY)).unwrap();
    assert_eq!(replay, moved);
    assert_eq!(f.state(), after);
    assert_eq!(verifier.calls.get(), 4);
    assert_eq!(
        f.run(&["label", "list", id])["data"]["labels"],
        json!({"stable":b})
    );

    let store = f.store();
    let receipt: Value = store.one("operations", "promote").unwrap().unwrap();
    assert_eq!(receipt["role"], "Controller");
    assert_eq!(receipt["args_hash"], expected_fingerprint);
    assert_eq!(receipt["result"], moved["data"]);
    let committed_state = json!(
        [
            "metadata",
            "operations",
            "records",
            "edges",
            "registry",
            "source_files",
            "import_sources",
            "import_files",
            "import_members"
        ]
        .map(|table| store.rows::<Value>(table).unwrap())
    )
    .to_string();
    let audit_after = fs::read(f.root().join("consumed.jsonl")).unwrap();
    assert_eq!(audit, audit_after);
    for bytes in [ACCEPTED, RETRY, REFUSED] {
        let text = std::str::from_utf8(bytes).unwrap();
        assert!(!committed_state.contains(text));
        assert!(!receipt.to_string().contains(text));
        assert!(!moved.to_string().contains(text));
        assert!(!replay.to_string().contains(text));
        assert!(
            !audit_after
                .windows(bytes.len())
                .any(|window| window == bytes)
        );
    }
}

#[test]
fn controller_cannot_write_records_imports_schemas_reviews_or_recovery() {
    let f = Fixture::new();
    let proposal = f.run(&["new", "entity", "--title", "Review", "--body", "draft"]);
    let branch = proposal["data"]["branch"].as_str().unwrap();
    let inspection = f.run(&["review", branch]);
    let token = inspection["data"]["review_token"].as_str().unwrap();
    let before = f.state();
    let verifier = TestVerifier {
        calls: Cell::new(0),
        check: &|_| {},
    };
    let commands: &[&[&str]] = &[
        &["review", "accept", branch, "--review-token", token],
        &["review", "reject", branch, "--reason", "declined"],
        &["new", "note", "--title", "Refused", "--body", "draft"],
        &["schema", "apply", "unused.yml", "--expect", "old"],
        &["import", "apply", "unused.json"],
        &["recover", "accept", "--abandon", "--reason", "declined"],
        &["beads", "refresh", "--all"],
        &["render", "--out", "unused-export"],
    ];
    for command in commands {
        let args = ["--role", "controller", "--op-id", "refused"]
            .into_iter()
            .chain(command.iter().copied())
            .collect::<Vec<_>>();
        refusal(
            &f.cli(&args),
            &verifier,
            Some(ACCEPTED),
            "controller_operation_refused:",
        );
    }
    assert_eq!(f.state(), before);
    assert_eq!(verifier.calls.get(), 0);
}

#[test]
fn verifier_checks_human_review_before_opening_for_writes_and_agent_needs_no_credential() {
    let f = Fixture::new();
    let proposal_cli = f.cli(&[
        "--role", "agent", "--op-id", "draft", "new", "entity", "--title", "Review", "--body",
        "draft",
    ]);
    let verifier = TestVerifier {
        calls: Cell::new(0),
        check: &|request| {
            assert_eq!(request.actor, "alice");
            assert_eq!(request.claimed_role, Role::Human);
            assert_eq!(request.op_id, "accept");
            assert_eq!(request.request_fingerprint.len(), 64);
            assert!(request.label_move.is_none());
        },
    };
    let proposal =
        app::run_with_verifier(&proposal_cli, &DEFAULT_TENANT, Some(&verifier), None).unwrap();
    let branch = proposal["data"]["branch"].as_str().unwrap();
    assert!(branch.starts_with("proposal/"));
    assert_eq!(
        f.store()
            .one_at::<Value>("operations", "draft", branch)
            .unwrap()
            .unwrap()["role"],
        "Agent"
    );

    let inspection = f.run(&["review", branch]);
    let token = inspection["data"]["review_token"].as_str().unwrap();
    let accept = f.cli(&[
        "--role",
        "human",
        "--op-id",
        "accept",
        "review",
        "accept",
        branch,
        "--review-token",
        token,
    ]);
    let before = f.state();
    refusal(&accept, &verifier, None, "credential_required:");
    refusal(&accept, &verifier, Some(REFUSED), "credential_refused:");
    assert_eq!(f.state(), before);

    let reviewed =
        app::run_with_verifier(&accept, &DEFAULT_TENANT, Some(&verifier), Some(ACCEPTED)).unwrap();
    assert_eq!(reviewed["data"]["proposal"]["status"], "accepted");
    let receipt = f
        .store()
        .one::<Value>("operations", "accept")
        .unwrap()
        .unwrap();
    assert_eq!(receipt["role"], "Human");
    assert!(
        !receipt
            .to_string()
            .contains(std::str::from_utf8(ACCEPTED).unwrap())
    );
    assert_eq!(verifier.calls.get(), 2);
}

#[test]
fn a_refused_human_init_never_creates_a_store() {
    let f = Fixture::new();
    let mut cli = f.cli(&["--role", "human", "--op-id", "fresh-init", "init"]);
    cli.store = f.dir.path().join("uninitialized");
    let verifier = TestVerifier {
        calls: Cell::new(0),
        check: &|request| {
            assert_eq!(request.tenant, &DEFAULT_TENANT);
            assert_eq!(request.claimed_role, Role::Human);
            assert_eq!(request.op_id, "fresh-init");
        },
    };
    refusal(&cli, &verifier, Some(REFUSED), "credential_refused:");
    assert!(!cli.store.exists());
}

#[test]
fn embedded_dolt_rejects_another_tenant_before_opening_the_store() {
    let f = Fixture::new();
    let mut cli = f.cli(&["--role", "human", "--op-id", "wrong-tenant", "init"]);
    cli.store = f.dir.path().join("uninitialized");
    let tenant: TenantId = "other-tenant".parse().unwrap();
    let verifier = TestVerifier {
        calls: Cell::new(0),
        check: &|_| panic!("tenant mismatch must precede credential verification"),
    };

    let error = app::run_with_verifier(&cli, &tenant, Some(&verifier), Some(ACCEPTED))
        .unwrap_err()
        .to_string();
    assert_eq!(
        error,
        "tenant_mismatch: operation tenant other-tenant cannot open a Dolt store, which serves only the default tenant (default)"
    );
    assert!(!cli.store.exists());
    assert_eq!(verifier.calls.get(), 0);
}

#[test]
fn embedded_verifier_receives_the_operation_tenant() {
    let f = Fixture::new();
    let cli = f.cli(&["--role", "human", "--op-id", "verify-read", "doctor"]);
    let tenant: TenantId = "default".parse().unwrap();
    let verifier = TestVerifier {
        calls: Cell::new(0),
        check: &|request| {
            assert_eq!(request.tenant, &tenant);
            assert_eq!(request.op_id, "verify-read");
        },
    };

    let result = app::run_with_verifier(&cli, &tenant, Some(&verifier), Some(ACCEPTED)).unwrap();
    assert_eq!(result["data"]["tenant"], tenant.as_str());
    assert_eq!(verifier.calls.get(), 1);
}
