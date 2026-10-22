//! The board: `<repo-root>/${AMD_DIR:-tasks}` with a directory per column.
//!
//! Status is the folder, so "moving a task" is a rename — `git mv` when the
//! file is tracked, so the history is the audit trail.

use std::env;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};

use crate::git;
use crate::group::{GROUP_FILE, Group};
use crate::task::Task;
use crate::templates;

/// Default board directory name; override with `AMD_DIR`.
const DEFAULT_DIR: &str = "tasks";

/// Where archived tickets go. Not a column: it's off the board, and its
/// contents are gitignored, so archiving takes a ticket out of the history
/// rather than recording another status.
const ARCHIVE: &str = "archive";

/// Keeps the drawer itself in git while ignoring what's in it.
const ARCHIVE_GITIGNORE: &str = "*\n!.gitignore\n";

/// The verb that takes a ticket off the board (`amd abandon <ref>`). Reserved
/// the same way a column name is: an epic or sprint called `abandon` would
/// make the destination-verb dispatch ambiguous the moment it existed.
pub const ABANDON: &str = "abandon";

/// The board's id counter: the number the next ticket will take. One line,
/// tracked with the board, so ids keep climbing even when tickets are deleted
/// outright rather than archived — a scan can only see what's still there.
const COUNTER: &str = ".next-id";

/// A counter file's number, if it has one.
fn read_counter(path: &Path) -> Option<u32> {
    crate::eol::read(path).ok()?.trim().parse().ok()
}

/// What a column is for (ticket 223): the behaviour every rule keys on.
/// A board's columns each have one; a team can have several of a kind
/// (Review and QA both `Doing`), but the rules only ever ask the kind.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Kind {
    /// Where new tickets land, and where epics and sprints live.
    Backlog,
    /// Entering one makes the ticket's branch.
    Todo,
    /// Locked while in one, and held to its WIP limit.
    Doing,
    /// Finished: done waits for what a ticket requires, and a done ticket
    /// holds nothing back.
    Done,
}

impl Kind {
    pub const ALL: [Kind; 4] = [Kind::Backlog, Kind::Todo, Kind::Doing, Kind::Done];

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Backlog => "backlog",
            Kind::Todo => "todo",
            Kind::Doing => "doing",
            Kind::Done => "done",
        }
    }

    pub fn parse(name: &str) -> Option<Kind> {
        Kind::ALL
            .into_iter()
            .find(|kind| kind.as_str() == name.trim().to_ascii_lowercase())
    }
}

/// One column of a board (ticket 223): the folder its tickets live in and
/// what kind of column it is. Status is still the folder; the board's
/// [`Workflow`](crate::workflow::Workflow) says which folders there are, in
/// what order, and what each is called on screen.
///
/// Small and `Copy`: folder names are interned (a board has a handful), so
/// a column can be passed around and compared as freely as the enum it
/// replaced. The four standard columns keep that enum's names.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Column {
    folder: &'static str,
    kind: Kind,
}

#[allow(non_upper_case_globals)]
impl Column {
    pub const Backlog: Column = Column::standard("backlog", Kind::Backlog);
    pub const Todo: Column = Column::standard("todo", Kind::Todo);
    pub const Doing: Column = Column::standard("doing", Kind::Doing);
    pub const Done: Column = Column::standard("done", Kind::Done);

    /// The four every board has, in order: its standard workflow.
    pub const STANDARD: [Column; 4] = [Column::Backlog, Column::Todo, Column::Doing, Column::Done];

    const fn standard(folder: &'static str, kind: Kind) -> Column {
        Column { folder, kind }
    }

    /// A column in `folder` of `kind`. The folder name is interned.
    pub fn new(folder: &str, kind: Kind) -> Column {
        Column {
            folder: intern(folder),
            kind,
        }
    }

    /// Its folder under the board, which is its name in the CLI and in git.
    pub fn as_str(self) -> &'static str {
        self.folder
    }

    pub fn kind(self) -> Kind {
        self.kind
    }

    /// Whether it's a column of `kind`.
    pub fn is(self, kind: Kind) -> bool {
        self.kind == kind
    }
}

/// `name` as a `&'static str`, leaked once per distinct name: column
/// folders are few and live as long as the process.
fn intern(name: &str) -> &'static str {
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};
    static NAMES: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    for standard in Column::STANDARD {
        if standard.folder == name {
            return standard.folder;
        }
    }
    let mut names = NAMES
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(&known) = names.get(name) {
        return known;
    }
    let leaked: &'static str = Box::leak(name.to_string().into_boxed_str());
    names.insert(leaked);
    leaked
}

impl fmt::Display for Column {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A move was refused because its destination column is already at (or
/// past) its configured WIP limit. Gates entry only — a column that's
/// already over its limit stays movable-out-of, since this never looks at
/// where a ticket is leaving from.
#[derive(Debug)]
pub struct WipLimitExceeded {
    pub column: Column,
    pub limit: u32,
    pub current: usize,
}

impl fmt::Display for WipLimitExceeded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}/ already has {} of {} allowed — move something out first, or use --force",
            self.column, self.current, self.limit
        )
    }
}

impl std::error::Error for WipLimitExceeded {}

/// What `Board::empty_group` actually did — every ticket it moved, and
/// every ticket (stem, reason) it skipped rather than aborting the batch
/// over.
pub struct EmptiedGroup {
    pub moved: Vec<PathBuf>,
    pub skipped: Vec<(String, String)>,
}

pub struct Board {
    /// Absolute path to the board directory itself (`…/tasks`).
    pub root: PathBuf,
}

impl Board {
    /// A board at an explicit path, for tests and for a caller handed one
    /// rather than discovering it.
    pub fn at(root: PathBuf) -> Board {
        Board { root }
    }

    /// The board inside a repository whose root we already know — a registered
    /// one, rather than the repository we happen to be standing in.
    pub fn in_repo(repo: &std::path::Path) -> Board {
        Board {
            root: repo.join(dir_name()),
        }
    }
}

impl Board {
    /// Where the board *would* live for the current directory. Does not
    /// require it to exist — `amd init` needs the path before the directory.
    pub fn locate() -> Result<Board> {
        let dir = dir_name();
        let repo = git::repo_root().ok_or_else(|| {
            anyhow!("not inside a git repository (agile-md boards live at <repo>/{dir})")
        })?;
        Ok(Board {
            root: repo.join(dir),
        })
    }

    /// Open an existing board, or say where it should have been. Creating one
    /// is a decision for the caller — the CLI asks, the GUI offers a button.
    pub fn open() -> Result<Board> {
        let board = Board::locate()?;
        if board.root.is_dir() {
            return Ok(board);
        }
        bail!(
            "no board at {} — run 'amd init' (or set AMD_YES=1)",
            board.root.display()
        )
    }

    /// `locate()`, plus two fallbacks for the layout `amd init <project>`
    /// creates: a plain parent directory holding `{project}-tickets` (and
    /// `{project}-documents`) as siblings.
    ///
    /// **Standing inside a git repository that has no board of its own** —
    /// a team member working in the *code* repository, a sibling of the
    /// tickets repo rather than an ancestor of it — checks that repository's
    /// own parent directory for exactly one qualifying sibling before
    /// giving up. Ticket 061: `locate()` alone happily "succeeds" here,
    /// returning a `Board` at a `tasks/` that doesn't exist, so without this
    /// the sibling check below was never even reached — every command
    /// treated the code repository as a (nonexistent, emptyish) board of
    /// its own instead of finding the real one next door.
    ///
    /// **Standing outside any git repository at all** — the original case
    /// this was built for — checks the current directory's own children
    /// instead, so `amd`/`amdui` can run from that parent directory itself,
    /// not only from one `cd` deeper inside the tickets repository.
    ///
    /// **The two cases resolve ambiguity differently, deliberately.** The
    /// plain-directory case has no sane fallback if the scan is ambiguous —
    /// `locate()` already failed outright — so it errors, naming every
    /// candidate, never a silent guess. The in-a-repo case always has one:
    /// `locate()`'s own (possibly nonexistent) answer, the "no board here
    /// yet" a genuine single-repo board already relies on. A repository's
    /// parent is far more likely to hold unrelated other repositories than
    /// a directory purpose-built by `amd init` is — confirmed live, the
    /// hard way, by this project's own test suite: `tests/test.sh` alone
    /// leaves enough sibling scratch boards under a shared temp root that
    /// an unconditional ambiguity error here broke unrelated, pre-existing
    /// tests that create a fresh repo and expect today's plain "no board
    /// yet" behaviour, not a wall of unrelated names. So: found none, or
    /// found more than one, both fall back to `locate()`'s own answer
    /// silently, and only a genuinely unambiguous single sibling redirects.
    ///
    /// **Several projects in one folder** (ticket 093) refine both cases,
    /// through `current_project::pick`: a `.current-project` file in the
    /// folder names the one meant, and wins whenever it's there. Without
    /// it, the in-a-repo case's silent fallback above no longer applies to
    /// a folder holding two or more `*-tickets` repositories — that's the
    /// layout `amd init`/`amd join` make, so several of them is a folder of
    /// projects, not a temp directory, and a silent "no board" there hid
    /// the problem. It errors instead, naming `amd project use`.
    pub fn discover() -> Result<Board> {
        if let Ok(board) = Board::locate() {
            if board.root.is_dir() {
                return Ok(board);
            }
            if let Some(parent) = board.repo_root().parent()
                && let Some(only) = crate::current_project::pick(parent, false)?
            {
                return Ok(Board::in_repo(&only));
            }
            return Ok(board);
        }
        let cwd = env::current_dir().context("reading the current directory")?;
        match crate::current_project::pick(&cwd, true)? {
            Some(only) => Ok(Board::in_repo(&only)),
            None => bail!(
                "not inside a git repository, and no project board among this directory's own \
                 subdirectories either (agile-md boards live at <repo>/{})",
                dir_name()
            ),
        }
    }

    /// The board directory's name (`tasks`), for display and templates.
    pub fn name(&self) -> String {
        self.root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(dir_name)
    }

    /// The board's columns, in order, each with its kind (ticket 223): the
    /// `columns` its `.settings` records (ticket 224), else the standard
    /// four. One that can't be read is the standard four too, and
    /// [`workflow_problem`](Self::workflow_problem) says why.
    pub fn workflow(&self) -> crate::workflow::Workflow {
        self.read_workflow().0
    }

    /// Why the board's `columns` setting couldn't be used, if it couldn't.
    pub fn workflow_problem(&self) -> Option<String> {
        self.read_workflow().1
    }

    /// The workflow and any problem with it, parsed once per change to the
    /// settings file: every listing asks for it, often many times.
    ///
    /// A change is a change to the file's contents (ticket 317). Its
    /// modified time and size aren't enough: a reorder swaps two names,
    /// keeping the size, and two writes a few milliseconds apart often get
    /// the same time (always on Windows, sometimes on Linux), so the old
    /// order was kept. The file is a few hundred bytes; reading it costs
    /// about what asking for its time did.
    fn read_workflow(&self) -> (crate::workflow::Workflow, Option<String>) {
        use crate::workflow::Workflow;
        use std::collections::HashMap;
        use std::sync::{Mutex, OnceLock};
        type Seen = (Option<Vec<u8>>, Workflow, Option<String>);
        static CACHE: OnceLock<Mutex<HashMap<PathBuf, Seen>>> = OnceLock::new();
        let path = crate::board_settings::path(self);
        let stamp = fs::read(&path).ok();
        let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
        if let Ok(seen) = cache.lock()
            && let Some((when, workflow, problem)) = seen.get(&path)
            && *when == stamp
        {
            return (workflow.clone(), problem.clone());
        }
        let settings = crate::board_settings::load(self);
        let (workflow, mut problem) = match settings.columns() {
            None => (Workflow::standard(), None),
            Some(text) => match Workflow::parse(text) {
                Ok(workflow) => (workflow, None),
                Err(e) => (
                    Workflow::standard(),
                    Some(format!(
                        "the columns setting can't be used ({e:#}), so the board shows the standard four"
                    )),
                ),
            },
        };
        // The flow, extra moves and landing column (ticket 225); one that
        // can't be used falls back to the defaults, saying why.
        let flow = settings.flow().map(|f| {
            crate::workflow::Flow::parse(f).ok_or_else(|| anyhow!("flow {f:?} isn't next or any"))
        });
        let configured = match flow.transpose() {
            Ok(flow) => workflow.clone().configure(
                flow.unwrap_or_default(),
                settings.moves().unwrap_or(""),
                settings.new_tickets(),
            ),
            Err(e) => Err(e),
        };
        let workflow = match configured {
            Ok(workflow) => workflow,
            Err(e) => {
                let note = format!(
                    "the flow settings can't be used ({e:#}), so moves go one column at a time"
                );
                problem = Some(match problem {
                    Some(p) => format!("{p}; {note}"),
                    None => note,
                });
                workflow
            }
        };
        if let Ok(mut seen) = cache.lock() {
            seen.insert(path, (stamp, workflow.clone(), problem.clone()));
        }
        (workflow, problem)
    }

    pub fn dir(&self, column: Column) -> PathBuf {
        self.root.join(column.as_str())
    }

    /// The repository containing the board — where `README.md` and the
    /// board-local settings file live, as siblings of the board directory
    /// rather than inside it.
    pub fn repo_root(&self) -> &Path {
        self.root.parent().unwrap_or(&self.root)
    }

    /// Whether this board lives in a dedicated tickets repository, the
    /// `{project}-tickets` that `amd init <project>` and `amd join` make,
    /// rather than inside the code it tracks. A ticket's branch can't go in
    /// one, so it has to name the repository it's about (tickets 053, 066).
    pub fn is_tickets_board(&self) -> bool {
        self.repo_root()
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with("-tickets"))
    }

    pub fn templates_dir(&self) -> PathBuf {
        self.root.join("templates")
    }

    pub fn archive_dir(&self) -> PathBuf {
        self.root.join(ARCHIVE)
    }

    /// The id counter (ticket 150): `.next-id` at the repository root, beside
    /// `.settings`. It has a file of its own so `.settings` holds only
    /// settings, which lets a rule that only admins may change settings
    /// (151) leave everyone free to create tickets.
    pub fn counter_path(&self) -> PathBuf {
        self.repo_root().join(COUNTER)
    }

    /// Where the counter lived first, inside the board directory. Read, to
    /// migrate it, and never written.
    pub fn legacy_counter_path(&self) -> PathBuf {
        self.root.join(COUNTER)
    }

    /// What the next id should be, if something on disk says so: the
    /// highest of the counter file, a `next-id` key in `.settings` (a board
    /// from before ticket 150) and `<board>/.next-id` (an older one). Reads
    /// only: working out a number must change nothing, because it happens
    /// before a command's auto-commit snapshot (ticket 164), and on a dry
    /// run. [`migrate_counter`](Self::migrate_counter) moves them.
    fn stored_next_id(&self) -> Option<u32> {
        let current = read_counter(&self.counter_path());
        let in_settings = crate::board_settings::load(self).next_id();
        let legacy = read_counter(&self.legacy_counter_path());
        [current, in_settings, legacy].into_iter().flatten().max()
    }

    /// [`stored_next_id`](Self::stored_next_id), with the old places moved
    /// into the counter file and cleared: the highest value found wins, so
    /// it happens once per board and never hands out a number twice. Called
    /// when a number is recorded, which is after the snapshot, so the
    /// `.settings` change lands in that command's own commit (ticket 164;
    /// it used to happen while the number was worked out, and was never
    /// committed).
    fn migrate_counter(&self) -> Option<u32> {
        let current = read_counter(&self.counter_path());
        let mut settings = crate::board_settings::load(self);
        let in_settings = settings.next_id();
        let legacy = read_counter(&self.legacy_counter_path());
        if in_settings.is_none() && legacy.is_none() {
            return current;
        }
        let highest = [current, in_settings, legacy].into_iter().flatten().max()?;
        if self.write_counter(highest).is_ok() {
            if in_settings.is_some() && settings.remove_next_id() {
                let _ = settings.save(self);
            }
            if legacy.is_some() {
                let _ = fs::remove_file(self.legacy_counter_path());
            }
        }
        Some(highest)
    }

    fn write_counter(&self, next: u32) -> Result<()> {
        let path = self.counter_path();
        fs::write(&path, format!("{next}\n")).with_context(|| format!("writing {}", path.display()))
    }

    /// Scaffold the columns, the board-local settings file's counter, and
    /// the repository's README. Idempotent.
    pub fn create(&self) -> Result<()> {
        for column in self.workflow().columns() {
            let dir = self.dir(column);
            fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
            let keep = dir.join(".gitkeep");
            if !keep.exists() {
                fs::write(&keep, "").with_context(|| format!("creating {}", keep.display()))?;
            }
        }
        // The drawer is tracked; what you put in it is not.
        let archive = self.archive_dir();
        fs::create_dir_all(&archive).with_context(|| format!("creating {}", archive.display()))?;
        let ignore = archive.join(".gitignore");
        if !ignore.exists() {
            fs::write(&ignore, ARCHIVE_GITIGNORE)
                .with_context(|| format!("creating {}", ignore.display()))?;
        }

        if self.stored_next_id().is_none() {
            self.write_counter(1)?;
        }

        let readme = self.repo_root().join("README.md");
        if !readme.exists() {
            let rendered = templates::render_board_readme(self)?;
            fs::write(&readme, rendered)
                .with_context(|| format!("writing {}", readme.display()))?;
        }
        Ok(())
    }

    /// Tasks in one column, ordered by id then filename.
    /// Every ticket in a column, epic folders included.
    ///
    /// The backlog may hold subdirectories — one per epic or sprint — and their
    /// tickets are as real as the loose ones. Anything that counts ids must see
    /// them: a ticket invisible to the scan is an id waiting to be handed out
    /// twice, which silently repoints every reference to it.
    pub fn tasks_in(&self, column: Column) -> Result<Vec<Task>> {
        let dir = self.dir(column);
        let mut tasks = self.tasks_directly_in(&dir, column, None)?;
        for epic in self.epics_in(column)? {
            tasks.extend(self.tasks_directly_in(&dir.join(&epic), column, Some(epic))?);
        }
        tasks.sort_by(|a, b| a.id.cmp(&b.id).then_with(|| a.stem.cmp(&b.stem)));
        Ok(tasks)
    }

    /// The epic folders of a column, named by directory. Only the backlog has
    /// them: an epic is a way to group work you have not committed to yet, and
    /// letting them appear in every column would make "status is the folder"
    /// ambiguous.
    pub fn epics_in(&self, column: Column) -> Result<Vec<String>> {
        if column != Column::Backlog {
            return Ok(Vec::new());
        }
        let dir = self.dir(column);
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(err).with_context(|| format!("reading {}", dir.display())),
        };
        let mut epics = Vec::new();
        for entry in entries {
            let path = entry
                .with_context(|| format!("reading {}", dir.display()))?
                .path();
            if path.is_dir()
                && let Some(name) = path.file_name().and_then(|name| name.to_str())
                && !name.starts_with('.')
            {
                epics.push(name.to_string());
            }
        }
        epics.sort();
        Ok(epics)
    }

    fn tasks_directly_in(
        &self,
        dir: &Path,
        column: Column,
        epic: Option<String>,
    ) -> Result<Vec<Task>> {
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(err).with_context(|| format!("reading {}", dir.display())),
        };
        let mut tasks = Vec::new();
        for entry in entries {
            let path = entry
                .with_context(|| format!("reading {}", dir.display()))?
                .path();
            if path.file_name().and_then(|n| n.to_str()) == Some(GROUP_FILE) {
                continue;
            }
            if path.is_file()
                && let Some(mut task) = Task::from_path(&path, column)
            {
                task.epic = epic.clone();
                tasks.push(task);
            }
        }
        Ok(tasks)
    }

    /// Every group in the backlog — epics and sprints alike — read from their
    /// own directories.
    pub fn groups(&self) -> Result<Vec<Group>> {
        let mut groups = Vec::new();
        for name in self.epics_in(Column::Backlog)? {
            groups.push(Group::read(&self.dir(Column::Backlog).join(name))?);
        }
        Ok(groups)
    }

    /// Look one up by directory name.
    pub fn group(&self, name: &str) -> Result<Group> {
        let dir = self.dir(Column::Backlog).join(name);
        if !dir.is_dir() {
            bail!("no group called {name}");
        }
        Group::read(&dir)
    }

    /// Create an epic or a sprint. The directory name is the identity, so a
    /// second group of the same name is a mistake rather than a merge.
    pub fn create_group(&self, group: &Group) -> Result<()> {
        check_group_name(self, &group.name)?;
        if group.dir.exists() {
            bail!("{} already exists", group.name);
        }
        group.save()
    }

    /// Rename an epic or sprint everywhere it is recorded (ticket 083): its
    /// directory, the `name` in its `_group.md`, and the `epic` of every
    /// ticket in it — those in its folder and those that have moved on to
    /// other columns. Refuses, changing nothing, if any of those tickets is
    /// leased by someone else. Returns the group's new directory.
    pub fn rename_group(&self, from: &str, to: &str) -> Result<PathBuf> {
        let to = to.trim();
        let mut group = self.group(from)?;
        if to == from {
            bail!("{from} is already called that");
        }
        check_group_name(self, to)?;
        let dest = self.dir(Column::Backlog).join(to);
        // A different spelling of the same name is fine (`ui` → `UI`), but
        // never another group's folder.
        if dest.exists() && !from.eq_ignore_ascii_case(to) {
            bail!("{to} already exists");
        }
        let members = self.tasks_in_group(from)?;
        for task in &members {
            task.ensure_not_leased()?;
        }

        if git::is_tracked(&self.root, &group.dir.join(GROUP_FILE))
            || members.iter().any(|t| git::is_tracked(&self.root, &t.path))
        {
            git::mv(&self.root, &group.dir, &dest)?;
        } else {
            fs::rename(&group.dir, &dest)
                .with_context(|| format!("renaming {}", group.dir.display()))?;
        }
        group.dir = dest.clone();
        group.name = to.to_string();
        group.save()?;
        for task in self.tasks_in_group(from)?.into_iter().chain(
            self.tasks_in(Column::Backlog)?
                .into_iter()
                .filter(|t| t.epic.as_deref() == Some(to)),
        ) {
            if task.epic_meta().as_deref() != Some(to) {
                task.set_epic_meta(task.hash()?, to)?;
            }
        }
        Ok(dest)
    }

    /// File a ticket under an epic, or move it back to the loose backlog with
    /// `None`. Inside `backlog/` the folder is the move, and the frontmatter
    /// is updated to match; outside it (`todo`/`doing`/`done`) there is no
    /// group directory to move into, so this only ever writes the `epic`
    /// frontmatter key in place — the ticket stays exactly where it is,
    /// column included. Calling this on a `doing`/`done` ticket does not
    /// send it back to the backlog.
    pub fn set_epic(&self, task: &Task, epic: Option<&str>) -> Result<PathBuf> {
        self.file(task, epic, false)
    }

    /// A ticket with a due date can't be in an epic (ticket 253): an epic
    /// is open-ended, a due date isn't. A sprint may hold one (the page
    /// warns). Refuses ticket `id` in `epic` with `due`; anything else is
    /// fine, an epic that doesn't exist included.
    pub fn refuse_due_in_epic(
        &self,
        id: &str,
        epic: Option<&str>,
        due: Option<&str>,
    ) -> Result<()> {
        let (Some(epic), Some(due)) = (
            epic.filter(|e| !e.is_empty()),
            due.map(str::trim).filter(|d| !d.is_empty()),
        ) else {
            return Ok(());
        };
        match self.group(epic) {
            Ok(group) if !group.is_sprint() => bail!(
                "{id} is due {due}, so it can't be in the epic {epic}: an epic has no end date. \
                 Take the due date off, or keep it in the loose backlog or a sprint"
            ),
            _ => Ok(()),
        }
    }

    /// [`set_epic`](Self::set_epic), or with `deliberate` set, past the
    /// started-sprint rule: what closing a sprint (its unfinished tickets
    /// back to the backlog) and backfill (tickets into a running sprint) do
    /// on purpose (ticket 052). Every other check stays.
    pub(crate) fn file(
        &self,
        task: &Task,
        epic: Option<&str>,
        deliberate: bool,
    ) -> Result<PathBuf> {
        // Checked up front, not left to set_epic_meta's own write_checked
        // afterwards — that would risk a half-moved, half-refused ticket if
        // someone else holds the lease.
        task.ensure_not_leased()?;
        // A ticket with a due date can't go into an epic (ticket 253).
        self.refuse_due_in_epic(&task.id_display(), epic, task.due().as_deref())?;
        // A started sprint is a commitment (ticket 083, replacing ADR-0009's
        // "in and out" rule): it takes no more tickets, and gives none back —
        // its tickets only move on, to todo.
        let current = match task.column {
            Column::Backlog => task.epic.clone(),
            _ => task.epic_meta(),
        };
        if current.as_deref() != epic && !deliberate {
            if let Some(name) = current.as_deref()
                && let Ok(group) = self.group(name)
                && !group.accepts_changes()
            {
                bail!(
                    "{name} {}: its tickets can only move on to todo",
                    group.locked_word()
                );
            }
            if let Some(name) = epic {
                let group = self.group(name)?;
                if !group.accepts_changes() {
                    bail!(
                        "{name} {}: no more tickets can be added to it",
                        group.locked_word()
                    );
                }
            }
        }
        if let Some(name) = epic {
            let group = self.group(name)?;
            // An unsized ticket in a sprint makes the sprint's total a lie,
            // which is the one number a sprint is for. This rule stays.
            if group.is_sprint() && task.points().is_none() {
                bail!("{} needs points before it can go in {name}", task.stem);
            }
            // A sprint is a commitment, and a draft isn't ready to commit to
            // (ticket 114). An epic is fine: it's still the backlog.
            if group.is_sprint() && task.is_draft() {
                bail!(
                    "{} is a draft: remove {} before it can go in {name}",
                    task.id_display(),
                    crate::task::DRAFT_TAG
                );
            }
        }

        if task.column != Column::Backlog {
            let hash = task.hash()?;
            task.set_epic_meta(hash, epic.unwrap_or_default())?;
            return Ok(task.path.clone());
        }

        let dir = match epic {
            Some(name) => self.dir(Column::Backlog).join(name),
            None => self.dir(Column::Backlog),
        };
        fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        let dest = dir.join(task.file_name());
        if dest != task.path {
            if dest.exists() {
                bail!("{} already exists", dest.display());
            }
            // Same rule as move_task: `git mv` so history follows the rename
            // when the ticket is tracked, a plain rename when it is not.
            if git::is_tracked(&self.root, &task.path) {
                git::mv(&self.root, &task.path, &dest)?;
            } else {
                fs::rename(&task.path, &dest)
                    .with_context(|| format!("moving {}", task.path.display()))?;
            }
        }
        let moved = Task::from_path(&dest, Column::Backlog)
            .ok_or_else(|| anyhow::anyhow!("{} is not a task", dest.display()))?;
        let hash = moved.hash()?;
        moved.set_epic_meta(hash, epic.unwrap_or_default())?;
        Ok(dest)
    }

    /// Empty a group's folder into the loose backlog at once — every
    /// ticket currently in it, no precondition, the same "no precondition"
    /// as an epic already has for a single ticket. Reuses `set_epic` per
    /// ticket rather than a new mechanism, so it inherits the same
    /// lease/hash checks that setter already has. A ticket that can't move
    /// (someone else's active lease, say) is skipped rather than aborting
    /// the whole batch — reported the same way `amd import` already
    /// reports a skipped ticket.
    pub fn empty_group(&self, name: &str) -> Result<EmptiedGroup> {
        let mut moved = Vec::new();
        let mut skipped = Vec::new();
        for task in self.tasks_in_group(name)? {
            let stem = task.stem.clone();
            match self.set_epic(&task, None) {
                Ok(dest) => moved.push(dest),
                Err(err) => skipped.push((stem, err.to_string())),
            }
        }
        Ok(EmptiedGroup { moved, skipped })
    }

    /// Delete a group outright — its `_group.md` and the (now-empty)
    /// directory. Refuses if it still has any tickets, checked the same way
    /// `empty_group` finds them: every column, not just the physical
    /// folder, since a committed sprint's tickets can sit in `doing`/`done`
    /// while still claiming membership. `empty` it first, same two-step
    /// `create_group`'s own "already exists" refusal expects the caller to
    /// resolve deliberately rather than the tool guessing.
    ///
    /// Tickets that are done hold nothing back (ticket 204): a finished
    /// sprint or epic can go, and its done tickets keep their `epic` label
    /// as the record of what they were part of.
    pub fn delete_group(&self, name: &str) -> Result<()> {
        let group = self.group(name)?;
        let open = self
            .tasks_in_group(name)?
            .into_iter()
            .filter(|t| !t.column.is(Kind::Done))
            .count();
        if open > 0 {
            bail!("{name} still has {open} ticket(s) not done — finish them or empty it first");
        }
        fs::remove_dir_all(&group.dir).with_context(|| format!("removing {}", group.dir.display()))
    }

    /// Every task on the board, column order first.
    /// Every ticket belonging to a group, in whatever column it has reached.
    ///
    /// A sprint's scope does not shrink because somebody started work. Counting
    /// only `backlog/` made a sprint's points fall as tickets moved to
    /// `doing/`, which is the opposite of what that number is for.
    ///
    /// Which field answers "what group is this in" depends on where the ticket
    /// is, and the two are not interchangeable. Inside `backlog/` the **folder**
    /// decides — a ticket dragged into an epic directory by hand is in that
    /// epic, frontmatter or no. Outside it there are no group directories, so
    /// the `epic` key written by `set_epic` is the only carrier.
    ///
    /// The archive is deliberately not searched: an archived ticket has left
    /// the board.
    pub fn tasks_in_group(&self, name: &str) -> Result<Vec<Task>> {
        Ok(self
            .tasks()?
            .into_iter()
            .filter(|task| match task.column {
                Column::Backlog => task.epic.as_deref() == Some(name),
                _ => task.epic_meta().as_deref() == Some(name),
            })
            .collect())
    }

    /// Ticket ids held by more than one ticket on the board, sorted. Empty
    /// on a healthy board; `sync.rs` checks it after replaying a change on
    /// top of someone else's, so two tickets numbered alike never reach the
    /// remote.
    pub fn duplicate_ids(&self) -> Result<Vec<u32>> {
        let mut seen = std::collections::BTreeMap::<u32, usize>::new();
        for id in self.tasks()?.iter().filter_map(|task| task.id) {
            *seen.entry(id).or_default() += 1;
        }
        Ok(seen
            .into_iter()
            .filter(|&(_, count)| count > 1)
            .map(|(id, _)| id)
            .collect())
    }

    pub fn tasks(&self) -> Result<Vec<Task>> {
        let mut all = Vec::new();
        for column in self.workflow().columns() {
            all.extend(self.tasks_in(column)?);
        }
        Ok(all)
    }

    /// Every tag already in use on the board, in uppercase (ticket 122),
    /// sorted and deduplicated — the suggestions offered when a task is
    /// created interactively.
    pub fn tags(&self) -> Result<Vec<String>> {
        Ok(self.tag_counts()?.into_keys().collect())
    }

    /// How many tickets carry each tag, in uppercase, across every column
    /// and epic or sprint folder (not the archive). A ticket carrying a tag
    /// twice in different cases counts once.
    pub fn tag_counts(&self) -> Result<std::collections::BTreeMap<String, usize>> {
        let mut counts = std::collections::BTreeMap::new();
        for task in self.tasks()? {
            for tag in crate::task::normalize_tags(&task.tags()) {
                *counts.entry(tag).or_insert(0) += 1;
            }
        }
        Ok(counts)
    }

    /// Every ticket carrying `tag`, across every column, in board order.
    /// `amd import`'s `DUPLICATE` tag is the motivating case — reviewing
    /// what an import flagged — but this filters on any tag, not just that
    /// one.
    pub fn tasks_tagged(&self, tag: &str) -> Result<Vec<Task>> {
        Ok(self
            .tasks()?
            .into_iter()
            .filter(|task| task.has_tag(tag))
            .collect())
    }

    /// Everyone with a ticket here, sorted — the suggestions when assigning
    /// one.
    pub fn assignees(&self) -> Result<Vec<String>> {
        let mut assignees: Vec<String> = Vec::new();
        for task in self.tasks()? {
            for who in task.assignees() {
                if !assignees.contains(&who) {
                    assignees.push(who);
                }
            }
        }
        assignees.sort();
        Ok(assignees)
    }

    /// Every task's `NNN-slug`, the strings `amd new` completes a related ref
    /// against.
    pub fn stems(&self) -> Result<Vec<String>> {
        Ok(self.tasks()?.into_iter().map(|task| task.stem).collect())
    }

    /// Tickets that have been archived. Not part of the board, but they still
    /// hold ids.
    pub fn archived(&self) -> Result<Vec<Task>> {
        let dir = self.archive_dir();
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(err).with_context(|| format!("reading {}", dir.display())),
        };
        let mut tasks = Vec::new();
        for entry in entries {
            let path = entry
                .with_context(|| format!("reading {}", dir.display()))?
                .path();
            // Archived tickets keep the column they were last in; nothing reads
            // it, but it means a restored file lands somewhere sensible.
            if path.is_file()
                && let Some(task) = Task::from_path(&path, Column::Todo)
            {
                tasks.push(task);
            }
        }
        Ok(tasks)
    }

    /// The id the next ticket will take: whichever is higher of the counter
    /// file and one past the highest id on the board.
    ///
    /// The counter is the authority — it remembers ids whose tickets have gone,
    /// archived or deleted, which no scan can. The scan is the safety net: a
    /// lost, stale or badly merged counter can then never hand out an id that
    /// is already in use, and a ticket added by hand still pushes it along.
    /// Only the columns are scanned; the archive is off the board and the
    /// counter already accounts for it.
    ///
    /// That's `local` numbering, the default; the project can choose another
    /// (`numbering.rs`), and this dispatches to it.
    pub fn next_id(&self) -> Result<u32> {
        crate::numbering::Numbering::for_board(self)?.next_id(self)
    }

    /// Record that `used` has been taken — through whichever numbering the
    /// project uses (`numbering.rs`).
    pub fn record_id(&self, used: u32) -> Result<()> {
        crate::numbering::Numbering::for_board(self)?.record_id(self, used)
    }

    /// `local` numbering's next id: the counter, or one past the highest id
    /// in the columns, whichever is higher.
    pub(crate) fn local_next_id(&self) -> Result<u32> {
        let highest = self
            .tasks()?
            .iter()
            .filter_map(|task| task.id)
            .max()
            .unwrap_or(0);
        Ok(self.stored_next_id().unwrap_or(0).max(highest + 1))
    }

    /// Record that `used` has been taken, so the next ticket gets the one
    /// after. Called once the ticket is on disk, never when one is merely
    /// drafted — an abandoned draft should leave no gap.
    pub(crate) fn local_record_id(&self, used: u32) -> Result<()> {
        let next = used.saturating_add(1);
        if self.migrate_counter().is_some_and(|stored| stored >= next) {
            return Ok(());
        }
        self.write_counter(next)
    }

    /// Take a ticket off the board. The archive is gitignored, so a
    /// tracked ticket leaves the index (`git rm --cached`) and then moves —
    /// `git mv` would refuse the ignored destination, and forcing it would put
    /// the ticket back into the history the .gitignore is there to keep it out of.
    ///
    /// Also cleans up the ticket's branch, if it has one that's empty: a
    /// ticket can sit in `todo/` — where entering it creates a branch — and be
    /// abandoned before real work starts, so moving the trigger earlier just
    /// relocates which column produces unused branches (docs/adr/0006). The
    /// returned name is `Some` when a branch was actually deleted.
    pub fn archive(&self, task: &Task) -> Result<(PathBuf, Option<String>)> {
        task.ensure_not_leased()?;
        // Moving a ticket out of a started sprint leaves a `git mv` in the
        // history: the scope change is dated and the points are still readable
        // on disk. Archiving from inside one leaves neither — the file is
        // gitignored and untracked, so the board no longer shows that those
        // points were ever committed to. Take it out of the sprint first; then
        // archive it. Two steps, and the record survives the first.
        if let Some(name) = task.epic.as_deref()
            && let Ok(group) = self.group(name)
            && group.is_sprint()
            && !group.accepts_changes()
        {
            bail!(
                "{} is in {name}, which {} — move it on to todo before archiving it",
                task.stem,
                group.locked_word()
            );
        }
        let dir = self.archive_dir();
        fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        let dest = dir.join(task.file_name());
        if dest.exists() {
            bail!("{} already exists", dest.display());
        }
        // Off the board means off your hands: give the lease back (ticket
        // 092). Best-effort — a ticket that can't be rewritten is still
        // archived, and the archive is local and untracked anyway.
        let _ = task.release_if_mine();
        // Read before the move, not after: once the file lands in dest, it no
        // longer exists at task.path for task.branch() to read.
        let branch = task.branch();
        // Its branch lives in the repository it's about, when it names one
        // that's cloned here (ticket 107); otherwise where it always did.
        let branch_repo = task
            .repository()
            .and_then(|name| crate::registry::clone_of(&self.root, &name))
            .unwrap_or_else(|| self.root.clone());
        if git::is_tracked(&self.root, &task.path) {
            git::untrack(&self.root, &task.path)?;
        }
        fs::rename(&task.path, &dest).with_context(|| format!("moving {}", task.path.display()))?;
        let deleted = branch.and_then(|name| cleanup_branch(&branch_repo, &name));
        Ok((dest, deleted))
    }

    /// Resolve a task reference: a numeric id (`7`, `007`) or a unique slug
    /// substring. Ambiguity is an error, never a guess.
    pub fn find(&self, reference: &str) -> Result<Task> {
        let tasks = self.tasks()?;
        let mut hits: Vec<Task> = if let Ok(id) = reference.parse::<u32>() {
            tasks.into_iter().filter(|t| t.id == Some(id)).collect()
        } else {
            tasks
                .into_iter()
                .filter(|t| t.stem.contains(reference))
                .collect()
        };
        match hits.len() {
            0 => bail!("no task matches '{reference}'"),
            1 => Ok(hits.remove(0)),
            n => {
                let names: Vec<&str> = hits.iter().map(|t| t.stem.as_str()).collect();
                bail!(
                    "'{reference}' matches {n} tasks; be more specific ({})",
                    names.join(", ")
                )
            }
        }
    }

    /// Move a task to another column, preferring `git mv` so the rename is
    /// tracked. Deliberately does not commit — that stays the user's call.
    ///
    /// The rules (ticket 083): one column at a time, forwards or back. Any
    /// epic or sprint feeds todo, and the ticket keeps its `epic` there. Sent
    /// back from todo it returns to that epic or sprint — the loose backlog
    /// is just the default epic — unless the epic is gone, or it is a sprint
    /// that has started (or one it no longer qualifies for); then it lands in
    /// the loose backlog.
    ///
    /// Returns where the ticket landed. It deliberately says nothing: a window
    /// has nowhere to put a line of text, and a library that prints is a
    /// library that has decided what its caller's output looks like
    /// (docs/adr/0004-one-library-two-interfaces.md).
    pub fn move_task(&self, task: &Task, to: Column, force: bool) -> Result<PathBuf> {
        if task.column == to {
            bail!("already in {to}/");
        }
        if !self.workflow().allows(task.column, to) {
            let extra = if self.workflow().moves().is_empty() {
                ""
            } else {
                ", or by a move the team allows"
            };
            bail!(
                "{} cannot move from {}/ to {to}/: tickets move one column at a time{extra}",
                task.stem,
                task.column
            );
        }
        // A draft stays in the backlog until it's ready (ticket 114): any
        // backlog-kind column (a Triage, say) is still the backlog.
        if task.column.is(Kind::Backlog) && !to.is(Kind::Backlog) && task.is_draft() {
            bail!(
                "{} is a draft: remove {} (`amd ready {}`) to take it out of the backlog",
                task.id_display(),
                crate::task::DRAFT_TAG,
                task.id_display()
            );
        }
        // A started sprint's tickets only move on, to todo (ticket 083): with
        // a backlog-kind column before the backlog, that's not the only
        // neighbour any more.
        if task.column == Column::Backlog
            && !to.is(Kind::Todo)
            && let Some(name) = &task.epic
            && let Ok(group) = self.group(name)
            && group.is_sprint()
            && !group.accepts_changes()
        {
            let state = if group.state == crate::group::State::Closed {
                "closed"
            } else {
                "started"
            };
            bail!(
                "{} is in {state} sprint {name}: its tickets only move on, to todo",
                task.id_display()
            );
        }
        task.ensure_not_leased()?;
        // Done waits for what the ticket requires and for its children
        // (ticket 213). No `force`: the way past it is to change the link.
        // Any done-kind column (ticket 223).
        if to.is(Kind::Done) && !task.column.is(Kind::Done) {
            let waiting = crate::links::waiting_on(self, task)?;
            if !waiting.is_empty() {
                return Err(crate::links::NotReady {
                    id: task.id_display(),
                    waiting,
                }
                .into());
            }
        }
        if !force && let Some(limit) = crate::board_settings::load(self).wip_limit(to) {
            let current = self.tasks_in(to)?.len();
            if current as u32 >= limit {
                return Err(WipLimitExceeded {
                    column: to,
                    limit,
                    current,
                }
                .into());
            }
        }
        let dir = match self.home_group(task, to) {
            Some(group) => group.dir,
            None => self.dir(to),
        };
        fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        let dest = dir.join(task.file_name());
        if dest.exists() {
            bail!("{} already exists", dest.display());
        }
        if git::is_tracked(&self.root, &task.path) {
            git::mv(&self.root, &task.path, &dest)?;
        } else {
            fs::rename(&task.path, &dest)
                .with_context(|| format!("moving {}", task.path.display()))?;
        }
        Ok(dest)
    }

    /// The epic or sprint a ticket returns to when it goes back from todo to
    /// the backlog: the one its `epic` names, if that still exists and will
    /// take it.
    fn home_group(&self, task: &Task, to: Column) -> Option<Group> {
        if !task.column.is(Kind::Todo) || to != Column::Backlog {
            return None;
        }
        let group = self.group(&task.epic_meta()?).ok()?;
        let fits = group.accepts_changes() && (!group.is_sprint() || task.points().is_some());
        fits.then_some(group)
    }
}

/// Whether `name` can name an epic or sprint: it becomes a directory and a
/// destination verb, so it must be neither empty, nor a path, nor a verb
/// the board already uses.
fn check_group_name(board: &Board, name: &str) -> Result<()> {
    if name.trim().is_empty() {
        bail!("a group needs a name");
    }
    if name.contains(['/', '\\']) || name.starts_with('.') {
        bail!("'{name}' can't be a group name: no slashes, and it can't start with a dot");
    }
    // The destination-verb dispatch (`amd <name> <ref>`) is checked
    // against columns, `abandon`, and epics in that order — a group
    // sharing a name with the first two would be silently unreachable.
    if board.workflow().find(name).is_some() || name.eq_ignore_ascii_case(ABANDON) {
        bail!("'{name}' collides with the destination verb of the same name — pick another name");
    }
    Ok(())
}

fn dir_name() -> String {
    env::var("AMD_DIR")
        .ok()
        .filter(|dir| !dir.is_empty())
        .unwrap_or_else(|| DEFAULT_DIR.to_string())
}

/// Every immediate child of `dir` that is both a git repository and has a
/// board of its own — the candidates `Board::discover()` chooses from.
/// Separated out so it can be tested against a real temporary directory
/// without touching the process's current directory, which `discover()`
/// itself reads and which isn't safe to mutate with tests running in
/// parallel (`registry.rs`'s `remember_in` uses the same split, for the
/// same reason). A directory that can't be read (removed mid-scan, no
/// permission) contributes no candidates rather than failing the whole
/// scan — the same "best effort" stance the registry takes elsewhere.
/// Delete a branch in `repo` if it's safe to: it exists locally, isn't
/// checked out right now, has never been pushed, and carries no commits
/// `HEAD` doesn't already have. Anything else — real work on it, shared with
/// a remote — is left alone. Best effort: a git failure here doesn't fail the
/// archive, which has already happened.
fn cleanup_branch(repo: &Path, name: &str) -> Option<String> {
    if name.is_empty()
        || !git::branch_exists(repo, name)
        || git::current_branch(repo).as_deref() == Some(name)
        || git::branch_pushed(repo, name)
        || git::commits_ahead(repo, name) != Some(0)
    {
        return None;
    }
    git::delete_branch(repo, name).ok()?;
    Some(name.to_string())
}

/// Whether `repo` holds a board that counts as a project (ticket 175): a
/// `backlog/`, which every board since v4 has, or a declared `project`.
/// An old v3 board left in a code repository (todo/doing/done, nothing
/// declared) doesn't, so it's never offered or picked as a project.
fn is_project_board(repo: &Path) -> bool {
    let tasks = repo.join(dir_name());
    tasks.join(Column::Backlog.to_string()).is_dir()
        || (tasks.is_dir()
            && crate::board_settings::load(&Board::in_repo(repo))
                .project()
                .is_some())
}

pub(crate) fn project_siblings(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.join(".git").is_dir() && is_project_board(path))
        .collect();
    found.sort();
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn board_at(dir: &std::path::Path) -> Board {
        let board = Board::at(dir.join("tasks"));
        board.create().expect("board");
        board
    }

    /// Moves ticket `id` column by column until it reaches `to`.
    fn walk(board: &Board, id: &str, to: Column) {
        loop {
            let task = board.find(id).unwrap();
            if task.column == to {
                return;
            }
            let workflow = board.workflow();
            let (here, there) = (
                workflow.position(task.column).unwrap(),
                workflow.position(to).unwrap(),
            );
            let next = workflow
                .columns()
                .nth(if there > here { here + 1 } else { here - 1 })
                .unwrap();
            board.move_task(&task, next, false).unwrap();
        }
    }

    fn touch(board: &Board, name: &str) {
        fs::write(board.dir(Column::Todo).join(name), "---\n---\n").unwrap();
    }

    fn backlog_ticket(board: &Board, name: &str) {
        let body = format!("---\nid: \"{}\"\ntitle: \"T\"\n---\n\nbody\n", &name[..3]);
        fs::write(board.dir(Column::Backlog).join(name), body).unwrap();
    }

    fn backlog_ticket_with_branch(board: &Board, name: &str, branch: &str) {
        let body = format!(
            "---\nid: \"{}\"\ntitle: \"T\"\nbranch-name: \"{branch}\"\n---\n\nbody\n",
            &name[..3]
        );
        fs::write(board.dir(Column::Backlog).join(name), body).unwrap();
    }

    /// A real git repository, seeded with an initial commit so `HEAD` exists
    /// — the branch-cleanup checks need real history to compare against, not
    /// just a board directory on disk.
    fn git_repo_board(dir: &std::path::Path) -> Board {
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(args)
                .status()
                .unwrap();
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "t@t.co"]);
        git(&["config", "user.name", "t"]);
        let board = board_at(dir);
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "seed"]);
        board
    }

    #[test]
    fn archiving_deletes_a_branch_with_no_commits() {
        let dir = tempfile::tempdir().unwrap();
        let board = git_repo_board(dir.path());
        backlog_ticket_with_branch(&board, "001-thing.md", "feature/thing");
        std::process::Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(["branch", "feature/thing"])
            .status()
            .unwrap();

        let task = board.find("001").unwrap();
        let (_, deleted) = board.archive(&task).unwrap();
        assert_eq!(deleted.as_deref(), Some("feature/thing"));
        assert!(!git::branch_exists(&board.root, "feature/thing"));
    }

    #[test]
    fn archiving_leaves_a_branch_with_real_commits_alone() {
        let dir = tempfile::tempdir().unwrap();
        let board = git_repo_board(dir.path());
        backlog_ticket_with_branch(&board, "001-thing.md", "feature/thing");
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(args)
                .status()
                .unwrap();
        };
        // Commit the ticket to the base branch first: an uncommitted file
        // would vanish from the working tree the moment `switch` back finds
        // it missing from that commit's own tree.
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "add ticket"]);
        git(&["switch", "-c", "feature/thing"]);
        fs::write(dir.path().join("work.txt"), "did something\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "real work"]);
        git(&["switch", "-"]); // back to whichever branch was checked out before

        let task = board.find("001").unwrap();
        let (_, deleted) = board.archive(&task).unwrap();
        assert_eq!(deleted, None, "a branch with real commits must survive");
        assert!(git::branch_exists(&board.root, "feature/thing"));
    }

    #[test]
    fn archiving_leaves_the_currently_checked_out_branch_alone() {
        let dir = tempfile::tempdir().unwrap();
        let board = git_repo_board(dir.path());
        backlog_ticket_with_branch(&board, "001-thing.md", "feature/thing");
        std::process::Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(["switch", "-c", "feature/thing"])
            .status()
            .unwrap();

        let task = board.find("001").unwrap();
        let (_, deleted) = board.archive(&task).unwrap();
        assert_eq!(deleted, None, "can't delete the branch you're standing on");
        assert!(git::branch_exists(&board.root, "feature/thing"));
    }

    #[test]
    fn archiving_leaves_a_pushed_branch_alone() {
        let dir = tempfile::tempdir().unwrap();
        let board = git_repo_board(dir.path());
        backlog_ticket_with_branch(&board, "001-thing.md", "feature/thing");
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(args)
                .status()
                .unwrap();
        };
        git(&["branch", "feature/thing"]);
        // No real remote needed: a matching remote-tracking ref is what
        // `branch_pushed` actually checks for.
        git(&["update-ref", "refs/remotes/origin/feature/thing", "HEAD"]);

        let task = board.find("001").unwrap();
        let (_, deleted) = board.archive(&task).unwrap();
        assert_eq!(deleted, None, "a pushed branch might be in use elsewhere");
        assert!(git::branch_exists(&board.root, "feature/thing"));
    }

    #[test]
    fn archiving_a_ticket_with_no_branch_does_nothing_extra() {
        let dir = tempfile::tempdir().unwrap();
        let board = git_repo_board(dir.path());
        backlog_ticket(&board, "001-loose.md");

        let task = board.find("001").unwrap();
        let (_, deleted) = board.archive(&task).unwrap();
        assert_eq!(deleted, None);
    }

    #[test]
    fn tickets_inside_an_epic_folder_are_still_on_the_board() {
        let dir = tempfile::tempdir().unwrap();
        let board = board_at(dir.path());
        backlog_ticket(&board, "001-loose.md");
        let epic = board.dir(Column::Backlog).join("checkout");
        fs::create_dir_all(&epic).unwrap();
        fs::write(epic.join("002-filed.md"), "---\nid: \"002\"\n---\n").unwrap();

        let tasks = board.tasks_in(Column::Backlog).unwrap();
        assert_eq!(tasks.len(), 2, "the filed ticket must be seen");
        let filed = tasks.iter().find(|t| t.stem == "002-filed").unwrap();
        assert_eq!(filed.epic.as_deref(), Some("checkout"));

        // The reason this matters: an unseen ticket is an id handed out twice.
        assert_eq!(board.next_id().unwrap(), 3);
        assert_eq!(board.epics_in(Column::Backlog).unwrap(), ["checkout"]);
        // Epics are a backlog idea only.
        assert!(board.epics_in(Column::Todo).unwrap().is_empty());
    }

    fn sprint(board: &Board, name: &str) -> crate::group::Group {
        let group = crate::group::Group {
            dir: board.dir(Column::Backlog).join(name),
            name: name.to_string(),
            kind: crate::group::Kind::Sprint,
            description: String::new(),
            days: crate::group::DEFAULT_DAYS,
            state: crate::group::State::Pending,
        };
        board.create_group(&group).unwrap();
        group
    }

    #[test]
    fn a_draft_stays_in_the_backlog_until_it_is_ready() {
        let dir = tempfile::tempdir().unwrap();
        let board = board_at(dir.path());
        backlog_ticket(&board, "001-a.md");
        let task = board.find("001").unwrap();
        assert!(task.set_draft(task.hash().unwrap(), true).unwrap());
        assert!(board.find("001").unwrap().is_draft());

        let err = board
            .move_task(&board.find("001").unwrap(), Column::Todo, false)
            .unwrap_err();
        assert!(err.to_string().contains("is a draft"), "{err}");
        assert_eq!(board.find("001").unwrap().column, Column::Backlog);

        // Still workable while it's a draft: sized, and filed into an epic.
        let task = board.find("001").unwrap();
        task.set_points(task.hash().unwrap(), "3").unwrap();
        board
            .create_group(&crate::group::Group {
                dir: board.dir(Column::Backlog).join("epic-1"),
                name: "epic-1".into(),
                kind: crate::group::Kind::Epic,
                description: String::new(),
                days: crate::group::DEFAULT_DAYS,
                state: crate::group::State::Pending,
            })
            .unwrap();
        board
            .set_epic(&board.find("001").unwrap(), Some("epic-1"))
            .unwrap();

        // Ready, it moves on; marking it again changes nothing.
        let task = board.find("001").unwrap();
        assert!(task.set_draft(task.hash().unwrap(), false).unwrap());
        let task = board.find("001").unwrap();
        assert!(!task.set_draft(task.hash().unwrap(), false).unwrap());
        board
            .move_task(&board.find("001").unwrap(), Column::Todo, false)
            .unwrap();
        assert_eq!(board.find("001").unwrap().column, Column::Todo);
    }

    #[test]
    fn a_sprint_refuses_a_draft() {
        let dir = tempfile::tempdir().unwrap();
        let board = board_at(dir.path());
        sprint(&board, "sprint-1");
        backlog_ticket(&board, "001-a.md");
        let task = board.find("001").unwrap();
        task.set_points(task.hash().unwrap(), "3").unwrap();
        task.set_draft(task.hash().unwrap(), true).unwrap();

        let err = board
            .set_epic(&board.find("001").unwrap(), Some("sprint-1"))
            .unwrap_err();
        assert!(err.to_string().contains("is a draft"), "{err}");

        let task = board.find("001").unwrap();
        task.set_draft(task.hash().unwrap(), false).unwrap();
        board
            .set_epic(&board.find("001").unwrap(), Some("sprint-1"))
            .unwrap();
    }

    #[test]
    fn a_sprint_refuses_an_unsized_ticket() {
        let dir = tempfile::tempdir().unwrap();
        let board = board_at(dir.path());
        sprint(&board, "sprint-1");
        backlog_ticket(&board, "001-unsized.md");

        let task = board.find("001").unwrap();
        let err = board.set_epic(&task, Some("sprint-1")).unwrap_err();
        assert!(
            err.to_string().contains("needs points"),
            "unexpected: {err}"
        );
        assert!(task.path.exists(), "the ticket must not have moved");

        // Sized, it goes in.
        task.set_points(task.hash().unwrap(), "3").unwrap();
        board.set_epic(&task, Some("sprint-1")).unwrap();
        assert_eq!(board.find("001").unwrap().epic.as_deref(), Some("sprint-1"));
    }

    #[test]
    fn an_epic_takes_unsized_tickets() {
        let dir = tempfile::tempdir().unwrap();
        let board = board_at(dir.path());
        fs::create_dir_all(board.dir(Column::Backlog).join("checkout")).unwrap();
        backlog_ticket(&board, "001-unsized.md");
        let task = board.find("001").unwrap();
        // Sizing is a sprint's requirement, not an epic's: an epic is where
        // work goes before anyone has estimated it.
        board.set_epic(&task, Some("checkout")).unwrap();
        assert_eq!(board.find("001").unwrap().epic.as_deref(), Some("checkout"));
    }

    #[test]
    fn a_started_sprint_takes_nothing_more_and_gives_nothing_back() {
        // Ticket 083, replacing ADR-0009's "in and out": once started, a
        // sprint's scope is fixed. Its tickets only move on, to todo.
        let dir = tempfile::tempdir().unwrap();
        let board = board_at(dir.path());
        let mut group = sprint(&board, "sprint-1");
        backlog_ticket(&board, "001-in.md");
        backlog_ticket(&board, "002-out.md");

        let inside = board.find("001").unwrap();
        inside.set_points(inside.hash().unwrap(), "5").unwrap();
        board.set_epic(&inside, Some("sprint-1")).unwrap();
        group.start().unwrap();

        // Nothing more goes in, sized or not...
        let outside = board.find("002").unwrap();
        outside.set_points(outside.hash().unwrap(), "2").unwrap();
        let err = board
            .set_epic(&board.find("002").unwrap(), Some("sprint-1"))
            .unwrap_err();
        assert!(err.to_string().contains("no more tickets"), "{err}");

        // ...nothing comes back out to the backlog, or into the archive...
        let inside = board.find("001").unwrap();
        let err = board.set_epic(&inside, None).unwrap_err();
        assert!(err.to_string().contains("only move on to todo"), "{err}");
        let err = board.archive(&inside).unwrap_err();
        assert!(err.to_string().contains("before archiving"), "{err}");

        // ...but its tickets move on to todo, still in the sprint, and sent
        // back they cannot rejoin it: they land in the loose backlog.
        board.move_task(&inside, Column::Todo, false).unwrap();
        let moved = board.find("001").unwrap();
        assert_eq!(moved.epic_meta().as_deref(), Some("sprint-1"));
        let err = board.set_epic(&moved, None).unwrap_err();
        assert!(err.to_string().contains("only move on to todo"), "{err}");
        board.move_task(&moved, Column::Backlog, false).unwrap();
        let back = board.find("001").unwrap();
        assert_eq!((back.column, back.epic.as_deref()), (Column::Backlog, None));
    }

    #[test]
    fn tickets_move_one_column_at_a_time() {
        let dir = tempfile::tempdir().unwrap();
        let board = board_at(dir.path());
        backlog_ticket(&board, "001-a.md");
        let err = board
            .move_task(&board.find("001").unwrap(), Column::Done, false)
            .unwrap_err();
        assert!(err.to_string().contains("one column at a time"), "{err}");
        assert_eq!(board.find("001").unwrap().column, Column::Backlog);
        walk(&board, "001", Column::Done);
        let err = board
            .move_task(&board.find("001").unwrap(), Column::Todo, false)
            .unwrap_err();
        assert!(err.to_string().contains("one column at a time"), "{err}");
        walk(&board, "001", Column::Backlog);
    }

    #[test]
    fn any_epic_feeds_todo_and_takes_its_ticket_back() {
        let dir = tempfile::tempdir().unwrap();
        let board = board_at(dir.path());
        fs::create_dir_all(board.dir(Column::Backlog).join("checkout")).unwrap();
        backlog_ticket(&board, "001-a.md");
        board
            .set_epic(&board.find("001").unwrap(), Some("checkout"))
            .unwrap();

        // Straight from the epic to todo, keeping its label...
        board
            .move_task(&board.find("001").unwrap(), Column::Todo, false)
            .unwrap();
        let task = board.find("001").unwrap();
        assert_eq!(task.column, Column::Todo);
        assert_eq!(task.epic_meta().as_deref(), Some("checkout"));

        // ...and back into the epic, not the loose backlog.
        board.move_task(&task, Column::Backlog, false).unwrap();
        let back = board.find("001").unwrap();
        assert_eq!(back.epic.as_deref(), Some("checkout"));
        assert!(
            back.path
                .starts_with(board.dir(Column::Backlog).join("checkout"))
        );

        // With the epic gone, the loose backlog is home.
        board.move_task(&back, Column::Todo, false).unwrap();
        fs::remove_dir_all(board.dir(Column::Backlog).join("checkout")).unwrap();
        board
            .move_task(&board.find("001").unwrap(), Column::Backlog, false)
            .unwrap();
        assert_eq!(board.find("001").unwrap().epic, None);
    }

    #[test]
    fn a_sprint_keeps_its_tickets_when_they_leave_the_backlog() {
        // The bug: scope was counted from backlog/ alone, so a sprint's points
        // fell as work started on them.
        let dir = tempfile::tempdir().unwrap();
        let board = board_at(dir.path());
        sprint(&board, "sprint-1");
        backlog_ticket(&board, "001-in.md");

        let task = board.find("001").unwrap();
        task.set_points(task.hash().unwrap(), "5").unwrap();
        board.set_epic(&task, Some("sprint-1")).unwrap();
        assert_eq!(board.tasks_in_group("sprint-1").unwrap().len(), 1);

        walk(&board, "001", Column::Doing);
        assert_eq!(
            board.tasks_in_group("sprint-1").unwrap().len(),
            1,
            "a ticket in doing/ still belongs to its sprint"
        );
    }

    #[test]
    fn set_epic_tags_a_non_backlog_ticket_in_place_without_moving_it() {
        // Before this, set_epic's directory-move logic ran unconditionally,
        // so filing a doing/ ticket into an epic would have silently
        // relocated it back into backlog/<epic>/ — nothing called it on a
        // non-backlog task before the GUI's membership checkboxes needed to.
        let dir = tempfile::tempdir().unwrap();
        let board = board_at(dir.path());
        fs::create_dir_all(board.dir(Column::Backlog).join("checkout")).unwrap();
        backlog_ticket(&board, "001-a.md");
        walk(&board, "001", Column::Doing);
        let path_before = board.find("001").unwrap().path;

        let task = board.find("001").unwrap();
        board.set_epic(&task, Some("checkout")).unwrap();

        let after = board.find("001").unwrap();
        assert_eq!(after.column, Column::Doing, "must not move column");
        assert_eq!(after.path, path_before, "must not move the file");
        assert_eq!(after.epic_meta().as_deref(), Some("checkout"));
        assert_eq!(
            board.tasks_in_group("checkout").unwrap().len(),
            1,
            "membership is visible via tasks_in_group despite no directory move"
        );

        // And clearing it is likewise metadata-only.
        let task = board.find("001").unwrap();
        board.set_epic(&task, None).unwrap();
        let after = board.find("001").unwrap();
        assert_eq!(after.column, Column::Doing);
        assert_eq!(after.path, path_before);
        assert_eq!(after.epic_meta(), None);
    }

    #[test]
    fn the_group_file_is_not_a_ticket() {
        let dir = tempfile::tempdir().unwrap();
        let board = board_at(dir.path());
        sprint(&board, "sprint-1");
        let tasks = board.tasks_in(Column::Backlog).unwrap();
        assert!(tasks.is_empty(), "_group.md must not count as a ticket");
        assert_eq!(board.next_id().unwrap(), 1, "nor consume an id");
    }

    #[test]
    fn a_group_cannot_be_named_after_a_column_or_abandon() {
        let dir = tempfile::tempdir().unwrap();
        let board = board_at(dir.path());
        for name in ["todo", "DONE", "Abandon"] {
            let group = crate::group::Group {
                dir: board.dir(Column::Backlog).join(name),
                name: name.to_string(),
                kind: crate::group::Kind::Epic,
                description: String::new(),
                days: crate::group::DEFAULT_DAYS,
                state: crate::group::State::Pending,
            };
            let err = board.create_group(&group).unwrap_err();
            assert!(err.to_string().contains("collides"), "{name}: {err}");
            assert!(!group.dir.exists(), "{name}: must not have been created");
        }
    }

    #[test]
    fn filing_under_an_epic_moves_the_file_and_updates_the_ticket() {
        let dir = tempfile::tempdir().unwrap();
        let board = board_at(dir.path());
        backlog_ticket(&board, "001-loose.md");
        // The group has to exist first: filing onto a name that isn't there
        // would turn a typo into a new epic.
        for name in ["checkout", "payments"] {
            fs::create_dir_all(board.dir(Column::Backlog).join(name)).unwrap();
        }
        let task = board.find("001").unwrap();

        let dest = board.set_epic(&task, Some("checkout")).unwrap();
        assert!(dest.ends_with("backlog/checkout/001-loose.md"), "{dest:?}");
        assert!(!task.path.exists(), "the original must have moved");
        let filed = board.find("001").unwrap();
        assert_eq!(filed.epic_meta().as_deref(), Some("checkout"));
        assert!(
            crate::eol::read(&filed.path).unwrap().ends_with("body\n"),
            "the body must survive the move"
        );

        // Moving to another epic rewrites the key rather than accumulating.
        board.set_epic(&filed, Some("payments")).unwrap();
        let moved = board.find("001").unwrap();
        assert_eq!(moved.epic_meta().as_deref(), Some("payments"));
        assert_eq!(moved.epic.as_deref(), Some("payments"));

        // And back out to the loose backlog.
        board.set_epic(&moved, None).unwrap();
        let loose = board.find("001").unwrap();
        assert_eq!(loose.epic, None);
        assert_eq!(loose.epic_meta(), None);
    }

    // -- Leases block moves too (docs/adr/0007-concurrency-and-locking.md) -

    fn leased_backlog_ticket(board: &Board, name: &str, holder: &str, until: &str) {
        let body = format!(
            "---\nid: \"{}\"\ntitle: \"T\"\nlocked-by: \"{holder}\"\nlocked-until: \"{until}\"\n---\n\nbody\n",
            &name[..3]
        );
        fs::write(board.dir(Column::Backlog).join(name), body).unwrap();
    }

    fn future() -> String {
        (time::OffsetDateTime::now_utc() + time::Duration::hours(1))
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap()
    }

    #[test]
    fn move_task_refuses_a_ticket_leased_by_someone_else() {
        let dir = tempfile::tempdir().unwrap();
        let board = git_repo_board(dir.path());
        leased_backlog_ticket(&board, "001-a.md", "other@example.com", &future());
        let task = board.find("001").unwrap();
        assert!(board.move_task(&task, Column::Todo, false).is_err());
        assert_eq!(board.find("001").unwrap().column, Column::Backlog);
    }

    #[test]
    fn move_task_succeeds_when_the_lease_is_your_own() {
        let dir = tempfile::tempdir().unwrap();
        let board = git_repo_board(dir.path());
        leased_backlog_ticket(&board, "001-a.md", "t@t.co", &future());
        let task = board.find("001").unwrap();
        board.move_task(&task, Column::Todo, false).unwrap();
        assert_eq!(board.find("001").unwrap().column, Column::Todo);
    }

    #[test]
    fn archive_refuses_a_ticket_leased_by_someone_else() {
        let dir = tempfile::tempdir().unwrap();
        let board = git_repo_board(dir.path());
        leased_backlog_ticket(&board, "001-a.md", "other@example.com", &future());
        let task = board.find("001").unwrap();
        assert!(board.archive(&task).is_err());
    }

    #[test]
    fn set_epic_refuses_a_ticket_leased_by_someone_else() {
        let dir = tempfile::tempdir().unwrap();
        let board = git_repo_board(dir.path());
        fs::create_dir_all(board.dir(Column::Backlog).join("checkout")).unwrap();
        leased_backlog_ticket(&board, "001-a.md", "other@example.com", &future());
        let task = board.find("001").unwrap();
        assert!(board.set_epic(&task, Some("checkout")).is_err());
    }

    // -- WIP limits ----------------------------------------------------------

    fn set_wip_limit(board: &Board, key: &str, limit: u32) {
        fs::write(
            crate::board_settings::path(board),
            format!("{key} = {limit}\n"),
        )
        .unwrap();
    }

    #[test]
    fn move_task_refuses_when_doing_is_already_at_its_limit() {
        let dir = tempfile::tempdir().unwrap();
        let board = board_at(dir.path());
        for name in ["001-a.md", "002-b.md", "003-c.md"] {
            touch(&board, name); // all in todo/
        }
        // Two fit in doing by default; the third is refused.
        for id in ["001", "002"] {
            board
                .move_task(&board.find(id).unwrap(), Column::Doing, false)
                .unwrap();
        }
        let err = board
            .move_task(&board.find("003").unwrap(), Column::Doing, false)
            .unwrap_err()
            .to_string();
        assert!(err.contains("doing") && err.contains('2'), "{err}");
        assert_eq!(board.find("003").unwrap().column, Column::Todo);
    }

    #[test]
    fn move_task_force_overrides_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        let board = board_at(dir.path());
        touch(&board, "001-a.md");
        set_wip_limit(&board, "wip-limit", 0);
        board
            .move_task(&board.find("001").unwrap(), Column::Doing, false)
            .unwrap();
        set_wip_limit(&board, "wip-limit", 1);
        touch(&board, "002-b.md");
        let task = board.find("002").unwrap();
        assert!(board.move_task(&task, Column::Doing, false).is_err());
        board.move_task(&task, Column::Doing, true).unwrap();
        assert_eq!(board.find("002").unwrap().column, Column::Doing);
    }

    #[test]
    fn moving_out_of_an_over_limit_column_is_never_blocked() {
        let dir = tempfile::tempdir().unwrap();
        let board = board_at(dir.path());
        // Three tickets in doing/, then the limit is lowered to one — the
        // column is over its limit by hand. Moving one back OUT must work.
        for name in ["001-a.md", "002-b.md", "003-c.md"] {
            fs::write(board.dir(Column::Doing).join(name), "").unwrap();
        }
        set_wip_limit(&board, "wip-limit", 1);
        let task = board.find("001").unwrap();
        board.move_task(&task, Column::Done, false).unwrap();
        assert_eq!(board.find("001").unwrap().column, Column::Done);
    }

    #[test]
    fn todo_is_never_limited() {
        let dir = tempfile::tempdir().unwrap();
        let board = board_at(dir.path());
        set_wip_limit(&board, "wip-todo", 1);
        for name in ["001-a.md", "002-b.md", "003-c.md"] {
            backlog_ticket(&board, name);
        }
        for id in ["001", "002", "003"] {
            board
                .move_task(&board.find(id).unwrap(), Column::Todo, false)
                .unwrap();
        }
        assert_eq!(board.tasks_in(Column::Todo).unwrap().len(), 3);
    }

    // -- Renaming a group ----------------------------------------------

    #[test]
    fn renaming_a_group_relabels_its_tickets_everywhere() {
        let dir = tempfile::tempdir().unwrap();
        let board = board_at(dir.path());
        board
            .create_group(&Group {
                dir: board.dir(Column::Backlog).join("ui"),
                name: "ui".into(),
                kind: crate::group::Kind::Epic,
                description: "Screens".into(),
                days: crate::group::DEFAULT_DAYS,
                state: crate::group::State::Pending,
            })
            .unwrap();
        backlog_ticket(&board, "001-a.md");
        backlog_ticket(&board, "002-b.md");
        for id in ["001", "002"] {
            board
                .set_epic(&board.find(id).unwrap(), Some("ui"))
                .unwrap();
        }
        walk(&board, "002", Column::Doing);

        board.rename_group("ui", "Interface").unwrap();
        let group = board.group("Interface").unwrap();
        assert_eq!(
            (group.name.as_str(), group.description.as_str()),
            ("Interface", "Screens")
        );
        assert!(board.group("ui").is_err());
        let inside = board.find("001").unwrap();
        assert_eq!(inside.epic.as_deref(), Some("Interface"));
        assert_eq!(inside.epic_meta().as_deref(), Some("Interface"));
        let moved_on = board.find("002").unwrap();
        assert_eq!(moved_on.epic_meta().as_deref(), Some("Interface"));
        assert_eq!(board.tasks_in_group("Interface").unwrap().len(), 2);

        // Refused: a column's name, another group's, or nothing.
        board
            .create_group(&Group {
                dir: board.dir(Column::Backlog).join("api"),
                name: "api".into(),
                kind: crate::group::Kind::Epic,
                description: String::new(),
                days: crate::group::DEFAULT_DAYS,
                state: crate::group::State::Pending,
            })
            .unwrap();
        for bad in ["todo", "api", " ", "a/b"] {
            assert!(board.rename_group("Interface", bad).is_err(), "{bad}");
        }
        assert!(board.rename_group("nope", "x").is_err());
    }

    #[test]
    fn renaming_a_group_with_someone_elses_ticket_leased_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let board = board_at(dir.path());
        fs::create_dir_all(board.dir(Column::Backlog).join("ui")).unwrap();
        leased_backlog_ticket(&board, "001-a.md", "other@example.com", &future());
        fs::rename(
            board.dir(Column::Backlog).join("001-a.md"),
            board.dir(Column::Backlog).join("ui").join("001-a.md"),
        )
        .unwrap();
        let err = board.rename_group("ui", "Interface").unwrap_err();
        assert!(err.to_string().contains("other@example.com"), "{err}");
        assert!(board.group("ui").is_ok(), "nothing changed");
    }

    // -- Emptying a group ------------------------------------------------

    #[test]
    fn empty_group_moves_every_ticket_it_holds() {
        let dir = tempfile::tempdir().unwrap();
        let board = board_at(dir.path());
        fs::create_dir_all(board.dir(Column::Backlog).join("checkout")).unwrap();
        backlog_ticket(&board, "001-a.md");
        backlog_ticket(&board, "002-b.md");
        board
            .set_epic(&board.find("001").unwrap(), Some("checkout"))
            .unwrap();
        board
            .set_epic(&board.find("002").unwrap(), Some("checkout"))
            .unwrap();
        assert_eq!(board.tasks_in_group("checkout").unwrap().len(), 2);

        let result = board.empty_group("checkout").unwrap();
        assert_eq!(result.moved.len(), 2);
        assert!(result.skipped.is_empty());
        assert_eq!(board.tasks_in_group("checkout").unwrap().len(), 0);
        assert_eq!(board.find("001").unwrap().epic, None);
        assert_eq!(board.find("002").unwrap().epic, None);
    }

    #[test]
    fn empty_group_skips_a_ticket_it_cannot_move_rather_than_aborting() {
        let dir = tempfile::tempdir().unwrap();
        let board = git_repo_board(dir.path());
        let epic_dir = board.dir(Column::Backlog).join("checkout");
        fs::create_dir_all(&epic_dir).unwrap();
        backlog_ticket(&board, "001-a.md");
        board
            .set_epic(&board.find("001").unwrap(), Some("checkout"))
            .unwrap();
        let body = format!(
            "---\nid: \"002\"\ntitle: \"T\"\nlocked-by: \"other@example.com\"\nlocked-until: \"{}\"\n---\n\nbody\n",
            future()
        );
        fs::write(epic_dir.join("002-b.md"), body).unwrap();

        let result = board.empty_group("checkout").unwrap();
        assert_eq!(result.moved.len(), 1, "{:?}", result.moved);
        assert_eq!(result.skipped.len(), 1);
        assert_eq!(result.skipped[0].0, "002-b");
    }

    #[test]
    fn deleting_a_nonempty_group_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let board = board_at(dir.path());
        fs::create_dir_all(board.dir(Column::Backlog).join("checkout")).unwrap();
        backlog_ticket(&board, "001-a.md");
        board
            .set_epic(&board.find("001").unwrap(), Some("checkout"))
            .unwrap();

        let err = board.delete_group("checkout").unwrap_err();
        assert!(err.to_string().contains("1 ticket"), "{err}");
        assert!(
            board.group("checkout").is_ok(),
            "must not have been deleted"
        );
    }

    #[test]
    fn deleting_a_nonempty_group_counts_tickets_outside_the_backlog_too() {
        // A committed sprint's tickets can sit in doing/done while the
        // folder itself is physically empty — delete has to see them the
        // same way empty_group already does, or it would silently orphan
        // their epic reference.
        let dir = tempfile::tempdir().unwrap();
        let board = board_at(dir.path());
        sprint(&board, "sprint-1");
        backlog_ticket(&board, "001-a.md");
        let task = board.find("001").unwrap();
        task.set_points(task.hash().unwrap(), "3").unwrap();
        board.set_epic(&task, Some("sprint-1")).unwrap();
        walk(&board, "001", Column::Doing);

        let err = board.delete_group("sprint-1").unwrap_err();
        assert!(err.to_string().contains("1 ticket"), "{err}");
    }

    #[test]
    fn a_group_whose_tickets_are_all_done_can_be_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let board = board_at(dir.path());
        sprint(&board, "sprint-1");
        backlog_ticket(&board, "001-a.md");
        let task = board.find("001").unwrap();
        task.set_points(task.hash().unwrap(), "3").unwrap();
        board.set_epic(&task, Some("sprint-1")).unwrap();
        walk(&board, "001", Column::Done);

        board.delete_group("sprint-1").unwrap();
        assert!(board.group("sprint-1").is_err(), "the directory is gone");
        let done = board.find("001").unwrap();
        assert_eq!(
            done.epic_meta().as_deref(),
            Some("sprint-1"),
            "a done ticket keeps the record of its sprint"
        );
    }

    #[test]
    fn an_emptied_group_can_be_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let board = board_at(dir.path());
        fs::create_dir_all(board.dir(Column::Backlog).join("checkout")).unwrap();
        backlog_ticket(&board, "001-a.md");
        board
            .set_epic(&board.find("001").unwrap(), Some("checkout"))
            .unwrap();

        board.empty_group("checkout").unwrap();
        board.delete_group("checkout").unwrap();
        assert!(board.group("checkout").is_err(), "the directory is gone");
    }

    #[test]
    fn deleting_an_unknown_group_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let board = board_at(dir.path());
        assert!(board.delete_group("nope").is_err());
    }

    #[test]
    fn tasks_tagged_filters_across_every_column() {
        let dir = tempfile::tempdir().unwrap();
        let board = board_at(dir.path());
        fs::write(
            board.dir(Column::Backlog).join("001-a.md"),
            "---\nid: \"001\"\ntitle: \"A\"\ntags: [DUPLICATE]\n---\n\nbody\n",
        )
        .unwrap();
        fs::write(
            board.dir(Column::Done).join("002-b.md"),
            "---\nid: \"002\"\ntitle: \"B\"\ntags: [DUPLICATE,urgent]\n---\n\nbody\n",
        )
        .unwrap();
        fs::write(
            board.dir(Column::Todo).join("003-c.md"),
            "---\nid: \"003\"\ntitle: \"C\"\ntags: []\n---\n\nbody\n",
        )
        .unwrap();

        let tagged = board.tasks_tagged("DUPLICATE").unwrap();
        assert_eq!(
            tagged.iter().map(|t| t.id).collect::<Vec<_>>(),
            [Some(1), Some(2)]
        );
        assert_eq!(board.tasks_tagged("urgent").unwrap().len(), 1);
        assert!(board.tasks_tagged("missing").unwrap().is_empty());
    }

    #[test]
    fn a_new_board_starts_at_one() {
        let dir = tempfile::tempdir().unwrap();
        let board = board_at(dir.path());
        assert_eq!(board.next_id().unwrap(), 1);
        assert_eq!(
            crate::board_settings::load(&board).next_id(),
            None,
            "the counter lives in its own file (ticket 150), not .settings"
        );
        assert!(
            !board.legacy_counter_path().exists(),
            "a brand new board never writes the legacy .next-id file"
        );
    }

    #[test]
    fn a_new_board_keeps_its_counter_in_its_own_file() {
        let dir = tempfile::tempdir().unwrap();
        let board = board_at(dir.path());
        assert_eq!(crate::eol::read(board.counter_path()).unwrap().trim(), "1");
        assert_eq!(crate::board_settings::load(&board).next_id(), None);
        assert!(!board.legacy_counter_path().exists());
    }

    #[test]
    fn a_counter_in_settings_moves_to_its_own_file_once() {
        let dir = tempfile::tempdir().unwrap();
        let board = board_at(dir.path());
        touch(&board, "007-seven.md");
        // A board from before ticket 150: the counter in `.settings`.
        fs::remove_file(board.counter_path()).unwrap();
        let mut settings = crate::board_settings::load(&board);
        settings.set_next_id(12);
        settings.save(&board).unwrap();

        assert_eq!(board.next_id().unwrap(), 12);
        // Working out a number changes nothing (ticket 164): it happens
        // before the command's auto-commit snapshot.
        assert!(!board.counter_path().exists());
        assert_eq!(crate::board_settings::load(&board).next_id(), Some(12));

        touch(&board, "012-twelve.md");
        board.record_id(12).unwrap();
        assert_eq!(crate::eol::read(board.counter_path()).unwrap().trim(), "13");
        assert_eq!(
            crate::board_settings::load(&board).next_id(),
            None,
            "the key leaves .settings once a number is recorded"
        );
        assert_eq!(board.next_id().unwrap(), 13);
    }

    #[test]
    fn migration_keeps_the_highest_counter_it_finds() {
        let dir = tempfile::tempdir().unwrap();
        let board = board_at(dir.path());
        // All three places say something (a half-migrated board): the
        // highest wins, so no number already handed out comes round again.
        fs::write(board.counter_path(), "4\n").unwrap();
        let mut settings = crate::board_settings::load(&board);
        settings.set_next_id(9);
        settings.save(&board).unwrap();
        fs::write(board.legacy_counter_path(), "6\n").unwrap();
        assert_eq!(board.next_id().unwrap(), 9);
        touch(&board, "009-nine.md");
        board.record_id(9).unwrap();
        assert!(!board.legacy_counter_path().exists());
        assert_eq!(crate::board_settings::load(&board).next_id(), None);
        assert_eq!(board.next_id().unwrap(), 10);
    }

    #[test]
    fn a_legacy_next_id_file_is_migrated_and_removed() {
        let dir = tempfile::tempdir().unwrap();
        let board = board_at(dir.path());
        touch(&board, "007-seven.md");
        // The oldest boards: only `<board>/.next-id`.
        fs::remove_file(board.counter_path()).unwrap();
        fs::write(board.legacy_counter_path(), "9\n").unwrap();

        assert_eq!(board.next_id().unwrap(), 9);
        assert!(
            board.legacy_counter_path().exists(),
            "reading moves nothing"
        );
        touch(&board, "009-nine.md");
        board.record_id(9).unwrap();
        assert_eq!(crate::eol::read(board.counter_path()).unwrap().trim(), "10");
        assert!(
            !board.legacy_counter_path().exists(),
            "the legacy file should be gone once migrated, not kept around forever"
        );
        assert_eq!(board.next_id().unwrap(), 10);
    }

    #[test]
    fn the_counter_moves_on_as_tickets_are_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let board = board_at(dir.path());
        touch(&board, "001-one.md");
        board.record_id(1).unwrap();
        assert_eq!(board.next_id().unwrap(), 2);
        touch(&board, "002-two.md");
        board.record_id(2).unwrap();
        assert_eq!(board.next_id().unwrap(), 3);
    }

    #[test]
    fn a_deleted_ticket_does_not_hand_its_id_back() {
        let dir = tempfile::tempdir().unwrap();
        let board = board_at(dir.path());
        touch(&board, "001-one.md");
        board.record_id(1).unwrap();
        // Deleted outright, not archived: nothing on disk remembers it.
        fs::remove_file(board.dir(Column::Todo).join("001-one.md")).unwrap();
        assert_eq!(board.next_id().unwrap(), 2, "the counter still knows");
    }

    #[test]
    fn a_lost_or_stale_counter_cannot_collide() {
        let dir = tempfile::tempdir().unwrap();
        let board = board_at(dir.path());
        touch(&board, "007-seven.md");
        // Counter behind what's on disk — a bad merge, or a hand-added ticket.
        fs::write(board.counter_path(), "3\n").unwrap();
        assert_eq!(board.next_id().unwrap(), 8);
        // Counter missing entirely — its file is gone.
        fs::remove_file(board.counter_path()).unwrap();
        assert_eq!(board.next_id().unwrap(), 8);
    }

    #[test]
    fn recording_an_older_id_does_not_wind_the_counter_back() {
        let dir = tempfile::tempdir().unwrap();
        let board = board_at(dir.path());
        board.record_id(9).unwrap();
        board.record_id(2).unwrap();
        assert_eq!(board.next_id().unwrap(), 10);
    }

    #[test]
    fn columns_parse_case_insensitively() {
        let workflow = Board::at(PathBuf::from("/b")).workflow();
        assert_eq!(workflow.find("DONE"), Some(Column::Done));
        assert_eq!(workflow.find("todo"), Some(Column::Todo));
        assert_eq!(workflow.find("Backlog"), Some(Column::Backlog));
        assert_eq!(workflow.find("archive"), None);
    }

    /// A child that qualifies as a project sibling: a git repo with a board
    /// of its own. `project_siblings` only checks for directories, not a
    /// real git repository, so a bare `.git` dir is enough here.
    fn project_child(parent: &Path, name: &str) {
        let child = parent.join(name);
        fs::create_dir_all(child.join(".git")).unwrap();
        board_at(&child);
    }

    #[test]
    fn project_siblings_finds_nothing_in_an_empty_directory() {
        let dir = tempfile::tempdir().unwrap();
        assert!(project_siblings(dir.path()).is_empty());
    }

    #[test]
    fn project_siblings_ignores_plain_directories_and_half_qualified_ones() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("not-a-repo")).unwrap();
        // A git repo with no board of its own.
        fs::create_dir_all(dir.path().join("code-only/.git")).unwrap();
        // A board-shaped directory that isn't a git repository.
        board_at(&dir.path().join("board-but-no-git"));
        assert!(project_siblings(dir.path()).is_empty());
    }

    #[test]
    fn an_old_board_in_a_code_repo_is_not_a_project() {
        let dir = tempfile::tempdir().unwrap();
        project_child(dir.path(), "acme-tickets");
        // A v3 board: todo/doing/done only, nothing declared.
        let old = dir.path().join("versioning-tests");
        fs::create_dir_all(old.join(".git")).unwrap();
        for column in ["todo", "doing", "done"] {
            fs::create_dir_all(old.join("tasks").join(column)).unwrap();
        }
        assert_eq!(
            project_siblings(dir.path()),
            vec![dir.path().join("acme-tickets")]
        );
        // Declaring a project makes it one, backlog or not.
        fs::write(old.join(".settings"), "project = versioning\n").unwrap();
        assert_eq!(project_siblings(dir.path()).len(), 2);
    }

    #[test]
    fn project_siblings_finds_the_one_qualifying_child() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("not-a-repo")).unwrap();
        project_child(dir.path(), "acme-tickets");
        let found = project_siblings(dir.path());
        assert_eq!(found, vec![dir.path().join("acme-tickets")]);
    }

    #[test]
    fn project_siblings_finds_every_qualifying_child_sorted() {
        let dir = tempfile::tempdir().unwrap();
        project_child(dir.path(), "widgets-tickets");
        project_child(dir.path(), "acme-tickets");
        let found = project_siblings(dir.path());
        assert_eq!(
            found,
            vec![
                dir.path().join("acme-tickets"),
                dir.path().join("widgets-tickets"),
            ]
        );
    }
}
