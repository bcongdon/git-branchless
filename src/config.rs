//! Accesses repo-specific configuration.

/// Get the name of the main branch for the repository.
///
/// Args:
/// * `repo`: The Git repository.
///
/// Returns: The name of the main branch for the repository.
pub fn get_main_branch_name(repo: &git2::Repository) -> anyhow::Result<String> {
    let config = repo.config()?;
    config
        .get_string("branchless.mainBranch")
        .or_else(|_| Ok(String::from("master")))
}

/// If `true`, when restacking a commit, do not update its timestamp to the
/// current time.
///
/// TODO: document this configuration option in the wiki at
/// https://github.com/arxanas/git-branchless/wiki/Configuration
pub fn get_restack_preserve_timestamps(repo: &git2::Repository) -> anyhow::Result<bool> {
    let config = repo.config()?;
    config
        .get_bool("branchless.restack.preserveTimestamps")
        .or(Ok(false))
}
