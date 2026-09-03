use super::*;

impl App {
    /// Create a session for a commit range (used by revisions and commit selection).
    pub(in crate::app) fn load_or_create_commit_range_session(
        vcs_info: &VcsInfo,
        commit_ids: &[String],
    ) -> ReviewSession {
        let newest_commit_id = commit_ids.last().unwrap().clone();
        let mut session = ReviewSession::new(
            vcs_info.root_path.clone(),
            newest_commit_id,
            vcs_info.branch_name.clone(),
            SessionDiffSource::CommitRange,
        );
        session.commit_range = Some(commit_ids.to_vec());
        session
    }

    pub(in crate::app) fn load_or_create_staged_unstaged_and_commits_session(
        vcs_info: &VcsInfo,
        commit_ids: &[String],
    ) -> ReviewSession {
        let newest_commit_id = commit_ids.last().unwrap().clone();
        let mut session = ReviewSession::new(
            vcs_info.root_path.clone(),
            newest_commit_id,
            vcs_info.branch_name.clone(),
            SessionDiffSource::StagedUnstagedAndCommits,
        );
        session.commit_range = Some(commit_ids.to_vec());
        session
    }

    pub(in crate::app) fn load_or_create_session(
        vcs_info: &VcsInfo,
        diff_source: SessionDiffSource,
    ) -> ReviewSession {
        ReviewSession::new(
            vcs_info.root_path.clone(),
            vcs_info.head_commit.clone(),
            vcs_info.branch_name.clone(),
            diff_source,
        )
    }
}
