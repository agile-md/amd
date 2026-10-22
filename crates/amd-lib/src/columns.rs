//! Changing a board's columns (ticket 224): add, rename, remove, reorder,
//! and a doing-kind column's WIP limit. Each change edits the workflow,
//! checks the result the way [`Workflow::parse`] checks anything written by
//! hand, and saves it in the board's `.settings` (the tickets repository's
//! for a project), so the team gets it with the next pull. A front end
//! wraps a change in `transition::with_auto_commit`.
//!
//! A rename changes the label only: the folder, and so every ticket's
//! path and its history, stays put. The four standard folders can be
//! renamed and moved but never removed, and every kind keeps at least one
//! column.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};

use crate::board::{ABANDON, Board, Column, Kind};
use crate::board_settings;
use crate::git;
use crate::workflow::{ColumnDef, Workflow};

/// Where a column goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Position {
    Before(Column),
    After(Column),
    End,
}

/// Adds a column labelled `label` of `kind` at `at`, in `folder` (the
/// label made into a folder name when not given), and makes its folder.
/// Returns the new column.
pub fn add(
    board: &Board,
    label: &str,
    kind: Kind,
    at: Position,
    folder: Option<&str>,
) -> Result<Column> {
    let label = check_label(label)?;
    let folder = match folder {
        Some(f) => f.trim().to_string(),
        None => crate::task::slugify(&label),
    };
    if folder.is_empty() {
        bail!("{label:?} doesn't make a folder name: give one with --folder");
    }
    let workflow = board.workflow();
    clashes(board, &workflow, &label, None)?;
    if workflow.by_folder(&folder).is_some() {
        bail!("there's already a column in {folder}/");
    }
    clashes(board, &workflow, &folder, None)?;
    let column = Column::new(&folder, kind);
    let mut defs = workflow.defs().to_vec();
    let at = index_for(&workflow, at)?;
    defs.insert(at, ColumnDef { column, label });
    save(board, defs)?;
    let dir = board.dir(column);
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let keep = dir.join(".gitkeep");
    if !keep.exists() {
        fs::write(&keep, "").with_context(|| format!("creating {}", keep.display()))?;
    }
    Ok(column)
}

/// Gives `column` a new label. Its folder doesn't change.
pub fn rename(board: &Board, column: Column, label: &str) -> Result<()> {
    let label = check_label(label)?;
    let workflow = board.workflow();
    clashes(board, &workflow, &label, Some(column))?;
    let mut defs = workflow.defs().to_vec();
    let def = defs
        .iter_mut()
        .find(|d| d.column == column)
        .with_context(|| format!("{column} isn't one of this board's columns"))?;
    def.label = label;
    save(board, defs)
}

/// Moves `column` to `at`.
pub fn reorder(board: &Board, column: Column, at: Position) -> Result<()> {
    let workflow = board.workflow();
    if matches!(at, Position::Before(c) | Position::After(c) if c == column) {
        bail!("a column can't move next to itself");
    }
    let mut defs = workflow.defs().to_vec();
    let from = defs
        .iter()
        .position(|d| d.column == column)
        .with_context(|| format!("{column} isn't one of this board's columns"))?;
    let def = defs.remove(from);
    let rest = Workflow::from_defs(defs.clone());
    let at = index_for(&rest, at)?;
    defs.insert(at, def);
    save(board, defs)
}

/// Removes an added column. One that still holds tickets needs `move_to`,
/// where they all go first (a plain move, not a step through the
/// workflow); a ticket someone else holds stops it before anything moves.
/// Returns where each ticket went.
pub fn remove(board: &Board, column: Column, move_to: Option<Column>) -> Result<Vec<PathBuf>> {
    let workflow = board.workflow();
    if workflow.position(column).is_none() {
        bail!("{column} isn't one of this board's columns");
    }
    if Column::STANDARD.contains(&column) {
        bail!(
            "{} is one of the four standard columns: it can be renamed or moved, not removed",
            workflow.label(column)
        );
    }
    let tickets = board.tasks_in(column)?;
    let mut moved = Vec::new();
    if !tickets.is_empty() {
        let Some(to) = move_to else {
            bail!(
                "{} holds {} ticket(s): say where they go (--move-to)",
                workflow.label(column),
                tickets.len()
            );
        };
        if to == column || workflow.position(to).is_none() {
            bail!("{to} isn't another of this board's columns");
        }
        for task in &tickets {
            task.ensure_not_leased()?;
        }
        let dir = board.dir(to);
        fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        for task in &tickets {
            let dest = dir.join(task.file_name());
            if dest.exists() {
                bail!("{} already exists", dest.display());
            }
            if git::is_tracked(&board.root, &task.path) {
                git::mv(&board.root, &task.path, &dest)?;
            } else {
                fs::rename(&task.path, &dest)
                    .with_context(|| format!("moving {}", task.path.display()))?;
            }
            moved.push(dest);
        }
    }
    let defs: Vec<ColumnDef> = workflow
        .defs()
        .iter()
        .filter(|d| d.column != column)
        .cloned()
        .collect();
    save(board, defs)?;
    let mut settings = board_settings::load(board);
    settings.clear_column_wip(column);
    // Moves that name it, and landing there, go with it (ticket 225).
    let moves: Vec<(Column, Column)> = workflow
        .moves()
        .iter()
        .copied()
        .filter(|(from, to)| *from != column && *to != column)
        .collect();
    settings.set_moves(&moves);
    if workflow.landing() == column {
        settings.set_new_tickets(Column::Backlog);
    }
    settings.save(board)?;
    // Its folder, now empty but for the placeholder.
    let dir = board.dir(column);
    let keep = dir.join(".gitkeep");
    if keep.exists() {
        if git::is_tracked(&board.root, &keep) {
            let _ = std::process::Command::new("git")
                .arg("-C")
                .arg(&board.root)
                .args(["rm", "-q", "--cached", "--"])
                .arg(&keep)
                .status();
        }
        let _ = fs::remove_file(&keep);
    }
    let _ = fs::remove_dir(&dir);
    Ok(moved)
}

/// Sets or clears a doing-kind column's WIP limit (`None` is no limit).
pub fn set_wip(board: &Board, column: Column, limit: Option<u32>) -> Result<()> {
    if board.workflow().position(column).is_none() {
        bail!("{column} isn't one of this board's columns");
    }
    let mut settings = board_settings::load(board);
    // The standard doing column keeps `wip-limit`'s meanings: unset is the
    // default of 2, and 0 is off.
    let stored = match (column == Column::Doing, limit) {
        (true, None) => Some(0),
        (_, limit) => limit,
    };
    settings.set_wip_limit(column, stored)?;
    settings.save(board)
}

/// Sets which moves the board allows (ticket 225).
pub fn set_flow(board: &Board, flow: crate::workflow::Flow) -> Result<()> {
    let mut settings = board_settings::load(board);
    settings.set_flow(flow);
    settings.save(board)
}

/// Allows (or, with `on` false, stops allowing) the extra one-way move
/// `from` → `to` under the `next` flow.
pub fn allow_move(board: &Board, from: Column, to: Column, on: bool) -> Result<()> {
    let workflow = board.workflow();
    for c in [from, to] {
        if workflow.position(c).is_none() {
            bail!("{c} isn't one of this board's columns");
        }
    }
    if from == to {
        bail!("a move needs two different columns");
    }
    let mut moves = workflow.moves().to_vec();
    moves.retain(|m| *m != (from, to));
    if on {
        moves.push((from, to));
    }
    let mut settings = board_settings::load(board);
    settings.set_moves(&moves);
    settings.save(board)
}

/// Where new tickets land: a backlog-kind column.
pub fn set_landing(board: &Board, column: Column) -> Result<()> {
    let workflow = board.workflow();
    if workflow.position(column).is_none() {
        bail!("{column} isn't one of this board's columns");
    }
    if !column.is(Kind::Backlog) {
        bail!(
            "new tickets can only land in a backlog column, not {}",
            workflow.label(column)
        );
    }
    let mut settings = board_settings::load(board);
    settings.set_new_tickets(column);
    settings.save(board)
}

/// Puts the board back to the standard four (ticket 225): each ticket in
/// an added column goes to the standard column of its kind, then the
/// columns, names, flow, extra moves, landing column and every column's
/// own WIP limit go back to standard. A ticket someone else holds stops it
/// before anything moves. Returns each ticket moved and where to.
pub fn reset(board: &Board) -> Result<Vec<(String, Column)>> {
    let workflow = board.workflow();
    let added: Vec<Column> = workflow
        .columns()
        .filter(|c| !Column::STANDARD.contains(c))
        .collect();
    let mut tickets = Vec::new();
    for column in &added {
        for task in board.tasks_in(*column)? {
            task.ensure_not_leased()?;
            tickets.push((task, Column::STANDARD[column.kind() as usize]));
        }
    }
    let mut moved = Vec::new();
    for (task, to) in &tickets {
        let dest = board.dir(*to).join(task.file_name());
        if dest.exists() {
            bail!("{} already exists", dest.display());
        }
        if git::is_tracked(&board.root, &task.path) {
            git::mv(&board.root, &task.path, &dest)?;
        } else {
            fs::rename(&task.path, &dest)
                .with_context(|| format!("moving {}", task.path.display()))?;
        }
        moved.push((task.file_name(), *to));
    }
    let mut settings = board_settings::load(board);
    settings.set_columns(&Workflow::standard());
    settings.set_flow(crate::workflow::Flow::Next);
    settings.set_moves(&[]);
    settings.set_new_tickets(Column::Backlog);
    settings.clear_column_wips();
    settings.save(board)?;
    for column in added {
        let dir = board.dir(column);
        let keep = dir.join(".gitkeep");
        if keep.exists() {
            if git::is_tracked(&board.root, &keep) {
                let _ = std::process::Command::new("git")
                    .arg("-C")
                    .arg(&board.root)
                    .args(["rm", "-q", "--cached", "--"])
                    .arg(&keep)
                    .status();
            }
            let _ = fs::remove_file(&keep);
        }
        let _ = fs::remove_dir(&dir);
    }
    Ok(moved)
}

/// A label without the characters the written form uses.
fn check_label(label: &str) -> Result<String> {
    let label = label.trim();
    if label.is_empty() {
        bail!("a column needs a name");
    }
    if label.contains([',', '=', '\n']) {
        bail!("a column's name can't contain a comma or '='");
    }
    Ok(label.to_string())
}

/// Refuses a name another column, an epic or sprint, or `abandon` already
/// answers to (`skip` is the column being renamed): the CLI's
/// destination verbs try columns, then `abandon`, then epics.
fn clashes(board: &Board, workflow: &Workflow, name: &str, skip: Option<Column>) -> Result<()> {
    if let Some(other) = workflow.find(name)
        && Some(other) != skip
    {
        bail!("there's already a column called {name:?}");
    }
    if name.eq_ignore_ascii_case(ABANDON) || ["archive", "templates"].contains(&name) {
        bail!("{name:?} is taken by the board itself");
    }
    if board
        .epics_in(Column::Backlog)?
        .iter()
        .any(|e| e.eq_ignore_ascii_case(name))
    {
        bail!("there's an epic or sprint called {name:?}: pick another name");
    }
    Ok(())
}

/// Where `at` falls in `workflow`'s columns.
fn index_for(workflow: &Workflow, at: Position) -> Result<usize> {
    let found = |c: Column| {
        workflow
            .position(c)
            .with_context(|| format!("{c} isn't one of this board's columns"))
    };
    Ok(match at {
        Position::Before(c) => found(c)?,
        Position::After(c) => found(c)? + 1,
        Position::End => workflow.defs().len(),
    })
}

/// Checks `defs` as a workflow and saves it.
fn save(board: &Board, defs: Vec<ColumnDef>) -> Result<()> {
    let workflow = Workflow::parse(&Workflow::from_defs(defs).to_text())?;
    let mut settings = board_settings::load(board);
    settings.set_columns(&workflow);
    settings.save(board)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::Board;

    fn board() -> (tempfile::TempDir, Board) {
        let dir = tempfile::tempdir().unwrap();
        let board = Board::at(dir.path().join("tasks"));
        board.create().unwrap();
        (dir, board)
    }

    fn ticket(board: &Board, column: Column, id: u32) {
        let body = format!("---\nid: \"{id:03}\"\ntitle: \"T{id}\"\n---\n\nbody\n");
        fs::write(board.dir(column).join(format!("{id:03}-t.md")), body).unwrap();
    }

    fn folders(board: &Board) -> Vec<&'static str> {
        board.workflow().columns().map(Column::as_str).collect()
    }

    #[test]
    fn columns_are_added_where_asked_and_named_from_their_label() {
        let (_dir, board) = board();
        let review = add(
            &board,
            "Code review",
            Kind::Doing,
            Position::After(Column::Doing),
            None,
        )
        .unwrap();
        assert_eq!(review.as_str(), "code-review");
        assert!(board.dir(review).join(".gitkeep").exists());
        add(
            &board,
            "Triage",
            Kind::Backlog,
            Position::Before(Column::Backlog),
            None,
        )
        .unwrap();
        add(&board, "Released", Kind::Done, Position::End, None).unwrap();
        assert_eq!(
            folders(&board),
            [
                "triage",
                "backlog",
                "todo",
                "doing",
                "code-review",
                "done",
                "released"
            ]
        );
        assert_eq!(board.workflow().label(review), "Code review");
        let settings = crate::eol::read(board_settings::path(&board)).unwrap();
        assert!(
            settings.contains("Code review=code-review:doing"),
            "{settings}"
        );
    }

    #[test]
    fn a_name_already_answered_to_is_refused() {
        let (_dir, board) = board();
        let refused = |label: &str, says: &str| {
            let err = add(&board, label, Kind::Doing, Position::End, None).unwrap_err();
            assert!(err.to_string().contains(says), "{label}: {err}");
        };
        refused("Doing", "already a column");
        refused("to do", "already a column");
        refused("abandon", "taken by the board");
        refused("a,b", "comma");
        fs::create_dir_all(board.dir(Column::Backlog).join("checkout")).unwrap();
        fs::write(
            board.dir(Column::Backlog).join("checkout/_group.md"),
            "---\nname: \"checkout\"\n---\n",
        )
        .unwrap();
        refused("Checkout", "epic or sprint");
        assert!(board.workflow().is_standard());
    }

    #[test]
    fn a_rename_keeps_the_folder_and_a_reorder_keeps_the_rules() {
        let (_dir, board) = board();
        rename(&board, Column::Todo, "Ready").unwrap();
        assert_eq!(board.workflow().label(Column::Todo), "Ready");
        assert_eq!(board.workflow().find("ready"), Some(Column::Todo));
        assert!(board.dir(Column::Todo).is_dir());
        let review = add(&board, "Review", Kind::Doing, Position::End, None).unwrap();
        // Done last again: Review moves before it.
        reorder(&board, review, Position::Before(Column::Done)).unwrap();
        assert_eq!(
            folders(&board),
            ["backlog", "todo", "doing", "review", "done"]
        );
        assert!(reorder(&board, review, Position::After(review)).is_err());
    }

    #[test]
    fn a_reorder_is_seen_whatever_the_settings_file_s_time_and_size() {
        // Ticket 317: Windows gives two quick writes the same modified time
        // and a reorder keeps the size, so a cache keyed on those missed it.
        let (_dir, board) = board();
        let review = add(&board, "Review", Kind::Doing, Position::End, None).unwrap();
        assert_eq!(
            folders(&board),
            ["backlog", "todo", "doing", "done", "review"]
        );
        let path = board_settings::path(&board);
        let before = fs::metadata(&path).unwrap();
        reorder(&board, review, Position::Before(Column::Done)).unwrap();
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(before.modified().unwrap())
            .unwrap();
        let after = fs::metadata(&path).unwrap();
        assert_eq!(after.len(), before.len());
        assert_eq!(after.modified().unwrap(), before.modified().unwrap());
        assert_eq!(
            folders(&board),
            ["backlog", "todo", "doing", "review", "done"]
        );
    }

    #[test]
    fn only_an_added_column_goes_and_its_tickets_go_somewhere_first() {
        let (_dir, board) = board();
        let review = add(
            &board,
            "Review",
            Kind::Doing,
            Position::After(Column::Doing),
            None,
        )
        .unwrap();
        set_wip(&board, review, Some(3)).unwrap();
        ticket(&board, review, 1);
        let err = remove(&board, Column::Done, None).unwrap_err();
        assert!(err.to_string().contains("standard"), "{err}");
        let err = remove(&board, review, None).unwrap_err();
        assert!(err.to_string().contains("--move-to"), "{err}");
        let moved = remove(&board, review, Some(Column::Doing)).unwrap();
        assert_eq!(moved, [board.dir(Column::Doing).join("001-t.md")]);
        assert!(board.workflow().is_standard());
        assert!(!board.dir(review).exists());
        let settings = crate::eol::read(board_settings::path(&board)).unwrap();
        assert!(!settings.contains("wip-limit-review"), "{settings}");
        assert!(!settings.contains("columns"), "{settings}");
    }

    #[test]
    fn every_doing_kind_column_has_its_own_limit() {
        let (_dir, board) = board();
        let review = add(&board, "Review", Kind::Doing, Position::End, None).unwrap();
        set_wip(&board, review, Some(1)).unwrap();
        let settings = board_settings::load(&board);
        assert_eq!(settings.wip_limit(review), Some(1));
        assert_eq!(settings.wip_limit(Column::Doing), Some(2));
        set_wip(&board, Column::Doing, None).unwrap();
        assert_eq!(board_settings::load(&board).wip_limit(Column::Doing), None);
        assert!(set_wip(&board, Column::Todo, Some(1)).is_err());
    }

    #[test]
    fn a_columns_setting_that_cant_be_read_falls_back_and_says_why() {
        let (_dir, board) = board();
        fs::write(
            board_settings::path(&board),
            "columns = backlog:backlog, todo:todo\n",
        )
        .unwrap();
        assert!(board.workflow().is_standard());
        let problem = board.workflow_problem().unwrap();
        assert!(problem.contains("standard four"), "{problem}");
    }

    #[test]
    fn extra_moves_and_landing_follow_the_board_and_go_with_a_column() {
        use crate::workflow::Flow;
        let (_dir, board) = board();
        let triage = add(
            &board,
            "Triage",
            Kind::Backlog,
            Position::Before(Column::Backlog),
            None,
        )
        .unwrap();
        allow_move(&board, triage, Column::Todo, true).unwrap();
        allow_move(&board, triage, Column::Todo, true).unwrap();
        set_landing(&board, triage).unwrap();
        let w = board.workflow();
        assert!(w.allows(triage, Column::Todo));
        assert_eq!(w.moves().len(), 1);
        assert_eq!(w.landing(), triage);
        assert!(set_landing(&board, Column::Doing).is_err());
        set_flow(&board, Flow::Any).unwrap();
        assert!(board.workflow().allows(Column::Backlog, Column::Done));
        set_flow(&board, Flow::Next).unwrap();
        allow_move(&board, triage, Column::Todo, false).unwrap();
        assert!(!board.workflow().allows(triage, Column::Todo));
        allow_move(&board, triage, Column::Todo, true).unwrap();
        remove(&board, triage, None).unwrap();
        let settings = crate::eol::read(board_settings::path(&board)).unwrap();
        assert!(!settings.contains("moves"), "{settings}");
        assert!(!settings.contains("new-tickets"), "{settings}");
    }

    #[test]
    fn a_reset_moves_tickets_by_kind_and_forgets_everything() {
        use crate::workflow::Flow;
        let (_dir, board) = board();
        let triage = add(
            &board,
            "Triage",
            Kind::Backlog,
            Position::Before(Column::Backlog),
            None,
        )
        .unwrap();
        let review = add(
            &board,
            "Review",
            Kind::Doing,
            Position::After(Column::Doing),
            None,
        )
        .unwrap();
        let released = add(&board, "Released", Kind::Done, Position::End, None).unwrap();
        rename(&board, Column::Todo, "Ready").unwrap();
        set_wip(&board, review, Some(2)).unwrap();
        set_flow(&board, Flow::Any).unwrap();
        set_landing(&board, triage).unwrap();
        ticket(&board, triage, 1);
        ticket(&board, review, 2);
        ticket(&board, released, 3);
        let moved = reset(&board).unwrap();
        assert_eq!(moved.len(), 3);
        assert!(board.dir(Column::Backlog).join("001-t.md").exists());
        assert!(board.dir(Column::Doing).join("002-t.md").exists());
        assert!(board.dir(Column::Done).join("003-t.md").exists());
        let w = board.workflow();
        assert!(w.is_standard() && w.flow() == Flow::Next && w.landing() == Column::Backlog);
        assert!(!board.dir(review).exists());
        let settings = crate::eol::read(board_settings::path(&board)).unwrap();
        for key in [
            "columns",
            "flow",
            "moves",
            "new-tickets",
            "wip-limit-review",
        ] {
            assert!(!settings.contains(key), "{key}: {settings}");
        }
    }

    #[test]
    fn a_new_ticket_lands_where_the_team_says_unless_it_goes_into_an_epic() {
        use crate::create::{Draft, NewTicket};
        use crate::templates::{DEFAULT_TEMPLATE, Templates};
        let (_dir, board) = board();
        let triage = add(
            &board,
            "Triage",
            Kind::Backlog,
            Position::Before(Column::Backlog),
            None,
        )
        .unwrap();
        set_landing(&board, triage).unwrap();
        let templates = Templates::load(&board).unwrap();
        let new = |land_in| NewTicket {
            title: "Fresh".into(),
            template: DEFAULT_TEMPLATE.into(),
            land_in,
            ..NewTicket::default()
        };
        let draft = Draft::prepare(&board, &templates, new(None)).unwrap();
        assert_eq!(draft.column, triage);
        let created = draft.write(&board, None).unwrap();
        assert_eq!(created.task.column, triage);
        assert!(board.dir(triage).join(created.task.file_name()).exists());
        let filed = Draft::prepare(&board, &templates, new(Some(Column::Backlog))).unwrap();
        assert_eq!(filed.column, Column::Backlog);
    }
}
