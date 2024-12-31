use ffmpeg_cluster_common::models::messages::{JobConfig, JobInfo, JobStatus};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::info;
use uuid::Uuid;

#[derive(Debug)]
pub struct Job {
    pub info: JobInfo,
    pub file_path: PathBuf,
    pub config: JobConfig,
}

pub struct JobQueue {
    jobs: HashMap<String, Job>,
    queue: Vec<String>, // Job IDs in order
}

impl JobQueue {
    pub fn new() -> Self {
        Self {
            jobs: HashMap::new(),
            queue: Vec::new(),
        }
    }

    pub fn list_jobs(&self) -> Vec<JobInfo> {
        info!("Listing jobs. Current queue size: {}", self.jobs.len());
        let mut jobs: Vec<JobInfo> = self.jobs.values().map(|job| job.info.clone()).collect();

        // Sort by creation time, most recent first
        jobs.sort_by(|a, b| b.created_at.cmp(&a.created_at));

        for job in &jobs {
            info!(
                "Job {}: {:?} (Progress: {}%)",
                job.job_id, job.status, job.progress
            );
        }

        jobs
    }

    pub fn update_progress(
        &mut self,
        job_id: &str,
        completed_segments: usize,
        total_segments: usize,
    ) {
        if let Some(job) = self.jobs.get_mut(job_id) {
            if total_segments > 0 {
                job.info.progress = (completed_segments as f32 / total_segments as f32) * 100.0;
                info!("Updated job {} progress: {:.1}%", job_id, job.info.progress);
            }
        }
    }

    pub fn get_next_job(&mut self) -> Option<&mut Job> {
        if let Some(job_id) = self.queue.first() {
            info!("Getting next job: {}", job_id);
            self.jobs.get_mut(job_id)
        } else {
            info!("No jobs in queue");
            None
        }
    }

    pub fn add_job(&mut self, file_path: PathBuf, config: JobConfig) -> String {
        let job_id = Uuid::new_v4().to_string();
        info!("Adding new job {} for file {:?}", job_id, file_path);

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let job_info = JobInfo {
            job_id: job_id.clone(),
            file_name: file_path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string(),
            status: JobStatus::Queued,
            progress: 0.0,
            created_at: now,
            completed_at: None,
            error: None,
        };

        let job = Job {
            info: job_info,
            file_path,
            config,
        };

        self.jobs.insert(job_id.clone(), job);
        self.queue.push(job_id.clone());

        info!(
            "Added job {} to queue. Queue size: {}",
            job_id,
            self.queue.len()
        );
        job_id
    }

    pub fn mark_job_started(&mut self, job_id: &str) {
        if let Some(job) = self.jobs.get_mut(job_id) {
            job.info.status = JobStatus::WaitingForClients;
            info!("Job {} status updated to WaitingForClients", job_id);
        }
    }

    pub fn mark_job_completed(&mut self, job_id: &str) {
        if let Some(job) = self.jobs.get_mut(job_id) {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs();

            job.info.status = JobStatus::Completed;
            job.info.completed_at = Some(now);
            job.info.progress = 100.0;

            // Remove from queue
            if let Some(pos) = self.queue.iter().position(|x| x == job_id) {
                self.queue.remove(pos);
            }

            info!("Job {} marked as completed", job_id);
        }
    }

    pub fn mark_job_failed(&mut self, job_id: &str, error: String) {
        if let Some(job) = self.jobs.get_mut(job_id) {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs();

            job.info.status = JobStatus::Failed;
            job.info.completed_at = Some(now);
            job.info.error = Some(error);

            // Remove from queue
            if let Some(pos) = self.queue.iter().position(|x| x == job_id) {
                self.queue.remove(pos);
            }

            info!("Job {} marked as failed", job_id);
        }
    }

    pub fn cancel_job(&mut self, job_id: &str) -> bool {
        if let Some(job) = self.jobs.get_mut(job_id) {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs();

            job.info.status = JobStatus::Cancelled;
            job.info.completed_at = Some(now);

            // Remove from queue
            if let Some(pos) = self.queue.iter().position(|x| x == job_id) {
                self.queue.remove(pos);
                info!("Job {} cancelled", job_id);
                return true;
            }
        }
        false
    }

    pub fn update_job_progress(&mut self, job_id: &str, progress: f32) {
        if let Some(job) = self.jobs.get_mut(job_id) {
            job.info.progress = progress;
        }
    }

    pub fn get_job(&self, job_id: &str) -> Option<&Job> {
        self.jobs.get(job_id)
    }
}
