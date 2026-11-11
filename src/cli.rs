// BKLG-20261108-6zdg: retain the public CLI path while the model owns Role.
pub use crate::model::Role;
use crate::{query::Filters, schema::Direction};
// BKLG-20261110-7en6: retain the public CLI format path for existing Rust callers.
pub use crate::render::Format;
// BKLG-20261110-7en6: retain public CLI paths while Beads owns its command inputs.
pub use crate::beads::{Operation as BeadsCommand, Target};
use clap::{Args, Parser, Subcommand, ValueEnum};
use serde::Serialize;
use std::path::PathBuf;
#[derive(Parser, Debug, Serialize)]
#[command(
    version,
    about = "Local, versioned knowledge graphs",
    long_about = "Local, versioned knowledge graphs backed by private Dolt 2.2.0. Actor roles are cooperative attribution, not authentication."
)]
pub struct Cli {
    #[arg(long, global = true, default_value = ".scrolls")]
    pub store: PathBuf,
    /// Machine-local JSON connection file. Credentials never belong in repository bindings.
    #[arg(long, global = true)]
    pub connection: Option<PathBuf>,
    #[arg(long, global = true, default_value = ".tools/dolt")]
    pub dolt: PathBuf,
    #[arg(long, global = true)]
    pub json: bool,
    #[arg(long, global = true, env = "SCROLLS_ACTOR")]
    pub actor: Option<String>,
    #[arg(long, global = true, value_enum, default_value = "agent")]
    pub role: Role,
    #[arg(long, global = true)]
    pub proposal: Option<String>,
    #[arg(long, global = true)]
    pub op_id: Option<String>,
    #[arg(long, global = true)]
    pub dry_run: bool,
    #[arg(long, global = true, default_value = "local")]
    pub session: String,
    #[command(subcommand)]
    pub command: Cmd,
}
impl ValueEnum for Role {
    fn value_variants<'a>() -> &'a [Self] {
        &[Self::Human, Self::Agent]
    }

    fn to_possible_value(&self) -> Option<clap::builder::PossibleValue> {
        Some(clap::builder::PossibleValue::new(self.name()))
    }
}

impl ValueEnum for Direction {
    fn value_variants<'a>() -> &'a [Self] {
        &[Self::Incoming, Self::Outgoing, Self::Both]
    }

    fn to_possible_value(&self) -> Option<clap::builder::PossibleValue> {
        Some(clap::builder::PossibleValue::new(match self {
            Self::Incoming => "incoming",
            Self::Outgoing => "outgoing",
            Self::Both => "both",
        }))
    }
}
impl ValueEnum for Format {
    fn value_variants<'a>() -> &'a [Self] {
        &[Self::Native, Self::Liskov]
    }

    fn to_possible_value(&self) -> Option<clap::builder::PossibleValue> {
        Some(clap::builder::PossibleValue::new(match self {
            Self::Native => "native",
            Self::Liskov => "liskov",
        }))
    }
}

impl Args for Filters {
    fn group_id() -> Option<clap::Id> {
        Some("Filters".into())
    }

    fn augment_args(mut command: clap::Command) -> clap::Command {
        for (name, value) in [
            ("kind", "KIND"),
            ("status", "STATUS"),
            ("owner", "OWNER"),
            ("field", "FIELD"),
        ] {
            command = command.arg(
                clap::Arg::new(name)
                    .long(name)
                    .value_name(value)
                    .value_parser(clap::value_parser!(String))
                    .action(if name == "field" {
                        clap::ArgAction::Append
                    } else {
                        clap::ArgAction::Set
                    }),
            );
        }
        command.group(
            clap::ArgGroup::new("Filters")
                .multiple(true)
                .args(["kind", "status", "owner", "field"]),
        )
    }

    fn augment_args_for_update(command: clap::Command) -> clap::Command {
        Self::augment_args(command)
    }
}

impl clap::FromArgMatches for Filters {
    fn from_arg_matches(matches: &clap::ArgMatches) -> Result<Self, clap::Error> {
        let mut filters = Self::default();
        filters.update_from_arg_matches(matches)?;
        Ok(filters)
    }

    fn update_from_arg_matches(&mut self, matches: &clap::ArgMatches) -> Result<(), clap::Error> {
        for (name, field) in [
            ("kind", &mut self.kind),
            ("status", &mut self.status),
            ("owner", &mut self.owner),
        ] {
            if matches.contains_id(name) {
                *field = matches.get_one::<String>(name).cloned();
            }
        }
        if let Some(fields) = matches.get_many::<String>("field") {
            self.field = fields.cloned().collect();
        }
        Ok(())
    }
}

impl Args for Target {
    fn group_id() -> Option<clap::Id> {
        Some("Target".into())
    }

    fn augment_args(command: clap::Command) -> clap::Command {
        command
            .arg(
                clap::Arg::new("reference")
                    .value_name("REFERENCE")
                    .value_parser(clap::value_parser!(String))
                    .required_unless_present("all")
                    .conflicts_with("all"),
            )
            .arg(
                clap::Arg::new("all")
                    .long("all")
                    .action(clap::ArgAction::SetTrue),
            )
            .group(
                clap::ArgGroup::new("Target")
                    .multiple(true)
                    .args(["reference", "all"]),
            )
    }

    fn augment_args_for_update(command: clap::Command) -> clap::Command {
        Self::augment_args(command)
    }
}

impl clap::FromArgMatches for Target {
    fn from_arg_matches(matches: &clap::ArgMatches) -> Result<Self, clap::Error> {
        Ok(Self {
            reference: matches.get_one::<String>("reference").cloned(),
            all: matches.get_flag("all"),
        })
    }

    fn update_from_arg_matches(&mut self, matches: &clap::ArgMatches) -> Result<(), clap::Error> {
        if matches.contains_id("reference") {
            self.reference = matches.get_one::<String>("reference").cloned();
        }
        if matches.contains_id("all") {
            self.all = matches.get_flag("all");
        }
        Ok(())
    }
}

impl Subcommand for BeadsCommand {
    fn augment_subcommands(command: clap::Command) -> clap::Command {
        command
            .subcommand(
                clap::Command::new("bind")
                    .arg(clap::Arg::new("name").value_name("NAME").required(true))
                    .arg(
                        clap::Arg::new("directory")
                            .long("directory")
                            .value_name("DIRECTORY")
                            .value_parser(clap::value_parser!(PathBuf))
                            .required(true),
                    )
                    .arg(
                        clap::Arg::new("bd")
                            .long("bd")
                            .value_name("BD")
                            .value_parser(clap::value_parser!(PathBuf))
                            .default_value("bd"),
                    )
                    .arg(
                        clap::Arg::new("timeout")
                            .long("timeout")
                            .value_name("TIMEOUT")
                            .value_parser(clap::value_parser!(u64))
                            .default_value("30"),
                    ),
            )
            .subcommand(clap::Command::new("list"))
            .subcommand(
                clap::Command::new("refs").arg(
                    clap::Arg::new("reference")
                        .value_name("REFERENCE")
                        .required(true),
                ),
            )
            .subcommand(Target::augment_args(clap::Command::new("refresh")))
            .subcommand(Target::augment_args(clap::Command::new("publish")))
    }

    fn augment_subcommands_for_update(command: clap::Command) -> clap::Command {
        Self::augment_subcommands(command)
    }

    fn has_subcommand(name: &str) -> bool {
        matches!(name, "bind" | "list" | "refs" | "refresh" | "publish")
    }
}

impl clap::FromArgMatches for BeadsCommand {
    fn from_arg_matches(matches: &clap::ArgMatches) -> Result<Self, clap::Error> {
        let Some((name, args)) = matches.subcommand() else {
            return Err(clap::Error::raw(
                clap::error::ErrorKind::MissingSubcommand,
                "a Beads command is required",
            ));
        };
        Ok(match name {
            "bind" => Self::Bind {
                name: args
                    .get_one::<String>("name")
                    .expect("required name")
                    .clone(),
                directory: args
                    .get_one::<PathBuf>("directory")
                    .expect("required directory")
                    .clone(),
                bd: args.get_one::<PathBuf>("bd").expect("default bd").clone(),
                timeout: *args.get_one::<u64>("timeout").expect("default timeout"),
            },
            "list" => Self::List,
            "refs" => Self::Refs {
                reference: args
                    .get_one::<String>("reference")
                    .expect("required reference")
                    .clone(),
            },
            "refresh" => Self::Refresh(Target::from_arg_matches(args)?),
            "publish" => Self::Publish(Target::from_arg_matches(args)?),
            _ => {
                return Err(clap::Error::raw(
                    clap::error::ErrorKind::InvalidSubcommand,
                    "unknown Beads command",
                ));
            }
        })
    }

    fn update_from_arg_matches(&mut self, matches: &clap::ArgMatches) -> Result<(), clap::Error> {
        if matches.subcommand().is_some() {
            *self = Self::from_arg_matches(matches)?;
        }
        Ok(())
    }
}

#[derive(Debug, Serialize, Args)]
pub struct Body {
    #[arg(long, conflicts_with = "body_file")]
    pub body: Option<String>,
    #[arg(long)]
    pub body_file: Option<PathBuf>,
    #[arg(long,conflicts_with_all=["body","body_file"])]
    pub editor: bool,
}
#[derive(Debug, Serialize, Args)]
pub struct New {
    pub kind: String,
    #[arg(long)]
    pub title: String,
    #[command(flatten)]
    pub body: Body,
    #[arg(long)]
    pub owner: Option<String>,
    #[arg(long)]
    pub confidence: Option<String>,
    #[arg(long)]
    pub review_every: Option<u32>,
    #[arg(long)]
    pub field: Vec<String>,
}
#[derive(Debug, Serialize, Subcommand)]
pub enum Cmd {
    /// Serve knowledge reads over loopback HTTP. Never writes or repairs a store.
    Serve {
        #[arg(long, default_value = "127.0.0.1:0")]
        bind: std::net::SocketAddr,
    },
    Init {
        #[arg(long)]
        schema: Option<PathBuf>,
        /// Shared initialization: reviewed empty database revision from --dry-run.
        #[arg(long)]
        expect_base: Option<String>,
        /// Shared initialization: reviewed ontology digest from --dry-run.
        #[arg(long)]
        expect_schema: Option<String>,
    },
    Schema {
        #[command(subcommand)]
        command: SchemaCommand,
    },
    Batch {
        file: PathBuf,
    },
    Query {
        #[arg(long)]
        file: PathBuf,
        #[arg(long)]
        path: Vec<String>,
        #[arg(long)]
        at: Option<String>,
    },
    Doctor,
    /// Create a verified shared database backup, or restore one into a fresh local directory.
    Backup {
        #[command(subcommand)]
        command: BackupCommand,
    },
    /// Inspect, resume, or explicitly abandon an owned interrupted shared operation.
    Recover {
        operation: String,
        #[arg(long)]
        abandon: bool,
        #[arg(long, conflicts_with = "abandon")]
        resume: bool,
        #[arg(long)]
        reason: Option<String>,
    },
    /// Upgrade store metadata and relation registry explicitly.
    Migrate,
    Beads {
        #[command(subcommand)]
        command: BeadsCommand,
    },
    Import {
        #[command(subcommand)]
        command: ImportCommand,
    },
    Render {
        #[arg(long)]
        out: PathBuf,
        #[arg(long, value_enum, default_value = "native")]
        format: Format,
    },
    List {
        #[command(flatten)]
        filters: Filters,
    },
    Show {
        id: String,
        #[arg(long)]
        provenance: bool,
    },
    Search {
        terms: String,
        #[command(flatten)]
        filters: Filters,
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    Walk {
        id: String,
        #[arg(long, value_enum, default_value = "both")]
        direction: crate::schema::Direction,
        #[arg(long)]
        full: bool,
        #[arg(long)]
        rel: Option<String>,
        #[arg(long, default_value_t = 2)]
        depth: usize,
    },
    Context {
        #[arg(required_unless_present = "bead", conflicts_with = "bead")]
        id: Option<String>,
        #[arg(long)]
        bead: Option<String>,
        #[arg(long, default_value_t = 16000)]
        budget: usize,
    },
    Prime {
        #[arg(long, default_value_t = 16000)]
        budget: usize,
    },
    New(New),
    Edit {
        id: String,
        #[arg(long)]
        expect: u64,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        owner: Option<String>,
        #[arg(long)]
        field: Vec<String>,
        #[arg(long)]
        confidence: Option<String>,
        #[arg(long)]
        review_every: Option<u32>,
        #[arg(long)]
        last_reviewed: Option<String>,
        #[command(flatten)]
        body: Body,
    },
    Link {
        from: String,
        rel: String,
        to: String,
        #[arg(long, default_value = "")]
        why: String,
        #[arg(long)]
        expect: u64,
        #[arg(long)]
        expect_to: Option<u64>,
    },
    Unlink {
        from: String,
        rel: String,
        to: String,
        #[arg(long)]
        expect: u64,
        #[arg(long)]
        expect_to: Option<u64>,
    },
    Status {
        id: String,
        state: String,
        #[arg(long)]
        expect: u64,
    },
    #[command(hide = true)]
    Supersede {
        old: String,
        #[arg(long = "with")]
        new: String,
        #[arg(long)]
        why: String,
        #[arg(long)]
        expect: u64,
        #[arg(long)]
        expect_to: u64,
    },
    #[command(hide = true)]
    Ask {
        title: String,
        #[arg(long)]
        about: Vec<String>,
        #[arg(long)]
        owner: Option<String>,
        #[arg(long, default_value = "")]
        context: String,
        #[arg(long, default_value = "")]
        if_yes: String,
        #[arg(long, default_value = "")]
        if_no: String,
    },
    #[command(hide = true)]
    Answer {
        question: String,
        text: Option<String>,
        #[arg(long)]
        expect: u64,
        #[command(flatten)]
        body: Body,
    },
    #[command(hide = true)]
    Accept {
        id: String,
        #[arg(long)]
        expect: u64,
        #[arg(long)]
        expect_question: u64,
    },
    #[command(hide = true)]
    Inbox {
        #[arg(long)]
        owner: Option<String>,
    },
    Proposals,
    /// Inspect: review BRANCH. Accept with --review-token from that inspection.
    Review {
        action: String,
        branch: Option<String>,
        #[arg(long)]
        reason: Option<String>,
        /// Bind acceptance to the inspected proposal and main revisions.
        #[arg(long)]
        #[serde(skip_serializing_if = "Option::is_none")]
        review_token: Option<String>,
    },
    History {
        id: Option<String>,
    },
    Lint,
    #[command(hide = true)]
    Stale,
    #[command(hide = true)]
    Doctrine {
        #[arg(long = "for")]
        paths: Vec<String>,
    },
    Consumed {
        #[arg(long)]
        for_session: Option<String>,
    },
}
#[derive(Debug, Serialize, Subcommand)]
pub enum ImportCommand {
    Profiles {
        #[command(subcommand)]
        command: ProfileCommand,
    },
    Plan {
        #[command(subcommand)]
        command: PlanCommand,
    },
    Apply {
        plan: PathBuf,
    },
    Markdown {
        dir: PathBuf,
        #[arg(long)]
        rev: String,
    },
}
impl Cmd {
    pub fn mutation(&self) -> bool {
        matches!(
            self,
            Self::Init { .. }
                | Self::Batch { .. }
                | Self::Schema {
                    command: SchemaCommand::Apply { .. }
                }
                | Self::Migrate
                | Self::Import {
                    command: ImportCommand::Apply { .. } | ImportCommand::Markdown { .. }
                }
                | Self::New(_)
                | Self::Edit { .. }
                | Self::Link { .. }
                | Self::Unlink { .. }
                | Self::Status { .. }
                | Self::Supersede { .. }
                | Self::Ask { .. }
                | Self::Answer { .. }
                | Self::Accept { .. }
        ) || matches!(self,Self::Review{action,..} if action=="accept"||action=="reject")
            || matches!(
                self,
                Self::Recover { abandon: true, .. } | Self::Recover { resume: true, .. }
            )
    }
}

#[derive(Debug, Serialize, Subcommand)]
pub enum SchemaCommand {
    Show,
    Export,
    Validate {
        file: PathBuf,
    },
    Apply {
        file: PathBuf,
        #[arg(long)]
        expect: String,
    },
}

#[derive(Debug, Serialize, Subcommand)]
pub enum ProfileCommand {
    List,
    Show { name: String },
    Validate { file: PathBuf },
}
#[derive(Debug, Serialize, Subcommand)]
pub enum PlanCommand {
    Markdown {
        dir: PathBuf,
        #[arg(long)]
        rev: String,
        #[arg(long)]
        git_web_url: String,
        #[arg(long)]
        out: PathBuf,
    },
    Git {
        repo: PathBuf,
        #[arg(long)]
        root: String,
        #[arg(long)]
        rev: String,
        #[arg(long)]
        source: String,
        #[arg(
            long,
            conflicts_with = "profile_file",
            required_unless_present = "profile_file"
        )]
        profile: Option<String>,
        #[arg(long, conflicts_with = "profile", required_unless_present = "profile")]
        profile_file: Option<PathBuf>,
        #[arg(long)]
        out: PathBuf,
    },
}

#[derive(Debug, Serialize, Subcommand)]
pub enum BackupCommand {
    Create {
        #[arg(long)]
        out: PathBuf,
    },
    Restore {
        backup: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_formats_and_default_keep_engine_serialization() {
        for (flags, expected, serialized) in [
            (
                vec!["--format", "native"],
                crate::render::Format::Native,
                "\"Native\"",
            ),
            (
                vec!["--format", "liskov"],
                crate::render::Format::Liskov,
                "\"Liskov\"",
            ),
            (vec![], crate::render::Format::Native, "\"Native\""),
        ] {
            let cli = Cli::try_parse_from(
                ["scrolls", "render", "--out", "export"]
                    .into_iter()
                    .chain(flags),
            )
            .unwrap();
            let Cmd::Render { format, .. } = cli.command else {
                panic!("expected render command");
            };
            assert_eq!(format, expected);
            assert_eq!(serde_json::to_string(&format).unwrap(), serialized);
        }
    }

    #[test]
    fn list_and_search_parse_engine_filters_in_field_order() {
        for command in ["list", "search"] {
            let mut args = vec!["scrolls", command];
            if command == "search" {
                args.push("terms");
            }
            args.extend([
                "--kind", "K", "--status", "S", "--owner", "O", "--field", "a=b", "--field", "c=d",
            ]);
            let cli = Cli::try_parse_from(args).unwrap();
            let filters: crate::query::Filters = match cli.command {
                Cmd::List { filters } | Cmd::Search { filters, .. } => filters,
                _ => panic!("expected record filters"),
            };
            assert_eq!(filters.kind.as_deref(), Some("K"));
            assert_eq!(filters.status.as_deref(), Some("S"));
            assert_eq!(filters.owner.as_deref(), Some("O"));
            assert_eq!(filters.field, ["a=b", "c=d"]);
        }
    }

    #[test]
    fn beads_publish_keeps_existing_operation_identity() {
        let cli = Cli::try_parse_from(["scrolls", "beads", "publish", "work:bd-a"]).unwrap();
        let Cmd::Beads { command } = cli.command else {
            panic!("expected Beads command");
        };
        // This is the pre-extraction command representation persisted in operations.
        let expected = serde_json::json!({"Publish":{"reference":"work:bd-a","all":false}});
        assert_eq!(serde_json::to_value(&command).unwrap(), expected);
        let fingerprint =
            serde_json::json!({"command":command,"actor":"alice","revision":"previous-revision"});
        assert_eq!(
            crate::model::digest(serde_json::to_vec(&fingerprint).unwrap()),
            "310dca777221a6fe11f59e4d37625d1917ec193548caf3e186ccdf1819eaec19"
        );
    }

    #[test]
    fn beads_inputs_keep_cli_defaults_and_target_rules() {
        use serde_json::json;
        for (args, expected) in [
            (
                vec!["bind", "work", "--directory", "repo"],
                json!({"Bind":{"name":"work","directory":"repo","bd":"bd","timeout":30}}),
            ),
            (
                vec![
                    "bind",
                    "work",
                    "--directory",
                    "repo",
                    "--bd",
                    "custom",
                    "--timeout",
                    "12",
                ],
                json!({"Bind":{"name":"work","directory":"repo","bd":"custom","timeout":12}}),
            ),
            (vec!["list"], json!("List")),
            (
                vec!["refs", "work:bd-a"],
                json!({"Refs":{"reference":"work:bd-a"}}),
            ),
            (
                vec!["refresh", "work:bd-a"],
                json!({"Refresh":{"reference":"work:bd-a","all":false}}),
            ),
            (
                vec!["refresh", "--all"],
                json!({"Refresh":{"reference":null,"all":true}}),
            ),
            (
                vec!["publish", "--all"],
                json!({"Publish":{"reference":null,"all":true}}),
            ),
        ] {
            let cli = Cli::try_parse_from(["scrolls", "beads"].into_iter().chain(args)).unwrap();
            let Cmd::Beads { command } = cli.command else {
                panic!("expected Beads command");
            };
            assert_eq!(serde_json::to_value(command).unwrap(), expected);
        }
        for command in ["refresh", "publish"] {
            assert!(Cli::try_parse_from(["scrolls", "beads", command]).is_err());
            assert!(
                Cli::try_parse_from(["scrolls", "beads", command, "work:bd-a", "--all"]).is_err()
            );
        }
    }

    fn walk_direction(args: &[&str]) -> Direction {
        let cli = Cli::try_parse_from(args).unwrap();
        let Cmd::Walk { direction, .. } = cli.command else {
            panic!("expected walk command");
        };
        direction
    }

    #[test]
    fn walk_direction_incoming() {
        assert_eq!(
            walk_direction(&["scrolls", "walk", "record", "--direction", "incoming"]),
            Direction::Incoming
        );
    }

    #[test]
    fn walk_direction_outgoing() {
        assert_eq!(
            walk_direction(&["scrolls", "walk", "record", "--direction", "outgoing"]),
            Direction::Outgoing
        );
    }

    #[test]
    fn walk_direction_both() {
        assert_eq!(
            walk_direction(&["scrolls", "walk", "record", "--direction", "both"]),
            Direction::Both
        );
    }

    #[test]
    fn walk_direction_defaults_to_both() {
        assert_eq!(
            walk_direction(&["scrolls", "walk", "record"]),
            Direction::Both
        );
    }

    #[test]
    fn role_human() {
        let cli = Cli::try_parse_from(["scrolls", "--role", "human", "walk", "record"]).unwrap();
        assert_eq!(cli.role, Role::Human);
    }

    #[test]
    fn role_agent() {
        let cli = Cli::try_parse_from(["scrolls", "--role", "agent", "walk", "record"]).unwrap();
        assert_eq!(cli.role, Role::Agent);
    }

    #[test]
    fn role_defaults_to_agent() {
        let cli = Cli::try_parse_from(["scrolls", "walk", "record"]).unwrap();
        assert_eq!(cli.role, Role::Agent);
    }
}
