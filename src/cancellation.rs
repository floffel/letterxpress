//! Bounded cancellation of jobs that are still inside the provider's
//! fifteen-minute deletion window.
//!
//! The provider has no bulk-delete endpoint. A batch therefore means a
//! deliberately bounded sequence of the documented single-job `DELETE`
//! requests, never an arbitrary fan-out. The sequence is capped at the
//! documented 120 requests per minute, runs one request at a time, and reports
//! one typed result per input job so a failed deletion does not hide the
//! outcome of its neighbours.
//!
//! The deadline is based on a monotonic [`Instant`] captured when a job was
//! transmitted. The provider's timestamps do not carry a documented zone, so
//! this avoids inventing one. Capture the instant with [`Instant::now`] as
//! close to the successful submit as possible.
//!
//! ```
//! use std::time::{Duration, Instant};
//!
//! use letterxpress::{CancellationBatch, CancellationJob, JobId};
//!
//! let transmitted_at = Instant::now();
//! let mut batch = CancellationBatch::new();
//! batch.push(CancellationJob::print(
//!     JobId::new("6035138")?,
//!     transmitted_at,
//! )?)?;
//! batch.push(CancellationJob::email(
//!     JobId::new("3096")?,
//!     transmitted_at,
//! )?)?;
//!
//! assert_eq!(batch.len(), 2);
//! assert!(batch.iter().all(|job| job.remaining_at(transmitted_at) == Duration::from_secs(15 * 60)));
//! # Ok::<(), letterxpress::Error>(())
//! ```

use std::time::{Duration, Instant};

use crate::{Deletion, Error, JobId, Result};

/// The provider's deletion window: the first fifteen minutes after
/// transmission (documentation pp. 18 and 25).
///
/// ```
/// use std::time::Duration;
///
/// assert_eq!(letterxpress::CANCELLATION_WINDOW, Duration::from_secs(900));
/// ```
pub const CANCELLATION_WINDOW: Duration = Duration::from_secs(15 * 60);

/// The largest batch this helper will accept.
///
/// One deletion is one HTTP request, and the provider documents 120 requests
/// per minute (p. 3). This ceiling keeps a single batch from spending more than
/// one minute of that quota. Other traffic in the same minute still has to be
/// budgeted by the caller.
///
/// ```
/// assert_eq!(letterxpress::MAX_CANCELLATION_BATCH, 120);
/// assert_eq!(
///     letterxpress::MAX_CANCELLATION_BATCH,
///     letterxpress::RATE_LIMIT_PER_MINUTE as usize,
/// );
/// ```
pub const MAX_CANCELLATION_BATCH: usize = 120;

/// Which documented single-job deletion endpoint a batch entry uses.
///
/// ```
/// use letterxpress::CancellationKind;
///
/// assert_eq!(CancellationKind::PrintJob.as_str(), "printjobs");
/// assert_eq!(CancellationKind::EmailJob.as_str(), "emailjobs");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CancellationKind {
    /// `DELETE /v3/printjobs/{id}`.
    PrintJob,
    /// `DELETE /v3/emailjobs/{id}`.
    EmailJob,
}

impl CancellationKind {
    /// The provider collection this kind belongs to.
    ///
    /// ```
    /// use letterxpress::CancellationKind;
    ///
    /// assert_eq!(CancellationKind::PrintJob.as_str(), "printjobs");
    /// assert_eq!(CancellationKind::EmailJob.as_str(), "emailjobs");
    /// ```
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PrintJob => "printjobs",
            Self::EmailJob => "emailjobs",
        }
    }
}

/// One job in a bounded cancellation batch.
///
/// `transmitted_at` must be the monotonic instant captured when the provider
/// accepted the submit. It anchors the documented fifteen-minute deletion
/// window without relying on the provider's zone-less `created_at` string.
///
/// ```
/// use std::time::Instant;
///
/// use letterxpress::{CancellationJob, CancellationKind, JobId};
///
/// let started = Instant::now();
/// let job = CancellationJob::print(JobId::new("6035138")?, started)?;
/// assert_eq!(job.kind(), CancellationKind::PrintJob);
/// assert_eq!(job.id().as_str(), "6035138");
/// assert!(job.is_in_window_at(started));
/// # Ok::<(), letterxpress::Error>(())
/// ```
#[derive(Debug, Clone)]
pub struct CancellationJob {
    id: JobId,
    kind: CancellationKind,
    transmitted_at: Instant,
    deadline: Instant,
}

impl CancellationJob {
    /// A print job transmitted at `transmitted_at`.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] if the fifteen-minute deadline cannot be
    /// represented by [`Instant`].
    ///
    /// ```
    /// use std::time::Instant;
    ///
    /// use letterxpress::{CancellationJob, JobId};
    ///
    /// let job = CancellationJob::print(JobId::new("42")?, Instant::now())?;
    /// assert_eq!(job.kind().as_str(), "printjobs");
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    pub fn print(id: JobId, transmitted_at: Instant) -> Result<Self> {
        Self::new(id, CancellationKind::PrintJob, transmitted_at)
    }

    /// An e-mail job transmitted at `transmitted_at`.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] if the fifteen-minute deadline cannot be
    /// represented by [`Instant`].
    ///
    /// ```
    /// use std::time::Instant;
    ///
    /// use letterxpress::{CancellationJob, JobId};
    ///
    /// let job = CancellationJob::email(JobId::new("3096")?, Instant::now())?;
    /// assert_eq!(job.kind().as_str(), "emailjobs");
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    pub fn email(id: JobId, transmitted_at: Instant) -> Result<Self> {
        Self::new(id, CancellationKind::EmailJob, transmitted_at)
    }

    fn new(id: JobId, kind: CancellationKind, transmitted_at: Instant) -> Result<Self> {
        let deadline = transmitted_at
            .checked_add(CANCELLATION_WINDOW)
            .ok_or_else(|| {
                Error::invalid(
                    "cancellation.transmitted_at",
                    "the fifteen-minute cancellation deadline is not representable",
                )
            })?;
        Ok(Self {
            id,
            kind,
            transmitted_at,
            deadline,
        })
    }

    /// The provider job id.
    ///
    /// ```
    /// use std::time::Instant;
    ///
    /// use letterxpress::{CancellationJob, JobId};
    ///
    /// let job = CancellationJob::print(JobId::new("42")?, Instant::now())?;
    /// assert_eq!(job.id().as_str(), "42");
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    #[must_use]
    pub fn id(&self) -> &JobId {
        &self.id
    }

    /// The single-job endpoint selected for this entry.
    ///
    /// ```
    /// use std::time::Instant;
    ///
    /// use letterxpress::{CancellationJob, CancellationKind, JobId};
    ///
    /// let job = CancellationJob::email(JobId::new("42")?, Instant::now())?;
    /// assert_eq!(job.kind(), CancellationKind::EmailJob);
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    #[must_use]
    pub fn kind(&self) -> CancellationKind {
        self.kind
    }

    /// The monotonic instant supplied for the successful submit.
    ///
    /// ```
    /// use std::time::Instant;
    ///
    /// use letterxpress::{CancellationJob, JobId};
    ///
    /// let transmitted_at = Instant::now();
    /// let job = CancellationJob::print(JobId::new("42")?, transmitted_at)?;
    /// assert_eq!(job.transmitted_at(), transmitted_at);
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    #[must_use]
    pub fn transmitted_at(&self) -> Instant {
        self.transmitted_at
    }

    /// The exclusive end of the provider's fifteen-minute deletion window.
    ///
    /// ```
    /// use std::time::{Duration, Instant};
    ///
    /// use letterxpress::{CancellationJob, JobId};
    ///
    /// let transmitted_at = Instant::now();
    /// let job = CancellationJob::print(JobId::new("42")?, transmitted_at)?;
    /// assert_eq!(job.deadline(), transmitted_at + Duration::from_secs(900));
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    #[must_use]
    pub fn deadline(&self) -> Instant {
        self.deadline
    }

    /// Remaining time at `now`, saturated at zero.
    ///
    /// ```
    /// use std::time::{Duration, Instant};
    ///
    /// use letterxpress::{CancellationJob, JobId};
    ///
    /// let transmitted_at = Instant::now();
    /// let job = CancellationJob::print(JobId::new("42")?, transmitted_at)?;
    /// assert_eq!(job.remaining_at(transmitted_at), Duration::from_secs(900));
    /// assert_eq!(job.remaining_at(job.deadline()), Duration::ZERO);
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    #[must_use]
    pub fn remaining_at(&self, now: Instant) -> Duration {
        self.deadline.saturating_duration_since(now)
    }

    /// Whether the provider's window is still open at `now`.
    ///
    /// The end is exclusive: `now == deadline` is outside the window.
    ///
    /// ```
    /// use std::time::{Duration, Instant};
    ///
    /// use letterxpress::{CancellationJob, JobId};
    ///
    /// let transmitted_at = Instant::now();
    /// let job = CancellationJob::print(JobId::new("42")?, transmitted_at)?;
    /// assert!(job.is_in_window_at(transmitted_at + Duration::from_secs(899)));
    /// assert!(!job.is_in_window_at(transmitted_at + Duration::from_secs(900)));
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    #[must_use]
    pub fn is_in_window_at(&self, now: Instant) -> bool {
        now >= self.transmitted_at && now < self.deadline
    }
}

/// A validated, bounded list of jobs to cancel.
///
/// The empty batch is valid and performs no requests. A batch rejects more
/// than [`MAX_CANCELLATION_BATCH`] entries and duplicate `(kind, id)` pairs at
/// insertion time, before anything reaches the wire.
///
/// ```
/// use std::time::Instant;
///
/// use letterxpress::{CancellationBatch, CancellationJob, JobId};
///
/// let mut batch = CancellationBatch::new();
/// assert!(batch.is_empty());
/// batch.push(CancellationJob::print(JobId::new("1")?, Instant::now())?)?;
/// assert_eq!(batch.len(), 1);
/// # Ok::<(), letterxpress::Error>(())
/// ```
#[derive(Debug, Clone, Default)]
pub struct CancellationBatch {
    jobs: Vec<CancellationJob>,
}

impl CancellationBatch {
    /// Creates an empty batch.
    ///
    /// ```
    /// use letterxpress::CancellationBatch;
    ///
    /// assert!(CancellationBatch::new().is_empty());
    /// ```
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates an empty batch with room for `capacity` entries.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when `capacity` exceeds
    /// [`MAX_CANCELLATION_BATCH`]. The rejection happens before allocation.
    ///
    /// ```
    /// use letterxpress::{CancellationBatch, MAX_CANCELLATION_BATCH};
    ///
    /// assert!(CancellationBatch::try_with_capacity(MAX_CANCELLATION_BATCH).is_ok());
    /// assert!(CancellationBatch::try_with_capacity(MAX_CANCELLATION_BATCH + 1).is_err());
    /// ```
    pub fn try_with_capacity(capacity: usize) -> Result<Self> {
        if capacity > MAX_CANCELLATION_BATCH {
            return Err(Error::invalid(
                "cancellation.batch",
                format!("must contain at most {MAX_CANCELLATION_BATCH} jobs"),
            ));
        }
        Ok(Self {
            jobs: Vec::with_capacity(capacity),
        })
    }

    /// Adds one job after checking the batch ceiling and duplicate ids.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when the batch already contains
    /// [`MAX_CANCELLATION_BATCH`] jobs, or when the same `(kind, id)` pair is
    /// already present. An invalid push leaves the batch unchanged.
    ///
    /// ```
    /// use std::time::Instant;
    ///
    /// use letterxpress::{CancellationBatch, CancellationJob, JobId};
    ///
    /// let mut batch = CancellationBatch::new();
    /// let job = CancellationJob::print(JobId::new("1")?, Instant::now())?;
    /// batch.push(job.clone())?;
    /// assert!(batch.push(job).is_err());
    /// assert_eq!(batch.len(), 1);
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    pub fn push(&mut self, job: CancellationJob) -> Result<()> {
        if self.jobs.len() == MAX_CANCELLATION_BATCH {
            return Err(Error::invalid(
                "cancellation.batch",
                format!("must contain at most {MAX_CANCELLATION_BATCH} jobs"),
            ));
        }
        if self
            .jobs
            .iter()
            .any(|existing| existing.kind == job.kind && existing.id == job.id)
        {
            return Err(Error::invalid(
                "cancellation.batch",
                format!(
                    "contains {} job {} more than once",
                    job.kind.as_str(),
                    job.id
                ),
            ));
        }
        self.jobs.push(job);
        Ok(())
    }

    /// Number of jobs in the batch.
    ///
    /// ```
    /// assert_eq!(letterxpress::CancellationBatch::new().len(), 0);
    /// ```
    #[must_use]
    pub fn len(&self) -> usize {
        self.jobs.len()
    }

    /// Whether the batch contains no jobs.
    ///
    /// ```
    /// assert!(letterxpress::CancellationBatch::new().is_empty());
    /// ```
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.jobs.is_empty()
    }

    /// Iterates in insertion order, which is also wire order.
    ///
    /// ```
    /// let batch = letterxpress::CancellationBatch::new();
    /// assert_eq!(batch.iter().count(), 0);
    /// ```
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &CancellationJob> {
        self.jobs.iter()
    }
}

/// The result of trying to cancel one job.
///
/// ```
/// use letterxpress::{CancellationOutcome, Deletion};
///
/// let deleted = CancellationOutcome::Deleted(Deletion {
///     message: "Print job deleted successfully".into(),
/// });
/// assert!(deleted.is_deleted());
/// assert_eq!(deleted.message(), Some("Print job deleted successfully"));
/// ```
#[derive(Debug)]
#[non_exhaustive]
pub enum CancellationOutcome {
    /// The provider confirmed the deletion.
    Deleted(Deletion),
    /// This job failed. Later independent jobs in the batch were still tried.
    Failed(Error),
    /// The local fifteen-minute deadline had passed, so no request was sent
    /// for this job.
    WindowClosed {
        /// The monotonic transmission instant supplied by the caller.
        transmitted_at: Instant,
        /// The exclusive end of the provider's deletion window.
        deadline: Instant,
    },
}

impl CancellationOutcome {
    /// Whether the provider confirmed this deletion.
    ///
    /// ```
    /// use letterxpress::CancellationOutcome;
    ///
    /// assert!(!CancellationOutcome::WindowClosed {
    ///     transmitted_at: std::time::Instant::now(),
    ///     deadline: std::time::Instant::now(),
    /// }
    /// .is_deleted());
    /// ```
    #[must_use]
    pub fn is_deleted(&self) -> bool {
        matches!(self, Self::Deleted(_))
    }

    /// The provider's success message, when the deletion succeeded.
    ///
    /// ```
    /// use letterxpress::{CancellationOutcome, Deletion};
    ///
    /// let deleted = CancellationOutcome::Deleted(Deletion { message: "deleted".into() });
    /// assert_eq!(deleted.message(), Some("deleted"));
    /// ```
    #[must_use]
    pub fn message(&self) -> Option<&str> {
        match self {
            Self::Deleted(deletion) => Some(&deletion.message),
            _ => None,
        }
    }

    /// The typed per-job failure, when this job failed at the provider.
    ///
    /// ```
    /// use letterxpress::{CancellationOutcome, Error};
    ///
    /// let failed = CancellationOutcome::Failed(Error::NotFound { message: "gone".into() });
    /// assert_eq!(failed.error().and_then(Error::status), Some(404));
    /// ```
    #[must_use]
    pub fn error(&self) -> Option<&Error> {
        match self {
            Self::Failed(error) => Some(error),
            _ => None,
        }
    }
}

/// One job's cancellation outcome, paired with the batch entry it belongs to.
///
/// ```
/// use std::time::Instant;
///
/// use letterxpress::{CancellationBatch, CancellationJob, Client, Credentials, JobId, Mode};
/// use letterxpress::transport::MockTransport;
///
/// # async fn demo() -> Result<(), letterxpress::Error> {
/// let transmitted_at = Instant::now() - std::time::Duration::from_secs(901);
/// let mut batch = CancellationBatch::new();
/// batch.push(CancellationJob::print(JobId::new("1")?, transmitted_at)?)?;
/// let client = Client::new(MockTransport::default(), Credentials::new("u", "k", Mode::Test)?);
/// let report = client.cancel_batch(&batch).await;
/// assert_eq!(report.results()[0].id().as_str(), "1");
/// assert!(!report.results()[0].outcome().is_deleted());
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct JobCancellation {
    job: CancellationJob,
    outcome: CancellationOutcome,
}

impl JobCancellation {
    pub(crate) fn new(job: CancellationJob, outcome: CancellationOutcome) -> Self {
        Self { job, outcome }
    }

    /// The batch entry this result belongs to.
    ///
    /// ```
    /// use letterxpress::{CancellationBatch, Client, Credentials, Mode};
    /// use letterxpress::transport::MockTransport;
    ///
    /// # async fn demo() -> Result<(), letterxpress::Error> {
    /// let client = Client::new(MockTransport::default(), Credentials::new("u", "k", Mode::Test)?);
    /// let report = client.cancel_batch(&CancellationBatch::new()).await;
    /// assert!(report.results().is_empty());
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn job(&self) -> &CancellationJob {
        &self.job
    }

    /// The provider job id.
    ///
    /// ```
    /// use std::time::Instant;
    /// use letterxpress::{CancellationBatch, CancellationJob, Client, Credentials, JobId, Mode};
    /// use letterxpress::transport::MockTransport;
    ///
    /// # async fn demo() -> Result<(), letterxpress::Error> {
    /// let mut batch = CancellationBatch::new();
    /// batch.push(CancellationJob::email(JobId::new("1")?, Instant::now())?)?;
    /// let client = Client::new(MockTransport::default(), Credentials::new("u", "k", Mode::Test)?);
    /// let report = client.cancel_batch(&batch).await;
    /// assert_eq!(report.results()[0].id().as_str(), "1");
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn id(&self) -> &JobId {
        self.job.id()
    }

    /// The typed outcome for this job.
    ///
    /// ```
    /// use letterxpress::{CancellationBatch, Client, Credentials, Mode};
    /// use letterxpress::transport::MockTransport;
    ///
    /// # async fn demo() -> Result<(), letterxpress::Error> {
    /// let client = Client::new(MockTransport::default(), Credentials::new("u", "k", Mode::Test)?);
    /// let report = client.cancel_batch(&CancellationBatch::new()).await;
    /// assert!(report.results().is_empty());
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn outcome(&self) -> &CancellationOutcome {
        &self.outcome
    }
}

/// Per-job results for a bounded cancellation batch, in input order.
///
/// ```
/// use letterxpress::{CancellationBatch, Client, Credentials, Mode};
/// use letterxpress::transport::MockTransport;
///
/// # async fn demo() -> Result<(), letterxpress::Error> {
/// let client = Client::new(MockTransport::default(), Credentials::new("u", "k", Mode::Test)?);
/// let report = client.cancel_batch(&CancellationBatch::new()).await;
/// assert!(report.results().is_empty());
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct CancellationReport {
    results: Vec<JobCancellation>,
}

impl CancellationReport {
    pub(crate) fn new(results: Vec<JobCancellation>) -> Self {
        Self { results }
    }

    /// Results in the same order as the input batch.
    ///
    /// ```
    /// use letterxpress::{CancellationBatch, Client, Credentials, Mode};
    /// use letterxpress::transport::MockTransport;
    ///
    /// # async fn demo() -> Result<(), letterxpress::Error> {
    /// let client = Client::new(MockTransport::default(), Credentials::new("u", "k", Mode::Test)?);
    /// let batch = CancellationBatch::new();
    /// let report = client.cancel_batch(&batch).await;
    /// assert_eq!(report.results().len(), batch.len());
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn results(&self) -> &[JobCancellation] {
        &self.results
    }

    /// Whether the batch contained no jobs.
    ///
    /// ```
    /// use letterxpress::{CancellationBatch, Client, Credentials, Mode};
    /// use letterxpress::transport::MockTransport;
    ///
    /// # async fn demo() -> Result<(), letterxpress::Error> {
    /// let client = Client::new(MockTransport::default(), Credentials::new("u", "k", Mode::Test)?);
    /// assert!(client.cancel_batch(&CancellationBatch::new()).await.is_empty());
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.results.is_empty()
    }

    /// Number of per-job results.
    ///
    /// ```
    /// use letterxpress::{CancellationBatch, Client, Credentials, Mode};
    /// use letterxpress::transport::MockTransport;
    ///
    /// # async fn demo() -> Result<(), letterxpress::Error> {
    /// let client = Client::new(MockTransport::default(), Credentials::new("u", "k", Mode::Test)?);
    /// assert_eq!(client.cancel_batch(&CancellationBatch::new()).await.len(), 0);
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn len(&self) -> usize {
        self.results.len()
    }

    /// Number of jobs the provider confirmed as deleted.
    ///
    /// ```
    /// use letterxpress::{CancellationBatch, Client, Credentials, Mode};
    /// use letterxpress::transport::MockTransport;
    ///
    /// # async fn demo() -> Result<(), letterxpress::Error> {
    /// let client = Client::new(MockTransport::default(), Credentials::new("u", "k", Mode::Test)?);
    /// assert_eq!(client.cancel_batch(&CancellationBatch::new()).await.deleted(), 0);
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn deleted(&self) -> usize {
        self.results
            .iter()
            .filter(|result| result.outcome.is_deleted())
            .count()
    }

    /// Number of jobs that failed at the provider.
    ///
    /// ```
    /// use letterxpress::{CancellationBatch, Client, Credentials, Mode};
    /// use letterxpress::transport::MockTransport;
    ///
    /// # async fn demo() -> Result<(), letterxpress::Error> {
    /// let client = Client::new(MockTransport::default(), Credentials::new("u", "k", Mode::Test)?);
    /// assert_eq!(client.cancel_batch(&CancellationBatch::new()).await.failed(), 0);
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn failed(&self) -> usize {
        self.results
            .iter()
            .filter(|result| result.outcome.error().is_some())
            .count()
    }

    /// Number of jobs skipped because their fifteen-minute window had closed.
    ///
    /// ```
    /// use letterxpress::{CancellationBatch, Client, Credentials, Mode};
    /// use letterxpress::transport::MockTransport;
    ///
    /// # async fn demo() -> Result<(), letterxpress::Error> {
    /// let client = Client::new(MockTransport::default(), Credentials::new("u", "k", Mode::Test)?);
    /// assert_eq!(client.cancel_batch(&CancellationBatch::new()).await.window_closed(), 0);
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn window_closed(&self) -> usize {
        self.results
            .iter()
            .filter(|result| matches!(result.outcome, CancellationOutcome::WindowClosed { .. }))
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(value: &str) -> JobId {
        JobId::new(value).expect("valid id")
    }

    #[test]
    fn window_is_inclusive_at_the_start_and_exclusive_at_the_end() {
        let start = Instant::now();
        let job = CancellationJob::print(id("1"), start).expect("job");

        assert!(job.is_in_window_at(start));
        assert!(job.is_in_window_at(start + Duration::from_secs(899)));
        assert!(!job.is_in_window_at(start + CANCELLATION_WINDOW));
        assert!(!job.is_in_window_at(
            start
                .checked_sub(Duration::from_secs(1))
                .expect("an instant before now")
        ));
    }

    #[test]
    fn duplicate_ids_are_rejected_per_kind() {
        let start = Instant::now();
        let mut batch = CancellationBatch::new();
        batch
            .push(CancellationJob::print(id("1"), start).expect("job"))
            .expect("first");
        assert!(batch
            .push(CancellationJob::print(id("1"), start).expect("job"))
            .is_err());
        assert!(batch
            .push(CancellationJob::email(id("1"), start).expect("job"))
            .is_ok());
    }
}
