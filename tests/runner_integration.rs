mod support;

use jarvis::{
    EvidenceStatus, Failure, GateDecision, Job, JobError, JobState, Operation, Recovery,
    SpeculationKey, gate, recovery, speculation_is_current,
};
use std::fs;
use support::TempDir;

fn draft_job() -> Job {
    Job::new("job-a", "request-a", Operation::DraftMessage).unwrap()
}

#[test]
fn clarification_to_review_is_a_real_unsent_file_not_completed_work() {
    let dir = TempDir::new();
    let mut job = draft_job();
    assert_eq!(job.request_id(), "request-a");
    let question = job.ask("recipient", "Who receives this?").unwrap();
    assert!(matches!(
        job.prepare_review(job.revision(), &dir.join("premature"), b"draft"),
        Err(JobError::InvalidState)
    ));
    job.answer(&question, "Brian").unwrap();
    assert_eq!(job.answers(), &["Brian"]);
    assert_eq!(job.revision(), 2);
    let path = dir.join("message.txt");
    job.prepare_review(2, &path, b"UNSENT draft for Brian")
        .unwrap();
    assert_eq!(job.state(), JobState::ReadyForReview);
    assert_eq!(
        fs::read(job.review_path().unwrap()).unwrap(),
        b"UNSENT draft for Brian"
    );
    assert!(job.complete_verified_action().is_err());
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn sensitive_commit_operations_cannot_be_created_or_pass_gate() {
    for operation in [Operation::SendMessage, Operation::Pay, Operation::Delete] {
        assert!(matches!(
            Job::new("job", "request", operation),
            Err(JobError::FinalCommitForbidden)
        ));
        for evidence in [
            EvidenceStatus::Ready,
            EvidenceStatus::Missing,
            EvidenceStatus::Stale,
        ] {
            for now in [0, 1000, u64::MAX] {
                assert_eq!(
                    gate(operation, evidence, now, 1000),
                    GateDecision::DenyFinalCommit
                );
            }
        }
    }
}

#[test]
fn preparing_purchase_and_deletion_only_creates_review_artifacts() {
    let dir = TempDir::new();
    let original = dir.join("original.txt");
    fs::write(&original, b"must survive").unwrap();
    for (name, operation) in [
        ("purchase-review", Operation::PreparePurchase),
        ("deletion-review", Operation::PrepareDeletion),
    ] {
        let mut job = Job::new(name, "request", operation).unwrap();
        assert!(matches!(
            job.complete_verified_action(),
            Err(JobError::NotReviewable)
        ));
        job.prepare_review(job.revision(), &dir.join(name), b"Pending human review")
            .unwrap();
        assert_eq!(job.state(), JobState::ReadyForReview);
        assert!(job.review_path().unwrap().exists());
    }
    assert_eq!(fs::read(original).unwrap(), b"must survive");
}

#[test]
fn wrong_job_and_empty_answers_do_not_mutate_pending_question() {
    let mut first = draft_job();
    let mut second = Job::new("job-b", "request-b", Operation::DraftMessage).unwrap();
    let first_question = first.ask("same-id", "Who?").unwrap();
    let second_question = second.ask("same-id", "Who?").unwrap();
    assert!(matches!(
        first.answer(&second_question, "Wrong person"),
        Err(JobError::WrongQuestion)
    ));
    assert!(matches!(
        first.answer(&first_question, " \n\t "),
        Err(JobError::EmptyAnswer)
    ));
    assert_eq!(first.revision(), 1);
    assert!(first.answers().is_empty());
    assert_eq!(first.state(), JobState::AwaitingClarification);
    first.answer(&first_question, "Correct person").unwrap();
    assert_eq!(second.state(), JobState::AwaitingClarification);
}

#[test]
fn outdated_answer_cannot_satisfy_a_new_question_with_reused_id() {
    let mut job = draft_job();
    let stale = job.ask("recipient", "Who?").unwrap();
    job.dependency_changed().unwrap();
    assert_eq!(job.state(), JobState::Blocked);
    assert!(job.answer(&stale, "Old answer").is_err());
    assert!(matches!(
        job.resume_after_reconciliation(1),
        Err(JobError::StaleRevision)
    ));
    job.resume_after_reconciliation(2).unwrap();
    let current = job.ask("recipient", "Who?").unwrap();
    assert!(matches!(
        job.answer(&stale, "Old answer"),
        Err(JobError::StaleRevision)
    ));
    job.answer(&current, "Fresh answer").unwrap();
    assert_eq!(job.answers(), &["Fresh answer"]);
}

#[test]
fn cancelled_jobs_cannot_resume_answer_or_create_artifacts() {
    let dir = TempDir::new();
    let mut job = draft_job();
    let question = job.ask("q", "Who?").unwrap();
    job.cancel().unwrap();
    assert!(job.answer(&question, "Brian").is_err());
    assert!(job.resume_after_reconciliation(job.revision()).is_err());
    assert!(job.dependency_changed().is_err());
    assert!(
        job.prepare_review(job.revision(), &dir.join("forbidden"), b"draft")
            .is_err()
    );
    assert!(!dir.join("forbidden").exists());
    assert_eq!(job.state(), JobState::Cancelled);
}

#[test]
fn cancellation_revokes_review_without_deleting_user_artifact() {
    let dir = TempDir::new();
    let path = dir.join("draft");
    let mut job = draft_job();
    job.prepare_review(1, &path, b"draft").unwrap();
    job.cancel().unwrap();
    assert!(job.review_path().is_err());
    assert_eq!(fs::read(path).unwrap(), b"draft");
}

#[test]
fn conflict_invalidates_review_and_rejects_old_revision() {
    let dir = TempDir::new();
    let mut job = draft_job();
    job.prepare_review(1, &dir.join("old"), b"old constraints")
        .unwrap();
    job.dependency_changed().unwrap();
    assert!(job.review_path().is_err());
    job.resume_after_reconciliation(2).unwrap();
    assert!(matches!(
        job.prepare_review(1, &dir.join("stale"), b"old"),
        Err(JobError::StaleRevision)
    ));
    assert!(!dir.join("stale").exists());
    job.prepare_review(2, &dir.join("new"), b"new constraints")
        .unwrap();
    assert_eq!(
        fs::read(job.review_path().unwrap()).unwrap(),
        b"new constraints"
    );
}

#[test]
fn artifact_tampering_or_disappearance_revokes_ready_status() {
    for remove in [false, true] {
        let dir = TempDir::new();
        let path = dir.join("draft");
        let mut job = draft_job();
        job.prepare_review(1, &path, b"expected").unwrap();
        if remove {
            fs::remove_file(&path).unwrap();
        } else {
            fs::write(&path, b"unauthorized recipient").unwrap();
        }
        assert!(job.review_path().is_err());
        assert_eq!(job.state(), JobState::Blocked);
        assert!(job.review_path().is_err());
    }
}

#[test]
fn existing_artifact_is_never_overwritten() {
    let dir = TempDir::new();
    let path = dir.join("draft");
    fs::write(&path, b"user-owned").unwrap();
    let mut job = draft_job();
    assert!(job.prepare_review(1, &path, b"replacement").is_err());
    assert_eq!(fs::read(path).unwrap(), b"user-owned");
    assert_eq!(job.state(), JobState::Running);
    assert!(job.review_path().is_err());
}

#[cfg(unix)]
#[test]
fn existing_symlink_is_not_followed_when_preparing() {
    let dir = TempDir::new();
    let target = dir.join("target");
    let link = dir.join("link");
    fs::write(&target, b"user-owned").unwrap();
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let mut job = draft_job();
    assert!(job.prepare_review(1, &link, b"replacement").is_err());
    assert_eq!(fs::read(target).unwrap(), b"user-owned");
}

#[cfg(unix)]
#[test]
fn replacing_review_artifact_with_same_content_symlink_is_rejected() {
    let dir = TempDir::new();
    let path = dir.join("draft");
    let target = dir.join("unrelated");
    let mut job = draft_job();
    job.prepare_review(1, &path, b"same bytes").unwrap();
    fs::write(&target, b"same bytes").unwrap();
    fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink(&target, &path).unwrap();
    assert!(
        job.review_path().is_err(),
        "a replacement symlink must not be treated as the verified artifact"
    );
    assert_eq!(job.state(), JobState::Blocked);
}

#[test]
fn direct_action_completion_is_terminal_and_not_reviewable() {
    let dir = TempDir::new();
    let mut job = Job::new("close", "request", Operation::CloseTab).unwrap();
    assert!(matches!(
        job.prepare_review(1, &dir.join("not-a-draft"), b"x"),
        Err(JobError::NotReviewable)
    ));
    job.complete_verified_action().unwrap();
    assert_eq!(job.state(), JobState::Completed);
    assert!(job.cancel().is_err());
    assert!(job.dependency_changed().is_err());
    assert!(job.ask("q", "Who?").is_err());
    assert!(!dir.join("not-a-draft").exists());
}

#[cfg(unix)]
#[test]
fn replacement_regular_file_with_identical_bytes_requires_reverification() {
    let dir = TempDir::new();
    let path = dir.join("draft");
    let replacement = dir.join("replacement");
    let mut job = draft_job();
    job.prepare_review(1, &path, b"same bytes").unwrap();
    fs::write(&replacement, b"same bytes").unwrap();
    fs::rename(replacement, &path).unwrap();
    assert!(
        job.review_path().is_err(),
        "identical content in a different file is not the originally verified artifact"
    );
    assert_eq!(job.state(), JobState::Blocked);
}

#[test]
fn deadline_and_missing_evidence_never_allow_execution() {
    assert_eq!(
        gate(Operation::CloseTab, EvidenceStatus::Ready, 99, 100),
        GateDecision::Allow
    );
    assert_eq!(
        gate(Operation::CloseTab, EvidenceStatus::Ready, 100, 100),
        GateDecision::TimedOut
    );
    for evidence in [EvidenceStatus::Missing, EvidenceStatus::Stale] {
        assert_eq!(
            gate(Operation::CloseTab, evidence, 99, 100),
            GateDecision::AwaitEvidence
        );
        assert_eq!(
            gate(Operation::CloseTab, evidence, 100, 100),
            GateDecision::TimedOut
        );
    }
}

#[test]
fn recovery_budget_stops_repetition_but_never_retries_uncertain_writes() {
    for failure in [Failure::TransientRead, Failure::StaleTarget] {
        assert_eq!(recovery(failure, 0, 0), Recovery::Stop);
        assert_eq!(recovery(failure, 2, 2), Recovery::Stop);
        assert_eq!(recovery(failure, 3, 2), Recovery::Stop);
    }
    assert_eq!(recovery(Failure::TransientRead, 0, 2), Recovery::Retry);
    assert_eq!(
        recovery(Failure::StaleTarget, 0, 2),
        Recovery::RefreshAndRevalidate
    );
    for used in [0, 2, u32::MAX] {
        assert_eq!(
            recovery(Failure::UncertainExternalOutcome, used, 2),
            Recovery::ReconcileOutcome
        );
        assert_eq!(recovery(Failure::ConflictingEdit, used, 2), Recovery::Pause);
        assert_eq!(recovery(Failure::Permanent, used, 2), Recovery::Stop);
    }
}

#[test]
fn speculative_context_cannot_cross_request_or_transcript_revision() {
    let original = SpeculationKey {
        request_id: "request-a".into(),
        transcript_revision: 3,
    };
    assert!(speculation_is_current(&original, &original));
    for current in [
        SpeculationKey {
            request_id: "request-b".into(),
            transcript_revision: 3,
        },
        SpeculationKey {
            request_id: "request-a".into(),
            transcript_revision: 4,
        },
        SpeculationKey {
            request_id: "request-a".into(),
            transcript_revision: 2,
        },
    ] {
        assert!(!speculation_is_current(&original, &current));
    }
}
