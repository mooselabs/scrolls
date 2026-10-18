//! Stateless revision binding, not identity authentication or proof of reading.
use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReviewToken {
    branch: String,
    main_revision: String,
    proposal_revision: String,
}
impl ReviewToken {
    pub(crate) fn new(branch: &str, main: &str, proposal: &str) -> Self {
        Self {
            branch: branch.into(),
            main_revision: main.into(),
            proposal_revision: proposal.into(),
        }
    }
    pub(crate) fn encode(&self) -> Result<String> {
        Ok(format!(
            "v1.{}",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(self)?)
        ))
    }
    pub(crate) fn decode(token: &str) -> Result<Self> {
        let (version, payload) = token.split_once('.').context("malformed review token")?;
        if version != "v1" {
            bail!("unsupported review token version {version}")
        }
        let bytes = URL_SAFE_NO_PAD
            .decode(payload)
            .context("malformed review token encoding")?;
        let value: Self =
            serde_json::from_slice(&bytes).context("malformed review token payload")?;
        // Dolt uses full 32-character base32hex hashes, not Git SHA hex hashes.
        let full_hash = |h: &str| {
            h.len() == 32
                && h.bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'v').contains(&c))
        };
        if !value.branch.starts_with("proposal/")
            || !full_hash(&value.main_revision)
            || !full_hash(&value.proposal_revision)
            || value.encode()? != token
        {
            bail!("malformed review token; expected canonical branch and full Dolt revisions")
        }
        Ok(value)
    }
    pub(crate) fn check(&self, branch: &str, main: &str, proposal: &str) -> Result<()> {
        if self.branch != branch {
            bail!(
                "review token is for {}, not {branch}; run scrolls review {branch} again",
                self.branch
            )
        }
        let mut changes = Vec::new();
        if self.main_revision != main {
            changes.push(format!(
                "main revision changed from {} to {main}",
                self.main_revision
            ));
        }
        if self.proposal_revision != proposal {
            changes.push(format!(
                "proposal revision changed from {} to {proposal}",
                self.proposal_revision
            ));
        }
        if !changes.is_empty() {
            bail!(
                "stale review token: {}; run scrolls review {branch} again and pass its new review_token",
                changes.join("; ")
            )
        }
        Ok(())
    }
}
