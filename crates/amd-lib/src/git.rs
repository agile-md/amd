//! Thin wrappers over the `git` CLI.
//!
//! agile-md deliberately shells out rather than linking a git library: the
//! behaviour is then exactly what the user would get typing the command, and
//! the binary stays small and dependency-free on the git side.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};

/// Whether git's own output is kept off the terminal. The command line wants
/// to show it (`Switched to a new branch ...` is part of what `amd todo`
/// says); a full-screen front end can't have a child process writing over its
/// display, so it turns this on and gets the failure text back in the error
/// instead.
static QUIET: AtomicBool = AtomicBool::new(false);

/// Keep git's stdout and stderr off the terminal from now on, for the whole
/// process. Failures still carry git's own message.
pub fn set_quiet(quiet: bool) {
    QUIET.store(quiet, Ordering::Relaxed);
}

/// Run a git command whose output a person might want to see. Returns whether
/// it succeeded and, when output is being kept quiet, git's stderr (so the
/// reason a push failed isn't lost along with the noise).
fn run(command: &mut Command) -> std::io::Result<(bool, String)> {
    if QUIET.load(Ordering::Relaxed) {
        let out = command.stdin(Stdio::null()).output()?;
        let detail = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Ok((out.status.success(), detail))
    } else {
        Ok((command.status()?.success(), String::new()))
    }
}

/// `"git X failed"`, plus git's own reason when there is one.
fn failed(what: &str, detail: &str) -> String {
    match detail.lines().last() {
        Some(reason) if !reason.is_empty() => format!("{what} failed: {reason}"),
        _ => format!("{what} failed"),
    }
}
use std::time::SystemTime;

use anyhow::{Context, Result, bail};

/// The work-tree root of the repository containing the current directory.
pub fn repo_root() -> Option<PathBuf> {
    let out = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let root = String::from_utf8(out.stdout).ok()?;
    let root = root.trim();
    if root.is_empty() {
        None
    } else {
        Some(PathBuf::from(root))
    }
}

/// Is `file` tracked by the repository at `repo`?
pub fn is_tracked(repo: &Path, file: &Path) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["ls-files", "--error-unmatch"])
        .arg(file)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// `git mv` — the rename is then part of the history, so `git log --follow`
/// reconstructs when a task started and finished.
pub fn mv(repo: &Path, from: &Path, to: &Path) -> Result<()> {
    let (ok, detail) = run(Command::new("git")
        .arg("-C")
        .arg(repo)
        .arg("mv")
        .arg(from)
        .arg(to))
    .context("running git mv")?;
    if !ok {
        bail!(failed(
            &format!("git mv {} {}", from.display(), to.display()),
            &detail
        ));
    }
    Ok(())
}

/// The checked-out branch, or `None` on a detached HEAD.
pub fn current_branch(repo: &Path) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["branch", "--show-current"])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let branch = String::from_utf8(out.stdout).ok()?;
    let branch = branch.trim();
    if branch.is_empty() {
        None
    } else {
        Some(branch.to_string())
    }
}

pub fn branch_exists(repo: &Path, name: &str) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["show-ref", "--verify", "--quiet"])
        .arg(format!("refs/heads/{name}"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Switch to `name`, creating it from the current HEAD when `create` is set.
/// Any staged changes (such as the `git mv` of the task itself) come along.
pub fn switch_branch(repo: &Path, name: &str, create: bool) -> Result<()> {
    let mut command = Command::new("git");
    command.arg("-C").arg(repo).arg("switch");
    if create {
        command.arg("-c");
    }
    let (ok, detail) =
        run(command.arg(name)).with_context(|| format!("running git switch {name}"))?;
    if !ok {
        bail!(failed(&format!("git switch {name}"), &detail));
    }
    Ok(())
}

/// Drop a file from the index while leaving it on disk. Used when a ticket is
/// archived: the archive is gitignored, so `git mv` would refuse, and the
/// history should record the ticket leaving the board.
pub fn untrack(repo: &Path, file: &Path) -> Result<()> {
    let status = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rm", "--cached", "--quiet", "--"])
        .arg(file)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("running git rm --cached")?;
    if !status.success() {
        bail!("git rm --cached {} failed", file.display());
    }
    Ok(())
}

/// Commits on `name` that aren't reachable from the current `HEAD` — zero
/// means the branch adds nothing beyond wherever `HEAD` already is, which is
/// what "empty" means for cleaning up an abandoned ticket's branch.
pub fn commits_ahead(repo: &Path, name: &str) -> Option<u32> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-list", "--count"])
        .arg(format!("HEAD..{name}"))
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout).ok()?.trim().parse().ok()
}

/// Whether `name` has a matching branch on `origin` — if so, someone else
/// might be looking at it, so cleanup should leave it alone.
pub fn branch_pushed(repo: &Path, name: &str) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["show-ref", "--verify", "--quiet"])
        .arg(format!("refs/remotes/origin/{name}"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Delete a local branch. `-d`, not `-D`: callers are expected to have
/// already checked the branch is empty, and the safe form refusing an
/// unmerged branch is a second line of defence worth keeping.
pub fn delete_branch(repo: &Path, name: &str) -> Result<()> {
    let status = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["branch", "-d", name])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .with_context(|| format!("running git branch -d {name}"))?;
    if !status.success() {
        bail!("git branch -d {name} failed");
    }
    Ok(())
}

/// The `origin` remote's URL, if the repository has one.
pub fn remote_url(repo: &Path) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["remote", "get-url", "origin"])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let url = String::from_utf8(out.stdout).ok()?;
    let url = url.trim();
    if url.is_empty() {
        None
    } else {
        Some(url.to_string())
    }
}

/// What `repo`'s `origin` points at, in a form two clones of one remote
/// agree on (ticket 095): [`normalise_remote`], with SSH host aliases
/// resolved by `ssh -G`. A local path (a bare repository on this machine)
/// is canonicalised, relative to `repo` when it's relative. `None` with no
/// `origin`.
pub fn remote_identity(repo: &Path) -> Option<String> {
    let url = remote_url(repo)?;
    let local = url.strip_prefix("file://").unwrap_or(&url);
    if is_local_path(local) {
        let path = repo.join(local);
        return Some(path.canonicalize().unwrap_or(path).display().to_string());
    }
    Some(normalise_remote(&url, ssh_hostname))
}

/// A remote URL as `host/owner/repo`: lowercased; scheme, user, port and a
/// trailing `.git` or `/` dropped; the scp form `user@host:owner/repo`
/// treated as SSH. An SSH host goes through `resolve` (an alias from
/// `~/.ssh/config`, such as `github.com-oldippy`, becomes `github.com`);
/// whatever it can't resolve is kept as written.
pub fn normalise_remote(url: &str, resolve: impl Fn(&str) -> Option<String>) -> String {
    let url = url.trim();
    let lower = url.to_lowercase();
    let schemes = [
        ("ssh://", true),
        ("git+ssh://", true),
        ("https://", false),
        ("http://", false),
        ("git://", false),
    ];
    let (ssh, authority, path) = match schemes
        .iter()
        .find_map(|(scheme, ssh)| lower.strip_prefix(scheme).map(|rest| (*ssh, rest)))
    {
        Some((ssh, rest)) => {
            let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
            (ssh, authority, path)
        }
        None => {
            let (authority, path) = lower.split_once(':').unwrap_or((&lower, ""));
            (true, authority, path)
        }
    };
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let host = host.split_once(':').map_or(host, |(host, _)| host);
    let host = if ssh {
        resolve(host).map_or_else(|| host.to_string(), |real| real.to_lowercase())
    } else {
        host.to_string()
    };
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    format!("{host}/{path}")
}

/// A remote that's a path on this machine rather than a URL: anything
/// without a `scheme://`, and without an scp-style `host:` before its first
/// `/`.
pub(crate) fn is_local_path(url: &str) -> bool {
    if url.contains("://") {
        return false;
    }
    match (url.find(':'), url.find('/')) {
        (Some(colon), Some(slash)) => slash < colon,
        (Some(_), None) => false,
        (None, _) => true,
    }
}

/// The real host behind an SSH host alias: `ssh -G <alias>`'s `hostname`
/// line. Reads config only; never connects.
fn ssh_hostname(alias: &str) -> Option<String> {
    let out = Command::new("ssh")
        .args(["-G", alias])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|line| line.strip_prefix("hostname ").map(|h| h.trim().to_string()))
}

/// The repository's first commit — what two clones of one history share
/// even with no `origin` to compare. `None` before the first commit.
pub fn root_commit(repo: &Path) -> Option<String> {
    let roots = ask(repo, &["rev-list", "--max-parents=0", "HEAD"])?;
    roots.lines().last().map(str::to_string)
}

/// Points HEAD at `refs/heads/<name>` without requiring the branch to exist
/// yet — safe on a freshly cloned, still-empty repository, where HEAD is
/// "unborn" and there's nothing to rename. Used to make a fresh clone's
/// first commit land on `main` regardless of the local `init.defaultBranch`
/// git config, which a plain `git clone` of an empty repository otherwise
/// falls back to (still `master` on plenty of machines) — the remote's own
/// default branch name doesn't help here, since nothing has been pushed to
/// it yet for that setting to apply to.
pub fn set_head_branch(repo: &Path, name: &str) -> Result<()> {
    let status = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["symbolic-ref", "HEAD"])
        .arg(format!("refs/heads/{name}"))
        .status()
        .with_context(|| format!("running git symbolic-ref HEAD refs/heads/{name}"))?;
    if !status.success() {
        bail!("git symbolic-ref HEAD refs/heads/{name} failed");
    }
    Ok(())
}

/// Stages everything and commits — used once, to give a freshly created
/// tickets repository its first commit (an empty GitHub repo has no branch
/// to push to until one exists).
pub fn commit_all(repo: &Path, message: &str) -> Result<()> {
    let status = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["add", "-A"])
        .status()
        .context("running git add -A")?;
    if !status.success() {
        bail!("git add -A failed");
    }
    let status = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["commit", "-m", message])
        .status()
        .context("running git commit")?;
    if !status.success() {
        bail!("git commit failed");
    }
    Ok(())
}

/// `git push -u origin HEAD` — sets the upstream, which a freshly cloned,
/// still-commit-less GitHub repository doesn't have yet.
pub fn push(repo: &Path) -> Result<()> {
    let (ok, detail) = run(Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["push", "-u", "origin", "HEAD"]))
    .context("running git push")?;
    if !ok {
        bail!(failed("git push", &detail));
    }
    Ok(())
}

/// `git clone <url> <target>` — plain git, so the user's own SSH key (or
/// whatever credential helper they have) is the only thing it needs.
pub fn clone(url: &str, target: &Path) -> Result<()> {
    let (ok, detail) = run(Command::new("git")
        .args(["clone", "--quiet", url])
        .arg(target))
    .context("running git clone")?;
    if !ok {
        bail!(failed(&format!("git clone {url}"), &detail));
    }
    Ok(())
}

/// A git command's trimmed stdout when it succeeds — for questions, not
/// actions, so nothing is ever printed.
fn ask(repo: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Whether any tracked file has uncommitted changes, staged or not.
/// Untracked files don't count: they're nobody's half-finished edit to
/// what's on the remote. `None` if git couldn't say.
pub fn has_local_edits(repo: &Path) -> Option<bool> {
    ask(repo, &["status", "--porcelain", "--untracked-files=no"]).map(|out| !out.is_empty())
}

/// The commit `HEAD` points at, if there is one yet.
pub fn head(repo: &Path) -> Option<String> {
    ask(repo, &["rev-parse", "--verify", "--quiet", "HEAD"])
}

/// Whether the current branch tracks a remote branch (`@{u}`).
pub fn has_upstream(repo: &Path) -> bool {
    ask(
        repo,
        &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
    )
    .is_some()
}

/// `(ahead, behind)`: commits here that the upstream lacks, and commits the
/// upstream has that aren't here — as of the last fetch. `None` without an
/// upstream.
pub fn ahead_behind(repo: &Path) -> Option<(u32, u32)> {
    let counts = ask(
        repo,
        &["rev-list", "--left-right", "--count", "HEAD...@{u}"],
    )?;
    let mut parts = counts.split_whitespace().map(str::parse::<u32>);
    match (parts.next(), parts.next()) {
        (Some(Ok(ahead)), Some(Ok(behind))) => Some((ahead, behind)),
        _ => None,
    }
}

/// The full name of the branch's upstream, e.g. `refs/remotes/origin/main`.
pub fn upstream_ref(repo: &Path) -> Option<String> {
    ask(repo, &["rev-parse", "--symbolic-full-name", "@{u}"]).filter(|name| !name.is_empty())
}

/// Whether `ancestor` is `descendant` or one of its ancestors.
pub fn is_ancestor(repo: &Path, ancestor: &str, descendant: &str) -> bool {
    ask(repo, &["merge-base", "--is-ancestor", ancestor, descendant]).is_some()
}

/// Where `refname` has pointed, newest first, from its reflog: up to
/// `limit` commits, the current one first. Empty without a reflog.
pub fn reflog(repo: &Path, refname: &str, limit: usize) -> Vec<String> {
    ask(
        repo,
        &[
            "reflog",
            "show",
            "--format=%H",
            "-n",
            &limit.to_string(),
            refname,
        ],
    )
    .map(|out| out.lines().map(str::to_string).collect())
    .unwrap_or_default()
}

/// How many commits `rev-list` finds for `args` (e.g. `["A", "^B"]`).
pub fn count_commits(repo: &Path, args: &[&str]) -> Option<u32> {
    let mut all = vec!["rev-list", "--count"];
    all.extend_from_slice(args);
    ask(repo, &all)?.parse().ok()
}

/// Files under `path` that `to` no longer has but `from` did, renames not
/// counted (a ticket moved between columns hasn't gone).
pub fn deleted_between(repo: &Path, from: &str, to: &str, path: &Path) -> Vec<String> {
    let Some(path) = path.to_str() else {
        return Vec::new();
    };
    ask(
        repo,
        &[
            "diff",
            "-M",
            "--name-only",
            "--diff-filter=D",
            from,
            to,
            "--",
            path,
        ],
    )
    .map(|out| out.lines().map(str::to_string).collect())
    .unwrap_or_default()
}

/// `(short sha, author)` of each commit in `range` that deleted something
/// under `path`, oldest first.
pub fn deleting_commits(repo: &Path, range: &str, path: &Path) -> Vec<(String, String)> {
    let Some(path) = path.to_str() else {
        return Vec::new();
    };
    ask(
        repo,
        &[
            "log",
            "-M",
            "--diff-filter=D",
            "--reverse",
            "--format=%h%x1f%an",
            range,
            "--",
            path,
        ],
    )
    .map(|out| {
        out.lines()
            .filter_map(|line| line.split_once('\u{1f}'))
            .map(|(sha, who)| (sha.to_string(), who.to_string()))
            .collect()
    })
    .unwrap_or_default()
}

/// Whether amd's last [`fetch`] was under `seconds` ago (its marker's
/// mtime), so a read right after another needn't go back to the network.
pub fn fetched_within(repo: &Path, seconds: u64) -> bool {
    let Some(marker) = fetch_marker(repo) else {
        return false;
    };
    std::fs::metadata(marker)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|at| at.elapsed().ok())
        .is_some_and(|age| age.as_secs() < seconds)
}

/// `git merge --ff-only @{u}` — brings a clone that's only behind up to
/// date; refuses (an error) rather than merging, and rather than
/// overwriting local edits.
pub fn merge_ff_only(repo: &Path) -> Result<()> {
    let (ok, detail) = run(Command::new("git").arg("-C").arg(repo).args([
        "merge",
        "--ff-only",
        "--quiet",
        "@{u}",
    ]))
    .context("running git merge --ff-only")?;
    if !ok {
        bail!(failed("git merge --ff-only", &detail));
    }
    Ok(())
}

/// `git rebase @{u}` — replays local commits on top of the upstream. `false`
/// when it stopped on a conflict; the caller must `rebase_abort`.
pub fn rebase_upstream(repo: &Path) -> Result<bool> {
    let (ok, _) = run(Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rebase", "--quiet", "@{u}"]))
    .context("running git rebase")?;
    Ok(ok)
}

/// `git rebase --abort`, after `rebase_upstream` stopped.
pub fn rebase_abort(repo: &Path) -> Result<()> {
    let (ok, detail) = run(Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rebase", "--abort"]))
    .context("running git rebase --abort")?;
    if !ok {
        bail!(failed("git rebase --abort", &detail));
    }
    Ok(())
}

/// `git reset --keep <rev>` — moves the branch back to `rev`, dropping the
/// commits after it while keeping unrelated local edits; refuses rather
/// than overwriting one.
pub fn reset_keep(repo: &Path, rev: &str) -> Result<()> {
    let (ok, detail) = run(Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["reset", "--quiet", "--keep", rev]))
    .context("running git reset --keep")?;
    if !ok {
        bail!(failed(&format!("git reset --keep {rev}"), &detail));
    }
    Ok(())
}

/// `git fetch origin`, without writing `FETCH_HEAD` (ticket 118). That file
/// belongs to `git pull`, which writes it and then reads it back: amdui
/// fetching every 30 seconds made a pull that overlapped it fail with
/// "Cannot fast-forward to multiple branches". amd only ever wanted it as a
/// timestamp, so it records its own ([`fetched_within`]). Needs git 2.29.
pub fn fetch(repo: &Path) -> Result<()> {
    let (ok, detail) = run(Command::new("git").arg("-C").arg(repo).args([
        "fetch",
        "--quiet",
        "--no-write-fetch-head",
        "origin",
    ]))
    .context("running git fetch")?;
    if !ok {
        bail!(failed("git fetch", &detail));
    }
    // Only a timestamp: if it can't be written, the next read fetches again.
    if let Some(marker) = fetch_marker(repo) {
        let _ = std::fs::write(marker, b"");
    }
    Ok(())
}

/// `<git dir>/amd-fetched`, touched after each [`fetch`].
fn fetch_marker(repo: &Path) -> Option<PathBuf> {
    ask(repo, &["rev-parse", "--absolute-git-dir"]).map(|dir| Path::new(&dir).join("amd-fetched"))
}

/// `git reset --hard <rev>` — throws away local commits and changes. Only
/// for a clone whose local state is known to be disposable (`amd join`
/// re-applying its one change on top of a remote that moved).
pub fn reset_hard(repo: &Path, rev: &str) -> Result<()> {
    let (ok, detail) = run(Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["reset", "--quiet", "--hard", rev]))
    .context("running git reset")?;
    if !ok {
        bail!(failed(&format!("git reset --hard {rev}"), &detail));
    }
    Ok(())
}

/// Every path git currently considers changed, mapped to a cheap signature
/// of its current on-disk state (`None` if nothing is there — a rename's
/// old side, or a path that was deleted). `git status --porcelain=v1
/// --untracked-files=all` finds the paths; a rename line ("R  old -> new")
/// contributes both. The `=all` matters: git's default collapses a wholly-
/// untracked directory into a single line for the directory itself (real
/// case, hit directly: a board created with `amd init --local-only` has
/// never been committed, so `tasks/` is one untracked directory — without
/// `=all`, a brand new ticket inside it never appears as its own path).
///
/// The signature matters for the same underlying reason: an untracked
/// file's status is always "??" regardless of content, so a path already
/// dirty in one snapshot that changes *again* by the next snapshot is
/// otherwise indistinguishable from one that never changed at all — hit
/// directly too, by `.settings` on that same never-committed board:
/// `record_id` bumps it every time `amd new` runs, but it was already
/// untracked before the second run, so presence alone can't tell the two
/// states apart. Modified-time is enough; this isn't trying to be a
/// content hash.
///
/// Used to snapshot a repo before and after a mutation
/// (`changed_paths` computes the diff; `commit_paths` is the caller),
/// rather than a hand-maintained paths list that would miss an incidental
/// write like this one.
pub fn dirty_paths(repo: &Path) -> Result<BTreeMap<PathBuf, Option<SystemTime>>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["status", "--porcelain=v1", "--untracked-files=all"])
        .output()
        .context("running git status")?;
    if !out.status.success() {
        bail!("git status failed");
    }
    let text = String::from_utf8(out.stdout).context("git status produced non-utf8 output")?;
    let mut paths = BTreeSet::new();
    for line in text.lines() {
        // Two status characters, a space, then the path (or "old -> new"
        // for a rename).
        let Some(rest) = line.get(3..) else { continue };
        match rest.split_once(" -> ") {
            Some((old, new)) => {
                paths.insert(PathBuf::from(old));
                paths.insert(PathBuf::from(new));
            }
            None => {
                paths.insert(PathBuf::from(rest));
            }
        }
    }
    Ok(paths
        .into_iter()
        .map(|path| {
            let mtime = std::fs::metadata(repo.join(&path))
                .and_then(|meta| meta.modified())
                .ok();
            (path, mtime)
        })
        .collect())
}

/// Which paths actually changed between two `dirty_paths` snapshots, and
/// are still something `after`'s own `git status` currently knows about —
/// not just newly dirty, but also a path already dirty in `before` that
/// changed again by `after` (see `dirty_paths`'s own doc for why presence
/// alone isn't enough for an untracked file).
///
/// The `after.contains_key` filter matters on its own, confirmed live
/// against a real repo rather than reasoned out in advance: a ticket that
/// already had an *uncommitted* rename sitting from before this operation
/// (`backlog` staged to `todo`, say) and is then moved again before either
/// side is ever committed collapses, in git's own index, straight from
/// `backlog` to the final destination — `todo` never existed in any commit,
/// so it simply stops being mentioned anywhere in `after`'s status at all.
/// Without this filter, `todo` still shows up as "changed" (dirty in
/// `before`, absent from `after`) and gets handed to `git commit` as a
/// pathspec it has no memory of, which fails outright ("did not match any
/// files known to git") — a real failure, hit directly, not a hypothetical
/// this filter guards against speculatively.
pub fn changed_paths(
    before: &BTreeMap<PathBuf, Option<SystemTime>>,
    after: &BTreeMap<PathBuf, Option<SystemTime>>,
) -> BTreeSet<PathBuf> {
    let mut all: BTreeSet<&PathBuf> = before.keys().collect();
    all.extend(after.keys());
    all.into_iter()
        .filter(|path| before.get(*path) != after.get(*path) && after.contains_key(*path))
        .cloned()
        .collect()
}

/// Commits exactly `paths` (relative to `repo`'s root, as `dirty_paths`
/// returns them) with `message`, then pushes. Never touches anything else
/// staged, even if something else already was — the pathspec is on both
/// `add` and `commit` itself, since a pathspec on `add` alone still lets a
/// bare `git commit -m` sweep in the whole index. A no-op on an empty set,
/// so a caller doesn't need its own check first.
pub fn commit_paths(repo: &Path, message: &str, paths: &BTreeSet<PathBuf>) -> Result<()> {
    if paths.is_empty() {
        return Ok(());
    }
    commit_paths_locally(repo, message, paths)?;
    push(repo)
}

/// [`commit_paths`] without the push — for a repository with nowhere to
/// push to, or a caller that decides about pushing itself.
pub fn commit_paths_locally(repo: &Path, message: &str, paths: &BTreeSet<PathBuf>) -> Result<()> {
    if paths.is_empty() {
        return Ok(());
    }
    // Only `add` paths that still exist on disk. A path that's gone — the
    // old side of a `git mv`-based rename, or a `git rm --cached`'d
    // archived ticket — is already fully staged by whatever produced that
    // rename or removal; re-adding it errors ("did not match any files"),
    // since there's nothing left for `add` to reconcile. The commit's own
    // pathspec below still needs every path, gone or not, to capture the
    // whole change — same reason a rename needs both sides there, not just
    // on `add`.
    let existing: Vec<&PathBuf> = paths
        .iter()
        .filter(|path| repo.join(path).exists())
        .collect();
    if !existing.is_empty() {
        let (ok, detail) = run(Command::new("git")
            .arg("-C")
            .arg(repo)
            .arg("add")
            .arg("--")
            .args(existing))
        .context("running git add")?;
        if !ok {
            bail!(failed("git add", &detail));
        }
    }
    let (ok, detail) = run(Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["commit", "-m", message])
        .arg("--")
        .args(paths))
    .context("running git commit")?;
    if !ok {
        bail!(failed("git commit", &detail));
    }
    Ok(())
}

/// A `git config` value (used to fill `author` in task templates, and to
/// resolve the current identity for a ticket lease). `-C repo` like every
/// other function here, rather than relying on the process's current
/// directory — the caller may not be standing in the repo in question (a
/// desktop board open across several registered repositories, a test), and
/// git itself resolves the repository boundary from any path inside it, so
/// this doesn't need `repo` to be the root.
pub fn config(repo: &Path, key: &str) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["config", "--get", key])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let value = String::from_utf8(out.stdout).ok()?;
    let value = value.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

/// `git -C repo <args>`, its stdout trimmed; an error carrying git's own
/// reason when it fails. For commands whose output is wanted, not shown.
pub fn checked(repo: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("running git {}", args.join(" ")))?;
    if !out.status.success() {
        let detail = String::from_utf8_lossy(&out.stderr);
        bail!(failed(&format!("git {}", args.join(" ")), detail.trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failure_carries_gits_own_reason_when_there_is_one() {
        assert_eq!(failed("git push", ""), "git push failed");
        assert_eq!(
            failed("git push", "hint: x\nfatal: no remote"),
            "git push failed: fatal: no remote"
        );
    }

    fn git(repo: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }

    fn init_repo(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        git(dir, &["init", "-q"]);
        git(dir, &["config", "user.email", "t@t.co"]);
        git(dir, &["config", "user.name", "t"]);
    }

    /// Ticket 118: amd's fetch leaves FETCH_HEAD to `git pull`, and keeps
    /// its own record of when it fetched.
    #[test]
    fn fetch_leaves_fetch_head_alone_and_marks_the_time() {
        let dir = tempfile::tempdir().unwrap();
        let bare = dir.path().join("origin.git");
        std::fs::create_dir_all(&bare).unwrap();
        git(&bare, &["init", "-q", "--bare"]);
        let working = dir.path().join("working");
        init_repo(&working);
        git(
            &working,
            &["remote", "add", "origin", bare.to_str().unwrap()],
        );
        std::fs::write(working.join("README.md"), "hello\n").unwrap();
        commit_all(&working, "seed").unwrap();
        push(&working).unwrap();

        assert!(!fetched_within(&working, 60), "never fetched yet");
        fetch(&working).unwrap();
        assert!(!working.join(".git/FETCH_HEAD").exists());
        assert!(working.join(".git/amd-fetched").exists());
        assert!(fetched_within(&working, 60));
    }

    #[test]
    fn remote_url_reads_back_what_was_configured() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        init_repo(&repo);
        assert_eq!(remote_url(&repo), None);
        git(&repo, &["remote", "add", "origin", "/tmp/wherever"]);
        assert_eq!(remote_url(&repo).as_deref(), Some("/tmp/wherever"));
    }

    #[test]
    fn commit_all_and_push_round_trip_against_a_local_bare_repo() {
        let dir = tempfile::tempdir().unwrap();
        let bare = dir.path().join("origin.git");
        std::fs::create_dir_all(&bare).unwrap();
        git(&bare, &["init", "-q", "--bare"]);

        let working = dir.path().join("working");
        init_repo(&working);
        git(
            &working,
            &["remote", "add", "origin", bare.to_str().unwrap()],
        );

        std::fs::write(working.join("README.md"), "hello\n").unwrap();
        commit_all(&working, "seed").unwrap();
        push(&working).unwrap();

        // A plain clone from the same bare repo sees the pushed commit.
        let second = dir.path().join("second");
        let status = Command::new("git")
            .args(["clone", "-q", bare.to_str().unwrap()])
            .arg(&second)
            .status()
            .unwrap();
        assert!(status.success());
        assert!(second.join("README.md").exists());
    }

    #[test]
    fn dirty_paths_reports_untracked_modified_and_renamed_files() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        init_repo(&repo);
        std::fs::write(repo.join("tracked.md"), "one\n").unwrap();
        std::fs::write(repo.join("renamed.md"), "two\n").unwrap();
        commit_all(&repo, "seed").unwrap();

        std::fs::write(repo.join("tracked.md"), "one, edited\n").unwrap();
        std::fs::write(repo.join("untracked.md"), "new\n").unwrap();
        git(&repo, &["mv", "renamed.md", "moved.md"]);

        let dirty = dirty_paths(&repo).unwrap();
        assert!(dirty.contains_key(&PathBuf::from("tracked.md")));
        assert!(dirty.contains_key(&PathBuf::from("untracked.md")));
        assert!(
            dirty.contains_key(&PathBuf::from("renamed.md")),
            "old side of the rename"
        );
        assert!(
            dirty.contains_key(&PathBuf::from("moved.md")),
            "new side of the rename"
        );
    }

    #[test]
    fn dirty_paths_lists_files_inside_a_wholly_untracked_directory() {
        // The real case that broke this: `amd init --local-only` never
        // commits, so `tasks/` starts life entirely untracked. Without
        // `--untracked-files=all`, git collapses that whole directory into
        // one line and a file created inside it later never looks "new".
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        init_repo(&repo);
        std::fs::create_dir_all(repo.join("tasks/backlog")).unwrap();
        std::fs::write(repo.join("tasks/backlog/001-first.md"), "one\n").unwrap();

        let dirty = dirty_paths(&repo).unwrap();
        assert!(
            dirty.contains_key(&PathBuf::from("tasks/backlog/001-first.md")),
            "{dirty:?}"
        );
    }

    #[test]
    fn changed_paths_notices_a_file_already_dirty_that_changed_again() {
        // The other real case that broke this: `.settings` is untracked
        // (never committed) from `amd init --local-only`, so its git status
        // is "??" both before and after a second `amd new` bumps its
        // counter — presence in the dirty set alone can't tell those two
        // moments apart, only its on-disk signature can.
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        init_repo(&repo);
        std::fs::write(repo.join(".settings"), "next-id = 1\n").unwrap();
        let before = dirty_paths(&repo).unwrap();

        std::thread::sleep(std::time::Duration::from_millis(10));
        std::fs::write(repo.join(".settings"), "next-id = 2\n").unwrap();
        let after = dirty_paths(&repo).unwrap();

        let changed = changed_paths(&before, &after);
        assert!(changed.contains(&PathBuf::from(".settings")), "{changed:?}");
    }

    #[test]
    fn changed_paths_excludes_a_file_dirty_both_times_but_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        init_repo(&repo);
        std::fs::write(repo.join("untouched.md"), "same\n").unwrap();
        let before = dirty_paths(&repo).unwrap();
        // Something else becomes dirty in between, but untouched.md itself
        // never changes.
        std::fs::write(repo.join("other.md"), "new\n").unwrap();
        let after = dirty_paths(&repo).unwrap();

        let changed = changed_paths(&before, &after);
        assert!(changed.contains(&PathBuf::from("other.md")));
        assert!(
            !changed.contains(&PathBuf::from("untouched.md")),
            "{changed:?}"
        );
    }

    #[test]
    fn changed_paths_drops_an_intermediate_hop_git_has_already_forgotten() {
        // The real bug, hit directly against a live repo: a ticket already
        // mid-rename (staged, never committed) before this operation even
        // starts, then moved again before either side lands in a commit.
        // Git's own index collapses the whole chain straight from the
        // original committed path to the final one — the middle hop never
        // existed in any commit, so `after`'s status stops mentioning it
        // entirely, even though it was genuinely dirty in `before`.
        let dir = tempfile::tempdir().unwrap();
        let bare = dir.path().join("origin.git");
        std::fs::create_dir_all(&bare).unwrap();
        git(&bare, &["init", "-q", "--bare"]);

        let repo = dir.path().join("repo");
        init_repo(&repo);
        git(&repo, &["remote", "add", "origin", bare.to_str().unwrap()]);
        std::fs::write(repo.join("start.md"), "content\n").unwrap();
        commit_all(&repo, "seed").unwrap();
        push(&repo).unwrap();

        // First hop, staged but never committed.
        git(&repo, &["mv", "start.md", "middle.md"]);
        let before = dirty_paths(&repo).unwrap();
        assert!(before.contains_key(&PathBuf::from("middle.md")));

        // Second hop, on top of the still-uncommitted first one — git
        // collapses this to start.md -> end.md directly.
        git(&repo, &["mv", "middle.md", "end.md"]);
        let after = dirty_paths(&repo).unwrap();
        assert!(
            !after.contains_key(&PathBuf::from("middle.md")),
            "git should have forgotten the intermediate hop: {after:?}"
        );

        let changed = changed_paths(&before, &after);
        assert!(
            !changed.contains(&PathBuf::from("middle.md")),
            "a path git has no memory of must never reach commit_paths: {changed:?}"
        );
        assert!(changed.contains(&PathBuf::from("end.md")));

        // The real proof: committing this must not error the way it did
        // live ("did not match any files known to git").
        commit_paths(&repo, "test: chained rename", &changed).unwrap();
    }

    #[test]
    fn dirty_paths_is_empty_on_a_clean_repo() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        init_repo(&repo);
        std::fs::write(repo.join("README.md"), "hello\n").unwrap();
        commit_all(&repo, "seed").unwrap();
        assert!(dirty_paths(&repo).unwrap().is_empty());
    }

    #[test]
    fn commit_paths_commits_only_the_given_paths() {
        let dir = tempfile::tempdir().unwrap();
        let bare = dir.path().join("origin.git");
        std::fs::create_dir_all(&bare).unwrap();
        git(&bare, &["init", "-q", "--bare"]);

        let repo = dir.path().join("repo");
        init_repo(&repo);
        git(&repo, &["remote", "add", "origin", bare.to_str().unwrap()]);
        std::fs::write(repo.join("README.md"), "hello\n").unwrap();
        commit_all(&repo, "seed").unwrap();
        push(&repo).unwrap();

        std::fs::write(repo.join("wanted.md"), "wanted\n").unwrap();
        std::fs::write(
            repo.join("unrelated.md"),
            "already staged by someone else\n",
        )
        .unwrap();
        git(&repo, &["add", "unrelated.md"]);

        let mut paths = BTreeSet::new();
        paths.insert(PathBuf::from("wanted.md"));
        commit_paths(&repo, "test: commit", &paths).unwrap();

        let show = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["show", "--name-only", "--pretty=format:"])
            .output()
            .unwrap();
        let committed = String::from_utf8(show.stdout).unwrap();
        assert!(committed.contains("wanted.md"));
        assert!(!committed.contains("unrelated.md"), "{committed}");

        // The unrelated file is still staged, untouched, not swept in.
        let status = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["status", "--porcelain=v1"])
            .output()
            .unwrap();
        let status = String::from_utf8(status.stdout).unwrap();
        assert!(status.contains("unrelated.md"));
    }

    #[test]
    fn commit_paths_handles_a_rename_whose_old_side_git_mv_already_staged() {
        // The real bug: `git mv` fully stages both sides of a rename in the
        // index by itself. Re-`add`ing the now-gone old path afterward
        // fails ("did not match any files") — commit_paths must skip `add`
        // for a path that no longer exists, while still passing both paths
        // to `commit` itself (the commit needs both to record the rename;
        // omitting the old side is the same gotcha a plain two-commit
        // rename has).
        let dir = tempfile::tempdir().unwrap();
        let bare = dir.path().join("origin.git");
        std::fs::create_dir_all(&bare).unwrap();
        git(&bare, &["init", "-q", "--bare"]);

        let repo = dir.path().join("repo");
        init_repo(&repo);
        git(&repo, &["remote", "add", "origin", bare.to_str().unwrap()]);
        std::fs::write(repo.join("old.md"), "content\n").unwrap();
        commit_all(&repo, "seed").unwrap();
        push(&repo).unwrap();

        git(&repo, &["mv", "old.md", "new.md"]);

        let mut paths = BTreeSet::new();
        paths.insert(PathBuf::from("old.md"));
        paths.insert(PathBuf::from("new.md"));
        commit_paths(&repo, "test: rename", &paths).unwrap();

        let show = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["show", "--name-status", "--pretty=format:"])
            .output()
            .unwrap();
        let committed = String::from_utf8(show.stdout).unwrap();
        assert!(
            committed.contains("old.md") && committed.contains("new.md"),
            "{committed}"
        );
        assert!(
            dirty_paths(&repo).unwrap().is_empty(),
            "nothing left uncommitted"
        );
    }

    #[test]
    fn commit_paths_pushes_to_origin() {
        let dir = tempfile::tempdir().unwrap();
        let bare = dir.path().join("origin.git");
        std::fs::create_dir_all(&bare).unwrap();
        git(&bare, &["init", "-q", "--bare"]);

        let repo = dir.path().join("repo");
        init_repo(&repo);
        git(&repo, &["remote", "add", "origin", bare.to_str().unwrap()]);
        std::fs::write(repo.join("README.md"), "hello\n").unwrap();
        commit_all(&repo, "seed").unwrap();
        push(&repo).unwrap();

        std::fs::write(repo.join("new.md"), "new\n").unwrap();
        let mut paths = BTreeSet::new();
        paths.insert(PathBuf::from("new.md"));
        commit_paths(&repo, "test: push", &paths).unwrap();

        let clone = dir.path().join("clone");
        let status = Command::new("git")
            .args(["clone", "-q", bare.to_str().unwrap()])
            .arg(&clone)
            .status()
            .unwrap();
        assert!(status.success());
        assert!(clone.join("new.md").exists());
    }

    #[test]
    fn commit_paths_is_a_noop_on_an_empty_set() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        init_repo(&repo);
        std::fs::write(repo.join("README.md"), "hello\n").unwrap();
        commit_all(&repo, "seed").unwrap();
        commit_paths(&repo, "should not run", &BTreeSet::new()).unwrap();
    }

    #[test]
    fn set_head_branch_forces_the_first_commit_onto_that_branch() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        // Simulate a machine whose local git defaults new repos to
        // something other than `main` — an unborn HEAD, no commits yet.
        git(&repo, &["init", "-q", "--initial-branch=trunk"]);
        git(&repo, &["config", "user.email", "t@t.co"]);
        git(&repo, &["config", "user.name", "t"]);

        set_head_branch(&repo, "main").unwrap();
        std::fs::write(repo.join("README.md"), "hello\n").unwrap();
        commit_all(&repo, "seed").unwrap();

        assert_eq!(current_branch(&repo).as_deref(), Some("main"));
    }

    #[test]
    fn remotes_normalise_to_host_owner_repo() {
        let none = |_: &str| None;
        for url in [
            "git@github.com:Agile-MD/amd.git",
            "ssh://git@github.com/agile-md/amd",
            "ssh://git@github.com:22/agile-md/amd.git/",
            "https://github.com/agile-md/amd.git",
            "https://user@GitHub.com/agile-md/amd/",
            "git+ssh://github.com/agile-md/amd.git",
        ] {
            assert_eq!(
                normalise_remote(url, none),
                "github.com/agile-md/amd",
                "{url}"
            );
        }
    }

    #[test]
    fn an_ssh_host_alias_resolves_to_the_real_host() {
        let sgs = |host: &str| (host == "github.com-oldippy").then(|| "GitHub.com".to_string());
        assert_eq!(
            normalise_remote("git@github.com-oldippy:agile-md/amd.git", sgs),
            "github.com/agile-md/amd"
        );
        // Only SSH hosts are aliases; an https host is taken as written.
        let everything = |_: &str| Some("elsewhere".to_string());
        assert_eq!(
            normalise_remote("https://github.com/agile-md/amd", everything),
            "github.com/agile-md/amd"
        );
    }

    #[test]
    fn local_paths_are_told_apart_from_urls() {
        assert!(is_local_path("/srv/git/board.git"));
        assert!(is_local_path("../origin.git"));
        assert!(is_local_path("./a:b/c"));
        assert!(!is_local_path("git@github.com:o/r.git"));
        assert!(!is_local_path("https://github.com/o/r"));
    }
}
