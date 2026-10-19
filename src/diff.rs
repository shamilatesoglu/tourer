//! Diff tours: what changed between a git commit and the files on disk, or another commit. The
//! new side is a folder of real files: the toured folder as it is now, or a checkout of the new
//! commit kept with tourer's data. Tours cite those files and the agents read them as usual; the
//! old lines are only in the diff, which tourer sends them because their tools cannot run git.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use crate::tour::DiffBase;

/// The most diff text sent to an agent. Files past it are listed, to be read instead.
const MAX_PROMPT_DIFF: usize = 120_000;
/// Untracked files bigger than this are not counted line by line.
const MAX_UNTRACKED_BYTES: u64 = 1 << 20;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Status {
    Modified,
    Added,
    Deleted,
    Renamed,
    /// A new file git does not track yet.
    Untracked,
}

/// What changed in one file, by line of its current version.
#[derive(Debug, Clone, PartialEq)]
pub struct FileChanges {
    pub status: Status,
    /// The name it had before a rename.
    pub old_path: Option<String>,
    /// Lines that are new.
    pub added: BTreeSet<usize>,
    /// Old lines removed just before a line. Lines removed at the end of the file sit before
    /// the line after the last one.
    pub removed: BTreeMap<usize, Vec<String>>,
}

/// A file with no changes.
pub static UNCHANGED: FileChanges = FileChanges {
    status: Status::Modified,
    old_path: None,
    added: BTreeSet::new(),
    removed: BTreeMap::new(),
};

impl FileChanges {
    fn new(status: Status) -> Self {
        FileChanges { status, ..UNCHANGED.clone() }
    }

    /// Old lines removed just above `line` of a file with `len` lines; at the last line, also
    /// the ones removed after it.
    pub fn removed_above(&self, line: usize, len: usize) -> Vec<&str> {
        let mut lines: Vec<&str> = self.removed.get(&line).into_iter().flatten().map(String::as_str).collect();
        if line == len {
            lines.extend(self.removed.get(&(len + 1)).into_iter().flatten().map(String::as_str));
        }
        lines
    }

    fn removed_count(&self) -> usize {
        self.removed.values().map(Vec::len).sum()
    }
}

/// The changes between a commit and the files on disk, or another commit.
#[derive(Debug, Clone, Default)]
pub struct Changes {
    /// The unified diff from git, untracked files not included.
    pub text: String,
    /// Changed files by path relative to the toured folder, in diff order.
    pub files: Vec<(String, FileChanges)>,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// The changes to a file; [`UNCHANGED`] if it has none.
    pub fn file(&self, path: &str) -> &FileChanges {
        self.files.iter().find(|(p, _)| p == path).map_or(&UNCHANGED, |(_, c)| c)
    }

    /// The list of changed files, then as much of the diff as an agent should get at once.
    pub fn for_prompt(&self) -> String {
        let mut s = String::from("Files changed:\n");
        for (path, c) in &self.files {
            let status = match c.status {
                Status::Modified => "modified",
                Status::Added => "added",
                Status::Deleted => "deleted",
                Status::Renamed => "renamed",
                Status::Untracked => "untracked",
            };
            let _ = write!(s, "  {status:<9} {path}");
            if let Some(old) = &c.old_path {
                let _ = write!(s, " (from {old})");
            }
            let _ = writeln!(s, "  +{} -{}", c.added.len(), c.removed_count());
        }
        let mut shown = String::new();
        let mut left_out = vec![];
        for chunk in split_files(&self.text) {
            if shown.len() + chunk.len() <= MAX_PROMPT_DIFF {
                shown.push_str(chunk);
            } else {
                left_out.push(chunk.lines().next().unwrap_or("").trim_start_matches("diff --git "));
            }
        }
        let _ = write!(s, "\n```diff\n{}```\n", shown);
        if !left_out.is_empty() {
            let _ = writeln!(s, "\nThe diff was too long to send whole. Left out, read these files instead: {}", left_out.join(", "));
        }
        if self.files.iter().any(|(_, c)| c.status == Status::Untracked) {
            s.push_str("\nUntracked files are new and not in the diff; read them.\n");
        }
        s
    }
}

/// The diff split at the start of each file's part.
fn split_files(text: &str) -> Vec<&str> {
    let mut starts: Vec<usize> = text.match_indices("\ndiff --git ").map(|(i, _)| i + 1).collect();
    if text.starts_with("diff --git ") {
        starts.insert(0, 0);
    }
    starts.push(text.len());
    starts.windows(2).map(|w| &text[w[0]..w[1]]).collect()
}

/// Runs git in `root` and returns what it printed.
fn git(root: &Path, args: &[&str]) -> Result<String> {
    git_with(root, args, &[])
}

/// Runs git in `root` with some environment variables set to paths, and returns what it printed.
fn git_with(root: &Path, args: &[&str], env: &[(&str, &Path)]) -> Result<String> {
    let out = Command::new("git")
        .current_dir(plain_path(root))
        .envs(env.iter().map(|&(k, v)| (k, plain_path(v))))
        .args(["-c", "core.quotepath=false"])
        .args(args)
        .output()
        .context("cannot run git; is it installed and on PATH?")?;
    if !out.status.success() {
        bail!("git {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// A path without the `\\?\` prefix Windows adds to canonical paths, which git does not expect.
fn plain_path(path: &Path) -> PathBuf {
    let s = path.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC\\") => PathBuf::from(rest),
        _ => path.to_path_buf(),
    }
}

/// What a diff tour compares, as typed: "REV" for the changes from REV to the files on disk,
/// "OLD..NEW" for the changes from OLD to NEW. A side left empty means HEAD, as in git.
/// "OLD...NEW" means the same as "OLD..NEW".
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Range<'a> {
    Since(&'a str),
    Between(&'a str, &'a str),
}

impl<'a> Range<'a> {
    pub fn parse(rev: &'a str) -> Self {
        let or_head = |r: &'a str| if r.is_empty() { "HEAD" } else { r };
        match rev.split_once("..") {
            // Git revisions cannot start with a dot, so a third one belongs to "...".
            Some((old, new)) => Range::Between(or_head(old), or_head(new.strip_prefix('.').unwrap_or(new))),
            None => Range::Since(rev),
        }
    }
}

/// The commits a diff tour of `rev` compares now: for "REV", from where REV meets HEAD to the
/// files on disk, so that `main` means the branch's own changes; for "OLD..NEW", from where OLD
/// meets NEW to NEW. Fails if `root` is not in a git repository.
pub fn resolve(root: &Path, rev: &str) -> Result<DiffBase> {
    git(root, &["rev-parse", "--git-dir"]).map_err(|_| anyhow!("--diff needs a folder in a git repository"))?;
    let (commit, to) = match Range::parse(rev) {
        Range::Since(old) => (merge_base(root, old, "HEAD")?, None),
        Range::Between(old, new) => {
            let to = commit_of(root, new)?;
            (merge_base(root, old, &to)?, Some(to))
        }
    };
    Ok(DiffBase { rev: rev.to_string(), commit, to })
}

/// The commit `rev` names.
fn commit_of(root: &Path, rev: &str) -> Result<String> {
    let commit = format!("{rev}^{{commit}}");
    let id = git(root, &["rev-parse", "--verify", "--quiet", &commit])
        .map_err(|_| anyhow!("`{rev}` is not a commit in this repository"))?;
    Ok(id.trim().to_string())
}

/// Where `rev` and `other` meet; `rev` itself if they share no history.
fn merge_base(root: &Path, rev: &str, other: &str) -> Result<String> {
    let commit = commit_of(root, rev)?;
    let base = git(root, &["merge-base", &commit, other]).map(|b| b.trim().to_string());
    Ok(base.unwrap_or(commit))
}

/// What changed in `root` between `base`'s commits. Changes up to the files on disk include
/// uncommitted and untracked files.
pub fn changes(root: &Path, base: &DiffBase) -> Result<Changes> {
    let args = ["diff", "--relative", "--no-color", "--no-ext-diff", "-M", "--src-prefix=a/", "--dst-prefix=b/"];
    let commits: Vec<&str> = [Some(base.commit.as_str()), base.to.as_deref()].into_iter().flatten().collect();
    let text = git(root, &[&args[..], &commits, &["--"]].concat())?;
    let mut files = parse(&text);
    if base.to.is_some() {
        return Ok(Changes { text, files });
    }
    for path in git(root, &["ls-files", "--others", "--exclude-standard"])?.lines() {
        let mut c = FileChanges::new(Status::Untracked);
        let file = root.join(path);
        if std::fs::metadata(&file).is_ok_and(|m| m.len() <= MAX_UNTRACKED_BYTES)
            && let Ok(bytes) = std::fs::read(&file)
        {
            c.added = (1..=String::from_utf8_lossy(&bytes).lines().count()).collect();
        }
        files.push((path.to_string(), c));
    }
    Ok(Changes { text, files })
}

/// Where the commits that diff tours end at are checked out, one folder per commit.
fn checkouts_dir() -> Result<PathBuf> {
    Ok(crate::daemon::data_dir()?.join("checkouts"))
}

/// For a diff tour that ends at a commit, `root` as it is in that commit: the folder the tour
/// shows and its agents read. Writes the commit's files unless they are there already, without
/// touching the repository's own index and work tree. `None` for a tour that ends at the files
/// on disk.
pub fn check_out(root: &Path, base: &DiffBase) -> Result<Option<PathBuf>> {
    let Some(to) = &base.to else { return Ok(None) };
    let commit_dir = checkouts_dir()?.join(to);
    let dir = commit_dir.join(git(root, &["rev-parse", "--show-prefix"])?.trim());
    if !commit_dir.exists() {
        write_checkout(root, to, &commit_dir).with_context(|| format!("cannot check out {}", short(to)))?;
    }
    if !dir.is_dir() {
        bail!("this folder does not exist at {}", short(to));
    }
    Ok(Some(dir))
}

/// Writes the files of `commit`, from the repository `root` is in, to the new folder `dir`.
fn write_checkout(root: &Path, commit: &str, dir: &Path) -> Result<()> {
    // Written to a folder of its own first, so a checkout cut short is never used.
    let tmp = dir.with_extension(format!("tmp{}", std::process::id()));
    let index = dir.with_extension(format!("index{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(dir.parent().context("no folder for the checkout")?)?;
    let top = PathBuf::from(git(root, &["rev-parse", "--show-toplevel"])?.trim());
    let prefix = format!("--prefix={}/", plain_path(&tmp).to_string_lossy().replace('\\', "/"));
    let env = [("GIT_INDEX_FILE", index.as_path())];
    let written = git_with(&top, &["read-tree", commit], &env)
        .and_then(|_| git_with(&top, &["checkout-index", "--all", "--force", &prefix], &env))
        .and_then(|_| Ok(std::fs::rename(&tmp, dir)?));
    let _ = std::fs::remove_file(&index);
    if written.is_err() {
        let _ = std::fs::remove_dir_all(&tmp);
    }
    written
}

/// Deletes the checkouts of commits that are not in `keep`, and any left half written.
pub fn remove_checkouts(keep: &BTreeSet<String>) {
    let Ok(dir) = checkouts_dir() else { return };
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if !keep.contains(entry.file_name().to_string_lossy().as_ref()) {
            let _ = if path.is_dir() { std::fs::remove_dir_all(path) } else { std::fs::remove_file(path) };
        }
    }
}

/// The first characters of a commit id, as git shows them.
pub fn short(commit: &str) -> &str {
    &commit[..commit.len().min(7)]
}

/// Whether a saved diff tour still compares the commits its revisions name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum BaseState {
    Current,
    /// The changes start at another commit now, e.g. after main moved or a rebase.
    Moved { planned: String, now: String },
    /// The new revision of "OLD..NEW" is another commit now, e.g. after commits were added to it.
    EndMoved { planned: String, now: String },
    /// A revision, or a commit the tour was planned from, cannot be found.
    Lost { reason: String },
}

impl BaseState {
    /// What to tell the person when the tour no longer compares what it should.
    pub fn warning(&self) -> Option<String> {
        match self {
            BaseState::Current => None,
            BaseState::Moved { planned, now } => {
                Some(format!("planned from {}, but the changes start at {} now", short(planned), short(now)))
            }
            BaseState::EndMoved { planned, now } => {
                Some(format!("planned up to {}, but the changes end at {} now", short(planned), short(now)))
            }
            BaseState::Lost { reason } => Some(reason.clone()),
        }
    }
}

/// Where a diff tour planned from `base` stands now.
pub fn base_state(root: &Path, base: &DiffBase) -> BaseState {
    for commit in [Some(&base.commit), base.to.as_ref()].into_iter().flatten() {
        if git(root, &["cat-file", "-e", &format!("{commit}^{{commit}}")]).is_err() {
            return BaseState::Lost {
                reason: format!("the commit this tour was planned from, {}, is gone", short(commit)),
            };
        }
    }
    match self::resolve(root, &base.rev) {
        Ok(now) if now.commit != base.commit => BaseState::Moved { planned: base.commit.clone(), now: now.commit },
        Ok(DiffBase { to: Some(now), .. }) if base.to.as_ref() != Some(&now) => {
            BaseState::EndMoved { planned: base.to.clone().unwrap_or_default(), now }
        }
        Ok(_) => BaseState::Current,
        Err(err) => BaseState::Lost { reason: format!("{err:#}") },
    }
}

/// What a folder's git repository offers to tour besides the whole folder.
#[derive(Debug, Clone, PartialEq)]
pub struct Offer {
    /// Files with uncommitted changes, untracked ones included.
    pub uncommitted: usize,
    /// The main branch and how many commits HEAD has on top of it, if it has any.
    pub branch: Option<(String, usize)>,
}

/// What there is to tour in `root`'s repository; `None` outside one or before its first commit.
pub fn offer(root: &Path) -> Option<Offer> {
    git(root, &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"]).ok()?;
    let status = git(root, &["status", "--porcelain", "--untracked-files=all", "--", "."]).unwrap_or_default();
    let branch = main_branch(root).and_then(|main| {
        let commits = git(root, &["rev-list", "--count", &format!("{main}..HEAD"), "--", "."]).ok()?;
        let commits: usize = commits.trim().parse().ok()?;
        (commits > 0).then_some((main, commits))
    });
    Some(Offer { uncommitted: status.lines().count(), branch })
}

/// The repository's main branch: the one `origin/HEAD` points at, else `main` or `master`.
/// A local branch is preferred over its remote one.
fn main_branch(root: &Path) -> Option<String> {
    let remote = git(root, &["symbolic-ref", "--quiet", "--short", "refs/remotes/origin/HEAD"]).ok();
    let remote = remote.map(|r| r.trim().to_string()).filter(|r| !r.is_empty());
    let local = remote.as_deref().and_then(|r| r.strip_prefix("origin/")).map(str::to_string);
    let fallbacks = ["main", "master"].map(str::to_string);
    local.into_iter().chain(remote).chain(fallbacks).find(|name| {
        git(root, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{name}")])
            .or_else(|_| git(root, &["rev-parse", "--verify", "--quiet", &format!("refs/remotes/{name}")]))
            .is_ok()
    })
}

/// Reads which lines each file's diff adds and removes.
fn parse(text: &str) -> Vec<(String, FileChanges)> {
    let mut files: Vec<(String, FileChanges)> = vec![];
    // Lines of the current hunk still to come, on the old and new side, and the next new line.
    let (mut old_left, mut new_left, mut line) = (0usize, 0usize, 0usize);
    let strip = |p: &str, prefix: &str| p.strip_prefix(prefix).unwrap_or(p).trim_end_matches('\t').to_string();
    for l in text.lines() {
        if old_left > 0 || new_left > 0 {
            let Some((_, c)) = files.last_mut() else { break };
            match l.as_bytes().first() {
                Some(b'+') => {
                    c.added.insert(line);
                    (line, new_left) = (line + 1, new_left.saturating_sub(1));
                }
                Some(b'-') => {
                    c.removed.entry(line).or_default().push(l[1..].to_string());
                    old_left = old_left.saturating_sub(1);
                }
                Some(b'\\') => {}
                _ => {
                    (line, old_left, new_left) = (line + 1, old_left.saturating_sub(1), new_left.saturating_sub(1));
                }
            }
            continue;
        }
        if let Some(rest) = l.strip_prefix("diff --git ") {
            // Taken from here in case no ---/+++ lines follow, as for binary files.
            let path = rest.rsplit_once(" b/").map_or(rest, |(_, p)| p);
            files.push((path.to_string(), FileChanges::new(Status::Modified)));
            continue;
        }
        let Some((path, c)) = files.last_mut() else { continue };
        if l.starts_with("new file mode") {
            c.status = Status::Added;
        } else if l.starts_with("deleted file mode") || l == "+++ /dev/null" {
            c.status = Status::Deleted;
        } else if let Some(old) = l.strip_prefix("rename from ") {
            (c.status, c.old_path) = (Status::Renamed, Some(old.to_string()));
        } else if let Some(new) = l.strip_prefix("rename to ") {
            *path = new.to_string();
        } else if let Some(new) = l.strip_prefix("+++ b/") {
            *path = strip(new, "");
        } else if let Some(old) = l.strip_prefix("--- ").filter(|_| c.status == Status::Deleted) {
            *path = strip(old, "a/");
        } else if let Some(hunk) = l.strip_prefix("@@ -") {
            // "@@ -12,3 +14,0 @@": a count of 0 puts the start one line before the change.
            let range = |r: &str| -> (usize, usize) {
                let (start, count) = r.split_once(',').unwrap_or((r, "1"));
                (start.parse().unwrap_or(0), count.parse().unwrap_or(0))
            };
            let mut parts = hunk.split(' ');
            let (_, old) = range(parts.next().unwrap_or(""));
            let (start, new) = range(parts.next().unwrap_or("").trim_start_matches('+'));
            (old_left, new_left, line) = (old, new, if new == 0 { start + 1 } else { start });
        }
    }
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIFF: &str = "\
diff --git a/src/a.rs b/src/a.rs
index 1111111..2222222 100644
--- a/src/a.rs
+++ b/src/a.rs
@@ -1,4 +1,4 @@
 fn main() {
-    old();
+    new();
+    more();
 }
--- not a header
@@ -10,2 +11,0 @@ fn other() {
-gone();
--- also gone
diff --git a/old.txt b/old.txt
deleted file mode 100644
index 3333333..0000000
--- a/old.txt
+++ /dev/null
@@ -1,2 +0,0 @@
-one
-two
diff --git a/x.rs b/y.rs
similarity index 100%
rename from x.rs
rename to y.rs
diff --git a/new.md b/new.md
new file mode 100644
--- /dev/null
+++ b/new.md
@@ -0,0 +1,2 @@
+# New
+
";

    #[test]
    fn reads_added_and_removed_lines() {
        let files = parse(DIFF);
        let paths: Vec<&str> = files.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(paths, ["src/a.rs", "old.txt", "y.rs", "new.md"]);

        let a = &files[0].1;
        assert_eq!(a.added, BTreeSet::from([2, 3]));
        assert_eq!(a.removed[&2], ["    old();"]);
        // Removed lines that look like headers stay part of their hunk.
        assert_eq!(a.removed[&5], ["-- not a header"]);
        assert_eq!(a.removed[&12], ["gone();", "-- also gone"]);
        assert_eq!(a.removed_above(11, 11), ["gone();", "-- also gone"]);
        assert!(a.removed_above(11, 20).is_empty());

        assert_eq!(files[1].1.status, Status::Deleted);
        assert_eq!(files[1].1.removed[&1], ["one", "two"]);
        assert_eq!((files[2].1.status, files[2].1.old_path.as_deref()), (Status::Renamed, Some("x.rs")));
        assert_eq!((files[3].1.status, files[3].1.added.len()), (Status::Added, 2));
    }

    /// A git repository in a temporary folder, deleted afterwards. It ignores the user's and the
    /// system's git settings.
    struct Repo(PathBuf);

    impl Repo {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("tourer-test-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let repo = Repo(dir);
            repo.git(&["init", "-q", "-b", "main"]);
            repo
        }

        fn git(&self, args: &[&str]) -> String {
            let out = Command::new("git")
                .current_dir(&self.0)
                .env("GIT_CONFIG_GLOBAL", if cfg!(windows) { "NUL" } else { "/dev/null" })
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .args(["-c", "user.name=test", "-c", "user.email=test@example.com"])
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        }

        fn commit(&self, file: &str) -> String {
            std::fs::write(self.0.join(file), file).unwrap();
            self.git(&["add", file]);
            self.git(&["commit", "-q", "-m", file]);
            self.git(&["rev-parse", "HEAD"])
        }
    }

    impl Drop for Repo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn offers_uncommitted_and_branch_changes() {
        let repo = Repo::new("offer");
        let fork = repo.commit("a.txt");
        assert_eq!(offer(&repo.0), Some(Offer { uncommitted: 0, branch: None }));

        repo.git(&["checkout", "-q", "-b", "feature"]);
        repo.commit("b.txt");
        std::fs::write(repo.0.join("c.txt"), "new").unwrap();
        assert_eq!(offer(&repo.0), Some(Offer { uncommitted: 1, branch: Some(("main".into(), 1)) }));
        assert_eq!(resolve(&repo.0, "main").unwrap().commit, fork);
    }

    #[test]
    fn tells_when_the_base_moved_or_is_lost() {
        let repo = Repo::new("base");
        let first = repo.commit("a.txt");
        let at_head = DiffBase { rev: "HEAD".into(), commit: first.clone(), to: None };
        assert_eq!(base_state(&repo.0, &at_head), BaseState::Current);

        let second = repo.commit("b.txt");
        let moved = base_state(&repo.0, &at_head);
        assert_eq!(moved, BaseState::Moved { planned: first.clone(), now: second.clone() });
        assert_eq!(
            moved.warning().unwrap(),
            format!("planned from {}, but the changes start at {} now", &first[..7], &second[..7])
        );

        let no_rev = DiffBase { rev: "nope".into(), commit: first, to: None };
        assert!(matches!(base_state(&repo.0, &no_rev), BaseState::Lost { reason } if reason.contains("`nope`")));
        let no_commit = DiffBase { rev: "HEAD".into(), commit: "0".repeat(40), to: None };
        assert!(matches!(base_state(&repo.0, &no_commit), BaseState::Lost { reason } if reason.contains("is gone")));
    }

    #[test]
    fn reads_two_revisions() {
        assert_eq!(Range::parse("main"), Range::Since("main"));
        assert_eq!(Range::parse("main..feature"), Range::Between("main", "feature"));
        assert_eq!(Range::parse("main...feature"), Range::Between("main", "feature"));
        assert_eq!(Range::parse("..feature"), Range::Between("HEAD", "feature"));
        assert_eq!(Range::parse("main.."), Range::Between("main", "HEAD"));
    }

    #[test]
    fn tours_the_changes_between_two_revisions() {
        let repo = Repo::new("between");
        let fork = repo.commit("a.txt");
        repo.git(&["checkout", "-q", "-b", "feature"]);
        let feature = repo.commit("b.txt");
        repo.git(&["checkout", "-q", "main"]);
        repo.commit("c.txt");
        std::fs::write(repo.0.join("untracked.txt"), "new").unwrap();

        // From where main meets feature, so main's own commit and the files on disk are left out.
        let base = resolve(&repo.0, "main..feature").unwrap();
        assert_eq!(base, DiffBase { rev: "main..feature".into(), commit: fork, to: Some(feature.clone()) });
        let changed: Vec<String> = changes(&repo.0, &base).unwrap().files.into_iter().map(|(p, _)| p).collect();
        assert_eq!(changed, ["b.txt"]);

        let dir = repo.0.join("checkout");
        write_checkout(&repo.0, &feature, &dir).unwrap();
        let mut files: Vec<String> =
            std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into()).collect();
        files.sort();
        assert_eq!(files, ["a.txt", "b.txt"]);
        // The repository's own index and work tree stay as they were.
        assert_eq!(repo.git(&["status", "--porcelain"]), "?? checkout/
?? untracked.txt");

        assert_eq!(base_state(&repo.0, &base), BaseState::Current);
        repo.git(&["checkout", "-q", "feature"]);
        let moved = repo.commit("d.txt");
        assert_eq!(base_state(&repo.0, &base), BaseState::EndMoved { planned: feature, now: moved });
    }

    #[test]
    fn lists_files_for_the_prompt() {
        let changes = Changes { text: DIFF.to_string(), files: parse(DIFF) };
        let prompt = changes.for_prompt();
        assert!(prompt.contains("  modified  src/a.rs  +2 -4\n"));
        assert!(prompt.contains("  renamed   y.rs (from x.rs)  +0 -0\n"));
        assert!(prompt.contains("```diff\ndiff --git a/src/a.rs"));
        assert_eq!(split_files(DIFF).len(), 4);
        assert_eq!(changes.file("nope.rs"), &UNCHANGED);
    }
}
