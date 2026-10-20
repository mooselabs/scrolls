//! Link projection affects generated views only. Originals and record bodies stay unchanged.
use crate::markdown_import::Plan;
use anyhow::{Context, Result};
use pulldown_cmark::{Event, LinkType, Parser, Tag};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    ops::Range,
};
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Reference {
    pub document: String,
    pub target: String,
    pub resolved: Option<String>,
    pub url: Option<String>,
    pub byte_offset: usize,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Report {
    pub rewritten: Vec<Reference>,
    pub unresolved: Vec<Reference>,
}
impl Report {
    pub fn extend(&mut self, other: Self) {
        self.rewritten.extend(other.rewritten);
        self.unresolved.extend(other.unresolved);
    }
}
struct Targets<'a> {
    plan: &'a Plan,
    files: BTreeSet<&'a str>,
    dirs: BTreeSet<String>,
    imported: BTreeSet<&'a str>,
}
impl<'a> Targets<'a> {
    fn new(plan: &'a Plan) -> Self {
        let files = plan.inventory.iter().map(|e| e.path.as_str()).collect();
        let mut dirs = BTreeSet::new();
        for e in &plan.inventory {
            let mut path = e.path.as_str();
            while let Some((parent, _)) = path.rsplit_once('/') {
                dirs.insert(parent.into());
                path = parent;
            }
        }
        let imported = plan
            .inventory
            .iter()
            .filter(|e| {
                e.path.starts_with(&format!("{}/", plan.prefix))
                    && e.path.ends_with(".md")
                    && [0o100644, 0o100755].contains(&e.mode)
            })
            .map(|e| e.path.as_str())
            .collect();
        Self {
            plan,
            files,
            dirs,
            imported,
        }
    }
    fn resolve(&self, document: &str, target: &str, image: bool) -> Option<Reference> {
        if target.is_empty()
            || target.starts_with('#')
            || target.split('/').next().is_some_and(|s| s.contains(':'))
        {
            return None;
        }
        let split = target.find(['#', '?']).unwrap_or(target.len());
        let path = decode(&target[..split]);
        let from = format!("{}/{}", self.plan.prefix, document);
        let resolved = path
            .as_ref()
            .and_then(|p| crate::import::resolve_reference(&from, p));
        if resolved.as_deref().is_some_and(|p| {
            self.imported.contains(p)
                || (self.dirs.contains(p)
                    && self
                        .imported
                        .iter()
                        .any(|f| f.starts_with(&format!("{p}/"))))
        }) {
            return None;
        }
        let url = resolved.as_deref().and_then(|p| {
            let category = if self.files.contains(p) {
                "blob"
            } else if self.dirs.contains(p) {
                "tree"
            } else {
                return None;
            };
            let category = if image && category == "blob" {
                "raw"
            } else {
                category
            };
            Some(format!(
                "{}/{}/{}/{}{}",
                self.plan.git_web_url,
                category,
                self.plan.commit,
                encode(p),
                &target[split..]
            ))
        });
        Some(Reference {
            document: document.into(),
            target: target.into(),
            resolved,
            url,
            byte_offset: 0,
        })
    }
}
fn decode(s: &str) -> Option<String> {
    let mut bytes = vec![];
    let mut i = 0;
    let b = s.as_bytes();
    while i < b.len() {
        if b[i] == b'%' {
            bytes.push(
                u8::from_str_radix(std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?, 16).ok()?,
            );
            i += 3;
        } else if b[i] == b'\\' && b.get(i + 1).is_some_and(|c| c.is_ascii_punctuation()) {
            bytes.push(b[i + 1]);
            i += 2;
        } else {
            bytes.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(bytes).ok()
}
fn encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"/-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}
fn destination(text: &str, start: usize) -> Option<Range<usize>> {
    let bytes = text.as_bytes();
    let mut i = start;
    while bytes.get(i).is_some_and(u8::is_ascii_whitespace) {
        i += 1;
    }
    if bytes.get(i) == Some(&b'<') {
        let end = text[i + 1..].find('>')? + i + 1;
        return Some(i + 1..end);
    }
    let start = i;
    let mut depth = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if i + 1 < bytes.len() => {
                i += 2;
                continue;
            }
            b'(' => depth += 1,
            b')' if depth == 0 => break,
            b')' => depth -= 1,
            c if c.is_ascii_whitespace() && depth == 0 => break,
            _ => {}
        }
        i += 1;
    }
    Some(start..i)
}
fn suffix_valid(source: &str, range: &Range<usize>, inline: bool) -> bool {
    let end = range.end
        + usize::from(
            range.start > 0
                && source.as_bytes()[range.start - 1] == b'<'
                && source.as_bytes().get(range.end) == Some(&b'>'),
        );
    let mut rest = source[end..].trim();
    if let Some(first) = rest
        .as_bytes()
        .first()
        .copied()
        .filter(|c| b"\"'(".contains(c))
    {
        let closing = if first == b'(' { b')' } else { first };
        let bytes = rest.as_bytes();
        let mut i = 1;
        while i < bytes.len() {
            if bytes[i] == b'\\' && i + 1 < bytes.len() {
                i += 2;
                continue;
            }
            if bytes[i] == closing {
                break;
            }
            i += 1;
        }
        if i == bytes.len() {
            return false;
        }
        rest = rest[i + 1..].trim();
    }
    rest == if inline { ")" } else { "" }
}
fn source_destination(
    source: &str,
    marker: &str,
    expected: &str,
    inline: bool,
) -> Option<Range<usize>> {
    source.rmatch_indices(marker).find_map(|(n, _)| {
        let range = destination(source, n + marker.len())?;
        if !suffix_valid(source, &range, inline) {
            return None;
        }
        // Entity escapes are interpreted by CommonMark; selecting the syntactically
        // valid destination is primary. Decoded equality also handles escaped punctuation.
        let raw = &source[range.clone()];
        (decode(raw) == decode(expected) || raw.contains('&')).then_some(range)
    })
}
fn body_spans(text: &str) -> Vec<(Range<usize>, String, bool)> {
    let images: BTreeSet<String> = Parser::new(text)
        .filter_map(|e| match e {
            Event::Start(Tag::Image { dest_url, .. }) => Some(dest_url.to_string()),
            _ => None,
        })
        .collect();
    let parser = Parser::new(text);
    let mut spans = vec![];
    for (_, definition) in parser.reference_definitions().iter() {
        let range = definition.span.clone();
        let source = &text[range.clone()];
        if let Some(dest) = source_destination(source, "]:", &definition.dest, false) {
            spans.push((
                range.start + dest.start..range.start + dest.end,
                definition.dest.to_string(),
                images.contains(definition.dest.as_ref()),
            ));
        }
    }
    for (event, range) in parser.into_offset_iter() {
        let (dest_url, image) = match event {
            Event::Start(Tag::Link {
                link_type: LinkType::Inline,
                dest_url,
                ..
            }) => (dest_url, false),
            Event::Start(Tag::Image {
                link_type: LinkType::Inline,
                dest_url,
                ..
            }) => (dest_url, true),
            _ => continue,
        };
        let source = &text[range.clone()];
        if let Some(dest) = source_destination(source, "](", &dest_url, true) {
            spans.push((
                range.start + dest.start..range.start + dest.end,
                dest_url.to_string(),
                image,
            ));
        }
    }
    spans
}
/// libyaml scalar token marks retain quoting and exact offsets; only the top-level
/// source_documents value is inspected. The YAML itself is never reserialized.
fn metadata_spans(yaml: &str) -> Result<Vec<(Range<usize>, String)>> {
    use unsafe_libyaml::*;
    let mut result = vec![];
    // SAFETY: parser and token lifetimes are bracketed by initialize/delete;
    // scalar bytes are copied before deleting each token.
    unsafe {
        let mut parser = std::mem::MaybeUninit::<yaml_parser_t>::uninit();
        anyhow::ensure!(
            yaml_parser_initialize(parser.as_mut_ptr()).ok,
            "reference parser initialization"
        );
        let mut parser = parser.assume_init();
        yaml_parser_set_input_string(&mut parser, yaml.as_ptr(), yaml.len() as u64);
        let parsed = (|| -> Result<()> {
            let mut depth = 0;
            let mut key = false;
            let mut sources = false;
            loop {
                let mut token = std::mem::MaybeUninit::<yaml_token_t>::uninit();
                // Legacy metadata that is not strict YAML is preserved unchanged.
                if !yaml_parser_scan(&mut parser, token.as_mut_ptr()).ok {
                    break;
                }
                let mut token = token.assume_init();
                let ty = token.type_;
                let value = if ty == YAML_SCALAR_TOKEN {
                    Some(String::from_utf8(
                        std::slice::from_raw_parts(
                            token.data.scalar.value,
                            token.data.scalar.length as usize,
                        )
                        .to_vec(),
                    )?)
                } else {
                    None
                };
                let start = token.start_mark.index as usize;
                let end = token.end_mark.index as usize;
                match ty {
                    YAML_BLOCK_MAPPING_START_TOKEN
                    | YAML_FLOW_MAPPING_START_TOKEN
                    | YAML_BLOCK_SEQUENCE_START_TOKEN
                    | YAML_FLOW_SEQUENCE_START_TOKEN => depth += 1,
                    YAML_BLOCK_END_TOKEN
                    | YAML_FLOW_MAPPING_END_TOKEN
                    | YAML_FLOW_SEQUENCE_END_TOKEN => depth -= 1,
                    YAML_KEY_TOKEN if depth == 1 => {
                        key = true;
                        sources = false;
                    }
                    YAML_SCALAR_TOKEN if depth == 1 && key => {
                        sources = value.as_deref() == Some("source_documents");
                        key = false;
                    }
                    YAML_SCALAR_TOKEN if sources => {
                        if let Some(value) = value {
                            result.push((start..end, value));
                        }
                    }
                    _ => {}
                }
                yaml_token_delete(&mut token);
                if ty == YAML_STREAM_END_TOKEN {
                    break;
                }
            }
            Ok(())
        })();
        yaml_parser_delete(&mut parser);
        parsed?;
    }
    Ok(result)
}
fn project_with(text: &str, document: &str, targets: &Targets<'_>) -> Result<(String, Report)> {
    let (_, body) = crate::import::frontmatter(text);
    let offset = text.len() - body.len();
    let mut spans = body_spans(&body)
        .into_iter()
        .map(|(r, s, image)| (r.start + offset..r.end + offset, s, false, image))
        .collect::<Vec<_>>();
    if offset > 0 {
        let start = text.find('\n').context("frontmatter boundary")? + 1;
        let closing = text[..offset].rfind("---").context("frontmatter close")?;
        // libyaml marks count Unicode scalar values, so convert them to byte offsets.
        let yaml = &text[start..closing];
        let mut offsets: Vec<_> = yaml.char_indices().map(|(n, _)| n).collect();
        offsets.push(yaml.len());
        for (r, s) in metadata_spans(yaml)? {
            if let (Some(a), Some(b)) = (offsets.get(r.start), offsets.get(r.end)) {
                spans.push((start + a..start + b, s, true, false));
            }
        }
    }
    let mut report = Report::default();
    let mut edits = BTreeMap::new();
    for (range, target, yaml, image) in spans {
        if let Some(mut reference) = targets.resolve(document, &target, image) {
            reference.byte_offset = range.start;
            reference.target = text[range.clone()].to_string();
            if let Some(url) = &reference.url {
                edits.insert(
                    (range.start, range.end),
                    if yaml {
                        serde_json::to_string(url)?
                    } else {
                        url.clone()
                    },
                );
                report.rewritten.push(reference);
            } else {
                report.unresolved.push(reference);
            }
        }
    }
    let mut projected = text.to_string();
    let mut next = text.len();
    for ((start, end), replacement) in edits.into_iter().rev() {
        anyhow::ensure!(
            start <= end
                && end <= next
                && projected.is_char_boundary(start)
                && projected.is_char_boundary(end),
            "reference projection spans overlap"
        );
        projected.replace_range(start..end, &replacement);
        next = start;
    }
    Ok((projected, report))
}
pub struct Projector<'a>(Targets<'a>);
impl<'a> Projector<'a> {
    pub fn new(plan: &'a Plan) -> Self {
        Self(Targets::new(plan))
    }
    pub fn project(&self, text: &str, document: &str) -> Result<(String, Report)> {
        project_with(text, document, &self.0)
    }
}
pub fn project(text: &str, document: &str, plan: &Plan) -> Result<(String, Report)> {
    Projector::new(plan).project(text, document)
}
pub fn report(files: &[crate::model::SourceFile], plan: &Plan) -> Result<Report> {
    use base64::Engine;
    let targets = Targets::new(plan);
    let mut result = Report::default();
    for file in files {
        let bytes = base64::engine::general_purpose::STANDARD.decode(&file.bytes)?;
        result.extend(project_with(std::str::from_utf8(&bytes)?, &file.path, &targets)?.1);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import::GitEntry;
    fn plan() -> Plan {
        Plan {
            kind: "editable_markdown".into(),
            contract: String::new(),
            store_id: String::new(),
            main_revision: String::new(),
            schema_hash: String::new(),
            actor: String::new(),
            role: String::new(),
            directory: ".".into(),
            commit: "a".repeat(40),
            prefix: "docs".into(),
            git_web_url: "https://github.com/example/docs".into(),
            timestamp: String::new(),
            inventory: [
                "docs/knowledge/a.md",
                "docs/assets/a(b).bin",
                "docs/assets/data a.bin",
                "scripts/run.sh",
            ]
            .into_iter()
            .map(|p| GitEntry {
                path: p.into(),
                mode: 0o100644,
                oid: String::new(),
                size: 0,
            })
            .collect(),
            record_ids: vec![],
            candidate_digest: String::new(),
            summary: serde_json::Value::Null,
            digest: String::new(),
        }
    }
    #[test]
    fn spans_preserve_code_formatting_labels_titles_and_unicode_metadata() {
        let source = "---\ntitle: 'Über'\nsource_documents: ['../assets/data a.bin']\n---\n![image](../assets/a(b).bin \"title\")\n[run][r]\n\n[r]: ../../scripts/run.sh#L2 \"Run\"\n\n`[code](../../scripts/run.sh)`\n```md\n[x](../../scripts/run.sh)\n```\n[internal](a.md) [missing](../missing.json)\n";
        let (out, report) = project(source, "knowledge/a.md", &plan()).unwrap();
        assert_eq!(report.rewritten.len(), 3);
        assert_eq!(report.unresolved.len(), 1);
        assert!(out.contains("title: 'Über'"));
        assert!(out.contains("https://github.com/example/docs/raw/"));
        assert!(out.contains("https://github.com/example/docs/blob/"));
        assert!(out.contains("data%20a.bin\""));
        assert!(out.contains("a%28b%29.bin \"title\""));
        assert!(out.contains("#L2 \"Run\""));
        assert!(out.contains("`[code](../../scripts/run.sh)`"));
        assert!(out.contains("```md\n[x](../../scripts/run.sh)\n```"));
        assert!(out.contains("[internal](a.md) [missing](../missing.json)"));
    }
    #[test]
    fn delimiters_inside_titles_do_not_select_the_title_as_a_destination() {
        let source = "[x](../../scripts/run.sh \"say ](../../scripts/run.sh )\")\n\n[quoted]: ../../scripts/run.sh \"say ]: ../../scripts/run.sh\"\n[x][quoted]\n";
        let (out, report) = project(source, "knowledge/a.md", &plan()).unwrap();
        assert_eq!(report.rewritten.len(), 2);
        assert!(out.contains("\"say ](../../scripts/run.sh )\""));
        assert!(out.contains("\"say ]: ../../scripts/run.sh\""));
    }
    #[test]
    fn referenced_images_use_raw_content_in_shared_definitions() {
        let source = "![picture][asset] [open][asset]\n\n[asset]: ../assets/a(b).bin\n";
        let (out, report) = project(source, "knowledge/a.md", &plan()).unwrap();
        assert_eq!(report.rewritten.len(), 1);
        assert!(out.contains("https://github.com/example/docs/raw/"));
        assert!(out.starts_with("![picture][asset] [open][asset]"));
    }
    #[test]
    fn nested_image_reference_quotes_and_encoded_paths() {
        let source = "---\nsource_documents:\n  - '../../scripts/run.sh'\n---\n[![label](<../assets/data a.bin>)](../../scripts/run.sh)\n";
        let (out, report) = project(source, "knowledge/a.md", &plan()).unwrap();
        assert_eq!(report.rewritten.len(), 3);
        assert!(report.unresolved.is_empty());
        assert!(out.contains("![label](<https://"));
        assert_eq!(out.matches("/scripts/run.sh").count(), 2);
    }
}
