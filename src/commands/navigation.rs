//! Convenience commands to help the user move through a stack of commits.

use std::fmt::Write;
use std::io::stdin;
use std::time::SystemTime;

use eden_dag::DagAlgorithm;
use tracing::instrument;

use crate::commands::smartlog::render;
use crate::commands::smartlog::{make_smartlog_graph, number_nodes, render_graph, smartlog};
use crate::core::eventlog::{EventLogDb, EventReplayer};
use crate::core::formatting::printable_styled_string;
use crate::core::metadata::{
    BranchesProvider, CommitMessageProvider, CommitNumberProvider, CommitOidProvider,
    DifferentialRevisionProvider, ObsolescenceExplanationProvider, RelativeTimeProvider,
};
use crate::git::{sort_commit_set, CommitSet, Dag, GitRunInfo, NonZeroOid, Repo};
use crate::tui::Effects;

/// Go back a certain number of commits.
#[instrument]
pub fn prev(
    effects: &Effects,
    git_run_info: &GitRunInfo,
    num_commits: Option<isize>,
) -> eyre::Result<isize> {
    let exit_code = match num_commits {
        None => git_run_info.run(effects, None, &["checkout", "HEAD^"])?,
        Some(num_commits) => git_run_info.run(
            effects,
            None,
            &["checkout", &format!("HEAD~{}", num_commits)],
        )?,
    };
    if exit_code != 0 {
        return Ok(exit_code);
    }
    smartlog(effects, &Default::default())?;
    Ok(0)
}

/// Some commits have multiple children, which makes `next` ambiguous. These
/// values disambiguate which child commit to go to, according to the committed
/// date.
#[derive(Clone, Copy, Debug)]
pub enum Towards {
    /// When encountering multiple children, select the newest one.
    Newest,

    /// When encountering multiple children, select the oldest one.
    Oldest,
}

#[instrument]
fn advance(
    effects: &Effects,
    repo: &Repo,
    dag: &Dag,
    current_oid: NonZeroOid,
    num_commits: isize,
    towards: Option<Towards>,
    interactive: bool,
) -> eyre::Result<Option<NonZeroOid>> {
    let glyphs = effects.get_glyphs();
    let mut current_oid = current_oid;
    for i in 0..num_commits {
        let children = dag
            .query()
            .children(CommitSet::from(current_oid))?
            .difference(&dag.obsolete_commits);
        let children = sort_commit_set(repo, dag, &children)?;

        current_oid = match (towards, children.as_slice()) {
            (_, []) => {
                // It would also make sense to issue an error here, rather than
                // silently stop going forward commits.
                break;
            }
            (_, [only_child]) => only_child.get_oid(),
            (Some(Towards::Newest), [.., newest_child]) => newest_child.get_oid(),
            (Some(Towards::Oldest), [oldest_child, ..]) => oldest_child.get_oid(),
            (None, [_, _, ..]) => {
                writeln!(
                    effects.get_output_stream(),
                    "Found multiple possible next commits to go to after traversing {} children:",
                    i
                )?;

                for (j, child) in (0..).zip(children.iter()) {
                    let prefix = if interactive {
                        format!(" [{}] ", j + 1)
                    } else {
                        "".into()
                    };
                    let descriptor = if j == 0 {
                        " (oldest)"
                    } else if j + 1 == children.len() {
                        " (newest)"
                    } else {
                        ""
                    };

                    writeln!(
                        effects.get_output_stream(),
                        "  {} {}{}{}",
                        glyphs.bullet_point,
                        prefix,
                        printable_styled_string(glyphs, child.friendly_describe()?)?,
                        descriptor
                    )?;
                }
                if interactive {
                    match prompt_for_range(effects, 1, children.len())? {
                        Some(selected) => children[selected - 1].get_oid(),
                        None => return Ok(None),
                    }
                } else {
                    writeln!(effects.get_output_stream(), "(Pass --oldest (-o) or --newest (-n) to select between ambiguous next commits)")?;
                    return Ok(None);
                }
            }
        };
    }
    Ok(Some(current_oid))
}

/// Go forward a certain number of commits.
#[instrument]
pub fn next(
    effects: &Effects,
    git_run_info: &GitRunInfo,
    num_commits: Option<isize>,
    towards: Option<Towards>,
    interactive: bool,
) -> eyre::Result<isize> {
    let repo = Repo::from_current_dir()?;
    let references_snapshot = repo.get_references_snapshot()?;
    let conn = repo.get_db_conn()?;
    let event_log_db = EventLogDb::new(&conn)?;
    let event_replayer = EventReplayer::from_event_log_db(effects, &repo, &event_log_db)?;
    let event_cursor = event_replayer.make_default_cursor();
    let dag = Dag::open_and_sync(
        effects,
        &repo,
        &event_replayer,
        event_cursor,
        &references_snapshot,
    )?;

    let head_oid = match references_snapshot.head_oid {
        Some(head_oid) => head_oid,
        None => {
            eyre::bail!("No HEAD present; cannot calculate next commit");
        }
    };

    let num_commits = num_commits.unwrap_or(1);
    let current_oid = advance(
        effects,
        &repo,
        &dag,
        head_oid,
        num_commits,
        towards,
        interactive,
    )?;
    let current_oid = match current_oid {
        None => return Ok(1),
        Some(current_oid) => current_oid,
    };

    let result = git_run_info.run(effects, None, &["checkout", &current_oid.to_string()])?;
    if result != 0 {
        return Ok(result);
    }

    smartlog(effects, &Default::default())?;
    Ok(0)
}

/// Pick a specific commit to checkout.
#[instrument]
pub fn pick(effects: &Effects, git_run_info: &GitRunInfo) -> eyre::Result<isize> {
    let repo = Repo::from_current_dir()?;
    let references_snapshot = repo.get_references_snapshot()?;
    let conn = repo.get_db_conn()?;
    let event_log_db = EventLogDb::new(&conn)?;
    let event_replayer = EventReplayer::from_event_log_db(effects, &repo, &event_log_db)?;
    let event_cursor = event_replayer.make_default_cursor();
    let dag = Dag::open_and_sync(
        effects,
        &repo,
        &event_replayer,
        event_cursor,
        &references_snapshot,
    )?;

    let graph = make_smartlog_graph(effects, &repo, &dag, &event_replayer, event_cursor, true)?;

    let root_oids = render::split_commit_graph_by_roots(effects, &repo, &dag, &graph);
    let numbered_nodes = number_nodes(&graph, &root_oids);

    let lines = render_graph(
        effects,
        &repo,
        &dag,
        &graph,
        references_snapshot.head_oid,
        &mut [
            &mut CommitOidProvider::new(true)?,
            &mut RelativeTimeProvider::new(&repo, SystemTime::now())?,
            &mut ObsolescenceExplanationProvider::new(&event_replayer, event_cursor)?,
            &mut BranchesProvider::new(&repo, &references_snapshot)?,
            &mut DifferentialRevisionProvider::new(&repo)?,
            &mut CommitMessageProvider::new()?,
            &mut CommitNumberProvider::new(&numbered_nodes)?,
        ],
    )?;
    for line in lines {
        writeln!(
            effects.get_output_stream(),
            "{}",
            printable_styled_string(effects.get_glyphs(), line)?
        )?;
    }

    match prompt_for_range(effects, 1, numbered_nodes.len())? {
        Some(selected) => {
            let oid = numbered_nodes
                .iter()
                .find(|&(_, num)| *num == selected)
                .map(|(oid, _)| oid)
                .expect("fixme");

            let result = git_run_info.run(effects, None, &["checkout", &oid.to_string()])?;
            if result != 0 {
                return Ok(result);
            }
            Ok(0)
        }
        None => Ok(1),
    }
}

fn prompt_for_range(effects: &Effects, min: usize, max: usize) -> eyre::Result<Option<usize>> {
    write!(
        effects.get_output_stream(),
        "Select the commit to advance to [{}-{}]: ",
        min,
        max
    )?;
    let mut in_ = String::new();
    stdin().read_line(&mut in_)?;
    let selected = in_.trim().parse::<usize>().unwrap_or(0);
    if selected < min || selected > max {
        writeln!(
            effects.get_error_stream(),
            "Invalid selection. Must be in range [{}-{}]",
            min,
            max
        )?;
        Ok(None)
    } else {
        Ok(Some(selected))
    }
}
