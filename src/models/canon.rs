use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CanonFrontmatter {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub triggers: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exists: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanonHeading {
    pub level: u32,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanonCodeBlock {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanonAst {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frontmatter: Option<CanonFrontmatter>,
    pub raw_markdown: String,
    pub headings: Vec<CanonHeading>,
    pub code_blocks: Vec<CanonCodeBlock>,
}

impl CanonAst {
    pub fn new(path: impl Into<String>, raw_markdown: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            frontmatter: None,
            raw_markdown: raw_markdown.into(),
            headings: Vec::new(),
            code_blocks: Vec::new(),
        }
    }

    pub fn canon_id(&self) -> String {
        if let Some(fm) = &self.frontmatter {
            if let Some(id) = &fm.id {
                return id.clone();
            }
        }
        std::path::Path::new(&self.path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanonViolation {
    pub rule: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanonVerdict {
    pub canon_id: String,
    pub admitted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub violations: Vec<CanonViolation>,
    pub duration_ms: u64,
}

impl CanonVerdict {
    pub fn admit(canon_id: impl Into<String>, duration_ms: u64) -> Self {
        Self {
            canon_id: canon_id.into(),
            admitted: true,
            reason: None,
            violations: Vec::new(),
            duration_ms,
        }
    }

    pub fn reject(
        canon_id: impl Into<String>,
        reason: impl Into<String>,
        violations: Vec<CanonViolation>,
        duration_ms: u64,
    ) -> Self {
        Self {
            canon_id: canon_id.into(),
            admitted: false,
            reason: Some(reason.into()),
            violations,
            duration_ms,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_canon_id_fallback() {
        let ast = CanonAst::new(".canons/no-unhandled-rejections.md", "# Title");
        assert_eq!(ast.canon_id(), "no-unhandled-rejections");

        let mut ast_with_fm = CanonAst::new(".canons/file.md", "");
        ast_with_fm.frontmatter = Some(CanonFrontmatter {
            id: Some("custom-id".into()),
            ..Default::default()
        });
        assert_eq!(ast_with_fm.canon_id(), "custom-id");
    }

    #[test]
    fn test_verdict_admit_and_reject() {
        let pass = CanonVerdict::admit("test-canon", 12);
        assert!(pass.admitted);
        assert_eq!(pass.violations.len(), 0);

        let violation = CanonViolation {
            rule: "no-todos".into(),
            file: Some("src/main.rs".into()),
            line: Some(42),
            message: "Found TODO comment".into(),
        };
        let fail = CanonVerdict::reject("test-canon", "Violations detected", vec![violation], 15);
        assert!(!fail.admitted);
        assert_eq!(fail.violations.len(), 1);
        assert_eq!(fail.violations[0].line, Some(42));
    }
}

