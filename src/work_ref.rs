//! Portable external identities. Local aliases never enter the reviewed graph.
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct BeadRef {
    pub project_id: String,
    pub issue_id: String,
}
impl BeadRef {
    pub fn parse(s: &str) -> Result<Self> {
        let Some((project, issue)) = s.strip_prefix("beads://").and_then(|s| s.split_once('/'))
        else {
            bail!("expected beads://PROJECT-UUID/FULL-ISSUE-ID");
        };
        let project_id = uuid::Uuid::parse_str(project)?.to_string();
        if issue.is_empty()
            || issue.len() > 128
            || !issue
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
            || !issue
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphanumeric())
        {
            bail!("invalid Beads issue ID");
        }
        Ok(Self {
            project_id,
            issue_id: issue.into(),
        })
    }
    pub fn uri(&self) -> String {
        format!("beads://{}/{}", self.project_id, self.issue_id)
    }
}
pub fn relation(rel: &str) -> bool {
    matches!(
        rel,
        "related_work" | "implemented_by" | "produced_by" | "blocks"
    )
}
pub fn document_uri(store: &str, id: &str) -> String {
    format!("scrolls://{store}/{id}")
}
