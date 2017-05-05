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

//! Configuration for a Habitat JobSrv Worker

use std::net::{IpAddr, Ipv4Addr};

use hab_core::config::ConfigFile;

use error::Error;

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Token for authenticating with the public builder-api
    pub auth_token: String,
    /// Filepath where persistent application data is stored
    pub data_path: String,
    /// List of Job Servers to connect to
    pub jobsrv: JobSrvCfg,
}

impl Config {
    pub fn jobsrv_addrs(&self) -> Vec<(String, String, String)> {
        let mut addrs = vec![];
        for job_server in &self.jobsrv {
            let hb = format!("tcp://{}:{}", job_server.host, job_server.heartbeat);
            let queue = format!("tcp://{}:{}", job_server.host, job_server.port);
            let log = format!("tcp://{}:{}", job_server.host, job_server.log_port);
            addrs.push((hb, queue, log));
        }
        addrs
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            auth_token: "".to_string(),
            data_path: "/tmp".to_string(),
            jobsrv: vec![JobSrvAddr::default()],
        }
    }
}

impl ConfigFile for Config {
    type Error = Error;
}

pub type JobSrvCfg = Vec<JobSrvAddr>;

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct JobSrvAddr {
    pub host: IpAddr,
    pub port: u16,
    pub heartbeat: u16,
    pub log_port: u16,
}

impl Default for JobSrvAddr {
    fn default() -> Self {
        JobSrvAddr {
            host: IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
            port: 5566,
            heartbeat: 5567,
            log_port: 5569
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_from_file() {
        let content = r#"
        auth_token = "mytoken"
        data_path = "/path/to/data"

        [[jobsrv]]
        host = "1:1:1:1:1:1:1:1"
        port = 9000
        heartbeat = 9001

        [[jobsrv]]
        host = "2.2.2.2"
        port = 9000
        "#;

        let config = Config::from_raw(&content).unwrap();
        assert_eq!(&config.auth_token, "mytoken");
        assert_eq!(&config.data_path, "/path/to/data");
        assert_eq!(&format!("{}", config.jobsrv[0].host), "1:1:1:1:1:1:1:1");
        assert_eq!(config.jobsrv[0].port, 9000);
        assert_eq!(config.jobsrv[0].heartbeat, 9001);
        assert_eq!(&format!("{}", config.jobsrv[1].host), "2.2.2.2");
        assert_eq!(config.jobsrv[1].port, 9000);
        assert_eq!(config.jobsrv[1].heartbeat, 5567);
    }
}
