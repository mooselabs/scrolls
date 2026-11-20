//! Stateless revision binding, not identity authentication or proof of reading.
use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};

const VERSION: &str = "v2";

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReviewToken {
    proposal: String,
    main_revision: String,
    proposal_revision: String,
}
impl ReviewToken {
    pub(crate) fn issue(
        proposal: &str,
        main_revision: &str,
        proposal_revision: &str,
    ) -> Result<String> {
        Self {
            proposal: proposal.into(),
            main_revision: main_revision.into(),
            proposal_revision: proposal_revision.into(),
        }
        .encode()
    }

    pub(crate) fn require_current(
        token: &str,
        proposal: &str,
        main_revision: &str,
        proposal_revision: &str,
    ) -> Result<()> {
        Self::decode(token)
            .with_context(|| format!("run scrolls review {proposal} again for a {VERSION} token"))?
            .check(proposal, main_revision, proposal_revision)
    }

    fn encode(&self) -> Result<String> {
        Ok(format!(
            "{VERSION}.{}",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(self)?)
        ))
    }

    fn decode(token: &str) -> Result<Self> {
        let (version, payload) = token.split_once('.').context("malformed review token")?;
        if version != VERSION {
            bail!("unsupported review token version {version}")
        }
        let bytes = URL_SAFE_NO_PAD
            .decode(payload)
            .context("malformed review token encoding")?;
        let value: Self =
            serde_json::from_slice(&bytes).context("malformed review token payload")?;
        let bindings = [
            &value.proposal,
            &value.main_revision,
            &value.proposal_revision,
        ];
        if bindings
            .iter()
            .any(|s| s.is_empty() || s.chars().any(char::is_whitespace))
            || value.encode()? != token
        {
            bail!(
                "malformed review token; expected canonical encoding and nonempty bindings without whitespace"
            )
        }
        Ok(value)
    }
    fn check(&self, proposal: &str, main: &str, revision: &str) -> Result<()> {
        if self.proposal != proposal {
            bail!(
                "review token is for {}, not {proposal}; run scrolls review {proposal} again",
                self.proposal
            )
        }
        let mut changes = Vec::new();
        if self.main_revision != main {
            changes.push(format!(
                "main revision changed from {} to {main}",
                self.main_revision
            ));
        }
        if self.proposal_revision != revision {
            changes.push(format!(
                "proposal revision changed from {} to {revision}",
                self.proposal_revision
            ));
        }
        if !changes.is_empty() {
            bail!(
                "stale review token: {}; run scrolls review {proposal} again and pass its new review_token",
                changes.join("; ")
            )
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encoded(version: &str, payload: &str) -> String {
        format!("{version}.{}", URL_SAFE_NO_PAD.encode(payload))
    }

    #[test]
    fn binds_opaque_proposal_and_revisions() {
        let proposal = "draft-7";
        let main = "42";
        let revision = "a".repeat(64);
        let token = ReviewToken::issue(proposal, main, &revision).unwrap();
        assert!(token.starts_with("v2."));
        assert_eq!(
            ReviewToken::decode(&token).unwrap().encode().unwrap(),
            token
        );
        ReviewToken::require_current(&token, proposal, main, &revision).unwrap();

        for (name, base, head, expected) in [
            ("draft-8", main, revision.as_str(), "token is for draft-7"),
            (
                proposal,
                "43",
                revision.as_str(),
                "main revision changed from 42 to 43",
            ),
            (proposal, main, "next", "proposal revision changed from"),
        ] {
            let error = ReviewToken::require_current(&token, name, base, head).unwrap_err();
            let message = error.to_string();
            assert!(message.contains(expected), "{message}");
            assert!(message.contains(&format!("run scrolls review {name} again")));
        }
    }

    #[test]
    fn rejects_noncanonical_encoding_and_invalid_bindings() {
        let canonical = r#"{"proposal":"draft","main_revision":"42","proposal_revision":"43"}"#;
        for payload in [
            format!(" {canonical}"),
            canonical.replace(r#""proposal":"draft""#, r#""unknown":"draft""#),
            canonical.replace(r#""proposal":"draft","#, ""),
            canonical.replace("draft", ""),
            canonical.replace("42", "4 2"),
            canonical.replace("43", r#"4\n3"#),
            canonical.replace("draft", "draft\u{2003}"),
        ] {
            let error = ReviewToken::decode(&encoded("v2", &payload)).unwrap_err();
            assert!(error.to_string().contains("malformed"), "{error:#}");
        }
        let padded = format!("{}=", encoded("v2", canonical));
        assert!(
            ReviewToken::decode(&padded)
                .unwrap_err()
                .to_string()
                .contains("malformed")
        );
    }

    #[test]
    fn refuses_v1_with_fresh_review_instructions() {
        let payload = format!(
            r#"{{"branch":"proposal/draft","main_revision":"{}","proposal_revision":"{}"}}"#,
            "0".repeat(32),
            "1".repeat(32)
        );
        let token = encoded("v1", &payload);
        let error = ReviewToken::require_current(&token, "proposal/draft", "42", "43").unwrap_err();
        assert_eq!(
            format!("{error:#}"),
            "run scrolls review proposal/draft again for a v2 token: unsupported review token version v1"
        );
    }
}
