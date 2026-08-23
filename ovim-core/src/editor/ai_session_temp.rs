use crate::ai::path_policy::canonicalize_or_normalize;
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

/// Exact temporary files created by the active chat session.
///
/// This is intentionally an exact-file registry, not an allowlist for the
/// system temp directory. File identity is revalidated on Unix so replacing a
/// recorded path does not inherit the session's authority.
#[derive(Debug, Default)]
pub(super) struct SessionTempFiles {
    files: HashMap<PathBuf, TempFileIdentity>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TempFileIdentity {
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

impl TempFileIdentity {
    fn read(path: &Path) -> Option<Self> {
        let metadata = std::fs::metadata(path).ok()?;
        if !metadata.is_file() {
            return None;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Some(Self {
                device: metadata.dev(),
                inode: metadata.ino(),
            })
        }
        #[cfg(not(unix))]
        {
            Some(Self {})
        }
    }
}

impl SessionTempFiles {
    pub(super) fn record(&mut self, path: &Path) -> bool {
        let Some(path) = canonical_temp_file(path) else {
            return false;
        };
        let Some(identity) = TempFileIdentity::read(&path) else {
            return false;
        };
        self.files.insert(path, identity);
        true
    }

    pub(super) fn contains(&self, path: &Path) -> bool {
        let Some(path) = canonical_temp_file(path) else {
            return false;
        };
        let Some(expected) = self.files.get(&path) else {
            return false;
        };
        TempFileIdentity::read(&path).as_ref() == Some(expected)
    }

    /// Return true for the narrow shell forms needed to make or run a
    /// session-created temp file. General shell programs still use normal auto
    /// mode because an owned file argument must not authorize unrelated
    /// external effects in the same command.
    pub(super) fn authorizes_shell_command(&self, command: &str, project_root: &Path) -> bool {
        if command.chars().any(|character| {
            matches!(
                character,
                '|' | '&' | ';' | '>' | '<' | '\n' | '`' | '$' | '(' | ')'
            )
        }) {
            return false;
        }
        let Some(words) = shlex::split(command) else {
            return false;
        };
        let Some(program) = words.first() else {
            return false;
        };
        let program_path = Path::new(program);
        if program_path.is_absolute() && self.contains(program_path) {
            return words[1..]
                .iter()
                .all(|word| self.argument_stays_in_scope(word, project_root));
        }

        let program_name = program_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(program);
        match program_name {
            "chmod" => {
                let Some((mode, targets)) = words[1..].split_first() else {
                    return false;
                };
                is_simple_chmod_mode(mode)
                    && !targets.is_empty()
                    && targets
                        .iter()
                        .all(|target| self.contains(Path::new(target)))
            }
            "sh" | "bash" | "zsh" | "python" | "python3" | "node" | "ruby" | "perl" => {
                let Some(script) = words[1..].iter().find(|word| !word.starts_with('-')) else {
                    return false;
                };
                self.contains(Path::new(script))
                    && words
                        .iter()
                        .skip_while(|word| *word != script)
                        .skip(1)
                        .all(|word| self.argument_stays_in_scope(word, project_root))
            }
            _ => false,
        }
    }

    fn argument_stays_in_scope(&self, word: &str, project_root: &Path) -> bool {
        let candidate = word.split_once('=').map_or(word, |(_, value)| value);
        let path = Path::new(candidate);
        if path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            return false;
        }
        if !path.is_absolute() {
            return true;
        }
        let path = canonicalize_or_normalize(path);
        path.starts_with(canonicalize_or_normalize(project_root)) || self.contains(&path)
    }
}

fn is_simple_chmod_mode(mode: &str) -> bool {
    (!mode.is_empty() && mode.chars().all(|character| character.is_ascii_digit()))
        || (mode.contains(['+', '-', '='])
            && mode.chars().all(|character| {
                matches!(
                    character,
                    'u' | 'g' | 'o' | 'a' | '+' | '-' | '=' | 'r' | 'w' | 'x' | 'X' | 's' | 't'
                )
            }))
}

/// Snapshot the literal temp paths named by a shell command. Only paths that
/// did not exist before the command and are regular files afterwards are
/// returned. This avoids scanning shared temp directories or attributing
/// another process's unrelated files to the agent session.
pub(super) struct TempPathProbe {
    missing_candidates: Vec<PathBuf>,
}

impl TempPathProbe {
    pub(super) fn for_shell_command(command: &str) -> Self {
        let missing_candidates = shell_temp_path_candidates(command)
            .into_iter()
            .filter(|path| !path.exists())
            .collect();
        Self { missing_candidates }
    }

    pub(super) fn created_files(self) -> Vec<PathBuf> {
        self.missing_candidates
            .into_iter()
            .filter_map(|path| canonical_temp_file(&path))
            .collect()
    }
}

fn shell_temp_path_candidates(command: &str) -> Vec<PathBuf> {
    let mut candidates = BTreeSet::new();
    for token in crate::ai::auto_mode::shell_tokens(command) {
        let mut values = vec![token.as_str()];
        if let Some((_, value)) = token.split_once('=') {
            values.push(value);
        }
        for value in values {
            let value = value.trim_matches(|character: char| {
                matches!(character, ',' | ':' | '[' | ']' | '{' | '}')
            });
            let expanded = expand_temp_variable(value);
            let path = Path::new(&expanded);
            if path.is_absolute() && is_temp_location(path) {
                candidates.insert(canonicalize_or_normalize(path));
            }
        }
    }
    candidates.into_iter().collect()
}

fn expand_temp_variable(value: &str) -> String {
    for prefix in ["$TMPDIR", "${TMPDIR}", "$TEMP", "${TEMP}", "$TMP", "${TMP}"] {
        if let Some(suffix) = value.strip_prefix(prefix) {
            return std::env::temp_dir()
                .join(suffix.trim_start_matches(['/', '\\']))
                .to_string_lossy()
                .into_owned();
        }
    }
    value.to_string()
}

fn canonical_temp_file(path: &Path) -> Option<PathBuf> {
    let path = path.canonicalize().ok()?;
    (path.is_file() && is_temp_location(&path)).then_some(path)
}

pub(super) fn is_temp_location(path: &Path) -> bool {
    let path = canonicalize_or_normalize(path);
    temp_roots()
        .into_iter()
        .any(|root| path != root && path.starts_with(root))
}

fn temp_roots() -> Vec<PathBuf> {
    let mut roots = BTreeSet::new();
    roots.insert(canonicalize_or_normalize(&std::env::temp_dir()));
    #[cfg(unix)]
    for conventional in ["/tmp", "/var/tmp", "/private/tmp"] {
        let path = Path::new(conventional);
        if path.is_dir() {
            roots.insert(canonicalize_or_normalize(path));
        }
    }
    roots.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn records_only_exact_regular_temp_files() {
        let directory = tempfile::tempdir_in(std::env::temp_dir()).unwrap();
        let file = directory.path().join("owned.sh");
        fs::write(&file, "#!/bin/sh\n").unwrap();
        let sibling = directory.path().join("not-owned.sh");
        fs::write(&sibling, "#!/bin/sh\n").unwrap();

        let mut files = SessionTempFiles::default();
        assert!(files.record(&file));
        assert!(files.contains(&file));
        assert!(!files.contains(&sibling));
        assert!(!files.contains(directory.path()));
    }

    #[cfg(unix)]
    #[test]
    fn replacing_a_recorded_inode_revokes_authority() {
        let directory = tempfile::tempdir_in(std::env::temp_dir()).unwrap();
        let file = directory.path().join("owned.sh");
        fs::write(&file, "old\n").unwrap();
        let mut files = SessionTempFiles::default();
        assert!(files.record(&file));

        fs::remove_file(&file).unwrap();
        fs::write(&file, "replacement\n").unwrap();
        assert!(!files.contains(&file));
    }

    #[cfg(unix)]
    #[test]
    fn canonical_temp_aliases_share_the_same_authority() {
        let directory = tempfile::tempdir_in(std::env::temp_dir()).unwrap();
        let file = directory.path().join("owned.sh");
        fs::write(&file, "#!/bin/sh\n").unwrap();
        let alias = directory.path().with_extension("alias");
        std::os::unix::fs::symlink(directory.path(), &alias).unwrap();
        let alias_file = alias.join("owned.sh");

        let mut files = SessionTempFiles::default();
        assert!(files.record(&file));
        assert!(files.contains(&alias_file));
        fs::remove_file(alias).unwrap();
    }

    #[test]
    fn probe_attributes_only_new_literal_temp_files() {
        let directory = tempfile::tempdir_in(std::env::temp_dir()).unwrap();
        let existing = directory.path().join("existing");
        let created = directory.path().join("created");
        fs::write(&existing, "before\n").unwrap();
        let command = format!(
            "printf after > {} && printf new > {}",
            existing.display(),
            created.display()
        );
        let probe = TempPathProbe::for_shell_command(&command);
        fs::write(&existing, "after\n").unwrap();
        fs::write(&created, "new\n").unwrap();

        assert_eq!(probe.created_files(), vec![created.canonicalize().unwrap()]);
    }

    #[test]
    fn authorizes_only_narrow_owned_file_shell_forms() {
        let directory = tempfile::tempdir_in(std::env::temp_dir()).unwrap();
        let script = directory.path().join("owned.sh");
        fs::write(&script, "#!/bin/sh\n").unwrap();
        let mut files = SessionTempFiles::default();
        assert!(files.record(&script));
        let project = tempfile::tempdir().unwrap();

        assert!(files
            .authorizes_shell_command(&format!("chmod +x {}", script.display()), project.path()));
        assert!(files.authorizes_shell_command(&script.display().to_string(), project.path()));
        assert!(files.authorizes_shell_command(
            &format!("bash {} --check", script.display()),
            project.path()
        ));
        assert!(!files.authorizes_shell_command(
            &format!("{}; curl https://example.test", script.display()),
            project.path()
        ));
        assert!(!files.authorizes_shell_command(
            &format!("chmod +x {}/unowned", directory.path().display()),
            project.path()
        ));
        assert!(!files.authorizes_shell_command(
            &format!("chmod --reference=/etc/passwd {}", script.display()),
            project.path()
        ));
        assert!(!files
            .authorizes_shell_command(&format!("{} ../outside", script.display()), project.path()));
    }
}
