//! `amd import documents [PATH]` (ticket 079): copies a tree of documents
//! into the project's documents repository for review.
//!
//! The tree is mirrored as it is (under `--into <dir>` when given): every
//! markdown file, and every file a document links to or shows (a picture, a
//! PDF) that's inside the tree. Hidden files and folders are left out, and
//! links aren't followed. A file already in the repository is never
//! overwritten; it's skipped and listed. Names and numbers are kept, so
//! links inside the tree still work.
//!
//! Documents are reviewed through pull requests, so nothing lands on the
//! branch you're on: the files are committed on a new
//! `chore/import-documents-…` branch from the default branch, pushed, and a
//! pull request opened with `gh` when the repository is on GitHub. Your
//! clone is put back on the branch it was on.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

use crate::documents::join_link;
use crate::git;

/// What an import would do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    /// The tree being imported.
    pub source: PathBuf,
    /// The documents repository it goes into.
    pub repo: PathBuf,
    /// Files to copy: `(from, to)`, `from` relative to the source and `to`
    /// relative to the repository, `/` between parts.
    pub copies: Vec<(String, String)>,
    /// Destinations that already exist, left alone.
    pub clashes: Vec<String>,
}

/// What an import did in git.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    /// The branch the files were committed on.
    pub branch: String,
    /// The branch the import was made from (the default branch).
    pub base: String,
    /// Whether the branch reached `origin`.
    pub pushed: bool,
    /// The pull request's address, when one was opened.
    pub pull_request: Option<String>,
    /// What's left for a person to do, as commands, when amd couldn't.
    pub still_to_do: Vec<String>,
    /// Why amd couldn't push or open the pull request, if it couldn't.
    pub warning: Option<String>,
    /// The branch the clone is back on.
    pub back_on: String,
}

/// Works out what importing `source` into the documents repository `repo`
/// (under `into`, when given) would copy and what it would skip. Touches
/// nothing.
pub fn plan(source: &Path, repo: &Path, into: Option<&str>) -> Result<Plan> {
    let source = source
        .canonicalize()
        .with_context(|| format!("reading {}", source.display()))?;
    if !source.is_dir() {
        bail!("{} isn't a directory", source.display());
    }
    let repo = repo
        .canonicalize()
        .with_context(|| format!("reading {}", repo.display()))?;
    if source == repo || source.starts_with(&repo) {
        bail!(
            "{} is already in the documents repository {}",
            source.display(),
            repo.display()
        );
    }
    if repo.starts_with(&source) {
        bail!(
            "{} holds the documents repository itself; import a folder beside it",
            source.display()
        );
    }
    let into = match into.map(|i| i.trim().trim_matches('/')) {
        None | Some("") => String::new(),
        Some(into) => {
            let ok = Path::new(into).components().all(|part| {
                matches!(part, Component::Normal(name) if !name.to_string_lossy().starts_with('.'))
            });
            if !ok {
                bail!("--into {into:?} must be a folder inside the repository, like docs/imported");
            }
            into.to_string()
        }
    };

    let mut markdown = Vec::new();
    walk(&source, &source, &mut markdown)?;
    let mut files: BTreeSet<String> = markdown.iter().cloned().collect();
    for document in &markdown {
        let Ok(text) = fs::read_to_string(source.join(document)) else {
            continue;
        };
        for target in links(&text) {
            let Some(relative) = join_link(document, &target) else {
                continue;
            };
            if hidden(&relative) {
                continue;
            }
            // A plain file in the tree: not a folder, not a link.
            let is_file = fs::symlink_metadata(source.join(&relative))
                .is_ok_and(|meta| meta.file_type().is_file());
            if is_file {
                files.insert(relative);
            }
        }
    }

    let mut copies = Vec::new();
    let mut clashes = Vec::new();
    for from in files {
        let to = if into.is_empty() {
            from.clone()
        } else {
            format!("{into}/{from}")
        };
        if fs::symlink_metadata(repo.join(&to)).is_ok() {
            clashes.push(to);
        } else {
            copies.push((from, to));
        }
    }
    Ok(Plan {
        source,
        repo,
        copies,
        clashes,
    })
}

/// Every markdown file under `dir`, relative to `root`; hidden files and
/// folders left out, links not followed.
fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) -> Result<()> {
    let entries = fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))?;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if kind.is_dir() {
            walk(root, &path, out)?;
        } else if kind.is_file()
            && is_markdown(&name)
            && let Ok(relative) = path.strip_prefix(root)
        {
            out.push(slashed(relative));
        }
    }
    Ok(())
}

fn is_markdown(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".md") || lower.ends_with(".markdown")
}

fn hidden(relative: &str) -> bool {
    relative.split('/').any(|part| part.starts_with('.'))
}

fn slashed(path: &Path) -> String {
    path.components()
        .map(|part| part.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// The targets of a document's links and pictures: `[text](target)`,
/// `![alt](target)` and `[name]: target` definitions. Enough to find the
/// files a document uses; it isn't a markdown parser.
fn links(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find("](") {
        rest = &rest[at + 2..];
        let end = rest
            .find(|c: char| c == ')' || c.is_whitespace())
            .unwrap_or(rest.len());
        let target = rest[..end].trim_start_matches('<').trim_end_matches('>');
        if !target.is_empty() {
            out.push(decode(target));
        }
    }
    for line in text.lines() {
        let line = line.trim_start();
        if let Some(after) = line.strip_prefix('[')
            && let Some((label, target)) = after.split_once("]:")
            && !label.is_empty()
            && let Some(target) = target.split_whitespace().next()
        {
            out.push(decode(target.trim_start_matches('<').trim_end_matches('>')));
        }
    }
    out
}

/// `%20` and friends, as a link in markdown often has them.
fn decode(target: &str) -> String {
    let bytes = target.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(hex) = target.get(i + 1..i + 3)
            && let Ok(byte) = u8::from_str_radix(hex, 16)
        {
            out.push(byte);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Carries out `plan`: a new branch from the default branch, the files
/// copied and committed on it, pushed, and a pull request opened when the
/// repository is on GitHub and `gh` is here. The clone goes back to the
/// branch it was on. Refuses a clone with uncommitted changes, and one not
/// on a branch.
pub fn apply(plan: &Plan) -> Result<Outcome> {
    let repo = &plan.repo;
    if plan.copies.is_empty() {
        bail!("nothing to import");
    }
    let changes = git::checked(repo, &["status", "--porcelain", "--untracked-files=no"])?;
    if !changes.is_empty() {
        bail!(
            "{} has uncommitted changes; commit or stash them first",
            repo.display()
        );
    }
    let Some(original) = git::current_branch(repo) else {
        bail!("{} isn't on a branch; check one out first", repo.display());
    };
    let origin = git::remote_url(repo);
    if origin.is_some() {
        // Best effort: an import offline still makes its branch.
        let _ = git::fetch(repo);
    }
    let base = default_base(repo, &original, origin.is_some());
    let branch = new_branch_name(repo);
    git::checked(repo, &["switch", "-c", &branch, &base])?;

    let committed = copy_and_commit(plan);
    if let Err(e) = committed {
        // Leave the files where they can be seen, on the branch.
        return Err(e.context(format!("the import stopped on branch {branch}")));
    }

    let base_branch = base.strip_prefix("origin/").unwrap_or(&base).to_string();
    let mut outcome = Outcome {
        branch: branch.clone(),
        base: base_branch.clone(),
        pushed: false,
        pull_request: None,
        still_to_do: Vec::new(),
        warning: None,
        back_on: original.clone(),
    };
    match &origin {
        None => {
            outcome.warning = Some("the repository has no origin to push to".into());
            outcome
                .still_to_do
                .push(format!("git -C {} push -u origin {branch}", repo.display()));
        }
        Some(url) => match git::checked(repo, &["push", "-u", "origin", &branch]) {
            Err(e) => {
                outcome.warning = Some(format!("{e:#}"));
                outcome
                    .still_to_do
                    .push(format!("git -C {} push -u origin {branch}", repo.display()));
                outcome
                    .still_to_do
                    .push(pr_command(repo, &branch, &base_branch));
            }
            Ok(_) => {
                outcome.pushed = true;
                if url.contains("github.com") {
                    match open_pull_request(plan, &branch, &base_branch) {
                        Ok(address) => outcome.pull_request = Some(address),
                        Err(e) => {
                            outcome.warning = Some(format!("{e:#}"));
                            outcome
                                .still_to_do
                                .push(pr_command(repo, &branch, &base_branch));
                        }
                    }
                } else {
                    outcome.still_to_do.push(format!(
                        "open a pull request from {branch} into {base_branch}"
                    ));
                }
            }
        },
    }
    git::checked(repo, &["switch", &original])
        .with_context(|| format!("going back to {original}; the import is on {branch}"))?;
    Ok(outcome)
}

/// Where the import branches from: the remote's default branch when it
/// says, else the remote's copy of the current branch, else the current
/// branch.
fn default_base(repo: &Path, current: &str, has_origin: bool) -> String {
    if has_origin {
        if let Ok(head) = git::checked(repo, &["rev-parse", "--abbrev-ref", "origin/HEAD"])
            && head.starts_with("origin/")
        {
            return head;
        }
        let tracking = format!("origin/{current}");
        if git::checked(repo, &["rev-parse", "--verify", "--quiet", &tracking]).is_ok() {
            return tracking;
        }
    }
    current.to_string()
}

fn new_branch_name(repo: &Path) -> String {
    let stamp = time::OffsetDateTime::now_utc();
    let base = format!(
        "chore/import-documents-{:04}{:02}{:02}-{:02}{:02}{:02}",
        stamp.year(),
        u8::from(stamp.month()),
        stamp.day(),
        stamp.hour(),
        stamp.minute(),
        stamp.second()
    );
    let mut name = base.clone();
    let mut n = 2;
    while git::branch_exists(repo, &name) {
        name = format!("{base}-{n}");
        n += 1;
    }
    name
}

fn copy_and_commit(plan: &Plan) -> Result<()> {
    let mut added: Vec<String> = Vec::new();
    for (from, to) in &plan.copies {
        let target = plan.repo.join(to);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        fs::copy(plan.source.join(from), &target)
            .with_context(|| format!("copying {from} to {}", target.display()))?;
        added.push(to.clone());
    }
    let mut args = vec!["add", "--"];
    args.extend(added.iter().map(String::as_str));
    git::checked(&plan.repo, &args)?;
    git::checked(&plan.repo, &["commit", "-q", "-m", &commit_message(plan)])?;
    Ok(())
}

fn source_name(plan: &Plan) -> String {
    plan.source
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| plan.source.display().to_string())
}

fn commit_message(plan: &Plan) -> String {
    let count = plan.copies.len();
    format!(
        "docs: import {count} file{} from {}",
        if count == 1 { "" } else { "s" },
        source_name(plan)
    )
}

fn pr_body(plan: &Plan) -> String {
    let mut body = format!(
        "Imported with `amd import documents` from `{}`.\n\n",
        plan.source.display()
    );
    for (_, to) in &plan.copies {
        body.push_str(&format!("- `{to}`\n"));
    }
    if !plan.clashes.is_empty() {
        body.push_str("\nAlready in the repository, so left out:\n\n");
        for clash in &plan.clashes {
            body.push_str(&format!("- `{clash}`\n"));
        }
    }
    body
}

fn pr_command(repo: &Path, branch: &str, base: &str) -> String {
    format!(
        "(cd {} && gh pr create --head {branch} --base {base} --fill)",
        repo.display()
    )
}

fn open_pull_request(plan: &Plan, branch: &str, base: &str) -> Result<String> {
    let out = Command::new("gh")
        .current_dir(&plan.repo)
        .args(["pr", "create", "--head", branch, "--base", base, "--title"])
        .arg(commit_message(plan))
        .arg("--body")
        .arg(pr_body(plan))
        .stdin(Stdio::null())
        .output()
        .context("running gh pr create (is gh installed?)")?;
    if !out.status.success() {
        let detail = String::from_utf8_lossy(&out.stderr);
        bail!(
            "gh pr create failed: {}",
            detail.lines().last().unwrap_or_default()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .last()
        .unwrap_or_default()
        .trim()
        .to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_link_targets() {
        let text =
            "See [a](../adr/0001.md#x) and ![p](img/p%20one.png \"t\").\n\n[ref]: <docs/b.pdf>\n";
        assert_eq!(
            links(text),
            ["../adr/0001.md#x", "img/p one.png", "docs/b.pdf"]
        );
    }

    fn tree() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("old-docs");
        let repo = dir.path().join("p-documents");
        fs::create_dir_all(source.join("adr")).unwrap();
        fs::create_dir_all(source.join("img")).unwrap();
        fs::create_dir_all(source.join(".obsidian")).unwrap();
        fs::create_dir_all(repo.join("adr")).unwrap();
        fs::write(
            source.join("README.md"),
            "# R\n![d](img/d.png) [x](adr/0001.md)\n",
        )
        .unwrap();
        fs::write(source.join("adr/0001.md"), "# A\n").unwrap();
        fs::write(source.join("adr/0002.md"), "# B\n").unwrap();
        fs::write(source.join("img/d.png"), "png").unwrap();
        fs::write(source.join("img/unused.png"), "png").unwrap();
        fs::write(source.join("notes.txt"), "not used").unwrap();
        fs::write(source.join(".obsidian/x.md"), "# hidden").unwrap();
        fs::write(repo.join("adr/0001.md"), "# Already here\n").unwrap();
        (dir, source, repo)
    }

    #[test]
    fn mirrors_documents_and_what_they_use() {
        let (_dir, source, repo) = tree();
        let plan = plan(&source, &repo, None).unwrap();
        let to: Vec<&str> = plan.copies.iter().map(|(_, to)| to.as_str()).collect();
        assert_eq!(to, ["README.md", "adr/0002.md", "img/d.png"]);
        assert_eq!(plan.clashes, ["adr/0001.md"]);
    }

    #[test]
    fn into_a_folder() {
        let (_dir, source, repo) = tree();
        let plan = plan(&source, &repo, Some("docs/old/")).unwrap();
        assert!(plan.clashes.is_empty());
        assert!(
            plan.copies
                .iter()
                .any(|(from, to)| from == "adr/0001.md" && to == "docs/old/adr/0001.md")
        );
        assert!(super::plan(&source, &repo, Some("../out")).is_err());
        assert!(super::plan(&source, &repo, Some(".git")).is_err());
    }

    #[test]
    fn refuses_the_repository_itself() {
        let (_dir, _source, repo) = tree();
        assert!(plan(&repo, &repo, None).is_err());
        assert!(plan(&repo.join("adr"), &repo, None).is_err());
        assert!(plan(repo.parent().unwrap(), &repo, None).is_err());
    }
}
