use crate::models::{CanonAst, CanonCodeBlock, CanonFrontmatter, CanonHeading};
use pulldown_cmark::{Event, HeadingLevel, Parser, Tag, TagEnd};
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ValidationError {
    #[error("Failed to parse YAML frontmatter: {0}")]
    InvalidYaml(String),
    #[error("Canon file is empty: {0}")]
    EmptyCanon(String),
    #[error("Invalid canon status: '{0}'. Expected draft, active, deprecated, or experimental")]
    InvalidStatus(String),
    #[error("Canon frontmatter ID '{0}' does not match file stem '{1}'")]
    MismatchedId(String, String),
}

#[derive(Debug, Default)]
pub struct Validate;

impl Validate {
    pub fn parse_and_validate(path: &str, content: &str) -> Result<CanonAst, ValidationError> {
        let trimmed = content.trim();
        if trimmed.is_empty() {
            return Err(ValidationError::EmptyCanon(path.to_string()));
        }

        let (frontmatter, markdown_body) = Self::split_frontmatter(content)?;

        let mut ast = CanonAst::new(path, markdown_body);
        ast.frontmatter = frontmatter;

        // Validate frontmatter metadata
        if let Some(ref fm) = ast.frontmatter {
            if let Some(ref status) = fm.status {
                match status.as_str() {
                    "draft" | "active" | "deprecated" | "experimental" => {}
                    other => return Err(ValidationError::InvalidStatus(other.to_string())),
                }
            }

            if let Some(ref id) = fm.id {
                let file_stem = std::path::Path::new(path)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("");
                if !file_stem.is_empty() && id != file_stem {
                    return Err(ValidationError::MismatchedId(id.clone(), file_stem.to_string()));
                }
            }
        }

        // Parse markdown body with pulldown-cmark
        Self::extract_ast_nodes(&mut ast);

        Ok(ast)
    }

    fn split_frontmatter(
        content: &str,
    ) -> Result<(Option<CanonFrontmatter>, &str), ValidationError> {
        if !content.starts_with("---") {
            return Ok((None, content));
        }

        let rest = &content[3..];
        let rest = rest.strip_prefix('\n').or_else(|| rest.strip_prefix("\r\n")).unwrap_or(rest);

        if let Some(end_idx) = rest.find("\n---") {
            let yaml_str = &rest[..end_idx];
            let body_start = end_idx + 4; // "\n---"
            let body = &rest[body_start..];
            let body = body.strip_prefix('\n').or_else(|| body.strip_prefix("\r\n")).unwrap_or(body);

            let fm: CanonFrontmatter = serde_yaml::from_str(yaml_str)
                .map_err(|e| ValidationError::InvalidYaml(e.to_string()))?;
            Ok((Some(fm), body))
        } else {
            Ok((None, content))
        }
    }

    fn extract_ast_nodes(ast: &mut CanonAst) {
        let parser = Parser::new(&ast.raw_markdown);
        let mut current_heading_level: Option<u32> = None;
        let mut current_heading_text = String::new();

        let mut current_code_block: Option<String> = None; // lang
        let mut current_code_text = String::new();

        for event in parser {
            match event {
                Event::Start(Tag::Heading { level, .. }) => {
                    let lvl = match level {
                        HeadingLevel::H1 => 1,
                        HeadingLevel::H2 => 2,
                        HeadingLevel::H3 => 3,
                        HeadingLevel::H4 => 4,
                        HeadingLevel::H5 => 5,
                        HeadingLevel::H6 => 6,
                    };
                    current_heading_level = Some(lvl);
                    current_heading_text.clear();
                }
                Event::End(TagEnd::Heading(_)) => {
                    if let Some(level) = current_heading_level.take() {
                        ast.headings.push(CanonHeading {
                            level,
                            text: current_heading_text.trim().to_string(),
                        });
                    }
                }
                Event::Start(Tag::CodeBlock(kind)) => {
                    let lang = match kind {
                        pulldown_cmark::CodeBlockKind::Fenced(l) => {
                            let s = l.to_string();
                            if s.is_empty() {
                                None
                            } else {
                                Some(s)
                            }
                        }
                        pulldown_cmark::CodeBlockKind::Indented => None,
                    };
                    current_code_block = Some(lang.unwrap_or_default());
                    current_code_text.clear();
                }
                Event::End(TagEnd::CodeBlock) => {
                    if let Some(lang) = current_code_block.take() {
                        ast.code_blocks.push(CanonCodeBlock {
                            language: if lang.is_empty() { None } else { Some(lang) },
                            content: current_code_text.clone(),
                        });
                    }
                }
                Event::Text(t) => {
                    if current_heading_level.is_some() {
                        current_heading_text.push_str(&t);
                    }
                    if current_code_block.is_some() {
                        current_code_text.push_str(&t);
                    }
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_canon_with_frontmatter() {
        let content = r#"---
id: test-canon
title: Test Canon
status: active
triggers:
  - "src/**/*.rs"
---

# Test Heading

This is a paragraph.

```rust
fn example() {}
```
"#;

        let ast = Validate::parse_and_validate(".canons/test-canon.md", content).unwrap();
        assert_eq!(ast.canon_id(), "test-canon");
        assert_eq!(ast.headings.len(), 1);
        assert_eq!(ast.headings[0].level, 1);
        assert_eq!(ast.headings[0].text, "Test Heading");
        assert_eq!(ast.code_blocks.len(), 1);
        assert_eq!(ast.code_blocks[0].language.as_deref(), Some("rust"));
        assert!(ast.code_blocks[0].content.contains("fn example() {}"));
    }

    #[test]
    fn test_invalid_status() {
        let content = r#"---
status: unknown-status
---
# Content
"#;
        let err = Validate::parse_and_validate(".canons/rule.md", content).unwrap_err();
        assert!(matches!(err, ValidationError::InvalidStatus(_)));
    }

    #[test]
    fn test_mismatched_id() {
        let content = r#"---
id: other-id
---
# Content
"#;
        let err = Validate::parse_and_validate(".canons/rule.md", content).unwrap_err();
        assert_eq!(
            err,
            ValidationError::MismatchedId("other-id".into(), "rule".into())
        );
    }
}

