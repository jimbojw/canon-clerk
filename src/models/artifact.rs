use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChangeType {
    Added,
    Modified,
    Deleted,
    Renamed,
    Unchanged,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileArtifact {
    pub path: String,
    pub change_type: ChangeType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
}

impl FileArtifact {
    pub fn new(path: impl Into<String>, change_type: ChangeType) -> Self {
        let normalized = Self::normalize_path(&path.into());
        Self {
            path: normalized,
            change_type,
            old_path: None,
            content: None,
            diff: None,
        }
    }

    pub fn with_content(mut self, content: impl Into<String>) -> Self {
        self.content = Some(content.into());
        self
    }

    pub fn with_diff(mut self, diff: impl Into<String>) -> Self {
        self.diff = Some(diff.into());
        self
    }

    pub fn with_old_path(mut self, old_path: impl Into<String>) -> Self {
        self.old_path = Some(Self::normalize_path(&old_path.into()));
        self
    }

    pub fn normalize_path(raw: &str) -> String {
        let path = raw.replace('\\', "/");
        let stripped = path.strip_prefix("./").unwrap_or(&path);
        let trimmed = stripped.trim_matches('/');
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_path() {
        assert_eq!(FileArtifact::normalize_path("./src/main.rs"), "src/main.rs");
        assert_eq!(FileArtifact::normalize_path("src\\lib\\mod.rs"), "src/lib/mod.rs");
        assert_eq!(FileArtifact::normalize_path("/docs/arch.md/"), "docs/arch.md");
    }

    #[test]
    fn test_artifact_builder() {
        let artifact = FileArtifact::new("src/lib.rs", ChangeType::Modified)
            .with_content("pub fn init() {}")
            .with_diff("@@ -1 +1 @@")
            .with_old_path("src/old_lib.rs");

        assert_eq!(artifact.path, "src/lib.rs");
        assert_eq!(artifact.change_type, ChangeType::Modified);
        assert_eq!(artifact.content.as_deref(), Some("pub fn init() {}"));
        assert_eq!(artifact.diff.as_deref(), Some("@@ -1 +1 @@"));
        assert_eq!(artifact.old_path.as_deref(), Some("src/old_lib.rs"));
    }

    #[test]
    fn test_artifact_serialization() {
        let artifact = FileArtifact::new("src/main.rs", ChangeType::Added);
        let json = serde_json::to_string(&artifact).unwrap();
        assert!(json.contains("\"path\":\"src/main.rs\""));
        assert!(json.contains("\"change_type\":\"added\""));

        let deserialized: FileArtifact = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, artifact);
    }
}

