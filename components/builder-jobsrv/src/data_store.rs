// Copyright (c) 2016-2017 Chef Software Inc. and/or applicable contributors
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! The PostgreSQL backend for the Jobsrv.

use db::pool::Pool;
use db::async::{AsyncServer, EventOutcome};
use db::migration::Migrator;
use db::error::{Error as DbError, Result as DbResult};
use protocol::{originsrv, jobsrv, scheduler};
use protocol::net::NetOk;
use postgres;

use config::Config;
use error::{Result, Error};
use hab_net::routing::Broker;

/// DataStore inherints being Send + Sync by virtue of having only one member, the pool itself.
#[derive(Debug, Clone)]
pub struct DataStore {
    pool: Pool,
    pub async: AsyncServer,
}

impl Drop for DataStore {
    fn drop(&mut self) {
        self.async.stop();
    }
}

impl DataStore {
    /// Create a new DataStore.
    ///
    /// * Can fail if the pool cannot be created
    /// * Blocks creation of the datastore on the existince of the pool; might wait indefinetly.
    pub fn new(config: &Config) -> Result<DataStore> {
        let pool = Pool::new(&config.datastore, config.shards.clone())?;
        let ap = pool.clone();
        Ok(DataStore {
               pool: pool,
               async: AsyncServer::new(ap),
           })
    }

    /// Create a new DataStore from a pre-existing pool; useful for testing the database.
    pub fn from_pool(pool: Pool) -> Result<DataStore> {
        let ap = pool.clone();
        Ok(DataStore {
               pool: pool,
               async: AsyncServer::new(ap),
           })
    }

    /// Setup the datastore.
    ///
    /// This includes all the schema and data migrations, along with stored procedures for data
    /// access.
    pub fn setup(&self) -> Result<()> {
        let conn = self.pool.get_raw()?;
        let xact = conn.transaction().map_err(Error::DbTransactionStart)?;
        let mut migrator = Migrator::new(xact, self.pool.shards.clone());

        migrator.setup()?;

        migrator
            .migrate("jobsrv", r#"CREATE SEQUENCE IF NOT EXISTS job_id_seq;"#)?;

        // The core jobs table
        migrator
            .migrate("jobsrv",
                     r#"CREATE TABLE jobs (
                                    id bigint PRIMARY KEY DEFAULT next_id_v1('job_id_seq'),
                                    owner_id bigint,
                                    job_state text,
                                    project_id bigint,
                                    project_name text,
                                    project_owner_id bigint,
                                    project_plan_path text,
                                    vcs text,
                                    vcs_arguments text[],
                                    net_error_code int,
                                    net_error_msg text,
                                    scheduler_sync bool DEFAULT false,
                                    created_at timestamptz DEFAULT now(),
                                    updated_at timestamptz
                             )"#)?;

        // Insert a new job into the jobs table
        migrator.migrate("jobsrv",
                             r#"CREATE OR REPLACE FUNCTION insert_job_v1 (
                                owner_id bigint,
                                project_id bigint,
                                project_name text,
                                project_owner_id bigint,
                                project_plan_path text,
                                vcs text,
                                vcs_arguments text[]
                                ) RETURNS SETOF jobs AS $$
                                    BEGIN
                                        RETURN QUERY INSERT INTO jobs (owner_id, job_state, project_id, project_name, project_owner_id, project_plan_path, vcs, vcs_arguments)
                                            VALUES (owner_id, 'Pending', project_id, project_name, project_owner_id, project_plan_path, vcs, vcs_arguments)
                                            RETURNING *;
                                        RETURN;
                                    END
                                $$ LANGUAGE plpgsql VOLATILE
                                "#)?;
        // Hey, Adam - why did you do `select *` here? Isn't that bad?
        //
        // So glad you asked. In this case, it's better - essentially we have an API call that
        // returns a job object, which is flattened into the table structure above. We then
        // translate those job rows into Job structs. Since the table design is purely additive,
        // this allows us to add data to the table without having to re-roll functions that
        // generate Job structs, and keeps things DRY.
        //
        // Just make sure you always address the columns by name, not by position.
        migrator
            .migrate("jobsrv",
                     r#"CREATE OR REPLACE FUNCTION get_job_v1 (jid bigint) RETURNS SETOF jobs AS $$
                            BEGIN
                              RETURN QUERY SELECT * FROM jobs WHERE id = jid;
                              RETURN;
                            END
                            $$ LANGUAGE plpgsql STABLE"#)?;

        // The pending_jobs function acts as an internal queue. It looks for jobs that are
        // 'Pending', and sorts them according to the time they were entered - first in, first out.
        //
        // You can pass this function a number of jobs to return - it will return up-to that many.
        //
        // The way it works internally - we select the rows for update, skipping rows that are
        // already locked. That means multiple jobsrvs, or multiple worker managers, can run in
        // parallel against the table - they will simply skip other rows that are currently on the
        // way out the door.
        //
        // Any row selected gets its state updated to "Dispatched" before being sent back, ensuring
        // that no other worker could receive the job. If we fail to dispatch the job, its the
        // callers job to change the jobs status back to "Pending", which puts it back in the
        // queue.
        //
        // Note that the sort order ensures that jobs that fail to dispatch and are then returned
        // will be the first job selected, making FIFO a reality.
        migrator.migrate("jobsrv",
                         r#"CREATE OR REPLACE FUNCTION pending_jobs_v1 (integer) RETURNS SETOF jobs AS
                                $$
                                DECLARE
                                    r jobs % rowtype;
                                BEGIN
                                    FOR r IN
                                        SELECT * FROM jobs
                                        WHERE job_state = 'Pending'
                                        ORDER BY created_at ASC
                                        FOR UPDATE SKIP LOCKED
                                        LIMIT $1
                                    LOOP
                                        UPDATE jobs SET job_state='Dispatched', updated_at=now() WHERE id=r.id RETURNING * INTO r;
                                        RETURN NEXT r;
                                    END LOOP;
                                  RETURN;
                                END
                                $$ LANGUAGE plpgsql VOLATILE"#)?;

        // Reset the state of Dispatched jobs back to Pending (for failure recovery)
        migrator.migrate("jobsrv",
                         r#"CREATE OR REPLACE FUNCTION reset_jobs_v1 () RETURNS void AS $$
                                BEGIN
                                    UPDATE jobs SET job_state='Pending', updated_at=now() WHERE job_state='Dispatched';
                                END
                                $$ LANGUAGE plpgsql VOLATILE"#)?;

        // Update the state of a job. Takes a job id and a state, and updates that row.
        migrator.migrate("jobsrv",
                         r#"CREATE OR REPLACE FUNCTION set_job_state_v1 (jid bigint, jstate text) RETURNS void AS $$
                            BEGIN
                                UPDATE jobs SET job_state=jstate, scheduler_sync=false, updated_at=now() WHERE id=jid;
                            END
                         $$ LANGUAGE plpgsql VOLATILE"#)?;

        // Helpers to sync job state notifications
        migrator
            .migrate("jobsrv",
                     r#"CREATE OR REPLACE FUNCTION sync_jobs_v1() RETURNS SETOF jobs AS $$
                         BEGIN
                             RETURN QUERY SELECT * FROM jobs WHERE scheduler_sync = false;
                             RETURN;
                         END
                         $$ LANGUAGE plpgsql STABLE"#)?;
        migrator.migrate("jobsrv",
                          r#"CREATE OR REPLACE FUNCTION set_jobs_sync_v1(in_job_id bigint) RETURNS VOID AS $$
                         BEGIN
                             UPDATE jobs SET scheduler_sync = true WHERE id = in_job_id;
                         END
                         $$ LANGUAGE plpgsql VOLATILE"#)?;

        migrator
            .migrate("jobsrv", r#"DROP INDEX IF EXISTS pending_jobs_index_v1;"#)?;
        migrator.migrate("jobsrv",
                         r#"CREATE INDEX pending_jobs_index_v1 on jobs(created_at) WHERE job_state = 'Pending'"#)?;

        migrator.migrate("jobsrv", r#"ALTER TABLE jobs ADD COLUMN IF NOT EXISTS log_url TEXT DEFAULT NULL "#)?;
        migrator.migrate("jobsrv",
                         r#"CREATE OR REPLACE FUNCTION set_log_url_v1(p_job_id BIGINT, p_url TEXT)
                         RETURNS VOID
                         LANGUAGE SQL VOLATILE as $$
                           UPDATE jobs
                           SET log_url = p_url
                           WHERE id = p_job_id;
                         $$"#)?;
        migrator.finish()?;

        self.async.register("sync_jobs".to_string(), sync_jobs);

        Ok(())
    }

    pub fn start_async(&self) {
        // This is an arc under the hood
        let async_thread = self.async.clone();
        async_thread.start(4);
    }

    /// Create a new job. Sets the state to Pending.
    ///
    /// # Errors
    ///
    /// * If the pool has no connections available
    /// * If the job cannot be created
    /// * If the job has an unknown VCS type
    pub fn create_job(&self, job: &jobsrv::Job) -> Result<jobsrv::Job> {
        let conn = self.pool.get_shard(0)?;

        if job.get_project().get_vcs_type() == "git" {
            let project = job.get_project();

            let rows = conn.query("SELECT * FROM insert_job_v1($1, $2, $3, $4, $5, $6, $7)",
                                  &[&(job.get_owner_id() as i64),
                                    &(project.get_id() as i64),
                                    &project.get_name(),
                                    &(project.get_owner_id() as i64),
                                    &project.get_plan_path(),
                                    &project.get_vcs_type(),
                                    &vec![project.get_vcs_data()]])
                .map_err(Error::JobCreate)?;
            let job = row_to_job(&rows.get(0))?;
            return Ok(job);
        } else {
            return Err(Error::UnknownVCS);
        }
    }

    /// Get a job from the database. If the job does not exist, but the database was active, we'll
    /// get a None result.
    ///
    /// # Errors
    ///
    /// * If a connection cannot be gotten from the pool
    /// * If the job cannot be selected from the database
    pub fn get_job(&self, get_job: &jobsrv::JobGet) -> Result<Option<jobsrv::Job>> {
        let conn = self.pool.get_shard(0)?;
        let rows = &conn.query("SELECT * FROM get_job_v1($1)",
                               &[&(get_job.get_id() as i64)])
                        .map_err(Error::JobGet)?;
        for row in rows {
            let job = row_to_job(&row)?;
            return Ok(Some(job));
        }
        Ok(None)
    }

    /// Get a list of pending jobs, up to a maximum count of jobs.
    ///
    /// # Errors
    ///
    /// * If a connection cannot be gotten from the pool
    /// * If the pending jobs cannot be selected from the database
    /// * If the row returned cannot be translated into a Job
    pub fn pending_jobs(&self, count: i32) -> Result<Vec<jobsrv::Job>> {
        let mut jobs = Vec::new();
        let conn = self.pool.get_shard(0)?;
        let rows = &conn.query("SELECT * FROM pending_jobs_v1($1)", &[&count])
                        .map_err(Error::JobPending)?;
        for row in rows {
            let job = row_to_job(&row)?;
            jobs.push(job);
        }
        Ok(jobs)
    }

    /// Reset any Dispatched jobs back to Pending state
    /// This is used for recovery scenario
    ///
    /// # Errors
    /// * If a connection cannot be gotten from the pool
    /// * If the dispatched jobs cannot be selected from the database
    pub fn reset_jobs(&self) -> Result<()> {
        let conn = self.pool.get_shard(0)?;
        conn.query("SELECT reset_jobs_v1()", &[])
            .map_err(Error::JobReset)?;
        Ok(())
    }

    /// Set the state of a job. If the job does not exist in the database, its basically a no-op.
    ///
    /// # Errors
    ///
    /// * If a connection cannot be gotten from the pool
    /// * If the jobs state cannot be updated in the database
    pub fn set_job_state(&self, job: &jobsrv::Job) -> Result<()> {
        let conn = self.pool.get_shard(0)?;
        let job_id = job.get_id() as i64;
        let job_state = match job.get_state() {
            jobsrv::JobState::Dispatched => "Dispatched",
            jobsrv::JobState::Pending => "Pending",
            jobsrv::JobState::Processing => "Processing",
            jobsrv::JobState::Complete => "Complete",
            jobsrv::JobState::Rejected => "Rejected",
            jobsrv::JobState::Failed => "Failed",
        };
        conn.execute("SELECT set_job_state_v1($1, $2)", &[&job_id, &job_state])
            .map_err(Error::JobSetState)?;

        self.async.schedule("sync_jobs")?;

        Ok(())
    }

    /// Once all the logs have been collected for a job, we'll ship it
    /// off somewhere else for long-term storage (e.g., S3 or a local
    /// clone thereof). Once we do, this will serve as a pointer to
    /// that remote content.
    pub fn set_log_url(&self, job_id: u64, url: &str) -> Result<()> {
        let conn = self.pool.get_shard(0)?;
        conn.execute("SELECT set_log_url_v1($1, $2)",
                     &[&(job_id as i64), &url])
            .map_err(Error::JobSetLogUrl)?;
        Ok(())
    }
}

/// Translate a database `jobs` row to a `jobsrv::Job`.
///
/// # Errors
///
/// * If the job state is unknown
/// * If the VCS type is unknown
fn row_to_job(row: &postgres::rows::Row) -> Result<jobsrv::Job> {
    let mut job = jobsrv::Job::new();
    let id: i64 = row.get("id");
    job.set_id(id as u64);
    let owner_id: i64 = row.get("owner_id");
    job.set_owner_id(owner_id as u64);
    let js: String = row.get("job_state");
    let job_state = match &js[..] {
        "Dispatched" => jobsrv::JobState::Dispatched,
        "Pending" => jobsrv::JobState::Pending,
        "Processing" => jobsrv::JobState::Processing,
        "Complete" => jobsrv::JobState::Complete,
        "Rejected" => jobsrv::JobState::Rejected,
        "Failed" => jobsrv::JobState::Failed,
        _ => return Err(Error::UnknownJobState),
    };
    job.set_state(job_state);

    let mut project = originsrv::OriginProject::new();
    let project_id: i64 = row.get("project_id");
    project.set_id(project_id as u64);
    project.set_name(row.get("project_name"));
    let project_owner_id: i64 = row.get("project_owner_id");
    project.set_owner_id(project_owner_id as u64);
    project.set_plan_path(row.get("project_plan_path"));

    let rvcs: String = row.get("vcs");
    match rvcs.as_ref() {
        "git" => {
            let mut vcsa: Vec<String> = row.get("vcs_arguments");
            project.set_vcs_type(String::from("git"));
            project.set_vcs_data(vcsa.remove(0));
        }
        e => {
            error!("Unknown VCS, {}", e);
            return Err(Error::UnknownVCS);
        }
    }
    job.set_project(project);

    // A log URL can be null (e.g., the job may be running right now).
    //
    // Note that it's possible for the job itself to be marked as
    // "Complete" and to have no log URL set yet; log processing can
    // lag behind job execution.
    if let Some(Ok(url)) = row.get_opt::<&str, String>("log_url") {
        job.set_log_url(url);
    }

    Ok(job)
}

fn sync_jobs(pool: Pool) -> DbResult<EventOutcome> {
    let mut result = EventOutcome::Finished;
    for shard in pool.shards.iter() {
        let conn = pool.get_shard(*shard)?;
        let rows = &conn.query("SELECT * FROM sync_jobs_v1()", &[])
                        .map_err(DbError::AsyncFunctionCheck)?;
        if rows.len() > 0 {
            let mut bconn = Broker::connect()?;
            let mut request = scheduler::JobStatus::new();
            for row in rows.iter() {
                let job = match row_to_job(&row) {
                    Ok(job) => job,
                    Err(e) => {
                        warn!("Failed to convert row to job {}", e);
                        return Ok(EventOutcome::Retry);
                    }
                };
                let id = job.get_id();
                request.set_job(job);
                match bconn.route::<scheduler::JobStatus, NetOk>(&request) {
                    Ok(_) => {
                        conn.query("SELECT * FROM set_jobs_sync_v1($1)", &[&(id as i64)])
                            .map_err(DbError::AsyncFunctionUpdate)?;
                        debug!("Updated scheduler service with job status, {:?}", request);
                    }
                    Err(e) => {
                        warn!("Failed to sync job status with the scheduler service, {:?}: {}",
                              request,
                              e);
                        result = EventOutcome::Retry;
                    }
                }
            }
        }
    }
    Ok(result)
}
