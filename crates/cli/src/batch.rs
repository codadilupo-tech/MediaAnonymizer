//! Input discovery and output path planning.

use anyhow::{bail, Context, Result};
use mediaanon_core::media;
use std::path::{Path, PathBuf};

/// One unit of work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub input: PathBuf,
    pub output: PathBuf,
}

/// How outputs are named.
#[derive(Debug, Clone)]
pub enum Destination {
    /// Next to the input, with a suffix before the extension.
    Suffix(String),
    /// Replace the input.
    InPlace,
    /// A single output file (exactly one input file).
    File(PathBuf),
    /// Mirror the inputs into a directory.
    Directory(PathBuf),
}

impl Destination {
    pub fn new(output: Option<&Path>, suffix: &str, in_place: bool, inputs: &[PathBuf]) -> Result<Self> {
        if in_place {
            return Ok(Destination::InPlace);
        }
        let Some(out) = output else {
            if suffix.is_empty() {
                bail!("an empty --suffix would overwrite the inputs; use --in-place to do that on purpose");
            }
            return Ok(Destination::Suffix(suffix.to_string()));
        };
        let single_file = inputs.len() == 1 && inputs[0].is_file();
        let looks_like_file = out.extension().is_some() && !out.is_dir();
        Ok(if single_file && looks_like_file {
            Destination::File(out.to_path_buf())
        } else {
            Destination::Directory(out.to_path_buf())
        })
    }

    /// Output path for `input`, found under the user-given `root`.
    fn output_for(&self, input: &Path, root: &Path) -> PathBuf {
        match self {
            Destination::InPlace => input.to_path_buf(),
            Destination::File(f) => f.clone(),
            Destination::Suffix(s) => with_suffix(input, s),
            Destination::Directory(dir) => {
                let rel = if root.is_dir() {
                    input.strip_prefix(root).unwrap_or(input)
                } else {
                    Path::new(input.file_name().unwrap_or_default())
                };
                dir.join(rel)
            }
        }
    }
}

/// `photo.jpg` + `.anon` → `photo.anon.jpg`.
pub fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let name = match path.extension() {
        Some(ext) => format!("{stem}{suffix}.{}", ext.to_string_lossy()),
        None => format!("{stem}{suffix}"),
    };
    path.with_file_name(name)
}

/// Expands the inputs into jobs. Files given explicitly are always included;
/// directories contribute files with a supported extension, skipping our own
/// outputs (names containing the suffix).
pub fn plan(inputs: &[PathBuf], recursive: bool, dest: &Destination) -> Result<Vec<Job>> {
    let mut jobs = Vec::new();
    for root in inputs {
        if root.is_file() {
            jobs.push(Job { input: root.clone(), output: dest.output_for(root, root) });
            continue;
        }
        if !root.is_dir() {
            bail!("{}: no such file or directory", root.display());
        }
        let depth = if recursive { usize::MAX } else { 1 };
        let walker = walkdir::WalkDir::new(root).max_depth(depth).sort_by_file_name();
        for entry in walker {
            let entry = entry.with_context(|| format!("cannot read {}", root.display()))?;
            let p = entry.path();
            if !entry.file_type().is_file() || !media::has_supported_extension(p) || is_hidden(p) {
                continue;
            }
            if let Destination::Suffix(s) = dest {
                if p.file_stem().is_some_and(|n| n.to_string_lossy().ends_with(s.as_str())) {
                    continue;
                }
            }
            jobs.push(Job { input: p.to_path_buf(), output: dest.output_for(p, root) });
        }
    }
    Ok(jobs)
}

fn is_hidden(p: &Path) -> bool {
    p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suffix_naming() {
        assert_eq!(with_suffix(Path::new("/x/a.b.jpg"), ".anon"), PathBuf::from("/x/a.b.anon.jpg"));
        assert_eq!(with_suffix(Path::new("noext"), "_x"), PathBuf::from("noext_x"));
    }

    #[test]
    fn plans_directories() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir(root.join("sub")).unwrap();
        for f in ["a.jpg", "a.anon.jpg", "notes.txt", ".hidden.png", "sub/b.mp4"] {
            std::fs::write(root.join(f), b"x").unwrap();
        }
        let dest = Destination::Suffix(".anon".into());
        let flat = plan(&[root.to_path_buf()], false, &dest).unwrap();
        assert_eq!(flat, vec![Job { input: root.join("a.jpg"), output: root.join("a.anon.jpg") }]);

        let out = root.join("out");
        let dest = Destination::Directory(out.clone());
        let deep = plan(&[root.to_path_buf()], true, &dest).unwrap();
        let outputs: Vec<_> = deep.iter().map(|j| j.output.clone()).collect();
        assert_eq!(outputs, vec![out.join("a.anon.jpg"), out.join("a.jpg"), out.join("sub/b.mp4")]);
    }
}
