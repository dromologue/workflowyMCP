//! Background job queue for long-running operations.
//! Addresses: "Job queue timeouts", "Memory leak on job history".

use crate::config::JobQueueConfig;
use crate::error::{Result, WorkflowyError};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::sync::mpsc;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone)]
pub struct Job {
    pub id: String,
    pub status: JobStatus,
    pub created_at: SystemTime,
    pub completed_at: Option<SystemTime>,
    pub error: Option<String>,
    pub progress: Option<f64>,
}

pub struct JobQueue {
    config: JobQueueConfig,
    jobs: Arc<RwLock<HashMap<String, Job>>>,
    tx: mpsc::UnboundedSender<String>,
}

impl JobQueue {
    pub fn new(config: JobQueueConfig) -> (Self, mpsc::UnboundedReceiver<String>) {
        let (tx, rx) = mpsc::unbounded_channel();

        let queue = Self {
            config,
            jobs: Arc::new(RwLock::new(HashMap::new())),
            tx,
        };

        queue.start_cleanup_task();

        (queue, rx)
    }

    /// Submit a new job
    pub fn submit(&self) -> String {
        let job_id = Uuid::new_v4().to_string();

        let job = Job {
            id: job_id.clone(),
            status: JobStatus::Pending,
            created_at: SystemTime::now(),
            completed_at: None,
            error: None,
            progress: None,
        };

        self.jobs.write().insert(job_id.clone(), job);
        let _ = self.tx.send(job_id.clone());

        job_id
    }

    /// Get job status
    pub fn get_status(&self, job_id: &str) -> Option<JobStatus> {
        self.jobs
            .read()
            .get(job_id)
            .map(|j| j.status.clone())
    }

    /// Get full job details
    pub fn get_job(&self, job_id: &str) -> Option<Job> {
        self.jobs.read().get(job_id).cloned()
    }

    /// Update job progress (0.0 to 1.0)
    pub fn update_progress(&self, job_id: &str, progress: f64) -> Result<()> {
        let mut jobs = self.jobs.write();
        jobs.get_mut(job_id)
            .ok_or_else(|| WorkflowyError::JobFailed {
                job_id: job_id.to_string(),
                message: "Job not found".to_string(),
            })
            .map(|job| {
                job.progress = Some(progress.clamp(0.0, 1.0));
            })
    }

    /// Mark job as completed successfully
    pub fn complete(&self, job_id: &str) {
        if let Some(job) = self.jobs.write().get_mut(job_id) {
            job.status = JobStatus::Completed;
            job.completed_at = Some(SystemTime::now());
            job.progress = Some(1.0);
        }
    }

    /// Mark job as failed
    pub fn fail(&self, job_id: &str, error: impl Into<String>) {
        if let Some(job) = self.jobs.write().get_mut(job_id) {
            job.status = JobStatus::Failed;
            job.completed_at = Some(SystemTime::now());
            job.error = Some(error.into());
        }
    }

    /// Cancel a job
    pub fn cancel(&self, job_id: &str) {
        if let Some(job) = self.jobs.write().get_mut(job_id) {
            job.status = JobStatus::Cancelled;
            job.completed_at = Some(SystemTime::now());
        }
    }

    /// Start background cleanup task
    /// Addresses: "Memory leak on unbounded job history"
    fn start_cleanup_task(&self) {
        let jobs = Arc::clone(&self.jobs);
        let config = self.config.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(config.cleanup_interval_secs));

            loop {
                interval.tick().await;

                let mut jobs_map = jobs.write();

                // Find jobs to remove:
                // 1. Completed jobs older than TTL
                // 2. Keep only most recent max_job_history
                let now = SystemTime::now();
                let mut indices_to_remove = Vec::new();

                let completed_count = jobs_map
                    .values()
                    .filter(|j| matches!(j.status, JobStatus::Completed | JobStatus::Failed | JobStatus::Cancelled))
                    .count();

                if completed_count > config.max_job_history {
                    // Sort by completion time and keep only the most recent
                    let mut completed: Vec<_> = jobs_map
                        .iter()
                        .filter(|(_, j)| matches!(j.status, JobStatus::Completed | JobStatus::Failed | JobStatus::Cancelled))
                        .collect();

                    completed.sort_by_key(|(_, j)| j.completed_at.unwrap_or(j.created_at));

                    let remove_count = completed_count - config.max_job_history;
                    for (job_id, _) in completed.iter().take(remove_count) {
                        indices_to_remove.push(job_id.to_string());
                    }
                } else {
                    // Remove items older than TTL
                    for (job_id, job) in jobs_map.iter() {
                        if let Some(completed_at) = job.completed_at {
                            if let Ok(elapsed) = now.duration_since(completed_at) {
                                if elapsed.as_secs() > config.completed_job_ttl_secs {
                                    indices_to_remove.push(job_id.clone());
                                }
                            }
                        }
                    }
                }

                for job_id in indices_to_remove {
                    jobs_map.remove(&job_id);
                }
            }
        });
    }

    /// Get all jobs (for debugging/stats)
    pub fn list_jobs(&self) -> Vec<Job> {
        self.jobs.read().values().cloned().collect()
    }
}

impl Clone for JobQueue {
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
            jobs: Arc::clone(&self.jobs),
            tx: self.tx.clone(),
        }
    }
}

impl Clone for JobQueueConfig {
    fn clone(&self) -> Self {
        Self {
            completed_job_ttl_secs: self.completed_job_ttl_secs,
            max_job_history: self.max_job_history,
            cleanup_interval_secs: self.cleanup_interval_secs,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_job_lifecycle() {
        let config = JobQueueConfig::default();
        let (queue, _rx) = JobQueue::new(config);

        let job_id = queue.submit();
        assert_eq!(queue.get_status(&job_id), Some(JobStatus::Pending));

        queue.complete(&job_id);
        assert_eq!(queue.get_status(&job_id), Some(JobStatus::Completed));
    }

    #[tokio::test]
    async fn test_job_cleanup_enforces_max_history() {
        let config = JobQueueConfig {
            completed_job_ttl_secs: 10,
            max_job_history: 5,
            cleanup_interval_secs: 1,
        };
        let (queue, _rx) = JobQueue::new(config);

        // Create 10 jobs
        for _ in 0..10 {
            let job_id = queue.submit();
            queue.complete(&job_id);
        }

        assert_eq!(queue.list_jobs().len(), 10);

        // Wait for cleanup
        tokio::time::sleep(Duration::from_secs(2)).await;

        // Should now have <= 5 completed jobs (pending jobs stay)
        let completed = queue.list_jobs()
            .iter()
            .filter(|j| matches!(j.status, JobStatus::Completed))
            .count();
        assert!(completed <= 5);
    }
}
