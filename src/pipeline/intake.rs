use crate::models::{ChangeType, FileArtifact};

#[derive(Debug, Default)]
pub struct Intake;

impl Intake {
    pub fn ingest_paths(paths: &[impl AsRef<str>]) -> Vec<FileArtifact> {
        paths
            .iter()
            .map(|p| FileArtifact::new(p.as_ref(), ChangeType::Modified))
            .collect()
    }

    pub fn ingest_diff(diff_text: &str) -> Vec<FileArtifact> {
        let mut artifacts = Vec::new();
        let mut current_file: Option<String> = None;
        let mut current_old_file: Option<String> = None;
        let mut current_change_type = ChangeType::Modified;
        let mut current_diff_lines = Vec::new();

        for line in diff_text.lines() {
            if line.starts_with("diff --git ") {
                if let Some(path) = current_file.take() {
                    let mut artifact = FileArtifact::new(&path, current_change_type);
                    if let Some(old) = current_old_file.take() {
                        artifact = artifact.with_old_path(old);
                    }
                    if !current_diff_lines.is_empty() {
                        artifact = artifact.with_diff(current_diff_lines.join("\n"));
                        current_diff_lines.clear();
                    }
                    artifacts.push(artifact);
                }

                current_change_type = ChangeType::Modified;
                current_old_file = None;

                // Format: diff --git a/path/to/file b/path/to/file
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 4 {
                    let old_part = parts[2].strip_prefix("a/").unwrap_or(parts[2]);
                    let new_part = parts[3].strip_prefix("b/").unwrap_or(parts[3]);
                    current_old_file = Some(old_part.to_string());
                    current_file = Some(new_part.to_string());
                }
            } else if line.starts_with("new file mode ") {
                current_change_type = ChangeType::Added;
            } else if line.starts_with("deleted file mode ") {
                current_change_type = ChangeType::Deleted;
            } else if line.starts_with("--- ") {
                let path = line.strip_prefix("--- ").unwrap_or("").trim();
                if path == "/dev/null" {
                    current_change_type = ChangeType::Added;
                } else if let Some(stripped) = path.strip_prefix("a/") {
                    current_old_file = Some(stripped.to_string());
                }
            } else if line.starts_with("+++ ") {
                let path = line.strip_prefix("+++ ").unwrap_or("").trim();
                if path == "/dev/null" {
                    current_change_type = ChangeType::Deleted;
                } else if let Some(stripped) = path.strip_prefix("b/") {
                    current_file = Some(stripped.to_string());
                }
            } else if line.starts_with("@@") || !current_diff_lines.is_empty() {
                current_diff_lines.push(line);
            }
        }

        if let Some(path) = current_file.take() {
            let mut artifact = FileArtifact::new(&path, current_change_type);
            if let Some(old) = current_old_file.take() {
                artifact = artifact.with_old_path(old);
            }
            if !current_diff_lines.is_empty() {
                artifact = artifact.with_diff(current_diff_lines.join("\n"));
            }
            artifacts.push(artifact);
        }

        artifacts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ingest_paths() {
        let paths = ["src/lib.rs", "docs/arch.md", "./Cargo.toml"];
        let artifacts = Intake::ingest_paths(&paths);
        assert_eq!(artifacts.len(), 3);
        assert_eq!(artifacts[0].path, "src/lib.rs");
        assert_eq!(artifacts[1].path, "docs/arch.md");
        assert_eq!(artifacts[2].path, "Cargo.toml");
        assert_eq!(artifacts[0].change_type, ChangeType::Modified);
    }

    #[test]
    fn test_ingest_diff_single_file() {
        let diff = r#"diff --git a/src/main.rs b/src/main.rs
index e69de29..49cc545 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -1 +1,2 @@
-fn main() {}
+fn main() {
+    println!("hello");
+}"#;

        let artifacts = Intake::ingest_diff(diff);
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].path, "src/main.rs");
        assert_eq!(artifacts[0].change_type, ChangeType::Modified);
        assert!(artifacts[0].diff.as_ref().unwrap().contains("println!(\"hello\");"));
    }

    #[test]
    fn test_ingest_diff_new_and_deleted_files() {
        let diff = r#"diff --git a/new_file.txt b/new_file.txt
new file mode 100644
--- /dev/null
+++ b/new_file.txt
@@ -0,0 +1 @@
+hello new file
diff --git a/old_file.txt b/old_file.txt
deleted file mode 100644
--- a/old_file.txt
+++ /dev/null
@@ -1 +0,0 @@
-goodbye old file"#;

        let artifacts = Intake::ingest_diff(diff);
        assert_eq!(artifacts.len(), 2);
        assert_eq!(artifacts[0].path, "new_file.txt");
        assert_eq!(artifacts[0].change_type, ChangeType::Added);
        assert_eq!(artifacts[1].path, "old_file.txt");
        assert_eq!(artifacts[1].change_type, ChangeType::Deleted);
    }
}
