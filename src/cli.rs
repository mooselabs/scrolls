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
#[derive(Clone, Copy, Debug, Serialize, ValueEnum, PartialEq)]
pub enum Role {
    Human,
    Agent,
}
impl Role {
    pub fn name(self) -> &'static str {
        match self {
            Self::Human => "human",
            Self::Agent => "agent",
        }
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
    #[arg(long, default_value = "medium")]
    pub confidence: String,
    #[arg(long, default_value_t = 90)]
    pub review_every: u32,
    #[arg(long)]
    pub about: Vec<String>,
    #[arg(long)]
    pub supports: Vec<String>,
    #[arg(long)]
    pub field: Vec<String>,
}
#[derive(Debug, Serialize, Subcommand)]
pub enum Cmd {
    Init,
    Doctor,
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
        #[arg(long)]
        kind: Option<String>,
        #[arg(long)]
        status: Option<String>,
        #[arg(long)]
        about: Option<String>,
    },
    Show {
        id: String,
    },
    Search {
        terms: String,
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    Walk {
        id: String,
        #[arg(long)]
        rel: Option<String>,
        #[arg(long, default_value_t = 2)]
        depth: usize,
    },
    Context {
        id: String,
        #[arg(long, default_value_t = 16000)]
        budget: usize,
    },
    Prime {
        #[arg(long = "for")]
        paths: Vec<String>,
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
    Answer {
        question: String,
        text: Option<String>,
        #[arg(long)]
        expect: u64,
        #[command(flatten)]
        body: Body,
    },
    Accept {
        id: String,
        #[arg(long)]
        expect: u64,
        #[arg(long)]
        expect_question: u64,
    },
    Inbox {
        #[arg(long)]
        owner: Option<String>,
    },
    Proposals,
    /// Inspect: review BRANCH. Decide: review accept|reject BRANCH.
    Review {
        action: String,
        branch: Option<String>,
        #[arg(long)]
        reason: Option<String>,
    },
    History {
        id: Option<String>,
    },
    Lint,
    Stale,
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
            Self::Init
                | Self::Import { .. }
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
    }
}
