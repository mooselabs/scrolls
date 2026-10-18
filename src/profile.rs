//! Declarative import profile format 1. No source-controlled code is executed.
use crate::{model::digest, schema::Schema};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
pub const METADATA_LIMIT: usize = 1024 * 1024;
pub const DEPTH_LIMIT: usize = 64;
pub const PEP: &str = include_str!("../profiles/pep.yaml");
pub const HANDBOOK: &str = include_str!("../profiles/handbook.yaml");
pub fn bundled(name: &str) -> Result<&'static str> {
    match name {
        "pep" => Ok(PEP),
        "handbook" => Ok(HANDBOOK),
        _ => bail!("invalid_profile: unknown bundled profile {name}; use import profiles list"),
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub format: u32,
    pub id: String,
    pub description: String,
    pub assets: String,
    #[serde(default)]
    pub coverage: Coverage,
    pub documents: Vec<Rule>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Coverage {
    #[serde(default = "one")]
    pub min_documents: usize,
    #[serde(default)]
    pub min_by_rule: BTreeMap<String, usize>,
}
fn one() -> usize {
    1
}
impl Default for Coverage {
    fn default() -> Self {
        Self {
            min_documents: 1,
            min_by_rule: BTreeMap::new(),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub id: String,
    pub include: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
    pub codec: String,
    pub values: BTreeMap<String, Selection>,
    pub identity: Identity,
    pub record: Projection,
    #[serde(default)]
    pub references: Vec<Reference>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Selection {
    pub from: String,
    #[serde(default)]
    pub normalize: Option<String>,
    #[serde(default)]
    pub required: bool,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub default: Option<Value>,
    #[serde(default)]
    pub cases: BTreeMap<String, Value>,
    #[serde(default)]
    pub date_format: Option<String>,
}
fn present<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Option<Value>, D::Error> {
    Value::deserialize(d).map(Some)
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub key: Expression,
    #[serde(default)]
    pub aliases: Vec<Expression>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expression {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub literal: Option<Value>,
    #[serde(default)]
    pub value: Option<String>,
    #[serde(default)]
    pub template: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Projection {
    pub kind: Expression,
    pub title: Expression,
    #[serde(default)]
    pub owner: Option<Expression>,
    #[serde(default)]
    pub fields: BTreeMap<String, Expression>,
    pub body: Body,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Body {
    pub source: String,
    pub representation: String,
    #[serde(default)]
    pub language: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reference {
    pub id: String,
    pub value: String,
    pub source_relation: String,
    pub target_key: String,
    pub direction: String,
    #[serde(default)]
    pub relation: Option<String>,
    pub on_missing: String,
}

/// Scan before deserializing so aliases cannot expand, and depth is bounded before allocation.
pub fn strict_yaml(text: &str) -> Result<Value> {
    ensure!(
        text.len() <= METADATA_LIMIT,
        "input_limit: metadata/profile exceeds 1 MiB; split the source"
    );
    // SAFETY: parser/token lifetime follows libyaml's initialize/scan/delete contract;
    // input lives throughout the scan. Tokens are deleted on every successful scan.
    unsafe {
        use unsafe_libyaml::*;
        let mut p = std::mem::MaybeUninit::<yaml_parser_t>::uninit();
        ensure!(
            yaml_parser_initialize(p.as_mut_ptr()).ok,
            "parse_error: YAML parser initialization"
        );
        let mut p = p.assume_init();
        yaml_parser_set_input_string(&mut p, text.as_ptr(), text.len() as u64);
        let scan = (|| -> Result<()> {
            let mut depth = 0usize;
            loop {
                let mut token = std::mem::MaybeUninit::<yaml_token_t>::uninit();
                ensure!(
                    yaml_parser_scan(&mut p, token.as_mut_ptr()).ok,
                    "parse_error: malformed YAML at line {}",
                    p.problem_mark.line + 1
                );
                let mut token = token.assume_init();
                let ty = token.type_;
                yaml_token_delete(&mut token);
                ensure!(
                    !matches!(
                        ty,
                        YAML_ALIAS_TOKEN
                            | YAML_ANCHOR_TOKEN
                            | YAML_TAG_TOKEN
                            | YAML_TAG_DIRECTIVE_TOKEN
                    ),
                    "invalid_profile: YAML anchors, aliases and tags are forbidden"
                );
                if matches!(
                    ty,
                    YAML_BLOCK_SEQUENCE_START_TOKEN
                        | YAML_BLOCK_MAPPING_START_TOKEN
                        | YAML_FLOW_SEQUENCE_START_TOKEN
                        | YAML_FLOW_MAPPING_START_TOKEN
                ) {
                    depth += 1;
                }
                if matches!(
                    ty,
                    YAML_BLOCK_END_TOKEN
                        | YAML_FLOW_SEQUENCE_END_TOKEN
                        | YAML_FLOW_MAPPING_END_TOKEN
                ) {
                    depth = depth.saturating_sub(1);
                }
                ensure!(
                    depth <= DEPTH_LIMIT,
                    "input_limit: YAML nesting exceeds 64; simplify metadata"
                );
                if ty == YAML_STREAM_END_TOKEN {
                    break;
                }
            }
            Ok(())
        })();
        yaml_parser_delete(&mut p);
        scan?;
    }
    let yaml: serde_yaml::Value =
        serde_yaml::from_str(text).context("parse_error: invalid or duplicate YAML keys")?;
    fn convert(v: serde_yaml::Value, depth: usize) -> Result<Value> {
        ensure!(depth <= DEPTH_LIMIT, "input_limit: YAML nesting exceeds 64");
        Ok(match v {
            serde_yaml::Value::Mapping(m) => {
                let mut out = serde_json::Map::new();
                for (k, v) in m {
                    let k = k
                        .as_str()
                        .context("parse_error: metadata keys must be strings")?;
                    ensure!(k != "<<", "invalid_profile: YAML merge keys are forbidden");
                    out.insert(k.into(), convert(v, depth + 1)?);
                }
                Value::Object(out)
            }
            serde_yaml::Value::Sequence(a) => Value::Array(
                a.into_iter()
                    .map(|v| convert(v, depth + 1))
                    .collect::<Result<_>>()?,
            ),
            serde_yaml::Value::Tagged(_) => bail!("invalid_profile: tags forbidden"),
            v => serde_json::to_value(v)?,
        })
    }
    convert(yaml, 0)
}
fn glob(pattern: &str) -> Result<globset::GlobMatcher> {
    ensure!(
        !pattern.starts_with('/') && !pattern.split('/').any(|p| p == ".."),
        "invalid_profile: unsafe glob {pattern}"
    );
    Ok(globset::GlobBuilder::new(pattern)
        .literal_separator(true)
        .build()?
        .compile_matcher())
}
impl Profile {
    pub fn parse(text: &str, schema: &Schema) -> Result<Self> {
        let p: Self = serde_json::from_value(strict_yaml(text)?)
            .context("invalid_profile: invalid configuration")?;
        ensure!(
            p.format == 1
                && !p.id.is_empty()
                && p.assets == "unmatched"
                && !p.documents.is_empty()
                && p.coverage.min_documents > 0,
            "invalid_profile: require format 1, id, documents, assets: unmatched and positive minimum"
        );
        let invalid_pointer = regex::Regex::new(r"~(?:[^01]|$)")?;
        let mut ids = BTreeSet::new();
        for r in &p.documents {
            ensure!(
                !r.id.is_empty() && ids.insert(r.id.clone()),
                "invalid_profile: duplicate/empty rule {}",
                r.id
            );
            ensure!(
                !r.include.is_empty(),
                "invalid_profile: rule {} has no include patterns",
                r.id
            );
            for g in r.include.iter().chain(&r.exclude) {
                glob(g)?;
            }
            ensure!(
                ["rst_headers", "markdown_frontmatter"].contains(&r.codec.as_str()),
                "invalid_profile: unknown codec {}",
                r.codec
            );
            for (n, v) in &r.values {
                ensure!(
                    n != "source_id" && valid_name(n),
                    "invalid_profile: invalid/reserved value {n}"
                );
                ensure!(
                    v.from.starts_with('/') && !invalid_pointer.is_match(&v.from),
                    "invalid_profile: invalid JSON pointer {}",
                    v.from
                );
                ensure!(
                    v.normalize.as_deref().is_none_or(|n| [
                        "trim",
                        "unsigned_integer",
                        "unsigned_integer_list",
                        "string_list",
                        "lookup",
                        "date"
                    ]
                    .contains(&n)),
                    "invalid_profile: unknown normalizer"
                );
                ensure!(
                    (v.normalize.as_deref() == Some("lookup")) == !v.cases.is_empty(),
                    "invalid_profile: lookup requires cases; other normalizers forbid cases"
                );
                ensure!(
                    (v.normalize.as_deref() == Some("date")) == v.date_format.is_some(),
                    "invalid_profile: date requires date_format"
                );
                if let Some(f) = &v.date_format {
                    ensure!(
                        !f.is_empty()
                            && !chrono::format::StrftimeItems::new(f)
                                .any(|i| matches!(i, chrono::format::Item::Error)),
                        "invalid_profile: invalid date format"
                    );
                }
            }
            for e in std::iter::once(&r.identity.key)
                .chain(&r.identity.aliases)
                .chain([&r.record.kind, &r.record.title])
                .chain(r.record.owner.iter())
                .chain(r.record.fields.values())
            {
                e.validate(&r.values)?;
            }
            for e in std::iter::once(&r.identity.key)
                .chain(&r.identity.aliases)
                .chain([&r.record.kind, &r.record.title])
                .chain(r.record.owner.iter())
            {
                if let Some(v) = &e.literal {
                    ensure!(
                        v.as_str().is_some_and(|v| !v.trim().is_empty()),
                        "invalid_profile: identity/kind/title/owner literals must be nonempty strings"
                    );
                }
            }
            let mut possible_kinds = vec![];
            if let Some(kind) = r.record.kind.literal.as_ref() {
                possible_kinds.push(kind);
            }
            if let Some(name) = r.record.kind.value.as_ref() {
                let selection = &r.values[name];
                if selection.normalize.as_deref() == Some("lookup") {
                    possible_kinds.extend(selection.cases.values());
                }
            }
            for kind in possible_kinds {
                let k = schema.kind(
                    kind.as_str()
                        .context("invalid_profile: kind must be a string")?,
                )?;
                for (f, expression) in &r.record.fields {
                    ensure!(
                        k.additional_fields || k.fields.contains_key(f),
                        "schema_mismatch: unknown field {f}"
                    );
                    if let (Some(definition), Some(value)) = (k.fields.get(f), &expression.literal)
                    {
                        definition
                            .validate(value)
                            .with_context(|| format!("schema_mismatch: field={f}"))?;
                    }
                }
            }
            ensure!(
                r.record.body.source == "body"
                    && ["markdown", "fenced_source"]
                        .contains(&r.record.body.representation.as_str()),
                "invalid_profile: unsupported body mapping"
            );
            ensure!(
                r.record.body.representation != "markdown" || r.codec == "markdown_frontmatter",
                "invalid_profile: markdown representation requires Markdown codec"
            );
            if let Some(lang) = &r.record.body.language {
                ensure!(
                    lang.chars()
                        .all(|c| c.is_ascii_alphanumeric() || "_-".contains(c)),
                    "invalid_profile: unsafe fence language"
                );
            }
            let mut refs = BTreeSet::new();
            for x in &r.references {
                ensure!(
                    refs.insert(&x.id)
                        && r.values.contains_key(&x.value)
                        && !x.source_relation.is_empty(),
                    "invalid_profile: invalid/duplicate reference {}",
                    x.id
                );
                template_parts(&x.target_key, Some(&BTreeSet::from(["target".into()])))?;
                ensure!(
                    ["incoming", "outgoing"].contains(&x.direction.as_str())
                        && ["retain", "error"].contains(&x.on_missing.as_str()),
                    "invalid_profile: invalid reference direction or missing policy"
                );
                if let Some(rel) = &x.relation {
                    ensure!(
                        schema.relations.contains_key(rel),
                        "schema_mismatch: unknown relation {rel}"
                    );
                    let relation = &schema.relations[rel];
                    ensure!(
                        !relation.from.is_empty() && !relation.to.is_empty(),
                        "schema_mismatch: relation {rel} cannot map internal source references"
                    );
                    if let Some(kind) = r.record.kind.literal.as_ref().and_then(Value::as_str) {
                        let allowed = if x.direction == "incoming" {
                            &relation.to
                        } else {
                            &relation.from
                        };
                        ensure!(
                            allowed.iter().any(|k| k == "*" || k == kind),
                            "schema_mismatch: rule={} field={} relation {rel} forbids kind {kind} in this direction",
                            r.id,
                            x.source_relation
                        );
                    }
                }
            }
        }
        for r in p.coverage.min_by_rule.keys() {
            ensure!(
                ids.contains(r),
                "invalid_profile: unknown coverage rule {r}"
            );
        }
        Ok(p)
    }
    pub fn hash(&self) -> Result<String> {
        Ok(digest(serde_json::to_vec(&serde_json::to_value(self)?)?))
    }
    pub fn select(&self, path: &str) -> Result<Option<&Rule>> {
        let mut matched = None;
        for r in &self.documents {
            let yes = r
                .include
                .iter()
                .map(|s| glob(s))
                .collect::<Result<Vec<_>>>()?
                .iter()
                .any(|g| g.is_match(path));
            let no = r
                .exclude
                .iter()
                .map(|s| glob(s))
                .collect::<Result<Vec<_>>>()?
                .iter()
                .any(|g| g.is_match(path));
            if yes && !no {
                ensure!(
                    matched.is_none(),
                    "ambiguous_rule: {path} matches multiple document rules; narrow selectors"
                );
                matched = Some(r);
            }
        }
        Ok(matched)
    }
}
fn valid_name(n: &str) -> bool {
    !n.is_empty() && n.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}
type TemplatePart = (usize, usize, String, Option<usize>);
fn template_parts(t: &str, names: Option<&BTreeSet<String>>) -> Result<Vec<TemplatePart>> {
    let mut parts = vec![];
    let mut at = 0;
    while let Some(start) = t[at..].find(['{', '}']) {
        let start = at + start;
        ensure!(
            t.as_bytes()[start] == b'{',
            "invalid_profile: unmatched template brace"
        );
        let end = start
            + t[start..]
                .find('}')
                .context("invalid_profile: unclosed template")?;
        let spec = &t[start + 1..end];
        let (name, width) = if let Some((n, f)) = spec.split_once(':') {
            ensure!(
                f.starts_with('0') && f.ends_with('d'),
                "invalid_profile: only integer padding supported"
            );
            let w: usize = f[1..f.len() - 1]
                .parse()
                .context("invalid_profile: bad padding")?;
            ensure!(w <= 64, "invalid_profile: padding exceeds 64");
            (n, Some(w))
        } else {
            (spec, None)
        };
        ensure!(
            names.is_none_or(|names| names.contains(name)),
            "invalid_profile: undeclared template value {name}"
        );
        parts.push((start, end + 1, name.into(), width));
        at = end + 1;
    }
    Ok(parts)
}
pub fn template(t: &str, values: &BTreeMap<String, Value>) -> Result<String> {
    let parts = template_parts(t, None)?;
    let mut out = String::new();
    let mut at = 0;
    for (start, end, n, width) in parts {
        out.push_str(&t[at..start]);
        let v = values
            .get(&n)
            .with_context(|| format!("missing_value: template operand {n} is absent"))?;
        let s = if let Some(w) = width {
            format!(
                "{:0width$}",
                v.as_u64()
                    .context("parse_error: padding requires unsigned integer")?,
                width = w
            )
        } else {
            match v {
                Value::String(s) => s.clone(),
                Value::Bool(_) | Value::Number(_) => v.to_string(),
                _ => bail!("missing_value: template {n} requires scalar"),
            }
        };
        ensure!(!s.is_empty(), "missing_value: empty template component {n}");
        out.push_str(&s);
        at = end;
    }
    out.push_str(&t[at..]);
    Ok(out)
}
impl Expression {
    fn validate(&self, values: &BTreeMap<String, Selection>) -> Result<()> {
        ensure!(
            usize::from(self.literal.is_some())
                + usize::from(self.value.is_some())
                + usize::from(self.template.is_some())
                == 1,
            "invalid_profile: expression requires exactly one literal, value or template"
        );
        if let Some(n) = &self.value {
            ensure!(
                values.contains_key(n),
                "invalid_profile: undeclared value {n}"
            );
        }
        if let Some(t) = &self.template {
            let mut names: BTreeSet<_> = values.keys().cloned().collect();
            names.insert("source_id".into());
            template_parts(t, Some(&names))?;
        }
        Ok(())
    }
    pub fn eval(&self, values: &BTreeMap<String, Value>) -> Result<Option<Value>> {
        Ok(if let Some(v) = &self.literal {
            Some(v.clone())
        } else if let Some(n) = &self.value {
            values.get(n).cloned()
        } else {
            Some(json!(template(self.template.as_deref().unwrap(), values)?))
        })
    }
    pub fn text(&self, values: &BTreeMap<String, Value>) -> Result<String> {
        let v = self
            .eval(values)?
            .context("missing_value: required mapping absent")?;
        let s = v
            .as_str()
            .context("parse_error: mapping must produce a string")?;
        ensure!(
            !s.trim().is_empty(),
            "missing_value: required mapping is empty"
        );
        Ok(s.into())
    }
}
fn uint(v: &Value) -> Result<u64> {
    if let Some(n) = v.as_u64() {
        return Ok(n);
    }
    let s = v
        .as_str()
        .context("parse_error: unsigned integer required")?
        .trim();
    ensure!(
        !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit()),
        "parse_error: invalid unsigned integer {s}"
    );
    s.parse().context("parse_error: unsigned integer overflow")
}
impl Rule {
    pub fn values(&self, doc: &Value, source: &str) -> Result<BTreeMap<String, Value>> {
        let mut out = BTreeMap::from([("source_id".into(), json!(source))]);
        for (n, s) in &self.values {
            let v = doc.pointer(&s.from).or(s.default.as_ref());
            if s.required {
                ensure!(
                    v.is_some_and(|v| !v.is_null()),
                    "missing_value: rule={} field={}",
                    self.id,
                    s.from
                );
            }
            let Some(v) = v else {
                continue;
            };
            let normalized = (|| -> Result<Value> {
                Ok(match s.normalize.as_deref() {
                    None => v.clone(),
                    Some("trim") => json!(
                        v.as_str()
                            .context("parse_error: trim requires string")?
                            .trim()
                    ),
                    Some("unsigned_integer") => json!(uint(v)?),
                    Some("unsigned_integer_list") => {
                        let items = if let Some(a) = v.as_array() {
                            a.clone()
                        } else {
                            v.as_str()
                                .context("parse_error: integer list requires string or array")?
                                .split(|c: char| c == ',' || c.is_whitespace())
                                .filter(|s| !s.is_empty())
                                .map(|s| json!(s))
                                .collect()
                        };
                        json!(items.iter().map(uint).collect::<Result<Vec<_>>>()?)
                    }
                    Some("string_list") => {
                        let a = if v.is_string() {
                            vec![v.clone()]
                        } else {
                            v.as_array()
                                .context("parse_error: string_list requires string or array")?
                                .clone()
                        };
                        ensure!(
                            a.iter().all(Value::is_string),
                            "parse_error: string_list items must be strings"
                        );
                        json!(a)
                    }
                    Some("lookup") => s
                        .cases
                        .get(v.as_str().context("parse_error: lookup requires string")?)
                        .context("parse_error: unknown lookup value; extend cases")?
                        .clone(),
                    Some("date") => json!(
                        chrono::NaiveDate::parse_from_str(
                            v.as_str().context("parse_error: date requires string")?,
                            s.date_format.as_deref().unwrap()
                        )?
                        .format("%Y-%m-%d")
                        .to_string()
                    ),
                    _ => unreachable!(),
                })
            })()
            .with_context(|| format!("parse_error: field={}", s.from))?;
            out.insert(n.clone(), normalized);
        }
        Ok(out)
    }
    pub fn body(&self, text: &str) -> String {
        if self.record.body.representation == "markdown" {
            return text.into();
        }
        let fence = "`".repeat(
            text.split(|c| c != '`')
                .map(str::len)
                .max()
                .unwrap_or(0)
                .max(2)
                + 1,
        );
        format!(
            "{fence}{}\n{text}{}{fence}\n",
            self.record.body.language.as_deref().unwrap_or(""),
            if text.ends_with('\n') { "" } else { "\n" }
        )
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Decoded {
    pub document: Value,
    pub metadata: Value,
    pub spans: Vec<Value>,
    pub body: String,
}
pub fn decode(codec: &str, path: &str, bytes: &[u8]) -> Result<Decoded> {
    let text = std::str::from_utf8(bytes).context("parse_error: document must be UTF-8")?;
    let lines: Vec<_> = text.split_inclusive('\n').collect();
    let mut end = 0;
    let mut spans = vec![];
    let (key, metadata) = if codec == "rst_headers" {
        let mut headers = serde_json::Map::new();
        let mut current: Option<String> = None;
        let mut terminated = false;
        for (line_no, line) in lines.iter().enumerate() {
            let raw = line.trim_end_matches(['\r', '\n']);
            let start = end;
            end += line.len();
            ensure!(
                end <= METADATA_LIMIT,
                "input_limit: header metadata exceeds 1 MiB"
            );
            if raw.is_empty() {
                terminated = true;
                break;
            }
            if raw.starts_with([' ', '\t']) {
                let k = current.as_ref().with_context(|| {
                    format!("parse_error: orphan header continuation span={start}..{end}")
                })?;
                let old = headers[k].as_str().unwrap();
                headers.insert(k.clone(), json!(format!("{old} {}", raw.trim())));
                spans.push(json!({"name":k,"line":line_no+1,"start":start,"end":end,"raw":raw}));
            } else {
                let (n, v) = raw.split_once(':').with_context(|| {
                    format!("parse_error: malformed header span={start}..{end}")
                })?;
                ensure!(
                    !n.is_empty() && n.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-'),
                    "parse_error: invalid header name span={start}..{end}"
                );
                let k = n.to_ascii_lowercase();
                ensure!(
                    !headers.contains_key(&k),
                    "parse_error: duplicate header field={n} span={start}..{end}"
                );
                headers.insert(k.clone(), json!(v.trim()));
                spans.push(json!({"name":n,"line":line_no+1,"start":start,"end":end,"raw":raw}));
                current = Some(k);
            }
        }
        ensure!(terminated, "parse_error: missing header terminator");
        ("headers", Value::Object(headers))
    } else {
        ensure!(
            codec == "markdown_frontmatter",
            "invalid_profile: unsupported codec"
        );
        if lines
            .first()
            .is_some_and(|s| s.trim_end_matches(['\r', '\n']) == "---")
        {
            end = lines[0].len();
            let start = end;
            let mut close = None;
            for (line_no, line) in lines.iter().enumerate().skip(1) {
                ensure!(
                    end - start <= METADATA_LIMIT,
                    "input_limit: frontmatter exceeds 1 MiB"
                );
                if line.trim_end_matches(['\r', '\n']) == "---" {
                    close = Some(end);
                    end += line.len();
                    break;
                }
                spans.push(json!({"line":line_no+1,"start":end,"end":end+line.len(),"raw":line}));
                end += line.len();
            }
            let close = close.context("parse_error: unterminated frontmatter")?;
            let meta = strict_yaml(&text[start..close])?;
            ensure!(
                meta.is_object(),
                "parse_error: frontmatter must be an object"
            );
            ("frontmatter", meta)
        } else {
            ("frontmatter", json!({}))
        }
    };
    let body = text[end..].to_string();
    let stem = std::path::Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .context("parse_error: invalid path")?;
    let mut document = json!({"path":path,"stem":stem,"body":body});
    document[key] = metadata.clone();
    Ok(Decoded {
        document,
        metadata,
        spans,
        body,
    })
}
