use crate::models::{CanonAst, FileArtifact};
use glob::Pattern;

#[derive(Debug, Default)]
pub struct Discover;

impl Discover {
    pub fn is_canon_triggered(canon: &CanonAst, artifacts: &[FileArtifact]) -> bool {
        let frontmatter = match &canon.frontmatter {
            Some(fm) => fm,
            None => return true, // Canons without frontmatter are unconditionally active
        };

        // If no triggers and no exists rules, canon applies globally
        if frontmatter.triggers.is_empty() && frontmatter.exists.is_empty() {
            return true;
        }

        // Check trigger patterns against modified artifacts
        let trigger_matched = if frontmatter.triggers.is_empty() {
            true
        } else {
            frontmatter.triggers.iter().any(|pattern_str| {
                if let Ok(pattern) = Pattern::new(pattern_str) {
                    artifacts.iter().any(|artifact| pattern.matches(&artifact.path))
                } else {
                    false
                }
            })
        };

        // Check exists patterns: at least one matching file must be present in artifacts or on disk
        let exists_matched = if frontmatter.exists.is_empty() {
            true
        } else {
            frontmatter.exists.iter().all(|pattern_str| {
                if let Ok(pattern) = Pattern::new(pattern_str) {
                    artifacts.iter().any(|artifact| pattern.matches(&artifact.path))
                } else {
                    false
                }
            })
        };

        trigger_matched && exists_matched
    }

    pub fn discover_active_canons(
        canons: &[CanonAst],
        artifacts: &[FileArtifact],
    ) -> Vec<String> {
        canons
            .iter()
            .filter(|canon| Self::is_canon_triggered(canon, artifacts))
            .map(|canon| canon.canon_id())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{CanonFrontmatter, ChangeType};

    #[test]
    fn test_unconditional_canon() {
        let canon = CanonAst::new(".canons/general.md", "# General");
        let artifacts = vec![FileArtifact::new("src/main.rs", ChangeType::Modified)];
        assert!(Discover::is_canon_triggered(&canon, &artifacts));
    }

    #[test]
    fn test_trigger_match() {
        let mut canon = CanonAst::new(".canons/rust-fmt.md", "# Rust Format");
        canon.frontmatter = Some(CanonFrontmatter {
            triggers: vec!["src/**/*.rs".to_string(), "Cargo.toml".to_string()],
            ..Default::default()
        });

        let rust_artifacts = vec![FileArtifact::new("src/models/artifact.rs", ChangeType::Added)];
        assert!(Discover::is_canon_triggered(&canon, &rust_artifacts));

        let docs_artifacts = vec![FileArtifact::new("docs/arch.md", ChangeType::Modified)];
        assert!(!Discover::is_canon_triggered(&canon, &docs_artifacts));
    }

    #[test]
    fn test_discover_active_canons() {
        let mut canon1 = CanonAst::new(".canons/rust.md", "");
        canon1.frontmatter = Some(CanonFrontmatter {
            id: Some("rust-rules".into()),
            triggers: vec!["**/*.rs".into()],
            ..Default::default()
        });

        let mut canon2 = CanonAst::new(".canons/markdown.md", "");
        canon2.frontmatter = Some(CanonFrontmatter {
            id: Some("markdown-rules".into()),
            triggers: vec!["**/*.md".into()],
            ..Default::default()
        });

        let canons = vec![canon1, canon2];
        let artifacts = vec![FileArtifact::new("docs/readme.md", ChangeType::Modified)];

        let active = Discover::discover_active_canons(&canons, &artifacts);
        assert_eq!(active, vec!["markdown-rules"]);
    }
}

