pub use crate::model::Role;
use crate::schema::Direction;
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
#[derive(Debug, Clone, Copy, Serialize, ValueEnum, PartialEq)]
pub enum Format {
    Native,
    Liskov,
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
pub enum BeadsCommand {
    Bind {
        name: String,
        #[arg(long)]
        directory: PathBuf,
        #[arg(long, default_value = "bd")]
        bd: PathBuf,
        #[arg(long, default_value_t = 30)]
        timeout: u64,
    },
    List,
    Refs {
        reference: String,
    },
    Refresh(Target),
    Publish(Target),
}
#[derive(Debug, Serialize, Args)]
pub struct Target {
    #[arg(required_unless_present = "all", conflicts_with = "all")]
    pub reference: Option<String>,
    #[arg(long)]
    pub all: bool,
}

#[derive(Debug, Serialize, Args, Default)]
pub struct Filters {
    #[arg(long)]
    pub kind: Option<String>,
    #[arg(long)]
    pub status: Option<String>,
    #[arg(long)]
    pub owner: Option<String>,
    #[arg(long)]
    pub field: Vec<String>,
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
