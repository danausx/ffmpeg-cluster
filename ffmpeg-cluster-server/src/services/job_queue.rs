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
    pub format: String,
}

pub struct JobQueue {
    jobs: HashMap<String, Job>,
    queue: Vec<String>, // Job IDs in order
    active_job: Option<String>,
}

impl JobQueue {
    pub fn new() -> Self {
        Self {
            jobs: HashMap::new(),
            queue: Vec::new(),
            active_job: None,
        }
    }

    pub fn list_jobs(&self) -> Vec<JobInfo> {
        info!("Listing jobs. Current queue size: {}", self.jobs.len());
        let mut jobs: Vec<JobInfo> = self.jobs.values().map(|job| job.info.clone()).collect();
        jobs.sort_by(|a, b| b.created_at.cmp(&a.created_at));

        for job in &jobs {
            info!(
                "Job {}: {:?} (Progress: {}%)",
                job.job_id, job.status, job.progress
            );
        }

        jobs
    }

    pub fn add_job(&mut self, file_path: PathBuf, config: JobConfig, format: String) -> String {
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
            format,
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

    pub fn get_next_job(&mut self) -> Option<&Job> {
        // Only return the next job if there's no active job
        if self.active_job.is_none() && !self.queue.is_empty() {
            let job_id = &self.queue[0];
            self.active_job = Some(job_id.clone());
            self.jobs.get(job_id)
        } else {
            None
        }
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

            // Remove completed job from queue and clear active job
            if let Some(pos) = self.queue.iter().position(|x| x == job_id) {
                self.queue.remove(pos);
            }
            if self.active_job.as_ref() == Some(&job_id.to_string()) {
                self.active_job = None;
            }

            info!(
                "Job {} completed. Remaining jobs in queue: {}",
                job_id,
                self.queue.len()
            );
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

            // Remove from queue and clear active job
            if let Some(pos) = self.queue.iter().position(|x| x == job_id) {
                self.queue.remove(pos);
            }
            if self.active_job.as_ref() == Some(&job_id.to_string()) {
                self.active_job = None;
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

            // Remove from queue and clear active job
            if let Some(pos) = self.queue.iter().position(|x| x == job_id) {
                self.queue.remove(pos);
                if self.active_job.as_ref() == Some(&job_id.to_string()) {
                    self.active_job = None;
                }
                info!("Job {} cancelled", job_id);
                return true;
            }
        }
        false
    }

    pub fn notify_new_job(&mut self) -> Option<(String, JobConfig, String)> {
        if let Some(job) = self.queue.first().and_then(|id| self.jobs.get(id)) {
            Some((
                job.info.job_id.clone(),
                job.config.clone(),
                job.format.clone(),
            ))
        } else {
            None
        }
    }

    pub fn get_job(&self, job_id: &str) -> Option<&Job> {
        self.jobs.get(job_id)
    }

    pub fn get_job_mut(&mut self, job_id: &str) -> Option<&mut Job> {
        self.jobs.get_mut(job_id)
    }
}
